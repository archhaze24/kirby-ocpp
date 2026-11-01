#![allow(
    clippy::assert_is_empty,
    clippy::case_sensitive_file_extension_comparisons
)]

use chrono::{TimeZone, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use kirby_ocpp_core::StationConfig;
use kirby_ocpp_core::scenario::{ScenarioProgress, ScenarioReport, StepReport, StepStatus};
use kirby_ocpp_core::station::{Command, Direction, Event, FrameRecord, Snapshot};
use kirby_ocpp_core::ws::ConnectionState;
use serde_json::json;

use super::*;
use crate::actions::Tab;
use crate::forms::FormKind;

fn app() -> App {
    let config = StationConfig::new("ws://h/p").with_connectors(2);
    App::new(Snapshot::initial(&config), "ws://h/p", "Acme")
}

fn connected(app: &mut App) {
    let mut s = app.snapshot.clone();
    s.connection = ConnectionState::Connected;
    update(app, Msg::Snapshot(Box::new(s)));
}

fn key(code: KeyCode) -> Msg {
    Msg::Key(KeyEvent::new_with_kind(
        code,
        KeyModifiers::NONE,
        KeyEventKind::Press,
    ))
}

fn press(app: &mut App, keys: &str) -> Vec<Effect> {
    let mut fx = Vec::new();
    for c in keys.chars() {
        fx.extend(update(app, key(KeyCode::Char(c))));
    }
    fx
}

fn code(app: &mut App, code: KeyCode) -> Vec<Effect> {
    update(app, key(code))
}

fn frame_event(i: i64) -> Event {
    Event::Frame(FrameRecord {
        ts: Utc.timestamp_millis_opt(1_700_000_000_000 + i).unwrap(),
        direction: Direction::Out,
        msg_id: Some(format!("m{i}")),
        action: Some("Heartbeat".into()),
        frame: json!([2, format!("m{i}"), "Heartbeat", {}]),
    })
}

fn sent(fx: &[Effect]) -> Vec<&Command> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Send { command, .. } => Some(command),
            _ => None,
        })
        .collect()
}

#[test]
fn tab_keys_switch_tabs_and_reset_focus() {
    let mut a = app();
    press(&mut a, "3");
    assert_eq!(a.tab, Tab::Transaction);
    code(&mut a, KeyCode::Tab);
    assert_eq!(a.tab, Tab::Maintenance);
    code(&mut a, KeyCode::BackTab);
    code(&mut a, KeyCode::BackTab);
    assert_eq!(a.tab, Tab::Connector);
    press(&mut a, "6");
    assert_eq!((a.tab, a.focus), (Tab::Logs, Focus::Log));
    press(&mut a, "1");
    assert_eq!(a.focus, Focus::Actions);
}

#[test]
fn up_down_move_in_the_focused_pane() {
    let mut a = app();
    code(&mut a, KeyCode::Down);
    code(&mut a, KeyCode::Down);
    assert_eq!(a.action_sel[0], 2);
    code(&mut a, KeyCode::Up);
    assert_eq!(a.action_sel[0], 1);
    code(&mut a, KeyCode::Left);
    assert_eq!(a.focus, Focus::Connectors);
    code(&mut a, KeyCode::Down);
    assert_eq!(a.connector.get(), 2);
    code(&mut a, KeyCode::Down);
    assert_eq!(a.connector.get(), 2, "stops at the last connector");
    press(&mut a, "[");
    assert_eq!(a.connector.get(), 1);
    code(&mut a, KeyCode::Right);
    code(&mut a, KeyCode::Right);
    assert_eq!(a.focus, Focus::Log);
}

#[test]
fn enter_runs_the_selected_action() {
    let mut a = app();
    let fx = code(&mut a, KeyCode::Enter); // Boot, but disconnected
    assert!(fx.is_empty());
    connected(&mut a);
    let fx = code(&mut a, KeyCode::Enter);
    assert_eq!(sent(&fx), [&Command::Boot]);
}

#[test]
fn disabled_action_shows_its_reason_and_sends_nothing() {
    let mut a = app();
    let fx = press(&mut a, "b");
    assert!(fx.is_empty());
    let n = a.notice.as_ref().unwrap();
    assert_eq!(n.level, NoticeLevel::Error);
    assert!(n.text.contains("not connected"));
    assert_eq!(
        a.reason_for(ActionId::Boot).as_deref(),
        Some("not connected to the CSMS")
    );
}

