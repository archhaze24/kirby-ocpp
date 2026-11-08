//! Model tests for the commands, forms, palette and results, and the executor against a real
//! engine and a real station.

#![allow(
    clippy::assert_is_empty,
    reason = "test fixtures read better field by field"
)]

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyModifiers};
use kirby_csms_core::{CsmsConfig, CsmsEvent, CsmsHandle, CsmsServer, TransactionStatus};
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::types::{AvailabilityType, MessageTrigger, ResetType};
use kirby_ocpp_core::{Station, StationConfig, SystemClock};
use kirby_ocpp_tui::app::NoticeLevel;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::actions::{ActionId, Rules};
use crate::app::{App, ConfirmAction, Effect, Mode, Msg, update};
use crate::command::{CmdError, CmdOutcome, Command};
use crate::form::FaultKind;
use crate::tests::{app_with, at, info, press, type_text, wait_booted};
use crate::ui::tests::loaded;
use crate::{Exec, TuiOptions, drive, execute};

const STEP: Duration = Duration::from_secs(10);

fn notice(app: &App) -> String {
    app.notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default()
}

fn ok(answer: Value) -> CmdOutcome {
    CmdOutcome { result: Ok(answer) }
}

fn failed(text: &str) -> CmdOutcome {
    CmdOutcome {
        result: Err(CmdError(text.to_owned())),
    }
}

fn only_send(fx: Vec<Effect>) -> (String, Command) {
    match <[Effect; 1]>::try_from(fx) {
        Ok([Effect::Send { station, command }]) => (station, command),
        other => panic!("expected one Send, got {other:?}"),
    }
}

// ---- palette and availability ------------------------------------------------------------

#[test]
fn palette_filters_runs_and_explains_disabled_actions() {
    let mut app = loaded();
    press(&mut app, KeyCode::Char(':'));
    assert!(matches!(app.mode, Mode::Palette(_)));
    type_text(&mut app, "rset");
    let Mode::Palette(p) = &app.mode else {
        panic!("palette");
    };
    assert_eq!(p.chosen(), Some(ActionId::Reset));
    press(&mut app, KeyCode::Enter);
    assert!(matches!(&app.mode, Mode::Form(f) if f.title == "Reset"));
    press(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode::Normal));

    // an unavailable action stays closed and says why
    let (mut app, _) = app_with(&[]);
    for (key, why) in [('t', "no station selected"), ('F', "no station selected")] {
        press(&mut app, KeyCode::Char(key));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(notice(&app).contains(why), "{}", notice(&app));
    }
    assert_eq!(
        app.notice.as_ref().map(|n| n.level),
        Some(NoticeLevel::Error)
    );
    let mut app = loaded();
    press(&mut app, KeyCode::Char('F'));
    assert!(notice(&app).contains("station is connected"));
    app.stations[0].connected = false;
    press(&mut app, KeyCode::Char('t'));
    assert!(notice(&app).contains("station is disconnected"));
    press(&mut app, KeyCode::Char('x'));
    assert!(notice(&app).contains("station is disconnected"));
    press(&mut app, KeyCode::Char('r'));
    assert!(notice(&app).contains("no results yet"));
}

#[test]
fn remote_stop_needs_an_active_transaction() {
    let (mut app, _) = app_with(&["A"]);
    press(&mut app, KeyCode::Char('x'));
    assert!(notice(&app).contains("no active transaction"));
    let mut app = loaded();
    press(&mut app, KeyCode::Char('x'));
    let Mode::Form(f) = &app.mode else {
        panic!("form");
    };
    assert_eq!(f.fields[0].options.len(), 1);
    assert!(f.fields[0].options[0].starts_with("4242 (connector 1"));
    let (station, command) = only_send(press(&mut app, KeyCode::Enter));
    assert_eq!(station, "CP-1");
    assert_eq!(
        command,
        Command::RemoteStop {
            transaction_id: 4242
        }
    );
}

// ---- forms to effects ---------------------------------------------------------------------

