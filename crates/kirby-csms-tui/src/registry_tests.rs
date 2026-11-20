//! The action registry against the palette and the forms: nothing is listed in one place
//! only, and every form agrees with the spec it belongs to.

#![allow(
    clippy::assert_is_empty,
    reason = "test fixtures read better field by field"
)]

use std::collections::BTreeSet;

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kirby_csms_core::registry::{self, ActionSpec, FieldType};
use kirby_csms_core::{
    ACTIONS_PROTOCOL, Counters2, SeqTracker, StationInfo, StationState, StationState2,
    TransactionStatus, Tx2Record,
};
use kirby_ocpp_core::ocpp::v2::messages::TriggerReason;
use kirby_ocpp_core::ocpp::{PayloadKind, Protocol, SchemaRegistry};
use kirby_ocpp_tui::text::TextInput;
use serde_json::{Value, json};

use crate::actions::{ActionId, CATALOG, ScopedRule};
use crate::app::{App, Effect, Mode, Msg, update};
use crate::command::{CmdOutcome, Command};
use crate::form::{Form, StopChoice, Submitted};
use crate::tests::{app_with, info, press};

/// Registered actions that have no palette entry (the raw call reaches them).
const NO_PALETTE: &[&str] = &["CancelReservation", "GetLocalListVersion"];
/// Palette entries whose action takes no input and is sent without a form.
const NO_FORM: &[&str] = &["ClearCache"];

const PEM: &str = "-----BEGIN CERTIFICATE-----\n\
    MIIBszCCAVmgAwIBAgIUQ0ZWm3QmH4lQ0b1b5mCw3m0o1rYwCgYIKoZIzj0EAwIw\n\
    FjEUMBIGA1UEAwwLdGVzdCByb290IDEwHhcNMjYwMTAxMDAwMDAwWhcNMzYwMTAx\n\
    -----END CERTIFICATE-----\n";

fn specs() -> &'static [ActionSpec] {
    registry::protocol_actions(ACTIONS_PROTOCOL)
        .expect("the palette protocol has a table")
        .csms_actions
}

/// The form that the palette opens for `id`.
fn form_of(id: ActionId) -> Option<Form> {
    let now = Utc::now();
    let choice = StopChoice {
        id: 7,
        connector: 1,
        id_tag: "TAG1".to_owned(),
    };
    Some(match id {
        ActionId::RemoteStart => Form::remote_start("CP-1"),
        ActionId::RemoteStop => Form::remote_stop("CP-1", &[choice]),
        ActionId::Reset => Form::reset("CP-1"),
        ActionId::ChangeConfiguration => Form::change_configuration("CP-1"),
        ActionId::GetConfiguration => Form::get_configuration("CP-1"),
        ActionId::Trigger => Form::trigger("CP-1"),
        ActionId::Unlock => Form::unlock("CP-1", None),
        ActionId::Availability => Form::availability("CP-1"),
        ActionId::SetProfile => Form::set_profile("CP-1"),
        ActionId::DataTransfer => Form::data_transfer("CP-1"),
        ActionId::GetLog => Form::get_log("CP-1", 1),
        ActionId::InstallCertificate => Form::install_certificate("CP-1"),
        ActionId::DeleteCertificate => Form::delete_certificate("CP-1"),
        ActionId::CertificateIds => Form::get_installed_certificate_ids("CP-1"),
        ActionId::ExtendedTrigger => Form::extended_trigger("CP-1"),
        ActionId::SignedFirmware => Form::signed_update_firmware("CP-1", 1, now),
        ActionId::CertificateSigned => Form::certificate_signed("CP-1"),
        ActionId::ClearChargingProfile => Form::clear_charging_profile("CP-1"),
        ActionId::GetCompositeSchedule => Form::get_composite_schedule("CP-1"),
        ActionId::ReserveNow => Form::reserve_now("CP-1", 1, now),
        ActionId::UpdateFirmware => Form::update_firmware("CP-1", now),
        ActionId::GetDiagnostics => Form::get_diagnostics("CP-1"),
        ActionId::SendLocalList => Form::send_local_list("CP-1"),
        _ => return None,
    })
}

/// Entries that fill the required fields that have no default, by field index.
fn sample(action: &str) -> Vec<(usize, &'static str)> {
    match action {
        "RemoteStartTransaction" => vec![(0, "TAG1")],
        "ChangeConfiguration" => vec![(0, "HeartbeatInterval"), (1, "60")],
        "DataTransfer" => vec![(0, "vendor")],
        "GetLog" => vec![(2, "https://example.com/log")],
        "InstallCertificate" => vec![(1, PEM)],
        "DeleteCertificate" => vec![(1, "aa"), (2, "bb"), (3, "01")],
        "SignedUpdateFirmware" => {
            vec![(0, "https://example.com/fw"), (5, PEM), (6, "c2lnbmF0dXJl")]
        }
        "CertificateSigned" => vec![(0, PEM)],
        "ReserveNow" => vec![(2, "TAG1")],
        "UpdateFirmware" => vec![(0, "https://example.com/fw")],
        "GetDiagnostics" => vec![(0, "https://example.com/diag")],
        _ => Vec::new(),
    }
}

#[test]
fn the_palette_and_the_registry_cover_each_other() {
    let registered: BTreeSet<&str> = specs().iter().map(|spec| spec.name).collect();
    let mut shown: BTreeSet<&str> = NO_PALETTE.iter().copied().collect();
    for def in CATALOG {
        let Some(name) = def.ocpp else { continue };
        let spec = def
            .spec()
            .unwrap_or_else(|| panic!("{name} is not in the registry"));
        assert_eq!((def.label(), def.desc()), (spec.title, spec.summary));
        assert!(!spec.title.is_empty() && !spec.summary.is_empty(), "{name}");
        assert!(shown.insert(name), "{name} has two palette entries");
    }
    assert_eq!(registered, shown);
    for name in NO_PALETTE {
        assert!(registered.contains(name), "{name}");
    }
}

