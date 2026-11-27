//! `state path | show | reset`: look at and delete the persisted state of a station.
//!
//! Paths come from the same store code as the station uses, so an id is sanitized the same
//! way and nothing outside the state directory is ever touched. `show` reads the state file
//! only (never the secrets file, never moves anything aside); `reset` deletes the station's
//! own files only.

use std::fmt::Write as _;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kirby_ocpp_core::config::default_state_dir;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::scenario::SCHEMA_VERSION;
use kirby_ocpp_core::station2::{Reservation, StoredProfile2};
use kirby_ocpp_core::store::{
    FileKind, Inspection, Inspection2, PersistedConnector, PersistedStation, PersistedStation2,
    STORE_VERSION, STORE2_VERSION, StationStore, StoreError,
};
use serde_json::{Value, json};

use crate::cli::StateCmd;
use crate::headless::stdout_line;
use crate::settings::absolute_text;
use crate::usage_error;

/// What `state` needs from the command line.
pub struct StateOptions {
    pub id: String,
    /// Whether the id was given (flag, environment variable or configuration file).
    pub id_given: bool,
    pub dir: Option<PathBuf>,
    pub json: bool,
    /// The protocol whose state file the command works on. The state file of the other
    /// protocol family is never read or deleted.
    pub protocol: Protocol,
}

/// The store of the file kind that belongs to the selected protocol.
fn store_for(dir: &Path, options: &StateOptions) -> StationStore {
    StationStore::new_for(FileKind::for_protocol(options.protocol), dir, &options.id)
}

pub fn run(action: &StateCmd, options: &StateOptions) -> ExitCode {
    let Some(dir) = options.dir.clone().or_else(default_state_dir) else {
        return usage_error(
            "cannot determine the state directory (no home directory): use --state-dir",
        );
    };
    match action {
        StateCmd::Path => path(&dir, options),
        StateCmd::Show | StateCmd::Reset { .. } if !options.id_given => usage_error(
            "`state show` and `state reset` need the station: use --id (or set OCPP_CHARGE_POINT_ID)",
        ),
        StateCmd::Show => show(&store_for(&dir, options), options),
        StateCmd::Reset { yes } => reset(&store_for(&dir, options), options, *yes),
    }
}

fn print_json(value: &Value) {
    stdout_line(&value.to_string(), true);
}

fn path(dir: &Path, options: &StateOptions) -> ExitCode {
    let store = options.id_given.then(|| store_for(dir, options));
    let dir_text = absolute_text(dir);
    if options.json {
        print_json(&json!({
            "schema": SCHEMA_VERSION,
            "protocol": options.protocol.token(),
            "dir": dir_text,
            "station": store.as_ref().map(|_| options.id.as_str()),
            "file": store.as_ref().map(|s| absolute_text(s.path())),
            "secrets_file": store.as_ref().map(|s| absolute_text(&s.secrets_path())),
        }));
    } else if let Some(store) = store {
        stdout_line(&absolute_text(store.path()), false);
    } else {
        stdout_line(&dir_text, false);
    }
    ExitCode::SUCCESS
}

fn store_error(error: &StoreError) -> ExitCode {
    eprintln!("error: {error}");
    ExitCode::FAILURE
}

/// Reports a missing or unusable state file (`reason` is set for an unusable one).
fn report_unavailable(
    options: &StateOptions,
    file: &str,
    secrets: bool,
    reason: Option<&str>,
) -> ExitCode {
    if options.json {
        let mut value = json!({
            "schema": SCHEMA_VERSION,
            "protocol": options.protocol.token(),
            "station": options.id,
            "file": file,
            "status": if reason.is_some() { "unusable" } else { "missing" },
            "secrets_file": secrets,
            "state": null,
        });
        if let (Some(reason), Some(map)) = (reason, value.as_object_mut()) {
            map.insert("reason".to_owned(), json!(reason));
        }
        print_json(&value);
    } else if let Some(reason) = reason {
        eprintln!(
            "error: the state file {file} is unusable: {reason} (it was left as it is; a run of the station moves it to a .bak file)"
        );
    } else {
        eprintln!("no saved state for station `{}` ({file})", options.id);
    }
    ExitCode::FAILURE
}