#[test]
fn remote_start_validates_then_sends_in_the_background() {
    let mut app = loaded();
    press(&mut app, KeyCode::Char('t'));
    assert!(press(&mut app, KeyCode::Enter).is_empty(), "idTag missing");
    let Mode::Form(f) = &app.mode else {
        panic!("form stays open");
    };
    assert_eq!(f.fields[0].error.as_deref(), Some("idTag is required"));
    type_text(&mut app, "ABC");
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "1");
    let (station, command) = only_send(press(&mut app, KeyCode::Enter));
    assert_eq!(station, "CP-1");
    assert_eq!(
        command,
        Command::RemoteStart {
            id_tag: "ABC".into(),
            connector: Some(1)
        }
    );
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.pending, 1);
    assert!(notice(&app).contains("sending RemoteStartTransaction to CP-1"));
}

#[test]
fn reset_asks_before_it_sends() {
    let mut app = loaded();
    press(&mut app, KeyCode::Char('R'));
    press(&mut app, KeyCode::Char('h'));
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    let Mode::Confirm(c) = &app.mode else {
        panic!("confirm");
    };
    assert!(c.message.contains("Reset (Hard)") && c.message.contains("CP-1"));
    assert!(matches!(
        &c.action,
        ConfirmAction::Send {
            command: Command::Reset(ResetType::Hard),
            ..
        }
    ));
    assert!(press(&mut app, KeyCode::Char('n')).is_empty());
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.pending, 0);
    press(&mut app, KeyCode::Char('R'));
    press(&mut app, KeyCode::Enter);
    let (_, command) = only_send(press(&mut app, KeyCode::Char('y')));
    assert_eq!(command, Command::Reset(ResetType::Soft));
    assert_eq!(app.pending, 1);
}

#[test]
fn every_station_form_maps_to_its_command() {
    let mut app = loaded();
    let (_, clear) = only_send(press(&mut app, KeyCode::Char('C')));
    assert_eq!(clear, Command::ClearCache, "no form, sent at once");
    let mut run = |keys: &[KeyCode], text: &[&str]| -> Command {
        for k in keys {
            press(&mut app, *k);
        }
        for t in text {
            type_text(&mut app, t);
            press(&mut app, KeyCode::Tab);
        }
        only_send(press(&mut app, KeyCode::Enter)).1
    };
    assert_eq!(
        run(&[KeyCode::Char('c')], &["HeartbeatInterval", "60"]),
        Command::ChangeConfiguration {
            key: "HeartbeatInterval".into(),
            value: "60".into()
        }
    );
    assert_eq!(
        run(&[KeyCode::Char('g')], &["A, B"]),
        Command::GetConfiguration {
            keys: vec!["A".into(), "B".into()]
        }
    );
    assert_eq!(
        run(&[KeyCode::Char('T'), KeyCode::Char('h')], &[]),
        Command::TriggerMessage {
            message: MessageTrigger::Heartbeat,
            connector: None
        }
    );
    assert_eq!(
        run(&[KeyCode::Char('u')], &[]),
        Command::UnlockConnector { connector: 1 },
        "the first connector is prefilled"
    );
    assert_eq!(
        run(&[KeyCode::Char('A'), KeyCode::Tab, KeyCode::Char('i')], &[]),
        Command::ChangeAvailability {
            connector: 0,
            kind: AvailabilityType::Inoperative
        }
    );
    assert!(matches!(
        run(&[KeyCode::Char('P')], &[]),
        Command::SetChargingProfile { connector: 0, .. }
    ));
    assert_eq!(
        run(&[KeyCode::Char('D')], &["Acme"]),
        Command::DataTransfer {
            vendor_id: "Acme".into(),
            message_id: None,
            data: None
        }
    );
}