#[test]
fn rule_and_fault_forms_offer_the_registry_station_actions() {
    let table = registry::protocol_actions(ACTIONS_PROTOCOL).unwrap();
    for form in [Form::rule(), Form::fault()] {
        let options: Vec<&str> = form.fields[0].options.iter().map(String::as_str).collect();
        assert_eq!(options, table.station_actions);
    }
    let raw = Form::raw("CP-1", None);
    let mut names: Vec<&str> = specs().iter().map(|spec| spec.name).collect();
    names.sort_unstable();
    let options: Vec<&str> = raw.fields[0].options.iter().map(String::as_str).collect();
    assert_eq!(options, names);
}

#[test]
fn every_form_matches_its_spec() {
    let mut checked = 0;
    for def in CATALOG {
        let Some(name) = def.ocpp else { continue };
        let spec = def.spec().unwrap();
        let Some(form) = form_of(def.id) else {
            assert!(NO_FORM.contains(&name), "{name} has no form");
            assert!(spec.fields.is_empty(), "{name} takes input but has no form");
            continue;
        };
        checked += 1;
        let labels: Vec<&str> = form.fields.iter().map(|field| field.label).collect();
        let wanted: Vec<&str> = spec.fields.iter().map(|field| field.label).collect();
        assert_eq!(labels, wanted, "{name}: labels");
        for (field, want) in form.fields.iter().zip(spec.fields) {
            let at = format!("{name}.{}", want.name);
            if want.ty == FieldType::Choice && !want.choices.is_empty() {
                let options: Vec<&str> = field
                    .options
                    .iter()
                    .map(String::as_str)
                    .filter(|option| *option != "(any)")
                    .collect();
                assert_eq!(options, want.choices, "{at}: choices");
            }
            if let Some(default) = want.default {
                let shown = field.chosen().unwrap_or_else(|| field.input.text());
                assert_eq!(shown, default, "{at}: default");
            }
            if field.label.contains("(optional") {
                assert!(!want.required, "{at}: marked optional in the form");
            }
        }
    }
    assert_eq!(checked, 23);
}

/// The value at a dotted path of a JSON object.
fn at<'a>(payload: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(payload, |value, key| value.get(key))
}

#[test]
fn every_form_filled_in_submits_a_request_with_the_spec_keys() {
    for def in CATALOG {
        let Some(name) = def.ocpp else { continue };
        let Some(mut form) = form_of(def.id) else {
            continue;
        };
        for (index, text) in sample(name) {
            form.fields[index].input = TextInput::new(text);
        }
        assert!(form.validate_all(), "{name}: {:?}", form.fields);
        let Some(Submitted::Send(command)) = form.submit() else {
            panic!("{name}: the form did not submit: {:?}", form.error);
        };
        assert_eq!(command.action(), name);
        let payload = command.payload();
        for spec in def.spec().unwrap().fields.iter().filter(|f| f.required) {
            assert!(
                at(&payload, spec.name).is_some(),
                "{name}: required key {} is missing from {payload}",
                spec.name
            );
        }
    }
}

#[test]
fn rule_and_fault_forms_take_an_optional_station() {
    let mut rule = Form::rule();
    assert!(matches!(
        rule.submit(),
        Some(Submitted::Rule { station: None, .. })
    ));
    rule.fields[2].input = TextInput::new(" CP-1 ");
    assert!(matches!(
        rule.submit(),
        Some(Submitted::Rule { station: Some(id), .. }) if id == "CP-1"
    ));
    rule.fields[2].input = TextInput::new("x".repeat(129));
    assert!(!rule.validate_all());
    assert!(rule.submit().is_none());

    let mut fault = Form::fault();
    assert!(matches!(
        fault.submit(),
        Some(Submitted::Fault { station: None, .. })
    ));
    fault.fields[4].input = TextInput::new("CP-2");
    assert!(matches!(
        fault.submit(),
        Some(Submitted::Fault { station: Some(id), .. }) if id == "CP-2"
    ));
    fault.fields[4].input = TextInput::new("x".repeat(129));
    assert!(!fault.validate_all());
    assert!(fault.submit().is_none());
}

#[test]
fn the_remove_rule_form_lists_station_rules_next_to_global_ones() {
    let scoped = [ScopedRule {
        station: Some("CP-1".to_owned()),
        action: "Reset".to_owned(),
        protocol: None,
    }];
    let mut form = Form::remove_rule_scoped(&["Authorize".to_owned()], &scoped);
    let options: Vec<&str> = form.fields[0].options.iter().map(String::as_str).collect();
    assert_eq!(options, ["Authorize", "Reset@CP-1"]);
    assert_eq!(
        form.submit(),
        Some(Submitted::RemoveRule("Authorize".into()))
    );
    form.fields[0].option = 1;
    assert_eq!(
        form.submit(),
        Some(Submitted::RemoveScopedRule {
            station: Some("CP-1".to_owned()),
            action: "Reset".to_owned(),
            protocol: None,
        })
    );
}

// ---- OCPP 2.0.1 stations ----------------------------------------------------------------

const V201: &str = "ocpp2.0.1";
/// A transaction id of 36 characters, as a station makes them.
const TX_ID: &str = "0b9c5bd8-6f0e-4f8e-9a52-0c9a4d1c7e11";

fn table201() -> &'static registry::ProtocolActions {
    registry::protocol_actions(V201).expect("the 2.0.1 table")
}

