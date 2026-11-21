//! Tests of the model: `update2` with key sequences, effects and disabled reasons.

#![allow(
    clippy::assert_is_empty,
    reason = "`assert!(effects.is_empty())` reads as \"nothing was sent\""
)]

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kirby_ocpp_core::StationConfig;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::v2::messages::{Mutability, RegistrationStatus};
use kirby_ocpp_core::ocpp::v2::types::{
    ChargingState, Component, IdTokenType, StopReason, Variable, WireEnum,
};
use kirby_ocpp_core::scenario::builtin;
use kirby_ocpp_core::station::{Direction, Event, FrameRecord};
use kirby_ocpp_core::station2::{
    Command2, ConnectorSnapshot2, EvseSnapshot, Outcome2, Snapshot2, TransactionSnapshot,
    VariableView,
};
use kirby_ocpp_core::ws::ConnectionState;
use serde_json::json;

use crate::app::NoticeLevel;
use crate::v2::actions::{ActionId2, palette_rows};
use crate::v2::browser::{BrowserEvent, DeviceBrowser};
use crate::v2::forms::{
    SECURITY_EVENT_NAMES, charging_state_choice, set_variable_form, stop_reason_choice,
};
use crate::v2::keys::browser_event;
use crate::v2::{App2, Effect2, Mode2, Msg2, Tab2, catalog2, update2};

const SENTINEL: &str = "hunter2-sentinel-value";

fn connector(id: u32) -> ConnectorSnapshot2 {
    ConnectorSnapshot2 {
        id,
        status: kirby_ocpp_core::ocpp::v2::messages::ConnectorStatus::Available,
        ev_connected: false,
        fault: false,
    }
}

fn evse(id: u32) -> EvseSnapshot {
    EvseSnapshot {
        id,
        operative: true,
        connectors: vec![connector(1)],
        transaction: None,
        meter_wh: 0,
        reservation: None,
        offered_w: None,
        external_limit: false,
        transaction_limit: None,
        limit_reached: None,
        total_cost: None,
    }
}

fn transaction() -> TransactionSnapshot {
    TransactionSnapshot {
        transaction_id: "tx-0123456789abcdef".to_owned(),
        next_seq_no: 1,
        charging_state: ChargingState::Charging,
        id_token: Some("TOKEN".to_owned()),
        started_at: Utc::now(),
        energy_wh: 0,
        remote_start_id: None,
    }
}

/// A connected, booted station with EVSEs `1..=2`.
fn snapshot() -> Snapshot2 {
    let config = StationConfig::new("ws://localhost/ocpp");
    let mut snap = Snapshot2::initial(&config);
    snap.connection = ConnectionState::Connected;
    snap.registration = Some(RegistrationStatus::Accepted);
    snap.evses = vec![evse(1), evse(2)];
    snap
}

fn app_with(snap: Snapshot2) -> App2 {
    App2::new(snap, "ws://localhost/ocpp", "vendor")
}

fn app() -> App2 {
    app_with(snapshot())
}

fn plugged(app: &mut App2, id: u32) {
    for e in &mut app.snapshot.evses {
        if e.id == id {
            e.connectors[0].ev_connected = true;
        }
    }
}

fn with_transaction(app: &mut App2, id: u32) {
    for e in &mut app.snapshot.evses {
        if e.id == id {
            e.transaction = Some(transaction());
        }
    }
}

fn key(code: KeyCode) -> Msg2 {
    Msg2::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ctrl_c() -> Msg2 {
    Msg2::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
}

/// Presses `text` one character at a time; returns every effect.
fn press(app: &mut App2, text: &str) -> Vec<Effect2> {
    let mut all = Vec::new();
    for c in text.chars() {
        all.extend(update2(app, key(KeyCode::Char(c))));
    }
    all
}

fn press_code(app: &mut App2, code: KeyCode) -> Vec<Effect2> {
    update2(app, key(code))
}

fn send(label: &str, commands: Vec<Command2>, show_response: bool) -> Effect2 {
    Effect2::Send {
        label: label.to_owned(),
        commands,
        show_response,
    }
}

fn commands(effects: &[Effect2]) -> Vec<Command2> {
    effects
        .iter()
        .flat_map(|e| match e {
            Effect2::Send { commands, .. } => commands.clone(),
            _ => Vec::new(),
        })
        .collect()
}

fn notice(app: &App2) -> (String, NoticeLevel) {
    let n = app.notice.as_ref().expect("a notice");
    (n.text.clone(), n.level)
}

fn form_of(app: &App2) -> &crate::forms::Form {
    match &app.mode {
        Mode2::Form(f) => f,
        other => panic!("expected a form, found {}", other.name()),
    }
}

fn variable(component: &str, name: &str, value: Option<&str>, m: Mutability) -> VariableView {
    VariableView {
        component: component.to_owned(),
        instance: None,
        variable: name.to_owned(),
        value: value.map(str::to_owned),
        mutability: m,
    }
}

// ---- catalogue ----------------------------------------------------------------------

#[test]
fn accelerators_are_unique_per_tab_and_never_reserved() {
    let builtins = builtin::all_for(kirby_ocpp_core::ocpp::Protocol::V201);
    let cat = catalog2(&builtins);
    let mut seen = std::collections::HashSet::new();
    for a in &cat {
        if let Some(k) = a.key {
            assert!(
                !crate::actions::RESERVED_KEYS.contains(&k),
                "{} uses reserved {k}",
                a.label
            );
            assert!(seen.insert((a.tab, k)), "duplicate {k} in {:?}", a.tab);
        }
    }
    assert_eq!(
        cat.iter().filter(|a| a.tab == Tab2::Scenarios).count(),
        builtins.len() + 2
    );
}

#[test]
fn actions_without_a_2_0_1_meaning_are_not_in_the_catalogue_or_the_palette() {
    let app = app();
    let rows = palette_rows(&app.catalog);
    assert_eq!(rows.len(), app.catalog.len());
    let text: Vec<String> = app
        .catalog
        .iter()
        .map(|a| format!("{} {} {}", a.tab.title(), a.label, a.desc).to_lowercase())
        .collect();
    for hidden in [
        "add connector",
        "cycle status",
        "set status",
        "response rule",
        "config keys",
        "local auth",
        "charging profiles",
        "certificate",
        "diagnostics",
        "firmware",
        "idtag",
        "preparing",
        "finishing",
    ] {
        assert!(
            text.iter().all(|t| !t.contains(hidden)),
            "{hidden} must not be offered"
        );
    }
    // No Maintenance tab: the six tabs all have actions.
    for tab in Tab2::ALL {
        assert!(app.catalog.iter().any(|a| a.tab == tab), "{tab:?} is empty");
    }
}

#[test]
fn palette_runs_the_chosen_action() {
    let mut app = app();
    press(&mut app, ":");
    assert!(matches!(app.mode, Mode2::Palette(_)));
    press(&mut app, "heartbeat");
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        fx,
        vec![send("heartbeat", vec![Command2::Heartbeat], false)]
    );
    assert!(matches!(app.mode, Mode2::Normal));
}