#[test]
fn raw_form_reports_schema_errors_inline_and_sends_valid_payloads() {
    let mut app = loaded();
    press(&mut app, KeyCode::Char('w'));
    // pick Reset, whose payload needs a type
    for _ in 0..20 {
        let Mode::Form(f) = &app.mode else {
            panic!("form");
        };
        if f.fields[0].chosen() == Some("Reset") {
            break;
        }
        press(&mut app, KeyCode::Right);
    }
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    let Mode::Form(f) = &app.mode else {
        panic!("form stays open");
    };
    assert!(f.fields[1].error.is_some());
    assert_eq!(f.focus, 1);
    type_text(&mut app, "");
    let Mode::Form(f) = &mut app.mode else {
        panic!("form");
    };
    f.fields[1].input = kirby_ocpp_tui::text::TextInput::new(r#"{"type":"Hard"}"#);
    press(&mut app, KeyCode::End);
    let (_, command) = only_send(press(&mut app, KeyCode::Enter));
    assert_eq!(
        command,
        Command::Raw {
            action: "Reset".into(),
            payload: json!({"type": "Hard"})
        }
    );
}

// ---- server-side scripting ----------------------------------------------------------------

#[test]
fn rules_and_faults_are_set_listed_removed_and_cleared() {
    let (mut app, _) = app_with(&[]);
    press(&mut app, KeyCode::Char('M'));
    assert!(notice(&app).contains("no rules or faults"));
    press(&mut app, KeyCode::Char('m'));
    let fx = press(&mut app, KeyCode::Enter);
    assert_eq!(
        fx,
        [Effect::Respond {
            action: "Authorize".into(),
            payload: json!({"status": "Rejected"}),
            station: None,
        }]
    );
    press(&mut app, KeyCode::Char('i'));
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "");
    let fx = press(&mut app, KeyCode::Enter);
    assert_eq!(
        fx,
        [Effect::Inject {
            action: "Authorize".into(),
            kind: FaultKind::Silence,
            times: 1,
            payload: Value::Null,
            station: None,
        }]
    );

    update(
        &mut app,
        Msg::Rules(Rules {
            actions: vec!["Authorize".into(), "Heartbeat".into()],
            faults: 2,
            ..Rules::default()
        }),
    );
    // removing one rule from the palette
    press(&mut app, KeyCode::Char(':'));
    type_text(&mut app, "remove rule");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Right);
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        [Effect::RemoveRule("Heartbeat".into())]
    );
    // clearing everything asks first
    press(&mut app, KeyCode::Char('M'));
    assert!(matches!(app.mode, Mode::Confirm(_)));
    assert_eq!(press(&mut app, KeyCode::Char('y')), [Effect::ClearRules]);
}

#[test]
fn active_rules_are_polled_once_a_second_and_only_while_any_exist() {
    let (mut app, _) = app_with(&["A"]);
    assert!(update(&mut app, Msg::Tick(at(1))).is_empty());
    app.rules = Rules {
        actions: vec![],
        faults: 3,
        ..Rules::default()
    };
    assert!(update(&mut app, Msg::Tick(at(1))).is_empty(), "same second");
    assert_eq!(update(&mut app, Msg::Tick(at(2))), [Effect::ReadRules]);
}

// ---- housekeeping -------------------------------------------------------------------------

#[test]
fn forget_asks_and_only_applies_to_disconnected_stations() {
    let mut app = loaded();
    app.stations[0].connected = false;
    press(&mut app, KeyCode::Char('F'));
    assert!(matches!(app.mode, Mode::Confirm(_)));
    assert!(press(&mut app, KeyCode::Esc).is_empty());
    press(&mut app, KeyCode::Char('F'));
    assert_eq!(
        press(&mut app, KeyCode::Char('y')),
        [Effect::Forget("CP-1".into())]
    );
    // the engine's Forgotten event drops the station and what we remembered of it
    update(
        &mut app,
        Msg::CommandDone {
            station: "CP-1".into(),
            command: Command::ClearCache,
            outcome: ok(json!({"status": "Accepted"})),
        },
    );
    assert!(app.results.contains_key("CP-1"));
    update(
        &mut app,
        Msg::Event(CsmsEvent::Forgotten {
            station: "CP-1".into(),
        }),
    );
    assert!(!app.results.contains_key("CP-1"));
}

#[test]
fn copy_station_id_emits_a_copy_effect() {
    let mut app = loaded();
    let fx = press(&mut app, KeyCode::Char('n'));
    assert!(matches!(
        fx.as_slice(),
        [Effect::Copy { text, what }] if text == "CP-1" && what == "station id"
    ));
}

// ---- results ------------------------------------------------------------------------------