fn show(store: &StationStore, options: &StateOptions) -> ExitCode {
    let file = absolute_text(store.path());
    let secrets = store.has_secrets();
    if store.kind() == FileKind::V2 {
        return show2(store, options, &file, secrets);
    }
    let state = match store.inspect() {
        Ok(Inspection::Loaded(state)) => *state,
        Ok(Inspection::Missing) => {
            if options.json {
                print_json(&json!({
                    "schema": SCHEMA_VERSION,
                    "protocol": options.protocol.token(),
                    "station": options.id,
                    "file": file,
                    "status": "missing",
                    "secrets_file": secrets,
                    "state": null,
                }));
            } else {
                eprintln!("no saved state for station `{}` ({file})", options.id);
            }
            return ExitCode::FAILURE;
        }
        Ok(Inspection::Unusable { reason }) => {
            if options.json {
                print_json(&json!({
                    "schema": SCHEMA_VERSION,
                    "protocol": options.protocol.token(),
                    "station": options.id,
                    "file": file,
                    "status": "unusable",
                    "reason": reason,
                    "secrets_file": secrets,
                    "state": null,
                }));
            } else {
                eprintln!(
                    "error: the state file {file} is unusable: {reason} (it was left as it is; a run of the station moves it to a .bak file)"
                );
            }
            return ExitCode::FAILURE;
        }
        Ok(_) => {
            eprintln!("error: the state file {file} cannot be shown by this build");
            return ExitCode::FAILURE;
        }
        Err(error) => return store_error(&error),
    };
    if options.json {
        print_json(&json!({
            "schema": SCHEMA_VERSION,
            "protocol": options.protocol.token(),
            "station": options.id,
            "file": file,
            "status": "found",
            "store_version": state.version,
            "supported_store_version": STORE_VERSION,
            "secrets_file": secrets,
            "state": state_json(&state),
        }));
    } else {
        for line in state_text(&options.id, &file, secrets, &state).lines() {
            stdout_line(line, false);
        }
    }
    ExitCode::SUCCESS
}

/// `state show` for an OCPP 2.x state file. The secrets file is never read: only its
/// presence is reported.
fn show2(store: &StationStore, options: &StateOptions, file: &str, secrets: bool) -> ExitCode {
    let state = match store.inspect2() {
        Ok(Inspection2::Loaded(state)) => *state,
        Ok(Inspection2::Missing) => return report_unavailable(options, file, secrets, None),
        Ok(Inspection2::Unusable { reason }) => {
            return report_unavailable(options, file, secrets, Some(&reason));
        }
        Ok(_) => {
            eprintln!("error: the state file {file} cannot be shown by this build");
            return ExitCode::FAILURE;
        }
        Err(error) => return store_error(&error),
    };
    if options.json {
        print_json(&json!({
            "schema": SCHEMA_VERSION,
            "protocol": options.protocol.token(),
            "station": options.id,
            "file": file,
            "status": "found",
            "store_version": state.version,
            "supported_store_version": STORE2_VERSION,
            "secrets_file": secrets,
            "state": state2_json(&state),
        }));
    } else {
        for line in state2_text(&options.id, file, secrets, &state).lines() {
            stdout_line(line, false);
        }
    }
    ExitCode::SUCCESS
}

/// The wire text of a serializable enum (`"Charging"`), or `-` when it is not a string.
fn enum_text(value: serde_json::Result<Value>) -> String {
    match value {
        Ok(Value::String(text)) => text,
        _ => "-".to_owned(),
    }
}

fn state2_json(state: &PersistedStation2) -> Value {
    let evses: Vec<Value> = state
        .evse_availability
        .iter()
        .map(|e| {
            let connectors: Vec<Value> = e
                .connectors
                .iter()
                .map(|(id, operative)| json!({"id": id, "operative": operative}))
                .collect();
            json!({"id": e.evse, "operative": e.operative, "connectors": connectors})
        })
        .collect();
    let transactions: Vec<Value> = state
        .tx
        .active
        .iter()
        .map(|t| {
            json!({
                "id": t.id.0,
                "evse": t.evse,
                "next_seq_no": t.next_seq_no,
                "started_at": t.started_at,
                "charging_state": enum_text(serde_json::to_value(t.charging_state)),
                "id_token": t.token.as_ref().map(|token| token.id_token.as_str()),
                "remote_start_id": t.remote_start_id,
                "energy_wh": t.energy_wh,
            })
        })
        .collect();
    let device_model: serde_json::Map<String, Value> = state
        .device_model
        .values
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(shown_value(key, value))))
        .collect();
    json!({
        "protocol": state.protocol,
        "boot_count": state.boot_count,
        "availability": if state.station_operative { "Operative" } else { "Inoperative" },
        "evses": evses,
        "active_transactions": transactions,
        "stopped_transaction_ids": state.tx.stopped_ids.len(),
        "queued_events": state.queue.events.len(),
        "authorization_cache_entries": state.auth_cache.entries.len(),
        "local_authorization_list": {
            "version": state.auth_cache.local_list.version,
            "entries": state.auth_cache.local_list.entries.len(),
        },
        "reservations": state.reservations.reservations.iter().map(reservation_json).collect::<Vec<_>>(),
        "default_tariffs": state.default_tariffs.iter().map(tariff_json).collect::<Vec<_>>(),
        "charging_profiles": state.charging_profiles.profiles.iter().map(profile_json).collect::<Vec<_>>(),
        "device_model": device_model,
    })
}