// ---- Station tab --------------------------------------------------------------------

#[test]
fn station_tab_keys_send_their_commands() {
    let mut app = app();
    assert_eq!(
        press(&mut app, "b"),
        vec![send("boot", vec![Command2::Boot], false)]
    );
    assert_eq!(
        press(&mut app, "h"),
        vec![send("heartbeat", vec![Command2::Heartbeat], false)]
    );
    assert_eq!(
        press(&mut app, "r"),
        vec![send("reconnect", vec![Command2::Reconnect], false)]
    );
    assert_eq!(
        press(&mut app, "x"),
        vec![send("disconnect", vec![Command2::Disconnect], false)]
    );
    app.snapshot.connection = ConnectionState::Disconnected;
    assert_eq!(
        press(&mut app, "c"),
        vec![send("connect", vec![Command2::Connect], false)]
    );
}

#[test]
fn station_tab_opens_the_device_model_security_form_and_pending_list() {
    let mut app = app();
    press(&mut app, "K");
    assert!(matches!(app.mode, Mode2::DeviceModel(_)));
    press_code(&mut app, KeyCode::Esc);
    assert!(
        matches!(app.mode, Mode2::Normal),
        "Escape leaves the browser"
    );
    press(&mut app, "S");
    assert_eq!(form_of(&app).title, "Security event");
    press_code(&mut app, KeyCode::Esc);
    press(&mut app, "W");
    assert!(matches!(app.mode, Mode2::Pending(0)));
}

#[test]
fn security_event_form_offers_the_appendix_names_and_a_custom_type() {
    let mut app = app();
    press(&mut app, "S");
    assert_eq!(form_of(&app).fields[0].options, SECURITY_EVENT_NAMES);
    assert_eq!(SECURITY_EVENT_NAMES.len(), 21);
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        fx,
        vec![send(
            "security event",
            vec![Command2::SendSecurityEvent {
                event_type: "FirmwareUpdated".to_owned(),
                tech_info: None,
            }],
            false
        )]
    );
    // A pick from the list, with techInfo.
    press(&mut app, "S");
    press_code(&mut app, KeyCode::Right);
    press_code(&mut app, KeyCode::Tab);
    press_code(&mut app, KeyCode::Tab);
    press(&mut app, "door open");
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        commands(&fx),
        vec![Command2::SendSecurityEvent {
            event_type: "FailedToAuthenticateAtCsms".to_owned(),
            tech_info: Some("door open".to_owned()),
        }]
    );
    // A custom type replaces the pick: 50 characters pass, 51 do not.
    press(&mut app, "S");
    press_code(&mut app, KeyCode::Tab);
    press(&mut app, &"x".repeat(51));
    assert!(press_code(&mut app, KeyCode::Enter).is_empty());
    assert!(form_of(&app).fields[1].error.is_some());
    press_code(&mut app, KeyCode::Backspace);
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        commands(&fx),
        vec![Command2::SendSecurityEvent {
            event_type: "x".repeat(50),
            tech_info: None,
        }]
    );
}

#[test]
fn security_event_tech_info_is_limited_to_255_characters() {
    let mut app = app();
    press(&mut app, "S");
    press_code(&mut app, KeyCode::Tab);
    press_code(&mut app, KeyCode::Tab);
    press(&mut app, &"t".repeat(256));
    assert!(press_code(&mut app, KeyCode::Enter).is_empty());
    assert!(form_of(&app).fields[2].error.is_some());
    press_code(&mut app, KeyCode::Backspace);
    assert_eq!(commands(&press_code(&mut app, KeyCode::Enter)).len(), 1);
}

#[test]
fn outbound_security_event_calls_are_counted() {
    let mut app = app();
    let frame = |direction, action: &str, kind: u64| {
        Event::Frame(FrameRecord {
            ts: Utc::now(),
            direction,
            msg_id: Some("1".to_owned()),
            action: Some(action.to_owned()),
            frame: json!([kind, "1", action, {}]),
        })
    };
    update2(
        &mut app,
        Msg2::Event(frame(Direction::Out, "SecurityEventNotification", 2)),
    );
    update2(&mut app, Msg2::Event(frame(Direction::Out, "Heartbeat", 2)));
    update2(
        &mut app,
        Msg2::Event(frame(Direction::In, "SecurityEventNotification", 2)),
    );
    update2(
        &mut app,
        Msg2::Event(frame(Direction::Out, "SecurityEventNotification", 3)),
    );
    assert_eq!(app.security_events_sent, 1);
    assert_eq!(app.log.len(), 4);
}