fn schemas201() -> std::sync::Arc<SchemaRegistry> {
    SchemaRegistry::for_protocol(Protocol::V201).expect("2.0.1 schemas")
}

fn running_tx(id: &str, token: &str) -> Tx2Record {
    Tx2Record {
        id: id.to_owned(),
        evse: Some(1),
        connector: Some(1),
        id_token: Some(token.to_owned()),
        id_token_type: None,
        id_token_status: None,
        status: TransactionStatus::Active,
        charging_state: None,
        remote_start_id: None,
        reservation_id: None,
        trigger_reason: TriggerReason::RemoteStart,
        started_at: Utc::now(),
        stopped_at: None,
        stopped_reason: None,
        time_spent_charging: None,
        energy_wh: None,
        power_w: None,
        meter_start_wh: None,
        meter_stop_wh: None,
        last_meter_at: None,
        seq: SeqTracker::default(),
    }
}

/// An app whose selected station CP-1 speaks `subprotocol`, with the given running
/// transactions of a 2.0.1 session.
fn station_with(subprotocol: &str, transactions: Vec<Tx2Record>) -> App {
    let (mut app, _) = app_with(&["CP-1", "CP-2"]);
    let state = StationState {
        v2: Some(Box::new(StationState2 {
            protocol: V201.to_owned(),
            boot_reason: None,
            evses: std::collections::BTreeMap::new(),
            transactions,
            meters: std::collections::BTreeMap::new(),
            counters: Counters2::default(),
            last_report: None,
        })),
        ..StationState::default()
    };
    update(
        &mut app,
        Msg::Loaded {
            id: "CP-1".into(),
            history: vec![],
            info: Some(Box::new(StationInfo {
                subprotocol: Some(subprotocol.to_owned()),
                ..info("CP-1", true)
            })),
            state: Some(Box::new(state)),
        },
    );
    app
}

fn station201() -> App {
    station_with(V201, vec![running_tx(TX_ID, "TAG1")])
}

#[test]
fn transaction_detail_and_search_key_follow_focus() {
    let mut app = station201();
    press(&mut app, KeyCode::Right);
    assert_eq!(app.focus, crate::app::Focus::Details);
    press(&mut app, KeyCode::Enter);
    let Mode::Detail(detail) = &app.mode else {
        panic!("transaction detail")
    };
    assert_eq!(detail.copy, TX_ID);
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Char('s'));
    assert!(matches!(app.mode, Mode::Input(_)));
    app.mode = Mode::Normal;
    app.focus = crate::app::Focus::Stations;
    press(&mut app, KeyCode::Char('s'));
    assert!(matches!(app.mode, Mode::Form(_)));
}

/// Runs the table entry `name` and returns the form it opens.
fn open(app: &mut App, name: &str) -> Form {
    let index = table201()
        .csms_actions
        .iter()
        .position(|spec| spec.name == name)
        .unwrap_or_else(|| panic!("{name} is not in the table"));
    let mut fx = Vec::new();
    app.mode = Mode::Normal;
    app.run_action(ActionId::Call(index), &mut fx);
    match std::mem::replace(&mut app.mode, Mode::Normal) {
        Mode::Form(form) => *form,
        _ => panic!("{name} opened no form: {:?}", app.notice),
    }
}

fn set(form: &mut Form, name: &str, text: &str) {
    let field = form
        .fields
        .iter_mut()
        .find(|f| f.spec.is_some_and(|s| s.name == name))
        .unwrap_or_else(|| panic!("no field {name}"));
    field.input = TextInput::new(text);
}

fn sent(form: &mut Form) -> (String, Value) {
    match form.submit() {
        Some(Submitted::Send(Command::Raw { action, payload })) => (action, payload),
        other => panic!("the form did not submit ({other:?}): {:?}", form.error),
    }
}

fn notice_text(app: &App) -> String {
    app.notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default()
}

/// Input for the required fields that have no default, by field name.
fn typed_input(action: &str) -> Vec<(&'static str, &'static str)> {
    match action {
        "RequestStartTransaction" => vec![("idToken.idToken", "TAG1")],
        "DataTransfer" => vec![("vendorId", "vendor")],
        "UnlockConnector" => vec![("evseId", "1"), ("connectorId", "1")],
        "SendLocalList" => vec![("versionNumber", "1")],
        "GetVariables" => vec![(
            "getVariableData",
            r#"[{"component":{"name":"ChargingStation"},"variable":{"name":"Model"}}]"#,
        )],
        "SetVariables" => vec![(
            "setVariableData",
            r#"[{"component":{"name":"ChargingStation"},"variable":{"name":"Model"},"attributeValue":"x"}]"#,
        )],
        _ => Vec::new(),
    }
}

/// The schema node `node` with a local `$ref` followed.
fn resolved<'a>(root: &'a Value, node: &'a Value) -> &'a Value {
    match node.get("$ref").and_then(Value::as_str) {
        Some(reference) => reference
            .strip_prefix("#/definitions/")
            .and_then(|key| root.get("definitions")?.get(key))
            .expect("local definition"),
        None => node,
    }
}