/// A timestamp as the station writes it: UTC, whole seconds.
fn time_text(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn text_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// The identifier of a stored charging profile or tariff, as text.
fn id_field(value: &Value, key: &str) -> Option<String> {
    match value.get(key)? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn reservation_json(r: &Reservation) -> Value {
    json!({
        "id": r.id,
        "evse": r.evse,
        "expires_at": r.expires_at,
        "connector_type": r.connector_type,
        "id_token": r.id_token.id_token,
        "id_token_type": enum_text(serde_json::to_value(&r.id_token.kind)),
        "group_id_token": r.group_id_token,
        "lost": r.lost,
    })
}

fn tariff_json(entry: &Value) -> Value {
    let tariff = entry.get("tariff").unwrap_or(&Value::Null);
    json!({
        "evse": entry.get("evseId").and_then(Value::as_u64),
        "tariff_id": text_field(tariff, "tariffId"),
        "currency": text_field(tariff, "currency"),
        "valid_from": entry.get("validFrom"),
        "tariff": tariff,
    })
}

fn profile_json(p: &StoredProfile2) -> Value {
    json!({
        "evse": p.evse,
        "id": p.profile.get("id"),
        "stack_level": p.profile.get("stackLevel"),
        "purpose": text_field(&p.profile, "chargingProfilePurpose"),
        "kind": text_field(&p.profile, "chargingProfileKind"),
        "source": p.source,
        "transaction_id": p.transaction_id,
        "valid_from": p.profile.get("validFrom"),
        "valid_to": p.profile.get("validTo"),
        "installed_at": p.installed_at,
        "profile": p.profile,
    })
}

/// Whole Wh of a register value, as the station's snapshot rounds it (the fraction is dropped).
fn whole_wh(wh: f64) -> f64 {
    if wh.is_finite() && wh > 0.0 {
        wh.floor()
    } else {
        0.0
    }
}

fn evse_text(evse: Option<u64>) -> String {
    match evse {
        None => "any EVSE".to_owned(),
        Some(0) => "station".to_owned(),
        Some(n) => format!("EVSE {n}"),
    }
}

fn reservation_line(r: &Reservation) -> String {
    let token = format!(
        "{} ({})",
        kirby_ocpp_tui::text::sanitize(&r.id_token.id_token),
        enum_text(serde_json::to_value(&r.id_token.kind))
    );
    let mut line = format!(
        "{}  id {}, expires {}, idToken {token}",
        evse_text(r.evse.map(u64::from)),
        r.id,
        time_text(r.expires_at)
    );
    if let Some(group) = &r.group_id_token {
        let _ = write!(line, ", group {}", kirby_ocpp_tui::text::sanitize(group));
    }
    if let Some(kind) = &r.connector_type {
        let _ = write!(line, ", connector {}", kirby_ocpp_tui::text::sanitize(kind));
    }
    if r.lost {
        line.push_str(", report lost");
    }
    line
}

fn tariff_line(entry: &Value) -> String {
    let tariff = entry.get("tariff").unwrap_or(&Value::Null);
    let evse = match entry.get("evseId").and_then(Value::as_u64) {
        Some(0) | None => "all EVSEs".to_owned(),
        Some(n) => format!("EVSE {n}"),
    };
    let mut line = format!(
        "{evse}  id {}, {}",
        kirby_ocpp_tui::text::sanitize(&id_field(tariff, "tariffId").unwrap_or_else(|| "-".into())),
        kirby_ocpp_tui::text::sanitize(text_field(tariff, "currency").unwrap_or("no currency"))
    );
    if let Some(from) = entry.get("validFrom").and_then(Value::as_str) {
        let _ = write!(
            line,
            ", valid from {}",
            kirby_ocpp_tui::text::sanitize(from)
        );
    }
    line
}

fn profile_line(p: &StoredProfile2) -> String {
    let field =
        |key: &str| kirby_ocpp_tui::text::sanitize(text_field(&p.profile, key).unwrap_or("-"));
    let mut line = format!(
        "{}  id {}, stack {}, {}, {}, source {}",
        evse_text(Some(u64::from(p.evse))),
        kirby_ocpp_tui::text::sanitize(&id_field(&p.profile, "id").unwrap_or_else(|| "-".into())),
        kirby_ocpp_tui::text::sanitize(
            &id_field(&p.profile, "stackLevel").unwrap_or_else(|| "-".into())
        ),
        field("chargingProfilePurpose"),
        field("chargingProfileKind"),
        kirby_ocpp_tui::text::sanitize(&p.source)
    );
    if let Some(tx) = &p.transaction_id {
        let _ = write!(line, ", transaction {}", kirby_ocpp_tui::text::sanitize(tx));
    }
    for (label, key) in [("valid from", "validFrom"), ("valid to", "validTo")] {
        if let Some(at) = text_field(&p.profile, key) {
            let _ = write!(line, ", {label} {}", kirby_ocpp_tui::text::sanitize(at));
        }
    }
    line
}

/// The local list, reservations, default tariffs and charging profiles of a 2.x state: a count
/// and one line per item. The tariffs section is left out of a 2.0.1 file that holds none.
fn write_settings2(out: &mut String, state: &PersistedStation2) {
    let _ = writeln!(
        out,
        "Local list:     version {}, {} entries",
        state.auth_cache.local_list.version,
        state.auth_cache.local_list.entries.len()
    );
    let _ = writeln!(
        out,
        "Reservations:   {}",
        state.reservations.reservations.len()
    );
    for r in &state.reservations.reservations {
        let _ = writeln!(out, "  {}", reservation_line(r));
    }
    if !state.default_tariffs.is_empty() || state.protocol == Protocol::V21.token() {
        let _ = writeln!(
            out,
            "Tariffs:        {} default tariffs",
            state.default_tariffs.len()
        );
        for entry in &state.default_tariffs {
            let _ = writeln!(out, "  {}", tariff_line(entry));
        }
    }
    let _ = writeln!(
        out,
        "Profiles:       {} charging profiles",
        state.charging_profiles.profiles.len()
    );
    for p in &state.charging_profiles.profiles {
        let _ = writeln!(out, "  {}", profile_line(p));
    }
}

fn state2_text(id: &str, file: &str, secrets: bool, state: &PersistedStation2) -> String {
    let operative = |flag: bool| if flag { "Operative" } else { "Inoperative" };
    let mut out = String::new();
    let _ = writeln!(out, "Station:        {id}");
    let _ = writeln!(out, "Protocol:       {}", state.protocol);
    let _ = writeln!(out, "State file:     {file}");
    let _ = writeln!(
        out,
        "Store version:  {} (this build reads up to {STORE2_VERSION})",
        state.version
    );
    let _ = writeln!(
        out,
        "Secrets file:   {}",
        if secrets {
            "present (not read, not shown)"
        } else {
            "none"
        }
    );
    let _ = writeln!(out, "Boots:          {}", state.boot_count);
    let _ = writeln!(
        out,
        "Availability:   {}",
        operative(state.station_operative)
    );
    let _ = writeln!(out, "EVSEs:          {}", state.evse_availability.len());
    for e in &state.evse_availability {
        let connectors: Vec<String> = e
            .connectors
            .iter()
            .map(|(cid, flag)| format!("{cid} {}", operative(*flag)))
            .collect();
        let _ = writeln!(
            out,
            "  {}  {}, connectors: {}",
            e.evse,
            operative(e.operative),
            if connectors.is_empty() {
                "-".to_owned()
            } else {
                connectors.join(", ")
            }
        );
    }
    let _ = writeln!(
        out,
        "Transactions:   {} active, {} stopped ids remembered",
        state.tx.active.len(),
        state.tx.stopped_ids.len()
    );
    for t in &state.tx.active {
        let _ = writeln!(
            out,
            "  EVSE {}  id {}, next seqNo {}, {}, idToken {}, {} Wh",
            t.evse,
            kirby_ocpp_tui::text::sanitize(&t.id.0),
            t.next_seq_no,
            enum_text(serde_json::to_value(t.charging_state)),
            t.token.as_ref().map_or_else(
                || "-".to_owned(),
                |token| kirby_ocpp_tui::text::sanitize(&token.id_token)
            ),
            whole_wh(t.energy_wh)
        );
    }
    let _ = writeln!(
        out,
        "Event queue:    {} transaction events",
        state.queue.events.len()
    );
    let _ = writeln!(
        out,
        "Auth cache:     {} entries",
        state.auth_cache.entries.len()
    );
    write_settings2(&mut out, state);
    let _ = writeln!(
        out,
        "Device model:   {} values",
        state.device_model.values.len()
    );
    for (key, value) in &state.device_model.values {
        let _ = writeln!(
            out,
            "  {} = {}",
            kirby_ocpp_tui::text::sanitize(key),
            kirby_ocpp_tui::text::sanitize(&shown_value(key, value))
        );
    }
    out
}

/// Whether a configuration key may hold a secret (its value is never printed).
fn is_secret_key(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ["authorizationkey", "password", "secret", "privatekey"]
        .iter()
        .any(|part| lower.contains(part))
}

fn shown_value(name: &str, value: &str) -> String {
    if is_secret_key(name) {
        "***".to_owned()
    } else {
        value.to_owned()
    }
}

fn transaction_json(connector: &PersistedConnector) -> Value {
    use kirby_ocpp_core::station::transaction::TransactionState;
    match &connector.transaction {
        TransactionState::Idle => json!({"state": "idle", "id": null, "id_tag": null}),
        TransactionState::PendingStart(t) => {
            json!({"state": "pending_start", "id": t.tx_id.to_string(), "id_tag": t.id_tag.as_str()})
        }
        TransactionState::Active(t) => {
            json!({"state": "active", "id": t.tx_id.to_string(), "id_tag": t.id_tag.as_str()})
        }
        TransactionState::Stopping(t) => json!({
            "state": "stopping",
            "id": t.tx_id.to_string(),
            "id_tag": t.id_tag.as_ref().map(kirby_ocpp_core::ids::IdTag::as_str),
        }),
    }
}

fn state_json(state: &PersistedStation) -> Value {
    let connectors: Vec<Value> = state
        .connectors
        .iter()
        .map(|c| {
            json!({
                "id": c.id.get(),
                "status": c.status.to_string(),
                "availability": c.availability.to_string(),
                "ev_connected": c.ev_connected,
                "meter_wh": c.meter_wh,
                "last_id_tag": c.last_id_tag.as_ref().map(kirby_ocpp_core::ids::IdTag::as_str),
                "transaction": transaction_json(c),
            })
        })
        .collect();
    let configuration: serde_json::Map<String, Value> = state
        .configuration
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(shown_value(key, value))))
        .collect();
    json!({
        "availability": state.charge_point_availability.to_string(),
        "connector_count": state.connector_count,
        "connectors": connectors,
        "local_authorization_list": {
            "version": state.local_authorization_list.version(),
            "entries": state.local_authorization_list.len(),
        },
        "authorization_cache_entries": state.authorization_cache.len(),
        "charging_profiles": state.charging_profiles.len(),
        "offline_transactions": state.offline_queue.entries().len(),
        "offline_samples": state.offline_samples.len(),
        "certificates": {
            "roots": state.certificates.roots.len(),
            "station_certificate": state.certificates.charge_point_chain.is_some(),
        },
        "security_events": state.security_events.len(),
        "pending_profile_switch": state.pending_profile_switch.is_some(),
        "configuration": configuration,
    })
}