// ---- EVSE tab -----------------------------------------------------------------------

#[test]
fn evse_tab_keys_send_their_commands() {
    let mut app = app();
    press(&mut app, "2");
    assert_eq!(app.tab, Tab2::Evse);
    assert_eq!(
        press(&mut app, "p"),
        vec![send("plug in", vec![Command2::PlugIn { evse: 1 }], false)]
    );
    plugged(&mut app, 1);
    assert_eq!(
        press(&mut app, "p"),
        vec![send("unplug", vec![Command2::Unplug { evse: 1 }], false)]
    );
    assert_eq!(
        press(&mut app, "f"),
        vec![send(
            "fault",
            vec![Command2::Fault {
                evse: 1,
                connector: None
            }],
            false
        )]
    );
    app.snapshot.evses[0].connectors[0].fault = true;
    assert_eq!(
        press(&mut app, "f"),
        vec![send(
            "clear fault",
            vec![Command2::ClearFault {
                evse: 1,
                connector: None
            }],
            false
        )]
    );
    assert_eq!(
        press(&mut app, "n"),
        vec![send(
            "status notification",
            vec![Command2::StatusNotification {
                evse: 1,
                connector: 1
            }],
            false
        )]
    );
}

#[test]
fn the_selection_moves_between_evses() {
    let mut app = app();
    press(&mut app, "2");
    press(&mut app, "]");
    assert_eq!(app.evse, 2);
    assert_eq!(
        press(&mut app, "p"),
        vec![send("plug in", vec![Command2::PlugIn { evse: 2 }], false)]
    );
    press(&mut app, "]");
    assert_eq!(app.evse, 2, "stops at the last EVSE");
    press(&mut app, "[");
    assert_eq!(app.evse, 1);
}

// ---- Transaction tab ----------------------------------------------------------------

#[test]
fn authorize_form_sends_the_token_and_type() {
    let mut app = app();
    press(&mut app, "3a");
    assert_eq!(form_of(&app).title, "Authorize");
    press(&mut app, "TOKEN-1");
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        fx,
        vec![send(
            "authorize",
            vec![Command2::Authorize {
                id_token: "TOKEN-1".to_owned(),
                token_type: IdTokenType::Local,
            }],
            false
        )]
    );
}

#[test]
fn id_token_of_36_characters_is_accepted_and_37_refused() {
    let mut app = app();
    press(&mut app, "3a");
    press(&mut app, &"a".repeat(37));
    assert!(press_code(&mut app, KeyCode::Enter).is_empty());
    assert!(matches!(app.mode, Mode2::Form(_)), "the form stays open");
    assert!(form_of(&app).fields[0].error.is_some());
    press_code(&mut app, KeyCode::Backspace);
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(commands(&fx).len(), 1);
    assert!(matches!(app.mode, Mode2::Normal));
}

#[test]
fn token_type_options_are_the_wire_names() {
    let mut app = app();
    press(&mut app, "3a");
    let options = &form_of(&app).fields[1].options;
    let wire: Vec<&str> = IdTokenType::KNOWN.iter().map(IdTokenType::as_str).collect();
    assert_eq!(options.iter().map(String::as_str).collect::<Vec<_>>(), wire);
    assert!(wire.contains(&"eMAID") && wire.contains(&"ISO14443"));
    // `Right` moves from Local to MacAddress.
    press_code(&mut app, KeyCode::Tab);
    press_code(&mut app, KeyCode::Right);
    press_code(&mut app, KeyCode::BackTab);
    press(&mut app, "x");
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        commands(&fx),
        vec![Command2::Authorize {
            id_token: "x".to_owned(),
            token_type: IdTokenType::MacAddress,
        }]
    );
}

#[test]
fn present_token_with_and_without_an_evse() {
    let mut app = app();
    plugged(&mut app, 1);
    press(&mut app, "3t");
    assert_eq!(form_of(&app).title, "Present token");
    press_code(&mut app, KeyCode::Tab);
    press(&mut app, "CARD");
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        fx,
        vec![send(
            "present token",
            vec![Command2::PresentToken {
                evse: Some(1),
                id_token: "CARD".to_owned(),
                token_type: IdTokenType::Local,
            }],
            false
        )]
    );
    // Second EVSE selected, but "any" is chosen: the station picks.
    press(&mut app, "3");
    press(&mut app, "]");
    press(&mut app, "t");
    press_code(&mut app, KeyCode::Right);
    press_code(&mut app, KeyCode::Tab);
    press(&mut app, "CARD");
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        commands(&fx),
        vec![Command2::PresentToken {
            evse: None,
            id_token: "CARD".to_owned(),
            token_type: IdTokenType::Local,
        }]
    );
}

#[test]
fn stop_reasons_are_the_nineteen_wire_names_with_local_first() {
    let c = stop_reason_choice(1, Protocol::V201);
    assert_eq!(c.items.len(), 19);
    assert_eq!(c.items[0], "Local");
    let mut got: Vec<&str> = c.items.iter().map(String::as_str).collect();
    let mut want: Vec<&str> = StopReason::ALL
        .iter()
        .filter(|r| !r.is_v21_only())
        .map(WireEnum::as_str)
        .collect();
    got.sort_unstable();
    want.sort_unstable();
    assert_eq!(got, want);
    assert!(got.contains(&"EVDisconnected") && got.contains(&"SOCLimitReached"));
    assert!(!got.contains(&"ReqEnergyTransferRejected"));
}