#[test]
fn answers_become_notices_and_a_per_station_history() {
    let mut app = loaded();
    app.pending = 2;
    update(
        &mut app,
        Msg::CommandDone {
            station: "CP-1".into(),
            command: Command::Reset(ResetType::Soft),
            outcome: ok(json!({"status": "Accepted"})),
        },
    );
    assert_eq!(notice(&app), "Reset (Soft) on CP-1: Accepted");
    assert_eq!(app.pending, 1);
    assert_eq!(
        app.notice.as_ref().map(|n| n.level),
        Some(NoticeLevel::Success)
    );
    assert!(matches!(app.mode, Mode::Normal), "no popup for a status");

    update(
        &mut app,
        Msg::CommandDone {
            station: "CP-1".into(),
            command: Command::UnlockConnector { connector: 1 },
            outcome: ok(json!({"status": "UnlockFailed"})),
        },
    );
    assert_eq!(
        app.notice.as_ref().map(|n| n.level),
        Some(NoticeLevel::Error)
    );
    update(
        &mut app,
        Msg::CommandDone {
            station: "CP-1".into(),
            command: Command::ClearCache,
            outcome: ok(json!({})),
        },
    );
    assert_eq!(notice(&app), "ClearCache on CP-1: ok");
    update(
        &mut app,
        Msg::CommandDone {
            station: "CP-1".into(),
            command: Command::Reset(ResetType::Hard),
            outcome: failed("CALLERROR NotImplemented: nope"),
        },
    );
    assert_eq!(
        notice(&app),
        "Reset (Hard) on CP-1: CALLERROR NotImplemented: nope"
    );
    assert_eq!(app.pending, 0, "never below zero");

    // the history: newest first, openable
    press(&mut app, KeyCode::Char('r'));
    let Mode::Results(list) = &app.mode else {
        panic!("results list");
    };
    assert_eq!(list.station, "CP-1");
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Enter);
    let Mode::Detail(d) = &app.mode else {
        panic!("detail");
    };
    assert!(d.title.starts_with("Reset (Hard)"));
    assert!(d.lines.iter().any(|l| l.contains("--- error ---")));
    assert!(d.lines.iter().any(|l| l.contains("--- request ---")));
    assert_eq!(d.copy, "CALLERROR NotImplemented: nope");
}

#[test]
fn history_is_capped_per_station() {
    let mut app = loaded();
    for i in 0..30 {
        update(
            &mut app,
            Msg::CommandDone {
                station: "CP-1".into(),
                command: Command::ClearCache,
                outcome: ok(json!({"status": "Accepted", "n": i})),
            },
        );
    }
    assert_eq!(
        app.results["CP-1"].len(),
        crate::actions::RESULTS_PER_STATION
    );
    // an answer for a station that is gone is only a notice
    update(
        &mut app,
        Msg::CommandDone {
            station: "ghost".into(),
            command: Command::ClearCache,
            outcome: failed("unknown station"),
        },
    );
    assert!(!app.results.contains_key("ghost"));
    assert!(notice(&app).contains("ghost"));
}

#[test]
fn get_configuration_opens_a_readable_popup_unless_the_user_is_busy() {
    let answer = json!({
        "configurationKey": [
            {"key": "HeartbeatInterval", "readonly": false, "value": "300"},
            {"key": "NumberOfConnectors", "readonly": true, "value": "2"},
            {"key": "Empty", "readonly": false}
        ],
        "unknownKey": ["Bogus"]
    });
    let done = |app: &mut App| {
        update(
            app,
            Msg::CommandDone {
                station: "CP-1".into(),
                command: Command::GetConfiguration { keys: vec![] },
                outcome: ok(answer.clone()),
            },
        );
    };
    let mut app = loaded();
    done(&mut app);
    let Mode::Detail(d) = &app.mode else {
        panic!("popup");
    };
    let text = d.lines.join("\n");
    assert!(text.contains("3 keys"), "{text}");
    assert!(text.contains("HeartbeatInterval = \"300\""), "{text}");
    assert!(
        text.contains("NumberOfConnectors = \"2\"  (readonly)"),
        "{text}"
    );
    assert!(text.contains("Empty = (not set)"), "{text}");
    assert!(
        text.contains("unknown keys:") && text.contains("  Bogus"),
        "{text}"
    );
    assert!(d.copy.contains("configurationKey"));

    // a user typing in a form is not interrupted
    let mut app = loaded();
    press(&mut app, KeyCode::Char('c'));
    done(&mut app);
    assert!(matches!(app.mode, Mode::Form(_)));
    assert!(app.results["CP-1"].front().is_some());
}