/// The smallest value that satisfies `node`: the required properties only.
fn minimal(root: &Value, node: &Value) -> Value {
    let node = resolved(root, node);
    if let Some(first) = node
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|e| e.first())
    {
        return first.clone();
    }
    let size = |key: &str| node.get(key).and_then(Value::as_u64).unwrap_or(0);
    match node.get("type").and_then(Value::as_str) {
        Some("object") => {
            let mut object = serde_json::Map::new();
            let required = node.get("required").and_then(Value::as_array);
            for key in required.into_iter().flatten().filter_map(Value::as_str) {
                if let Some(child) = node.get("properties").and_then(|p| p.get(key)) {
                    object.insert(key.to_owned(), minimal(root, child));
                }
            }
            Value::Object(object)
        }
        Some("array") => {
            let item = node.get("items").map_or(Value::Null, |i| minimal(root, i));
            let n = usize::try_from(size("minItems").max(1)).unwrap_or(1);
            Value::Array(vec![item; n])
        }
        Some("integer" | "number") => {
            Value::from(node.get("minimum").and_then(Value::as_i64).unwrap_or(0))
        }
        Some("boolean") => Value::Bool(false),
        Some("string") if node.get("format").and_then(Value::as_str) == Some("date-time") => {
            Value::String("2026-01-01T00:00:00Z".to_owned())
        }
        Some("string") => {
            let n = usize::try_from(size("minLength").max(1)).unwrap_or(1);
            Value::String("a".repeat(n))
        }
        _ => Value::String("x".to_owned()),
    }
}

fn minimal_request(action: &str) -> Value {
    let registry = schemas201();
    let source = registry
        .schema_source(action, PayloadKind::Request)
        .expect("request schema");
    let root: Value = serde_json::from_str(source).expect("schema is JSON");
    minimal(&root, &root)
}

#[test]
fn the_2_0_1_hotkeys_follow_the_table() {
    let app = station201();
    let table = table201();
    let keys = [
        ("Reset", 'R'),
        ("TriggerMessage", 'T'),
        ("UnlockConnector", 'u'),
        ("ChangeAvailability", 'A'),
        ("ClearCache", 'C'),
        ("DataTransfer", 'D'),
        ("RequestStartTransaction", 't'),
        ("RequestStopTransaction", 'x'),
        ("GetVariables", 'g'),
        ("SetVariables", 'c'),
        ("GetBaseReport", 'b'),
        ("GetTransactionStatus", 's'),
    ];
    for (name, key) in keys {
        let Some(ActionId::Call(i)) = app.action_for_key(key) else {
            panic!("{key} is not a table entry");
        };
        assert_eq!(table.csms_actions[i].name, name, "key {key}");
    }
    // no other entry of the table has a hotkey, and no key is used twice
    let catalog = app.catalog();
    let mut used = BTreeSet::new();
    for def in &catalog {
        if let (Some(key), Some(name)) = (def.key, def.ocpp) {
            assert!(
                keys.contains(&(name, key)),
                "unexpected hotkey {key} for {name}"
            );
        }
        if let Some(key) = def.key {
            assert!(used.insert(key), "{key} is used twice");
        }
    }
}

#[test]
fn no_hotkey_collides_with_a_navigation_key() {
    // the plain keys of the normal mode (`s` only searches while the log has the focus)
    const RESERVED: &str = "q?:/afpyYGkj";
    for catalog in [CATALOG.to_vec(), station201().catalog()] {
        for def in catalog {
            if let Some(key) = def.key {
                assert!(!RESERVED.contains(key), "{key} is taken: {}", def.label());
            }
        }
    }
}

#[test]
fn the_1_6_palette_and_hotkeys_do_not_change() {
    let (app, _) = app_with(&["CP-1"]);
    assert_eq!(app.catalog(), CATALOG.to_vec());
    let loaded = crate::ui::tests::loaded();
    assert_eq!(loaded.catalog(), CATALOG.to_vec());
    for def in CATALOG {
        if let Some(key) = def.key {
            assert_eq!(loaded.action_for_key(key), Some(def.id));
        }
    }
    let mut app = loaded;
    app.open_palette();
    let Mode::Palette(p) = &app.mode else {
        panic!("palette");
    };
    assert_eq!(p.results, (0..CATALOG.len()).collect::<Vec<_>>());
}

#[test]
fn a_1_6_only_hotkey_on_a_2_0_1_station_says_so() {
    let mut app = station201();
    let table_keys: BTreeSet<char> = app.catalog().iter().filter_map(|d| d.key).collect();
    let mut checked = 0;
    for def in CATALOG.iter().filter(|d| d.ocpp.is_some()) {
        let Some(key) = def.key.filter(|k| !table_keys.contains(k)) else {
            continue;
        };
        let fx = update(
            &mut app,
            Msg::Key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE)),
        );
        assert!(fx.is_empty(), "{key}");
        assert!(matches!(app.mode, Mode::Normal), "{key}");
        assert!(
            notice_text(&app).ends_with("not an OCPP 2.0.1 action"),
            "{key}: {}",
            notice_text(&app)
        );
        checked += 1;
    }
    assert!(checked >= 14, "{checked}");
    // a 1.6 hotkey on a 1.6 station still opens its form
    let mut app = crate::ui::tests::loaded();
    press(&mut app, KeyCode::Char('P'));
    assert!(matches!(app.mode, Mode::Form(_)));
}

#[test]
fn a_subprotocol_without_a_table_disables_every_action() {
    let mut app = station_with("ocpp9.9", Vec::new());
    assert!(app.catalog().iter().all(|d| d.ocpp.is_none()));
    for key in ['R', 'w', 't'] {
        press(&mut app, KeyCode::Char(key));
        assert!(matches!(app.mode, Mode::Normal), "{key}");
        assert!(
            notice_text(&app).ends_with("no action table for ocpp9.9"),
            "{key}: {}",
            notice_text(&app)
        );
    }
    assert_eq!(
        app.reason_for(ActionId::Call(0)).as_deref(),
        Some("no action table for ocpp9.9")
    );
    // the local operations stay
    assert!(app.reason_for(ActionId::CopyId).is_none());
}