#[test]
fn a_2_1_station_also_offers_the_reasons_new_in_2_1() {
    let c = stop_reason_choice(1, Protocol::V21);
    assert_eq!(c.items.len(), 20);
    assert_eq!(c.items[0], "Local");
    assert!(c.items.iter().any(|i| i == "ReqEnergyTransferRejected"));
}

#[test]
fn charging_states_are_the_five_wire_names() {
    let c = charging_state_choice(1);
    let want: Vec<&str> = ChargingState::ALL.iter().map(WireEnum::as_str).collect();
    assert_eq!(c.items.iter().map(String::as_str).collect::<Vec<_>>(), want);
    assert_eq!(want.len(), 5);
    assert!(want.contains(&"EVConnected") && want.contains(&"SuspendedEVSE"));
}

#[test]
fn stop_transaction_picks_a_reason() {
    let mut app = app();
    with_transaction(&mut app, 1);
    press(&mut app, "3e");
    assert!(matches!(app.mode, Mode2::Choice(_)));
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        fx,
        vec![send(
            "stop transaction: Local",
            vec![Command2::StopTransaction {
                evse: 1,
                reason: StopReason::Local
            }],
            false
        )]
    );
    // The second item is the first of `ALL` after `Local`.
    press(&mut app, "e");
    press_code(&mut app, KeyCode::Down);
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        commands(&fx),
        vec![Command2::StopTransaction {
            evse: 1,
            reason: StopReason::DeAuthorized
        }]
    );
}

#[test]
fn set_external_limit_form_sends_a_schedule() {
    let mut app = app();
    press(&mut app, "2l");
    assert_eq!(form_of(&app).title, "External limit");
    // The defaults: the whole station, 16 A.
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        fx,
        vec![send(
            "set external limit",
            vec![Command2::SetExternalLimit {
                evse: 0,
                limit: serde_json::json!({
                    "id": 1000,
                    "chargingRateUnit": "A",
                    "chargingSchedulePeriod": [{"startPeriod": 0, "limit": 16.0}],
                }),
            }],
            false
        )]
    );
    // EVSE 2 in watts.
    press(&mut app, "l");
    press_code(&mut app, KeyCode::Backspace);
    press(&mut app, "2");
    press_code(&mut app, KeyCode::Tab);
    press_code(&mut app, KeyCode::Backspace);
    press_code(&mut app, KeyCode::Backspace);
    press(&mut app, "3000.5");
    press_code(&mut app, KeyCode::Tab);
    press_code(&mut app, KeyCode::Right);
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        commands(&fx),
        vec![Command2::SetExternalLimit {
            evse: 2,
            limit: serde_json::json!({
                "id": 1002,
                "chargingRateUnit": "W",
                "chargingSchedulePeriod": [{"startPeriod": 0, "limit": 3000.5}],
            }),
        }]
    );
}

#[test]
fn set_external_limit_form_refuses_bad_numbers() {
    let mut app = app();
    press(&mut app, "2l");
    // EVSE 3 does not exist (the station has 1 and 2).
    press_code(&mut app, KeyCode::Backspace);
    press(&mut app, "3");
    assert!(form_of(&app).fields[0].error.is_some());
    press_code(&mut app, KeyCode::Backspace);
    press(&mut app, "0");
    press_code(&mut app, KeyCode::Tab);
    press_code(&mut app, KeyCode::Backspace);
    press_code(&mut app, KeyCode::Backspace);
    press(&mut app, "0");
    assert!(form_of(&app).fields[1].error.is_some(), "limit 0 refused");
    assert!(press_code(&mut app, KeyCode::Enter).is_empty());
    assert!(matches!(app.mode, Mode2::Form(_)), "the form stays open");
}

#[test]
fn clear_external_limit_form_sends_the_evse() {
    let mut app = app();
    press(&mut app, "2L");
    assert_eq!(form_of(&app).title, "Clear external limit");
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        fx,
        vec![send(
            "clear external limit",
            vec![Command2::ClearExternalLimit { evse: 0 }],
            false
        )]
    );
    press(&mut app, "L");
    press_code(&mut app, KeyCode::Backspace);
    press(&mut app, "1");
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        commands(&fx),
        vec![Command2::ClearExternalLimit { evse: 1 }]
    );
}

#[test]
fn the_external_limit_actions_are_in_the_palette() {
    let app = app();
    let labels: Vec<&str> = app.catalog.iter().map(|a| a.label.as_str()).collect();
    assert!(labels.contains(&"Set external limit..."));
    assert!(labels.contains(&"Clear external limit..."));
}

#[test]
fn charging_state_choice_sends_the_state() {
    let mut app = app();
    with_transaction(&mut app, 1);
    press(&mut app, "3g");
    press_code(&mut app, KeyCode::End);
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        commands(&fx),
        vec![Command2::SetChargingState {
            evse: 1,
            state: ChargingState::Idle
        }]
    );
}

#[test]
fn meter_key_sends_the_advance_then_the_sample() {
    let mut app = app();
    with_transaction(&mut app, 1);
    let fx = press(&mut app, "3m");
    assert_eq!(
        fx,
        vec![send(
            "meter +120 Wh",
            vec![
                Command2::AdvanceMeter {
                    evse: 1,
                    wh_delta: 120
                },
                Command2::SampleMeter { evse: 1 },
            ],
            false
        )]
    );
}