fn state_text(id: &str, file: &str, secrets: bool, state: &PersistedStation) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Station:        {id}");
    let _ = writeln!(out, "State file:     {file}");
    let _ = writeln!(
        out,
        "Store version:  {} (this build reads up to {STORE_VERSION})",
        state.version
    );
    let _ = writeln!(
        out,
        "Secrets file:   {}",
        if secrets {
            "present (not read, not shown)"
        } else {
            "none"
        }
    );
    let _ = writeln!(out, "Availability:   {}", state.charge_point_availability);
    let _ = writeln!(out, "Connectors:     {}", state.connector_count);
    for c in &state.connectors {
        let tx = transaction_json(c);
        let transaction = match tx["state"].as_str() {
            Some("idle") | None => "none".to_owned(),
            Some(phase) => format!(
                "{phase} id {} idTag {}",
                tx["id"].as_str().unwrap_or("-"),
                tx["id_tag"].as_str().unwrap_or("-")
            ),
        };
        let _ = writeln!(
            out,
            "  {}  {}, {}, EV {}, meter {} Wh, last idTag {}, transaction: {transaction}",
            c.id,
            c.status,
            c.availability,
            if c.ev_connected {
                "connected"
            } else {
                "not connected"
            },
            c.meter_wh,
            c.last_id_tag.as_ref().map_or("-", |tag| tag.as_str()),
        );
    }
    let _ = writeln!(
        out,
        "Local list:     version {}, {} entries",
        state.local_authorization_list.version(),
        state.local_authorization_list.len()
    );
    let _ = writeln!(
        out,
        "Auth cache:     {} entries",
        state.authorization_cache.len()
    );
    let _ = writeln!(
        out,
        "Profiles:       {} charging profiles",
        state.charging_profiles.len()
    );
    let _ = writeln!(
        out,
        "Offline queue:  {} transactions, {} samples",
        state.offline_queue.entries().len(),
        state.offline_samples.len()
    );
    let _ = writeln!(
        out,
        "Certificates:   {} installed roots, station certificate: {}",
        state.certificates.roots.len(),
        if state.certificates.charge_point_chain.is_some() {
            "yes"
        } else {
            "no"
        }
    );
    let _ = writeln!(
        out,
        "Security events queued: {}{}",
        state.security_events.len(),
        if state.pending_profile_switch.is_some() {
            ", a security profile switch is pending"
        } else {
            ""
        }
    );
    let _ = writeln!(out, "Configuration:  {} keys", state.configuration.len());
    for (key, value) in &state.configuration {
        let _ = writeln!(out, "  {key} = {}", shown_value(key, value));
    }
    out
}