#[test]
fn hostile_answers_are_sanitised() {
    let mut app = loaded();
    update(
        &mut app,
        Msg::CommandDone {
            station: "CP-1".into(),
            command: Command::ClearCache,
            outcome: failed("CALLERROR X: \u{1b}[31mred\u{7}"),
        },
    );
    assert!(!notice(&app).chars().any(char::is_control));
    press(&mut app, KeyCode::Char('r'));
    press(&mut app, KeyCode::Enter);
    let Mode::Detail(d) = &app.mode else {
        panic!("detail");
    };
    assert!(d.lines.iter().all(|l| !l.chars().any(char::is_control)));
}

#[test]
fn pasting_into_a_form_and_the_palette_works() {
    let mut app = loaded();
    press(&mut app, KeyCode::Char('t'));
    update(&mut app, Msg::Paste("TAG\n1".into()));
    let Mode::Form(f) = &app.mode else {
        panic!("form");
    };
    assert_eq!(f.fields[0].input.text(), "TAG 1");
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char(':'));
    update(&mut app, Msg::Paste("clear".into()));
    let Mode::Palette(p) = &app.mode else {
        panic!("palette");
    };
    assert!(!p.results.is_empty() && p.results.len() < crate::actions::CATALOG.len());
}

// ---- executor against a real engine and station ---------------------------------------------

async fn start_engine() -> CsmsHandle {
    CsmsServer::start(CsmsConfig::new("127.0.0.1:0".parse().unwrap()))
        .await
        .expect("engine starts")
}