#[test]
fn meter_custom_form_sends_a_chosen_delta_with_or_without_a_sample() {
    let mut app = app();
    press(&mut app, "3c");
    assert_eq!(form_of(&app).title, "Meter");
    press_code(&mut app, KeyCode::Tab);
    for _ in 0..3 {
        press_code(&mut app, KeyCode::Backspace);
    }
    press(&mut app, "500");
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        fx,
        vec![send(
            "meter +500 Wh",
            vec![
                Command2::AdvanceMeter {
                    evse: 1,
                    wh_delta: 500
                },
                Command2::SampleMeter { evse: 1 },
            ],
            false
        )]
    );
    // EVSE 3 does not exist; "no sample" sends the advance only.
    press(&mut app, "c");
    press_code(&mut app, KeyCode::Backspace);
    press(&mut app, "3");
    assert!(press_code(&mut app, KeyCode::Enter).is_empty());
    press_code(&mut app, KeyCode::Backspace);
    press(&mut app, "2");
    press_code(&mut app, KeyCode::Tab);
    press_code(&mut app, KeyCode::Tab);
    press_code(&mut app, KeyCode::Right);
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        commands(&fx),
        vec![Command2::AdvanceMeter {
            evse: 2,
            wh_delta: 120
        }]
    );
}

// ---- Data tab -----------------------------------------------------------------------

#[test]
fn data_transfer_sends_any_json_value() {
    let mut app = app();
    press(&mut app, "4d");
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        fx,
        vec![send(
            "data transfer",
            vec![Command2::DataTransfer {
                vendor_id: "vendor".to_owned(),
                message_id: None,
                data: None,
            }],
            true
        )]
    );
    for (typed, value) in [
        ("\"text\"", json!("text")),
        ("42", json!(42)),
        ("[1,2]", json!([1, 2])),
        ("{\"a\":1}", json!({"a": 1})),
    ] {
        press(&mut app, "d");
        press_code(&mut app, KeyCode::Tab);
        press(&mut app, "msg");
        press_code(&mut app, KeyCode::Tab);
        press(&mut app, typed);
        let fx = press_code(&mut app, KeyCode::Enter);
        assert_eq!(
            commands(&fx),
            vec![Command2::DataTransfer {
                vendor_id: "vendor".to_owned(),
                message_id: Some("msg".to_owned()),
                data: Some(value),
            }],
            "{typed}"
        );
    }
    // Text that is not JSON is refused on the field.
    press(&mut app, "d");
    press_code(&mut app, KeyCode::Tab);
    press_code(&mut app, KeyCode::Tab);
    press(&mut app, "not json");
    assert!(press_code(&mut app, KeyCode::Enter).is_empty());
    assert!(form_of(&app).fields[2].error.is_some());
}

#[test]
fn data_transfer_limits_vendor_and_message_ids() {
    let mut app = app();
    press(&mut app, "4d");
    press(&mut app, &"v".repeat(250));
    assert!(
        press_code(&mut app, KeyCode::Enter).is_empty(),
        "vendor 256"
    );
    assert!(form_of(&app).fields[0].error.is_some());
    press_code(&mut app, KeyCode::Esc);
    press(&mut app, "d");
    press_code(&mut app, KeyCode::Tab);
    press(&mut app, &"m".repeat(51));
    assert!(
        press_code(&mut app, KeyCode::Enter).is_empty(),
        "message 51"
    );
}

#[test]
fn vendor_ping_sends_the_vendor_id_only() {
    let mut app = app();
    assert_eq!(
        press(&mut app, "4v"),
        vec![send(
            "vendor ping",
            vec![Command2::DataTransfer {
                vendor_id: "vendor".to_owned(),
                message_id: None,
                data: None,
            }],
            true
        )]
    );
}

#[test]
fn raw_call_is_checked_against_the_2_0_1_schema() {
    let mut app = app();
    press(&mut app, "4r");
    let options = form_of(&app).fields[0].options.clone();
    assert!(options.iter().any(|o| o == "BootNotification"));
    assert!(!options.iter().any(|o| o == "Reset"), "CSMS-only action");
    // The default Heartbeat with an empty object is valid.
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        fx,
        vec![send(
            "raw Heartbeat",
            vec![Command2::SendRaw {
                action: "Heartbeat".to_owned(),
                payload: json!({}),
                validate: true,
            }],
            true
        )]
    );
    // `{}` is not a valid BootNotification: the form stays open with the schema error.
    press(&mut app, "r");
    while form_of(&app).fields[0].options[form_of(&app).fields[0].option] != "BootNotification" {
        press_code(&mut app, KeyCode::Right);
    }
    assert!(press_code(&mut app, KeyCode::Enter).is_empty());
    let f = form_of(&app);
    assert_eq!(f.focus, 1);
    assert!(f.fields[1].error.is_some());
    // A payload that is not an object is refused by the field itself.
    press_code(&mut app, KeyCode::Backspace);
    press_code(&mut app, KeyCode::Backspace);
    press(&mut app, "[]");
    assert!(press_code(&mut app, KeyCode::Enter).is_empty());
}

// ---- Logs and Scenarios ---------------------------------------------------------------

#[test]
fn logs_tab_keys_work_as_in_1_6() {
    let mut app = app();
    press(&mut app, "5");
    assert_eq!(app.tab, Tab2::Logs);
    assert_eq!(app.focus, crate::app::Focus::Log);
    press(&mut app, "s");
    assert_eq!(form_of(&app).title, "Search log");
    press(&mut app, "boot");
    press_code(&mut app, KeyCode::Enter);
    assert_eq!(app.log.search, "boot");
    assert_eq!(notice(&app).0, "0 matching rows");
    press_code(&mut app, KeyCode::Esc);
    assert!(app.log.search.is_empty(), "Escape clears the search");
    press(&mut app, "f");
    assert_eq!(notice(&app).0, "log filter: calls");
    press(&mut app, "p");
    assert_eq!(notice(&app).0, "log paused");
    press(&mut app, "c");
    assert!(matches!(
        &app.mode,
        Mode2::Confirm(c) if c.message == "Clear all 0 log rows?"
    ));
    press(&mut app, "y");
    assert_eq!(notice(&app).0, "log cleared");
    press(&mut app, "e");
    assert_eq!(form_of(&app).title, "Export log (JSONL)");
    // Nothing to copy from an empty log.
    press_code(&mut app, KeyCode::Esc);
    assert!(press(&mut app, "y").is_empty());
    assert!(notice(&app).0.contains("no selected row"));
}