fn reset(store: &StationStore, options: &StateOptions, yes: bool) -> ExitCode {
    let existing: Vec<PathBuf> = store
        .owned_files()
        .into_iter()
        .filter(|path| path.is_file())
        .collect();
    if existing.is_empty() {
        report_reset(options, &[]);
        return ExitCode::SUCCESS;
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            return usage_error(
                "`state reset` deletes saved state: pass --yes (stdin is not a terminal, so it cannot ask)",
            );
        }
        let names: Vec<String> = existing.iter().map(|p| absolute_text(p)).collect();
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(
            stderr,
            "Delete the saved state of station `{}`:",
            kirby_ocpp_tui::text::sanitize(&options.id)
        );
        for name in &names {
            let _ = writeln!(stderr, "  {name}");
        }
        let _ = write!(stderr, "Delete? [y/N] ");
        let _ = stderr.flush();
        drop(stderr);
        let mut answer = String::new();
        let read = std::io::stdin().lock().read_line(&mut answer);
        if read.is_err() || !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            eprintln!("aborted: nothing was deleted");
            return ExitCode::FAILURE;
        }
    }
    match store.remove() {
        Ok(removed) => {
            report_reset(options, &removed);
            ExitCode::SUCCESS
        }
        Err(error @ StoreError::UnsupportedVersion { .. }) => {
            eprintln!(
                "error: {error}; nothing was deleted (delete the files by hand, or use the newer build)"
            );
            ExitCode::FAILURE
        }
        Err(error) => store_error(&error),
    }
}

fn report_reset(options: &StateOptions, removed: &[PathBuf]) {
    let files: Vec<String> = removed.iter().map(|p| absolute_text(p)).collect();
    if options.json {
        print_json(&json!({
            "schema": SCHEMA_VERSION,
            "station": options.id,
            "deleted": files,
        }));
    } else if files.is_empty() {
        stdout_line(
            &format!(
                "no saved state for station `{}`: nothing to delete",
                options.id
            ),
            false,
        );
    } else {
        stdout_line(
            &format!("deleted the saved state of station `{}`:", options.id),
            false,
        );
        for file in &files {
            stdout_line(&format!("  {file}"), false);
        }
    }
}