#[test]
fn every_2_0_1_registry_entry_has_a_palette_path() {
    let mut app = station201();
    let table = table201();
    app.open_palette();
    let Mode::Palette(p) = &app.mode else {
        panic!("palette");
    };
    let rows: Vec<_> = p.rows().copied().collect();
    for (i, spec) in table.csms_actions.iter().enumerate() {
        let def = rows
            .iter()
            .find(|d| d.id == ActionId::Call(i))
            .unwrap_or_else(|| panic!("{} has no palette entry", spec.name));
        assert_eq!(def.spec().map(|s| s.name), Some(spec.name));
        assert_eq!((def.label(), def.desc()), (spec.title, spec.summary));
    }
    // choosing an entry reaches `run_action`: a form, or a command for an action without input
    for (i, spec) in table.csms_actions.iter().enumerate() {
        let mut fx = Vec::new();
        app.mode = Mode::Normal;
        app.run_action(ActionId::Call(i), &mut fx);
        let opened = matches!(app.mode, Mode::Form(_)) || !fx.is_empty();
        assert!(opened, "{} did nothing: {}", spec.name, notice_text(&app));
    }
    // the 1.6 entries are not offered
    assert!(rows.iter().all(|d| d.protocol == V201 || d.ocpp.is_none()));
}

#[test]
fn every_2_0_1_form_builds_a_schema_valid_request() {
    let schemas = schemas201();
    let mut app = station201();
    let mut typed = 0;
    let mut raw = 0;
    for spec in table201().csms_actions {
        if matches!(spec.name, "ClearCache" | "GetLocalListVersion") {
            // no input: sent without a form
            let mut fx = Vec::new();
            let i = table201()
                .csms_actions
                .iter()
                .position(|s| s.name == spec.name)
                .unwrap();
            app.run_action(ActionId::Call(i), &mut fx);
            let (_, command) = match <[Effect; 1]>::try_from(fx) {
                Ok(
                    [
                        Effect::Send {
                            station, command, ..
                        },
                    ],
                ) => (station, command),
                other => panic!("expected one Send, got {other:?}"),
            };
            assert_eq!(
                command,
                Command::Raw {
                    action: spec.name.into(),
                    payload: json!({})
                }
            );
            continue;
        }
        let mut form = open(&mut app, spec.name);
        let payload = if spec.fields.is_empty() {
            raw += 1;
            assert_eq!(
                form.fields[0].options,
                [spec.name],
                "{}: action fixed",
                spec.name
            );
            let needs_input = schemas.validate_request(spec.name, &json!({})).is_err();
            if needs_input {
                assert!(
                    form.submit().is_none(),
                    "{}: {{}} must not submit",
                    spec.name
                );
                assert!(form.fields[1].error.is_some(), "{}", spec.name);
            }
            form.fields[1].input = TextInput::new(minimal_request(spec.name).to_string());
            let (action, payload) = sent(&mut form);
            assert_eq!(action, spec.name);
            payload
        } else {
            typed += 1;
            for (name, text) in typed_input(spec.name) {
                set(&mut form, name, text);
            }
            let (action, payload) = sent(&mut form);
            assert_eq!(action, spec.name);
            for field in spec.fields.iter().filter(|f| f.required) {
                assert!(
                    at(&payload, field.name).is_some(),
                    "{}: required key {} is missing from {payload}",
                    spec.name,
                    field.name
                );
            }
            payload
        };
        assert!(
            schemas.validate_request(spec.name, &payload).is_ok(),
            "{}: {payload}",
            spec.name
        );
    }
    assert_eq!(
        (typed, raw),
        (12, 26),
        "ClearCache and GetLocalListVersion are the other two typed actions"
    );
}

#[test]
fn required_fields_block_submit_and_integers_are_integers() {
    let mut app = station201();
    let mut start = open(&mut app, "RequestStartTransaction");
    assert!(start.submit().is_none());
    let at = start.fields.iter().position(|f| f.error.is_some()).unwrap();
    assert_eq!(
        start.fields[at].spec.map(|s| s.name),
        Some("idToken.idToken")
    );
    assert_eq!(start.focus, at);
    set(&mut start, "idToken.idToken", "TAG1");
    set(&mut start, "evseId", "2");
    let (_, payload) = sent(&mut start);
    assert!(payload["evseId"].is_u64(), "{payload}");
    assert!(payload["remoteStartId"].is_u64(), "{payload}");
    assert_eq!(
        payload["idToken"],
        json!({"idToken": "TAG1", "type": "ISO14443"})
    );

    for bad in ["-1", "x", "1.5", " "] {
        set(&mut start, "evseId", bad);
        let error = start.validate_all();
        if bad.trim().is_empty() {
            assert!(error, "an empty optional integer is fine");
        } else {
            assert!(!error, "{bad:?} must be refused");
            assert!(start.submit().is_none());
        }
    }
    set(&mut start, "evseId", "");
    let (_, payload) = sent(&mut start);
    assert!(
        payload.get("evseId").is_none(),
        "empty optional fields are left out: {payload}"
    );

    let mut unlock = open(&mut app, "UnlockConnector");
    assert!(unlock.submit().is_none());
    let mut variables = open(&mut app, "GetVariables");
    assert!(variables.submit().is_none());
    let error = variables.fields[0].error.clone().unwrap();
    assert!(
        error.contains("component"),
        "the hint tells the shape: {error}"
    );
}

#[test]
fn dotted_keys_nest_and_the_whole_request_is_checked() {
    let mut app = station201();
    let mut form = open(&mut app, "ChangeAvailability");
    set(&mut form, "evse.id", "1");
    set(&mut form, "evse.connectorId", "2");
    let (_, payload) = sent(&mut form);
    assert_eq!(
        payload,
        json!({"operationalStatus": "Operative", "evse": {"id": 1, "connectorId": 2}})
    );
    // a connector without its EVSE is a schema violation of the whole request
    set(&mut form, "evse.id", "");
    form.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    assert!(form.error.is_some(), "checked while typing");
    assert!(form.submit().is_none());
    assert!(form.error.is_some());

    let mut trigger = open(&mut app, "TriggerMessage");
    assert_eq!(trigger.fields[0].chosen(), Some("StatusNotification"));
    set(&mut trigger, "evse.id", "1");
    let (_, payload) = sent(&mut trigger);
    assert_eq!(
        payload,
        json!({"requestedMessage": "StatusNotification", "evse": {"id": 1}})
    );
}