#[test]
fn scenarios_run_once_and_can_be_cancelled_or_loaded() {
    let mut app = app();
    assert!(!app.scenarios.is_empty());
    let first = app.scenarios[0].clone();
    press(&mut app, "6");
    let fx = press(&mut app, "o");
    assert_eq!(fx, vec![Effect2::RunScenario(Box::new(first))]);
    assert!(app.scenario.running);
    // The run guard: a second run and a file run are refused while one is going.
    assert!(press(&mut app, "o").is_empty());
    assert_eq!(notice(&app).0, "a scenario is already running");
    assert!(press(&mut app, "l").is_empty());
    assert!(matches!(app.mode, Mode2::Normal));
    assert_eq!(press(&mut app, "x"), vec![Effect2::CancelScenario]);
    update2(
        &mut app,
        Msg2::ScenarioError("scenario failed to start".to_owned()),
    );
    assert!(!app.scenario.running);
    assert_eq!(notice(&app).1, NoticeLevel::Error);
    // Cancel without a run says why.
    assert!(press(&mut app, "x").is_empty());
    assert_eq!(notice(&app).0, "no scenario is running");
    // A file run.
    press(&mut app, "l");
    press(&mut app, "/tmp/scenario.toml");
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        fx,
        vec![Effect2::RunScenarioFile("/tmp/scenario.toml".to_owned())]
    );
    assert!(app.scenario.running);
}

// ---- disabled reasons ---------------------------------------------------------------

#[test]
fn disabled_actions_explain_themselves() {
    let mut app = app_with({
        let mut s = snapshot();
        s.connection = ConnectionState::Disconnected;
        s
    });
    for id in [
        ActionId2::Boot,
        ActionId2::Heartbeat,
        ActionId2::SecurityEvent,
        ActionId2::Authorize,
        ActionId2::DataTransfer,
        ActionId2::VendorPing,
        ActionId2::RawCall,
        ActionId2::StatusNotification,
    ] {
        assert_eq!(
            app.reason_for2(id).as_deref(),
            Some("not connected to the CSMS"),
            "{id:?}"
        );
    }
    assert!(app.reason_for2(ActionId2::Connect).is_none());
    assert_eq!(
        app.reason_for2(ActionId2::Disconnect).as_deref(),
        Some("already disconnected")
    );
    // The key says why and sends nothing.
    assert!(press(&mut app, "b").is_empty());
    assert_eq!(
        notice(&app),
        ("not connected to the CSMS".to_owned(), NoticeLevel::Error)
    );
}

#[test]
fn transaction_actions_need_a_transaction_but_a_token_does_not_need_an_ev() {
    let mut app = app();
    assert_eq!(
        app.reason_for2(ActionId2::StopTransaction).as_deref(),
        Some("no transaction on EVSE 1")
    );
    assert_eq!(
        app.reason_for2(ActionId2::ChargingState).as_deref(),
        Some("no transaction on EVSE 1")
    );
    // A token before the cable is a valid order of events (TxStartPoint decides).
    assert!(app.reason_for2(ActionId2::PresentToken).is_none());
    assert!(press(&mut app, "3e").is_empty());
    assert_eq!(notice(&app).0, "no transaction on EVSE 1");
    assert!(matches!(app.mode, Mode2::Normal));
    with_transaction(&mut app, 1);
    assert!(app.reason_for2(ActionId2::StopTransaction).is_none());
    assert!(app.reason_for2(ActionId2::ChargingState).is_none());
}

#[test]
fn a_station_without_evses_disables_the_evse_actions() {
    let mut s = snapshot();
    s.evses.clear();
    let app = app_with(s);
    assert_eq!(app.evse, 0);
    for id in [
        ActionId2::PlugToggle,
        ActionId2::FaultToggle,
        ActionId2::Meter120,
        ActionId2::MeterCustom,
        ActionId2::PresentToken,
        ActionId2::StopTransaction,
    ] {
        assert_eq!(
            app.reason_for2(id).as_deref(),
            Some("no EVSE selected"),
            "{id:?}"
        );
    }
}

// ---- pending list -------------------------------------------------------------------

#[test]
fn pending_commands_are_listed_and_cancelled() {
    let mut app = app();
    press(&mut app, "h");
    assert_eq!(app.pending.len(), 1);
    assert_eq!(app.pending[0].label, "heartbeat");
    assert_eq!(app.pending[0].action.as_deref(), Some("Heartbeat"));
    press(&mut app, "W");
    assert!(matches!(app.mode, Mode2::Pending(0)));
    assert_eq!(
        press(&mut app, "x"),
        vec![Effect2::CancelCall {
            action: "Heartbeat".to_owned()
        }]
    );
    assert!(matches!(app.mode, Mode2::Pending(0)));
    assert_eq!(
        press_code(&mut app, KeyCode::Delete),
        vec![Effect2::CancelCall {
            action: "Heartbeat".to_owned()
        }]
    );
    assert_eq!(app.pending.len(), 1, "nothing was cancelled");
    press_code(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode2::Normal));
    update2(
        &mut app,
        Msg2::CommandDone {
            label: "heartbeat".to_owned(),
            show_response: false,
            result: Ok(Outcome2::Done),
        },
    );
    assert!(app.pending.is_empty());
    assert_eq!(
        notice(&app),
        ("heartbeat: ok".to_owned(), NoticeLevel::Success)
    );
}