#[test]
fn accelerators_work_only_inside_their_own_tab() {
    let mut a = app();
    // Station tab: `y` (log copy) and `e` (stop tx) and `p` do nothing.
    for k in "yepuv".chars() {
        let fx = press(&mut a, &k.to_string());
        assert!(fx.is_empty(), "{k}");
        assert!(matches!(a.mode, Mode::Normal), "{k}");
        assert_eq!(a.tab, Tab::Station);
    }
    // `c` is Connect on Station but would clear the log on Logs: no hidden jump either way.
    let fx = press(&mut a, "c");
    assert_eq!(sent(&fx), [&Command::Connect]);
    assert_eq!(a.tab, Tab::Station);
    // `a` on Station adds a connector (not the Transaction tab's Authorize form).
    let fx = press(&mut a, "a");
    assert_eq!(sent(&fx), [&Command::AddConnector]);
    assert!(matches!(a.mode, Mode::Normal));
}

#[test]
fn clearing_the_log_needs_confirmation() {
    let mut a = app();
    update(&mut a, Msg::Event(frame_event(1)));
    press(&mut a, "6");
    press(&mut a, "c");
    assert!(matches!(a.mode, Mode::Confirm(_)));
    press(&mut a, "n");
    assert!(matches!(a.mode, Mode::Normal));
    assert_eq!(a.log.len(), 1);
    press(&mut a, "c");
    code(&mut a, KeyCode::Esc);
    assert_eq!(a.log.len(), 1);
    press(&mut a, "c");
    code(&mut a, KeyCode::Enter);
    assert_eq!(a.log.len(), 1, "Enter must not confirm a [y/N] prompt");
    press(&mut a, "cy");
    assert_eq!(a.log.len(), 0);
}

#[test]
fn palette_filters_fuzzily_and_runs_the_chosen_action() {
    let mut a = app();
    press(&mut a, "/");
    assert!(matches!(a.mode, Mode::Palette(_)));
    press(&mut a, "authz");
    let Mode::Palette(p) = &a.mode else { panic!() };
    assert!(p.results.len() < a.catalog.len());
    code(&mut a, KeyCode::Enter);
    let Mode::Form(f) = &a.mode else {
        panic!("{:?}", a.mode)
    };
    assert_eq!(f.kind, FormKind::Authorize);
    assert_eq!(a.tab, Tab::Station, "the palette does not switch tabs");
}

#[test]
fn palette_runs_disabled_actions_into_a_reason_and_escape_closes() {
    let mut a = app();
    press(&mut a, ":heartb");
    code(&mut a, KeyCode::Enter);
    assert!(a.notice.as_ref().unwrap().text.contains("not connected"));
    press(&mut a, "/");
    code(&mut a, KeyCode::Esc);
    assert!(matches!(a.mode, Mode::Normal));
}

#[test]
fn modes_own_their_keys() {
    let mut a = app();
    press(&mut a, "?");
    assert!(matches!(a.mode, Mode::Help));
    press(&mut a, "3x5");
    assert!(matches!(a.mode, Mode::Help), "help swallows other keys");
    assert_eq!(a.tab, Tab::Station);
    assert!(!a.quit);
    code(&mut a, KeyCode::Esc);
    assert!(matches!(a.mode, Mode::Normal));
    press(&mut a, "/");
    update(
        &mut a,
        Msg::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
    );
    assert!(a.quit, "Ctrl-C quits from any mode");
}

#[test]
fn start_transaction_form_validates_then_sends() {
    let mut a = app();
    press(&mut a, "3t");
    let Mode::Form(f) = &a.mode else { panic!() };
    assert_eq!(
        f.fields[0].input.text(),
        "1",
        "connector prefilled from the selection"
    );
    code(&mut a, KeyCode::Enter);
    let Mode::Form(f) = &a.mode else {
        panic!("invalid form stays open")
    };
    assert!(f.fields[1].error.is_some());
    update(&mut a, Msg::Paste("TAG-é".into()));
    // Paste goes to the focused field (idTag after the failed submit).
    let fx = code(&mut a, KeyCode::Enter);
    let cmds = sent(&fx);
    assert!(matches!(
        cmds[..],
        [Command::StartTransaction { connector, id_tag }] if connector.get() == 1 && id_tag.as_str() == "TAG-é"
    ));
    assert!(matches!(a.mode, Mode::Normal));
}

#[test]
fn escape_cancels_a_form_without_effects() {
    let mut a = app();
    press(&mut a, "3a");
    assert!(matches!(a.mode, Mode::Form(_)));
    press(&mut a, "abc");
    let fx = code(&mut a, KeyCode::Esc);
    assert!(fx.is_empty());
    assert!(matches!(a.mode, Mode::Normal));
}