fn drawn(app: &App) -> (String, bool) {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
    let buf = terminal.backend().buffer();
    let text: String = buf
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    let dark = buf.content().iter().any(|c| c.fg == Color::DarkGray);
    (text, dark)
}

#[test]
fn a_form_draws_the_hint_of_the_focused_field_only() {
    const VENDOR: &str = "at most 255 characters";
    const MESSAGE: &str = "at most 50 characters";
    let mut app = station201();
    let mut form = open(&mut app, "DataTransfer");
    assert_eq!(form.focus, 0);
    // A valid value: an error under the field would take the line of the hint.
    set(&mut form, "vendorId", "vendor");
    form.validate_all();
    app.mode = Mode::Form(Box::new(form.clone()));
    let (text, dark) = drawn(&app);
    assert!(text.contains(VENDOR), "{text}");
    assert!(!text.contains(MESSAGE), "{text}");
    assert!(!dark);
    form.focus = 1;
    app.mode = Mode::Form(Box::new(form));
    let (text, _) = drawn(&app);
    assert!(text.contains(MESSAGE), "{text}");
    assert!(!text.contains(VENDOR), "{text}");
}

#[test]
fn a_field_without_a_hint_draws_nothing_under_it() {
    let mut app = station201();
    let mut form = open(&mut app, "DataTransfer");
    set(&mut form, "vendorId", "vendor");
    form.validate_all();
    form.fields[0].hint = "";
    app.mode = Mode::Form(Box::new(form));
    let (text, _) = drawn(&app);
    assert!(!text.contains("at most"), "{text}");
    assert!(!text.contains("required"), "{text}");
}

