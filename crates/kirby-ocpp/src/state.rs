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
use kirby_ocpp_core::scenario::SCHEMA_VERSION;
use kirby_ocpp_core::store::{
    Inspection, PersistedConnector, PersistedStation, STORE_VERSION, StationStore, StoreError,
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
        StateCmd::Show => show(&StationStore::new(&dir, &options.id), options),
        StateCmd::Reset { yes } => reset(&StationStore::new(&dir, &options.id), options, *yes),
    }
}

fn print_json(value: &Value) {
    stdout_line(&value.to_string(), true);
}

fn path(dir: &Path, options: &StateOptions) -> ExitCode {
    let store = options
        .id_given
        .then(|| StationStore::new(dir, &options.id));
    let dir_text = absolute_text(dir);
    if options.json {
        print_json(&json!({
            "schema": SCHEMA_VERSION,
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

fn show(store: &StationStore, options: &StateOptions) -> ExitCode {
    let file = absolute_text(store.path());
    let secrets = store.has_secrets();
    let state = match store.inspect() {
        Ok(Inspection::Loaded(state)) => *state,
        Ok(Inspection::Missing) => {
            if options.json {
                print_json(&json!({
                    "schema": SCHEMA_VERSION,
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