#[test]
fn stop_reason_choice_sends_stop() {
    let mut a = app();
    press(&mut a, "3e");
    assert!(a.notice.as_ref().unwrap().text.contains("no transaction"));
    let mut s = a.snapshot.clone();
    s.connectors[0].transaction = kirby_ocpp_core::station::TransactionState::Active(
        kirby_ocpp_core::station::transaction::ActiveTransaction {
            tx_id: kirby_ocpp_core::ids::TxId::Csms(7),
            id_tag: "T".parse().unwrap(),
            started_at: Utc::now(),
            meter_start_wh: 0,
        },
    );
    update(&mut a, Msg::Snapshot(Box::new(s)));
    press(&mut a, "e");
    assert!(matches!(a.mode, Mode::Choice(_)));
    let fx = code(&mut a, KeyCode::Enter);
    assert!(matches!(sent(&fx)[..], [Command::StopTransaction { .. }]));
}

#[test]
fn log_filter_search_and_pause_do_not_touch_the_log_itself() {
    let mut a = app();
    update(&mut a, Msg::Event(frame_event(1)));
    press(&mut a, "6");
    let before = a.log.len();
    press(&mut a, "f");
    assert_eq!(a.log.filter.label(), "calls");
    assert!(a.notice.as_ref().unwrap().text.contains("calls"));
    assert_eq!(a.log.len(), before, "notices are not log rows");
    press(&mut a, "/");
    let Mode::Form(_) = &a.mode else { panic!() };
    press(&mut a, "nomatch");
    code(&mut a, KeyCode::Enter);
    assert_eq!(a.log.search, "nomatch");
    assert_eq!(a.log.visible_count(), 0);
    code(&mut a, KeyCode::Esc);
    assert!(a.log.search.is_empty());
    press(&mut a, "p");
    assert!(!a.log.follow);
}

#[test]
fn enter_on_a_log_row_opens_the_detail_view() {
    let mut a = app();
    update(&mut a, Msg::Event(frame_event(1)));
    press(&mut a, "6");
    code(&mut a, KeyCode::Enter);
    let Mode::Detail(d) = &a.mode else { panic!() };
    assert_eq!(d.title, "Heartbeat");
    assert!(d.lines.iter().any(|l| l.contains("Heartbeat")));
    let fx = press(&mut a, "c");
    assert!(matches!(&fx[..], [Effect::Copy { .. }]));
    code(&mut a, KeyCode::Esc);
    assert!(matches!(a.mode, Mode::Normal));
}

#[test]
fn export_uses_a_default_path_and_writes_jsonl() {
    let mut a = app();
    update(&mut a, Msg::Event(frame_event(1)));
    press(&mut a, "6e");
    let Mode::Form(f) = &a.mode else { panic!() };
    assert!(f.fields[0].input.text().starts_with("./kirby-ocpp-"));
    let fx = code(&mut a, KeyCode::Enter);
    let [Effect::WriteFile { path, contents }] = &fx[..] else {
        panic!("{fx:?}")
    };
    assert!(path.ends_with(".jsonl"));
    assert_eq!(contents.lines().count(), 1);
    let v: serde_json::Value = serde_json::from_str(contents.lines().next().unwrap()).unwrap();
    assert_eq!(v["type"], "frame");
}

#[test]
fn raw_call_form_sends_validated_payload() {
    let mut a = app();
    connected(&mut a);
    press(&mut a, "5r");
    let Mode::Form(f) = &a.mode else { panic!() };
    assert_eq!(f.kind, FormKind::Raw);
    let fx = code(&mut a, KeyCode::Enter);
    assert!(matches!(
        sent(&fx)[..],
        [Command::SendRaw { action, validate: true, .. }] if action == "Heartbeat"
    ));
}