fn station_config(csms: &CsmsHandle, id: &str, dir: &std::path::Path) -> StationConfig {
    let mut config = StationConfig::new(format!("{}/ocpp", csms.url()))
        .with_charge_point_id(id)
        .with_connectors(2)
        .with_call_timeout_ms(2_000)
        .with_reconnect(ReconnectConfig {
            enabled: false,
            initial_delay_ms: 30,
            max_delay_ms: 100,
            max_attempts: 0,
        })
        .with_state_dir(dir);
    config.ws_ping_interval_secs = 0;
    config
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_scopes_rules_and_faults_to_one_station() {
    let csms = start_engine().await;
    let (mut exec, _rx) = Exec::new();
    let rules = |msgs: Vec<Msg>| match msgs.into_iter().last() {
        Some(Msg::Rules(rules)) => rules,
        other => panic!("expected the rules, got {other:?}"),
    };
    let after = rules(
        execute(
            Effect::Respond {
                action: "Heartbeat".into(),
                payload: json!({"currentTime": "x"}),
                station: Some("CP-1".into()),
            },
            &csms,
            &mut exec,
        )
        .await,
    );
    assert_eq!(after.actions, Vec::<String>::new());
    assert_eq!(
        after.scoped,
        [crate::actions::ScopedRule {
            station: Some("CP-1".into()),
            action: "Heartbeat".into()
        }]
    );
    let after = rules(
        execute(
            Effect::Inject {
                action: "Heartbeat".into(),
                kind: FaultKind::Silence,
                times: 2,
                payload: Value::Null,
                station: Some("CP-1".into()),
            },
            &csms,
            &mut exec,
        )
        .await,
    );
    assert_eq!(after.faults, 2);
    assert_eq!(csms.faults()[0].scope.station.as_deref(), Some("CP-1"));
    let after = rules(
        execute(
            Effect::RemoveScopedRule {
                station: Some("CP-1".into()),
                action: "Heartbeat".into(),
            },
            &csms,
            &mut exec,
        )
        .await,
    );
    assert!(after.scoped.is_empty());
    execute(
        Effect::Respond {
            action: "Heartbeat".into(),
            payload: json!({}),
            station: Some("CP-2".into()),
        },
        &csms,
        &mut exec,
    )
    .await;
    // "Clear all" removes scoped rules and faults too.
    let after = rules(execute(Effect::ClearRules, &csms, &mut exec).await);
    assert!(after.is_empty());
    csms.shutdown().await;
}

/// Waits until `check` holds for the station's state, woken by the engine's events.
async fn wait_state(
    csms: &CsmsHandle,
    events: &mut tokio::sync::broadcast::Receiver<CsmsEvent>,
    id: &str,
    check: impl Fn(&kirby_csms_core::StationState) -> bool,
) {
    timeout(STEP, async {
        loop {
            if csms.state(id).is_some_and(|s| check(&s)) {
                return;
            }
            let _ = events.recv().await;
        }
    })
    .await
    .expect("state reached in time");
}

async fn run_command(
    app: &mut App,
    csms: &CsmsHandle,
    exec: &mut Exec,
    rx: &mut mpsc::UnboundedReceiver<Msg>,
    effect: Effect,
) {
    assert!(execute(effect, csms, exec).await.is_empty());
    let msg = timeout(STEP, rx.recv())
        .await
        .expect("the answer arrives")
        .expect("channel open");
    assert!(matches!(msg, Msg::CommandDone { .. }));
    update(app, msg);
}

#[tokio::test]
async fn executor_drives_a_real_station() {
    let csms = start_engine().await;
    let mut events = csms.subscribe();
    let dir = tempfile::tempdir().unwrap();
    let (station, join) = Station::spawn(
        station_config(&csms, "CP-E", dir.path()),
        Arc::new(SystemClock),
    );
    station.connect().await.expect("connect");
    wait_state(&csms, &mut events, "CP-E", |s| s.boot.is_some()).await;
    wait_booted(&station).await;

    let (mut app, _) = app_with(&["CP-E"]);
    let (mut exec, mut rx) = Exec::new();

    // RemoteStart -> the station starts a transaction the engine tracks
    app.pending = 1;
    run_command(
        &mut app,
        &csms,
        &mut exec,
        &mut rx,
        Effect::Send {
            station: "CP-E".into(),
            command: Command::RemoteStart {
                id_tag: "ABC".into(),
                connector: Some(1),
            },
        },
    )
    .await;
    assert_eq!(notice(&app), "RemoteStartTransaction on CP-E: Accepted");
    assert_eq!(app.pending, 0);
    wait_state(&csms, &mut events, "CP-E", |s| {
        s.transactions
            .iter()
            .any(|t| t.status == TransactionStatus::Active && t.id_tag == "ABC")
    })
    .await;

    // ChangeConfiguration
    run_command(
        &mut app,
        &csms,
        &mut exec,
        &mut rx,
        Effect::Send {
            station: "CP-E".into(),
            command: Command::ChangeConfiguration {
                key: "HeartbeatInterval".into(),
                value: "60".into(),
            },
        },
    )
    .await;
    assert_eq!(notice(&app), "ChangeConfiguration on CP-E: Accepted");

    // GetConfiguration: readable popup
    app.selected = Some("CP-E".into());
    run_command(
        &mut app,
        &csms,
        &mut exec,
        &mut rx,
        Effect::Send {
            station: "CP-E".into(),
            command: Command::GetConfiguration {
                keys: vec!["HeartbeatInterval".into(), "NoSuchKey".into()],
            },
        },
    )
    .await;
    let Mode::Detail(d) = &app.mode else {
        panic!("popup");
    };
    let text = d.lines.join("\n");
    assert!(text.contains("HeartbeatInterval = \"60\""), "{text}");
    assert!(text.contains("NoSuchKey"), "{text}");
    app.mode = Mode::Normal;

    // a CALLERROR from the station
    run_command(
        &mut app,
        &csms,
        &mut exec,
        &mut rx,
        Effect::Send {
            station: "CP-E".into(),
            command: Command::Raw {
                action: "NoSuchAction".into(),
                payload: json!({}),
            },
        },
    )
    .await;
    assert!(
        notice(&app).starts_with("NoSuchAction on CP-E: CALLERROR "),
        "{}",
        notice(&app)
    );
    assert_eq!(
        app.notice.as_ref().map(|n| n.level),
        Some(NoticeLevel::Error)
    );

    // not connected / unknown station
    run_command(
        &mut app,
        &csms,
        &mut exec,
        &mut rx,
        Effect::Send {
            station: "nobody".into(),
            command: Command::ClearCache,
        },
    )
    .await;
    assert!(
        notice(&app).ends_with("unknown station"),
        "{}",
        notice(&app)
    );

    // rules and faults reach the engine and are read back
    let msgs = execute(
        Effect::Respond {
            action: "DataTransfer".into(),
            payload: json!({"status": "Rejected"}),
            station: None,
        },
        &csms,
        &mut exec,
    )
    .await;
    assert_eq!(
        msgs.into_iter()
            .next()
            .map(|m| matches!(m, Msg::Rules(r) if r.actions == ["DataTransfer"])),
        Some(true)
    );
    let msgs = execute(
        Effect::Inject {
            action: "Heartbeat".into(),
            kind: FaultKind::Close,
            times: 4,
            payload: Value::Null,
            station: None,
        },
        &csms,
        &mut exec,
    )
    .await;
    assert!(matches!(msgs.as_slice(), [Msg::Rules(r)] if r.faults == 4));
    let msgs = execute(Effect::ClearRules, &csms, &mut exec).await;
    assert!(matches!(msgs.as_slice(), [Msg::Rules(r)] if r.is_empty()));

    // forget after the station left
    station.shutdown().await.expect("station shutdown");
    join.await.expect("actor ends");
    timeout(STEP, async {
        while csms.station("CP-E").is_some_and(|i| i.connected) {
            let _ = events.recv().await;
        }
    })
    .await
    .expect("disconnect seen");
    let msgs = execute(Effect::Forget("CP-E".into()), &csms, &mut exec).await;
    assert!(
        matches!(msgs.as_slice(), [Msg::Notice(NoticeLevel::Success, t)] if t == "forgot CP-E")
    );
    assert!(csms.station("CP-E").is_none());
    csms.shutdown().await;
}

#[tokio::test]
async fn run_loop_executes_a_hotkey_command_in_the_background() {
    let csms = start_engine().await;
    let mut events = csms.subscribe();
    let dir = tempfile::tempdir().unwrap();
    let (station, join) = Station::spawn(
        station_config(&csms, "CP-K", dir.path()),
        Arc::new(SystemClock),
    );
    station.connect().await.expect("connect");
    wait_state(&csms, &mut events, "CP-K", |s| s.boot.is_some()).await;
    wait_booted(&station).await;

    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<std::io::Result<TermEvent>>();
    let keys = Box::pin(futures_util::stream::poll_fn(move |cx| {
        key_rx.poll_recv(cx)
    }));
    let tx = key_tx.clone();
    let press_key = move |c: char| {
        let _ = tx.send(Ok(TermEvent::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::NONE,
        ))));
    };
    let mut stage = 0;
    let observe = move |app: &App| {
        if stage == 0 && app.selected.as_deref() == Some("CP-K") && app.info.is_some() {
            stage = 1;
            press_key('C'); // ClearCache: sent without a form
        } else if stage == 1 && app.results.get("CP-K").is_some_and(|r| !r.is_empty()) {
            stage = 2;
            press_key('q');
        }
    };
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let options = TuiOptions {
        tick_rate: Duration::from_millis(10),
    };
    timeout(
        STEP,
        drive(
            &mut terminal,
            &csms,
            &options,
            keys,
            std::future::pending::<()>(),
            observe,
        ),
    )
    .await
    .expect("the command ran in time")
    .expect("loop ends cleanly");
    let screen = crate::ui::tests::screen(terminal.backend());
    assert!(screen.contains("ClearCache on CP-K: Accepted"), "{screen}");

    drop(key_tx);
    station.shutdown().await.expect("station shutdown");
    join.await.expect("actor ends");
    csms.shutdown().await;
}

#[test]
fn an_unconnected_fixture_is_not_selectable_for_commands() {
    let mut app = App::new("ws://x", at(0));
    update(
        &mut app,
        Msg::Resynced {
            infos: vec![info("OFF", false)],
            states: Vec::new(),
        },
    );
    assert!(app.reason_for(ActionId::Raw).is_some());
    assert!(app.reason_for(ActionId::Forget).is_none());
}