#[test]
fn data_transfer_data_takes_a_string_a_number_and_an_array() {
    let mut app = station201();
    let mut form = open(&mut app, "DataTransfer");
    set(&mut form, "vendorId", "vendor");
    for (text, want) in [
        ("hello world", json!("hello world")),
        (r#""quoted""#, json!("quoted")),
        ("42", json!(42)),
        ("[1, 2, 3]", json!([1, 2, 3])),
        (r#"{"a": true}"#, json!({"a": true})),
    ] {
        set(&mut form, "data", text);
        let (_, payload) = sent(&mut form);
        assert_eq!(payload["data"], want, "{text}");
    }
    set(&mut form, "data", "[1, 2");
    assert!(!form.validate_all(), "broken JSON is refused");
    assert!(
        form.fields[2]
            .error
            .as_deref()
            .unwrap()
            .starts_with("invalid JSON")
    );
    set(&mut form, "data", "");
    let (_, payload) = sent(&mut form);
    assert!(payload.get("data").is_none());
}

#[test]
fn request_stop_picks_a_running_transaction_by_its_string_id() {
    let mut app = station_with(
        V201,
        vec![
            running_tx(TX_ID, "TAG1"),
            running_tx("11111111-2222-3333-4444-555555555555", "EVIL\u{1b}[31m"),
        ],
    );
    let mut form = open(&mut app, "RequestStopTransaction");
    assert_eq!(form.fields[0].options.len(), 2);
    // newest first, short id, EVSE/connector, idToken without control characters
    assert!(
        form.fields[0].options[0].starts_with("11111111  1/1  EVIL"),
        "{:?}",
        form.fields[0].options
    );
    assert!(form.fields[0].options.iter().all(|o| !o.contains('\u{1b}')));
    form.fields[0].option = 1;
    let (action, payload) = sent(&mut form);
    assert_eq!(action, "RequestStopTransaction");
    assert_eq!(payload, json!({"transactionId": TX_ID}));

    // without a running transaction the action is disabled
    let mut idle = station_with(V201, Vec::new());
    let i = table201()
        .csms_actions
        .iter()
        .position(|s| s.name == "RequestStopTransaction")
        .unwrap();
    assert_eq!(
        idle.reason_for(ActionId::Call(i)).as_deref(),
        Some("no active transaction")
    );
    press(&mut idle, KeyCode::Char('x'));
    assert!(notice_text(&idle).ends_with("no active transaction"));
    // an ended transaction does not count
    let mut ended = running_tx(TX_ID, "TAG1");
    ended.status = TransactionStatus::Stopped;
    let app = station_with(V201, vec![ended]);
    assert!(app.reason_for(ActionId::Call(i)).is_some());
}

#[test]
fn transaction_status_leaves_an_empty_id_out() {
    let mut app = station201();
    let mut form = open(&mut app, "GetTransactionStatus");
    let (_, payload) = sent(&mut form);
    assert_eq!(payload, json!({}));
    set(&mut form, "transactionId", TX_ID);
    let (_, payload) = sent(&mut form);
    assert_eq!(payload, json!({"transactionId": TX_ID}));
}

#[test]
fn remote_start_ids_come_from_the_session_counter() {
    let mut app = station201();
    let first = open(&mut app, "RequestStartTransaction");
    let second = open(&mut app, "RequestStartTransaction");
    let id = |form: &Form| {
        form.fields
            .iter()
            .find(|f| f.spec.is_some_and(|s| s.name == "remoteStartId"))
            .map(|f| f.input.text().to_owned())
            .unwrap()
    };
    assert_ne!(id(&first), id(&second));
}

#[test]
fn reset_asks_first_and_only_the_reports_open_a_popup() {
    let mut app = station201();
    let mut reset = open(&mut app, "Reset");
    let (_, payload) = sent(&mut reset);
    assert_eq!(payload, json!({"type": "OnIdle"}));
    let mut fx = Vec::new();
    app.submitted(
        "CP-1",
        Submitted::Send(Command::Raw {
            action: "Reset".into(),
            payload,
        }),
        &mut fx,
    );
    assert!(fx.is_empty());
    assert!(matches!(app.mode, Mode::Confirm(_)));
    // a 1.6 station does not ask for a raw Reset (its own form asks for the typed one)
    let mut old = crate::ui::tests::loaded();
    let mut fx = Vec::new();
    old.submitted(
        "CP-1",
        Submitted::Send(Command::Raw {
            action: "Reset".into(),
            payload: json!({"type": "Hard"}),
        }),
        &mut fx,
    );
    assert!(matches!(old.mode, Mode::Normal));

    let raw = |name: &str| Command::Raw {
        action: name.into(),
        payload: json!({}),
    };
    for (name, answer, content) in [
        ("GetVariables", json!({"getVariableResult": []}), true),
        ("SetVariables", json!({"setVariableResult": []}), true),
        ("GetBaseReport", json!({"status": "Accepted"}), true),
        (
            "GetTransactionStatus",
            json!({"messagesInQueue": false}),
            true,
        ),
        ("GetLog", json!({"status": "Accepted"}), true),
        ("Reset", json!({"status": "Accepted"}), false),
        ("ClearCache", json!({"status": "Accepted"}), false),
        (
            "RequestStartTransaction",
            json!({"status": "Accepted"}),
            false,
        ),
        ("DataTransfer", json!({"status": "Accepted"}), false),
        (
            "DataTransfer",
            json!({"status": "Accepted", "data": 1}),
            true,
        ),
    ] {
        assert_eq!(
            raw(name).answer_is_content_in(V201, &answer),
            content,
            "{name} {answer}"
        );
    }
    // the same raw command on a 1.6 station keeps its popup
    assert!(raw("Reset").answer_is_content_in("ocpp1.6", &json!({})));
}

#[test]
fn the_raw_form_of_a_2_0_1_station_lists_its_actions_and_checks_its_schemas() {
    let mut app = station201();
    press(&mut app, KeyCode::Char('w'));
    let Mode::Form(form) = &mut app.mode else {
        panic!("raw form");
    };
    let mut names: Vec<&str> = table201().csms_actions.iter().map(|s| s.name).collect();
    names.sort_unstable();
    let options: Vec<&str> = form.fields[0].options.iter().map(String::as_str).collect();
    assert_eq!(options, names);
    let at = options.iter().position(|o| *o == "Reset").unwrap();
    form.fields[0].option = at;
    form.fields[1].input = TextInput::new(r#"{"type":"Hard"}"#);
    assert!(!form.validate_all(), "Hard is a 1.6 value");
    form.fields[1].input = TextInput::new(r#"{"type":"Immediate"}"#);
    assert!(form.validate_all());
    // the 1.6 form is unchanged
    let old = Form::raw("CP-1", None);
    assert!(
        old.fields[0]
            .options
            .iter()
            .any(|o| o == "RemoteStartTransaction")
    );
}

#[test]
fn rule_and_fault_forms_pick_a_protocol_first() {
    let mut rule = Form::rule_in(V201);
    assert_eq!(rule.fields[0].chosen(), Some(V201));
    let actions = rule.fields[1].options.clone();
    assert_eq!(actions, table201().station_actions);
    assert!(matches!(
        rule.submit(),
        Some(Submitted::Rule { protocol: Some(p), station: None, .. }) if p == V201
    ));
    // another protocol re-lists the actions
    rule.focus = 0;
    rule.fields[1].option = actions.iter().position(|a| a == "Heartbeat").unwrap();
    rule.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(rule.fields[0].chosen(), Some("ocpp1.6"));
    let old: Vec<&str> = rule.fields[1].options.iter().map(String::as_str).collect();
    assert_eq!(
        old,
        registry::protocol_actions("ocpp1.6")
            .unwrap()
            .station_actions
    );
    assert_eq!(
        rule.fields[1].chosen(),
        Some("Heartbeat"),
        "the action is kept"
    );
    // 2.1 lists its own actions
    rule.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    rule.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(rule.fields[0].chosen(), Some(V21));
    assert_eq!(rule.fields[1].options, table21().station_actions);
    // (all) keeps the engine-wide behavior
    rule.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(rule.fields[0].chosen(), Some("(all)"));
    assert!(matches!(
        rule.submit(),
        Some(Submitted::Rule { protocol: None, .. })
    ));

    let mut fault = Form::fault_in(V201);
    assert!(matches!(
        fault.submit(),
        Some(Submitted::Fault { protocol: Some(p), times: 1, .. }) if p == V201
    ));
    fault.fields[5].input = TextInput::new("CP-9");
    assert!(matches!(
        fault.submit(),
        Some(Submitted::Fault { station: Some(s), .. }) if s == "CP-9"
    ));
    // the payload only matters for invalid_payload
    fault.focus = 2;
    fault.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    assert_eq!(fault.fields[2].chosen(), Some("invalid_payload"));
    fault.fields[4].input = TextInput::new("nope");
    assert!(!fault.validate_all());
}

// ---- OCPP 2.1 stations ----------------------------------------------------------------

const V21: &str = "ocpp2.1";

fn table21() -> &'static registry::ProtocolActions {
    registry::protocol_actions(V21).expect("the 2.1 table")
}

fn station21() -> App {
    station_with(V21, vec![running_tx(TX_ID, "TAG1")])
}

#[test]
fn a_2_1_station_offers_the_2_1_table_with_the_2_0_1_hotkeys() {
    let mut app = station21();
    let table = table21();
    assert_eq!(table.csms_actions.len(), 55);
    app.open_palette();
    let Mode::Palette(p) = &app.mode else {
        panic!("palette");
    };
    let rows: Vec<_> = p.rows().copied().collect();
    for (i, spec) in table.csms_actions.iter().enumerate() {
        let def = rows
            .iter()
            .find(|d| d.id == ActionId::Call(i))
            .unwrap_or_else(|| panic!("{} has no palette entry", spec.name));
        assert_eq!(def.spec().map(|s| s.name), Some(spec.name));
        assert_eq!((def.label(), def.desc()), (spec.title, spec.summary));
    }
    assert!(rows.iter().all(|d| d.protocol == V21 || d.ocpp.is_none()));
    for (name, key) in [("Reset", 'R'), ("RequestStartTransaction", 't')] {
        let Some(ActionId::Call(i)) = app.action_for_key(key) else {
            panic!("{key} is not a table entry");
        };
        assert_eq!(table.csms_actions[i].name, name, "key {key}");
    }
    // an action new in 2.1 is in the palette without a hotkey
    let tariffs = rows
        .iter()
        .find(|d| d.ocpp == Some("GetTariffs"))
        .expect("GetTariffs is offered");
    assert_eq!(tariffs.key, None);
    // the 1.6 entries are not offered, and one says why
    assert!(app.reason_for(ActionId::Reset).is_some());
}

#[test]
fn every_2_1_registry_entry_runs_from_the_palette() {
    let mut app = station21();
    for (i, spec) in table21().csms_actions.iter().enumerate() {
        let mut fx = Vec::new();
        app.mode = Mode::Normal;
        app.run_action(ActionId::Call(i), &mut fx);
        let opened = matches!(app.mode, Mode::Form(_)) || !fx.is_empty();
        assert!(opened, "{} did nothing: {}", spec.name, notice_text(&app));
    }
}

#[test]
fn the_raw_form_of_a_2_1_station_lists_its_actions_and_checks_its_schemas() {
    let mut app = station21();
    press(&mut app, KeyCode::Char('w'));
    let Mode::Form(form) = &mut app.mode else {
        panic!("raw form");
    };
    let mut names: Vec<&str> = table21().csms_actions.iter().map(|s| s.name).collect();
    names.sort_unstable();
    let options: Vec<&str> = form.fields[0].options.iter().map(String::as_str).collect();
    assert_eq!(options, names);
    let tariffs = options.iter().position(|o| *o == "GetTariffs").unwrap();
    let reset = options.iter().position(|o| *o == "Reset").unwrap();
    form.fields[0].option = tariffs;
    form.fields[1].input = TextInput::new("{}");
    assert!(!form.validate_all(), "GetTariffs needs an evseId");
    form.fields[1].input = TextInput::new(r#"{"evseId":1}"#);
    assert!(form.validate_all());
    // the 2.1 schema of Reset knows a type that 2.0.1 does not
    form.fields[0].option = reset;
    form.fields[1].input = TextInput::new(r#"{"type":"ImmediateAndResume"}"#);
    assert!(form.validate_all());
}

#[test]
fn rule_and_fault_forms_of_a_2_1_station_list_its_station_actions() {
    let mut app = station21();
    press(&mut app, KeyCode::Char('m'));
    let Mode::Form(form) = &app.mode else {
        panic!("rule form");
    };
    assert_eq!(form.fields[0].chosen(), Some(V21));
    assert_eq!(form.fields[1].options, table21().station_actions);
    assert!(
        form.fields[1]
            .options
            .iter()
            .any(|name| name == "NotifySettlement")
    );
    let fx = press(&mut app, KeyCode::Enter);
    assert!(matches!(fx.first(), Some(Effect::RespondScoped { .. })));
}

#[test]
fn rules_open_the_1_6_form_unless_the_station_has_a_table() {
    let mut old = crate::ui::tests::loaded();
    press(&mut old, KeyCode::Char('m'));
    let Mode::Form(form) = &old.mode else {
        panic!("rule form");
    };
    assert_eq!(form.fields.len(), 3);

    let mut app = station201();
    press(&mut app, KeyCode::Char('m'));
    let Mode::Form(form) = &app.mode else {
        panic!("rule form");
    };
    assert_eq!(form.fields.len(), 4);
    assert_eq!(form.fields[0].chosen(), Some(V201));
    let fx = press(&mut app, KeyCode::Enter);
    assert!(matches!(fx.first(), Some(Effect::RespondScoped { .. })));
}

#[test]
fn a_secret_variable_value_stays_out_of_results_and_notices() {
    const SECRET: &str = "s3cr3t-sentinel-value";
    let mut app = station201();
    let command = Command::Raw {
        action: "SetVariables".into(),
        payload: json!({"setVariableData": [{
            "component": {"name": "SecurityCtrlr"},
            "variable": {"name": "BasicAuthPassword"},
            "attributeValue": SECRET
        }]}),
    };
    app.command_done(
        "CP-1",
        &command,
        &CmdOutcome {
            result: Ok(json!({"setVariableResult": [{
                "attributeStatus": "Accepted",
                "component": {"name": "SecurityCtrlr"},
                "variable": {"name": "BasicAuthPassword"}
            }]})),
        },
    );
    let records = app.results.get("CP-1").expect("a result");
    for record in records {
        let text = format!("{record:?}");
        assert!(!text.contains(SECRET), "{text}");
    }
    assert!(!notice_text(&app).contains(SECRET));
    if let Mode::Detail(d) = &app.mode {
        assert!(!format!("{d:?}").contains(SECRET));
    }
}