#[test]
fn scenario_runs_one_at_a_time_and_reports_failures() {
    let mut a = app();
    press(&mut a, "7");
    let fx = press(&mut a, "o");
    assert!(matches!(&fx[..], [Effect::RunScenario(s)] if s.name == "charge-session"));
    assert!(a.scenario.running);
    assert!(!a.scenario.steps.is_empty());
    let again = press(&mut a, "w");
    assert!(again.is_empty());
    assert!(a.notice.as_ref().unwrap().text.contains("already running"));
    update(
        &mut a,
        Msg::Scenario(ScenarioProgress::StepStarted {
            index: 0,
            name: "boot".into(),
        }),
    );
    assert_eq!(a.scenario.steps[0].1, StepState::Running);
    let step = StepReport {
        index: 0,
        name: "boot".into(),
        status: StepStatus::Failed("rejected".into()),
        duration_ms: 5,
    };
    update(
        &mut a,
        Msg::Scenario(ScenarioProgress::StepFinished(step.clone())),
    );
    update(
        &mut a,
        Msg::ScenarioDone(Box::new(ScenarioReport {
            scenario: "charge-session".into(),
            passed: false,
            cancelled: false,
            steps: vec![step],
            duration_ms: 5,
        })),
    );
    assert!(!a.scenario.running);
    let n = a.notice.as_ref().unwrap();
    assert!(
        n.text.contains("failed") && n.text.contains("rejected"),
        "{}",
        n.text
    );
    // cancel is only offered while running
    assert!(press(&mut a, "x").is_empty());
    let fx = press(&mut a, "o");
    assert_eq!(fx.len(), 1);
    assert_eq!(press(&mut a, "x"), [Effect::CancelScenario]);
}

#[test]
fn command_results_become_notices_not_log_rows() {
    let mut a = app();
    update(
        &mut a,
        Msg::CommandDone {
            label: "boot".into(),
            show_response: false,
            result: Err("call timed out".into()),
        },
    );
    assert!(a.notice.as_ref().unwrap().text.contains("timed out"));
    assert!(a.log.is_empty());
    update(
        &mut a,
        Msg::CommandDone {
            label: "raw X".into(),
            show_response: true,
            result: Ok(Outcome::Response {
                payload: json!({"status": "Accepted"}),
            }),
        },
    );
    let Mode::Detail(d) = &a.mode else { panic!() };
    assert!(d.lines.join("\n").contains("Accepted"));
}

#[test]
fn notices_expire_on_ticks_and_ticks_alone_do_not_dirty_the_model() {
    let mut a = app();
    a.dirty = false;
    update(&mut a, Msg::Tick);
    assert!(!a.dirty);
    a.notify(NoticeLevel::Info, "x");
    for _ in 0..NOTICE_TICKS {
        update(&mut a, Msg::Tick);
    }
    assert!(a.notice.is_none());
    assert!(a.dirty);
}

#[test]
fn snapshot_with_new_connectors_keeps_selection_valid() {
    let mut a = app();
    a.connector = kirby_ocpp_core::ids::ConnectorId::new(9);
    let s = a.snapshot.clone();
    update(&mut a, Msg::Snapshot(Box::new(s)));
    assert_eq!(a.connector.get(), 1);
}

#[test]
fn response_rule_form_sets_and_clears_rules() {
    let mut a = app();
    press(&mut a, "m");
    let Mode::Form(f) = &a.mode else {
        panic!("the rule form opens on the Station tab")
    };
    assert_eq!(f.kind, FormKind::Rule);
    // An invalid rule keeps the form open and explains itself.
    press(&mut a, "Reset=explode");
    let fx = code(&mut a, KeyCode::Enter);
    assert!(fx.is_empty());
    let Mode::Form(f) = &a.mode else {
        panic!("invalid form stays open")
    };
    assert!(f.fields[0].error.as_deref().unwrap().contains("behavior"));
    for _ in 0.."explode".len() {
        code(&mut a, KeyCode::Backspace);
    }
    press(&mut a, "reject*2");
    let fx = code(&mut a, KeyCode::Enter);
    assert!(matches!(
        sent(&fx)[..],
        [Command::SetResponseRule { rule }] if rule.to_string() == "Reset=reject*2"
    ));
    assert!(matches!(a.mode, Mode::Normal));

    // `Action=off` removes one rule.
    press(&mut a, "mreset=off");
    let fx = code(&mut a, KeyCode::Enter);
    assert!(matches!(
        sent(&fx)[..],
        [Command::ClearResponseRules { action: Some(action) }] if action == "Reset"
    ));
}

#[test]
fn clearing_all_rules_needs_an_active_rule() {
    let mut a = app();
    let fx = press(&mut a, "M");
    assert!(fx.is_empty());
    assert!(
        a.notice
            .as_ref()
            .is_some_and(|n| n.text.contains("no response rules"))
    );
    let mut s = a.snapshot.clone();
    s.response_rules.push("Reset=drop".parse().unwrap());
    update(&mut a, Msg::Snapshot(Box::new(s)));
    let fx = press(&mut a, "M");
    assert!(matches!(
        sent(&fx)[..],
        [Command::ClearResponseRules { action: None }]
    ));
}