#[test]
fn losing_the_connection_drops_the_waiting_marks() {
    let mut app = app();
    press(&mut app, "h");
    let mut gone = snapshot();
    gone.connection = ConnectionState::Disconnected;
    update2(&mut app, Msg2::Snapshot(Box::new(gone)));
    assert!(app.pending.is_empty());
}

#[test]
fn the_selection_follows_the_snapshot() {
    let mut app = app();
    press(&mut app, "2]");
    assert_eq!(app.evse, 2);
    let mut s = snapshot();
    s.evses.truncate(1);
    update2(&mut app, Msg2::Snapshot(Box::new(s)));
    assert_eq!(app.evse, 1);
    update2(&mut app, Msg2::Snapshot(Box::new(snapshot())));
    assert_eq!(app.evse, 1);
}

// ---- command results ------------------------------------------------------------------

#[test]
fn results_are_reported_in_wire_words() {
    let mut app = app();
    let done = |app: &mut App2, label: &str, show: bool, result| {
        update2(
            app,
            Msg2::CommandDone {
                label: label.to_owned(),
                show_response: show,
                result,
            },
        );
    };
    done(
        &mut app,
        "present token",
        false,
        Ok(Outcome2::TransactionStarted {
            evse: 2,
            transaction_id: "tx-1".to_owned(),
        }),
    );
    assert_eq!(
        notice(&app),
        (
            "transaction tx-1 started on EVSE 2".to_owned(),
            NoticeLevel::Success
        )
    );
    done(
        &mut app,
        "authorize",
        false,
        Ok(Outcome2::Authorized {
            status: "Blocked".to_owned(),
            id_token_info: None,
        }),
    );
    assert_eq!(
        notice(&app),
        ("authorize: Blocked".to_owned(), NoticeLevel::Error)
    );
    done(&mut app, "boot", false, Err("not connected".to_owned()));
    assert_eq!(
        notice(&app),
        ("boot: not connected".to_owned(), NoticeLevel::Error)
    );
    done(
        &mut app,
        "raw Heartbeat",
        true,
        Ok(Outcome2::Response {
            payload: json!({"currentTime": "2030-01-01T00:00:00Z"}),
        }),
    );
    match &app.mode {
        Mode2::Detail(d) => {
            assert_eq!(d.title, "Response to raw Heartbeat");
            assert!(d.copy.contains("currentTime"));
        }
        other => panic!("expected a detail, found {}", other.name()),
    }
}

// ---- quit ---------------------------------------------------------------------------

#[test]
fn quit_is_immediate_when_nothing_would_be_lost() {
    let mut app = app();
    press(&mut app, "q");
    assert!(app.quit);
}

#[test]
fn quit_asks_while_a_transaction_runs() {
    let mut app = app();
    with_transaction(&mut app, 2);
    press(&mut app, "q");
    assert!(!app.quit);
    assert!(matches!(
        &app.mode,
        Mode2::Confirm(c) if c.message == "Quit? A transaction is active"
    ));
    press(&mut app, "n");
    assert!(!app.quit);
    assert!(matches!(app.mode, Mode2::Normal));
    press(&mut app, "q");
    press(&mut app, "y");
    assert!(app.quit);
}

#[test]
fn quit_asks_while_a_scenario_runs_and_ctrl_c_twice_quits() {
    let mut app = app();
    press(&mut app, "6o");
    assert!(app.scenario.running);
    update2(&mut app, ctrl_c());
    assert!(!app.quit);
    assert!(matches!(app.mode, Mode2::Confirm(_)));
    update2(&mut app, ctrl_c());
    assert!(app.quit);
}

#[test]
fn ctrl_c_in_a_form_asks_and_brings_the_form_back_on_no() {
    let mut app = app();
    with_transaction(&mut app, 1);
    press(&mut app, "3a");
    update2(&mut app, ctrl_c());
    assert!(matches!(app.mode, Mode2::Confirm(_)));
    press(&mut app, "n");
    assert!(matches!(app.mode, Mode2::Form(_)));
    assert!(!app.quit);
}

#[test]
fn the_signal_quits_at_once() {
    let mut app = app();
    with_transaction(&mut app, 1);
    update2(&mut app, Msg2::Signal);
    assert!(app.quit);
}

// ---- device model -------------------------------------------------------------------

#[test]
fn editing_a_variable_opens_a_prefilled_form_and_returns_to_the_browser() {
    let mut s = snapshot();
    s.variables = vec![variable(
        "OCPPCommCtrlr",
        "HeartbeatInterval",
        Some("300"),
        Mutability::ReadWrite,
    )];
    let mut app = app_with(s);
    press(&mut app, "K");
    let Mode2::DeviceModel(b) = std::mem::replace(&mut app.mode, Mode2::Normal) else {
        panic!("expected the browser");
    };
    browser_event(&mut app, b, BrowserEvent::Edit(0));
    let f = form_of(&app);
    assert_eq!(f.title, "Set OCPPCommCtrlr.HeartbeatInterval");
    assert_eq!(f.fields[0].input.text(), "OCPPCommCtrlr");
    assert_eq!(f.fields[2].input.text(), "HeartbeatInterval");
    assert_eq!(f.fields[3].input.text(), "300");
    assert!(!f.fields[3].secret);
    assert!(app.suspended.is_some());
    press_code(&mut app, KeyCode::End);
    for _ in 0..3 {
        press_code(&mut app, KeyCode::Tab);
    }
    press_code(&mut app, KeyCode::End);
    press_code(&mut app, KeyCode::Backspace);
    press(&mut app, "5");
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        fx,
        vec![send(
            "set OCPPCommCtrlr.HeartbeatInterval",
            vec![Command2::SetVariable {
                component: Component {
                    evse: None,
                    name: "OCPPCommCtrlr".to_owned(),
                    instance: None,
                    custom_data: None,
                },
                variable: Variable {
                    name: "HeartbeatInterval".to_owned(),
                    instance: None,
                    custom_data: None,
                },
                value: "305".to_owned(),
            }],
            false
        )]
    );
    assert!(
        matches!(app.mode, Mode2::DeviceModel(_)),
        "back in the browser"
    );
    assert!(app.suspended.is_none());
}

#[test]
fn a_read_only_variable_is_not_edited_and_a_shown_one_hides_secrets() {
    let mut s = snapshot();
    s.variables = vec![
        variable(
            "DeviceDataCtrlr",
            "ItemsPerMessage",
            Some("10"),
            Mutability::ReadOnly,
        ),
        variable(
            "SecurityCtrlr",
            "BasicAuthPassword",
            None,
            Mutability::WriteOnly,
        ),
    ];
    let mut app = app_with(s);
    browser_event(&mut app, DeviceBrowser::new(), BrowserEvent::Edit(0));
    assert!(matches!(app.mode, Mode2::DeviceModel(_)));
    assert_eq!(
        notice(&app),
        (
            "DeviceDataCtrlr.ItemsPerMessage is read-only".to_owned(),
            NoticeLevel::Error
        )
    );
    browser_event(&mut app, DeviceBrowser::new(), BrowserEvent::Show(1));
    match &app.mode {
        Mode2::Detail(d) => {
            assert!(d.lines.iter().any(|l| l.contains("(hidden)")));
            assert!(d.lines.iter().any(|l| l.contains("WriteOnly")));
        }
        other => panic!("expected a detail, found {}", other.name()),
    }
}

#[test]
fn a_password_is_hidden_in_the_form_and_absent_from_debug_and_labels() {
    for (component, name, value) in [
        ("SecurityCtrlr", "BasicAuthPassword", None),
        ("NetworkConfiguration", "BasicAuthPassword", None),
        // A password that does come back in a view is still not prefilled.
        ("SecurityCtrlr", "BasicAuthPassword", Some("old-secret")),
    ] {
        let view = variable(component, name, value, Mutability::ReadWrite);
        let mut form = set_variable_form(&view);
        assert!(form.fields[3].secret, "{component}");
        assert_eq!(form.fields[3].input.text(), "", "never prefilled");
        let mut app = app();
        form.focus = 3;
        app.mode = Mode2::Form(form);
        press(&mut app, SENTINEL);
        let debug = format!("{:?}", app.mode);
        assert!(!debug.contains(SENTINEL), "form Debug leaks the value");
        assert!(!debug.contains("old-secret"));
        let fx = press_code(&mut app, KeyCode::Enter);
        let [
            Effect2::Send {
                label, commands, ..
            },
        ] = fx.as_slice()
        else {
            panic!("expected one send");
        };
        assert!(!label.contains(SENTINEL), "label leaks the value");
        assert!(matches!(&commands[0], Command2::SetVariable { value, .. } if value == SENTINEL));
        assert!(!app.pending[0].label.contains(SENTINEL));
    }
}

#[test]
fn a_variable_instance_goes_to_the_variable() {
    let mut s = snapshot();
    let mut v = variable(
        "OCPPCommCtrlr",
        "MessageAttemptInterval",
        Some("5"),
        Mutability::ReadWrite,
    );
    v.instance = Some("TransactionEvent".to_owned());
    s.variables = vec![v];
    let mut app = app_with(s);
    browser_event(&mut app, DeviceBrowser::new(), BrowserEvent::Edit(0));
    assert_eq!(form_of(&app).fields[1].input.text(), "TransactionEvent");
    let fx = press_code(&mut app, KeyCode::Enter);
    let cmds = commands(&fx);
    assert!(matches!(
        &cmds[0],
        Command2::SetVariable { variable, component, .. }
            if variable.instance.as_deref() == Some("TransactionEvent") && component.instance.is_none()
    ));
}

// ---- misc ---------------------------------------------------------------------------

#[test]
fn tabs_follow_the_digits_and_tab_keys() {
    let mut app = app();
    for (c, tab) in ['1', '2', '3', '4', '5', '6'].into_iter().zip(Tab2::ALL) {
        press(&mut app, &c.to_string());
        assert_eq!(app.tab, tab);
    }
    press(&mut app, "7");
    assert_eq!(app.tab, Tab2::Scenarios, "there is no seventh tab");
    press_code(&mut app, KeyCode::Tab);
    assert_eq!(app.tab, Tab2::Station);
    press_code(&mut app, KeyCode::BackTab);
    assert_eq!(app.tab, Tab2::Scenarios);
}

#[test]
fn enter_runs_the_selected_action_and_arrows_move_the_selection() {
    let mut app = app();
    press_code(&mut app, KeyCode::Down);
    let fx = press_code(&mut app, KeyCode::Enter);
    assert_eq!(
        fx,
        vec![send("heartbeat", vec![Command2::Heartbeat], false)]
    );
}

#[test]
fn help_closes_with_escape_and_hides_nothing_else() {
    let mut app = app();
    press(&mut app, "?");
    assert!(matches!(app.mode, Mode2::Help));
    press(&mut app, "b");
    assert!(matches!(app.mode, Mode2::Help), "other keys do nothing");
    press_code(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode2::Normal));
}

#[test]
fn a_paste_goes_to_the_form() {
    let mut app = app();
    press(&mut app, "3a");
    update2(&mut app, Msg2::Paste("PASTED".to_owned()));
    assert_eq!(form_of(&app).fields[0].input.text(), "PASTED");
}

#[test]
fn notices_are_sanitized() {
    let mut app = app();
    update2(
        &mut app,
        Msg2::Notice(NoticeLevel::Error, "bad\u{1b}[31m text".to_owned()),
    );
    assert!(!notice(&app).0.contains('\u{1b}'));
}
