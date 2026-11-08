//! Pending-command list, cancel, monochrome markers and the `--time` clock in the CSMS UI.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::KeyCode;
use kirby_csms_core::{CsmsConfig, CsmsEvent, CsmsServer, DisconnectReason};
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::station::{Behavior, Command as StationCommand, ResponseRule};
use kirby_ocpp_core::timefmt::TimeMode;
use kirby_ocpp_core::{Station, StationConfig, SystemClock};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use serde_json::json;
use tokio::time::timeout;

use crate::app::{App, Effect, Mode, Msg, update};
use crate::command::{CmdError, CmdOutcome, Command};
use crate::tests::{app_with, at, press, type_text, wait_booted};
use crate::ui::draw;
use crate::ui::tests::{loaded, screen};
use crate::{Exec, execute};

/// The loaded app of the UI tests plus two station log lines: `connected` (success) and
/// `disconnected: ...` (warning).
fn with_lines() -> App {
    let mut app = loaded();
    for event in [
        CsmsEvent::Connected {
            station: "CP-1".to_owned(),
        },
        CsmsEvent::Disconnected {
            station: "CP-1".to_owned(),
            reason: DisconnectReason::Closed,
        },
    ] {
        update(&mut app, Msg::Event(event));
    }
    app
}

fn draw_app(app: &App, w: u16, h: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| draw(f, app)).unwrap();
    terminal
}

/// An app with one selected station and a `ClearCache` sent to it.
fn with_pending() -> (App, u64) {
    let (mut app, _) = app_with(&["CP-1"]);
    let fx = press(&mut app, KeyCode::Char('C'));
    let id = match fx.as_slice() {
        [Effect::Send { id, .. }] => *id,
        other => panic!("expected one Send, got {other:?}"),
    };
    (app, id)
}

#[test]
fn a_sent_command_is_a_row_with_its_elapsed_time() {
    let (mut app, _) = with_pending();
    assert_eq!(app.inflight.len(), 1);
    assert_eq!(app.pending, 1);
    press(&mut app, KeyCode::Char('W'));
    assert!(matches!(app.mode, Mode::Pending(0)));
    update(&mut app, Msg::Tick(at(7)));
    let screen = screen(draw_app(&app, 100, 30).backend());
    assert!(screen.contains("Pending calls (1)"), "{screen}");
    assert!(screen.contains("7s  CP-1 ClearCache"), "{screen}");
    assert!(screen.contains("x cancel"), "{screen}");
}

#[test]
fn cancel_drops_the_row_reports_it_and_ignores_the_late_answer() {
    let (mut app, id) = with_pending();
    press(&mut app, KeyCode::Char('W'));
    let fx = press(&mut app, KeyCode::Char('x'));
    assert_eq!(fx, [Effect::CancelCall { id }]);
    assert_eq!(app.inflight.len(), 0);
    assert_eq!(app.pending, 0);
    let notice = app.notice.as_ref().expect("a notice").text.clone();
    assert!(
        notice.contains("ClearCache on CP-1") && notice.contains("cancelled"),
        "{notice}"
    );
    let results = app
        .results
        .get("CP-1")
        .map_or(0, std::collections::VecDeque::len);
    assert_eq!(results, 1, "the cancelled call is in the results list");

    // The answer that was already on its way is dropped.
    update(
        &mut app,
        Msg::CommandDone {
            id,
            station: "CP-1".into(),
            command: Command::ClearCache,
            outcome: CmdOutcome {
                result: Ok(json!({"status": "Accepted"})),
            },
        },
    );
    assert_eq!(
        app.results
            .get("CP-1")
            .map_or(0, std::collections::VecDeque::len),
        1
    );
    assert!(app.cancelled.is_empty(), "the late answer used up its mark");

    // An answer with an id the model never handed out is still shown.
    update(
        &mut app,
        Msg::CommandDone {
            id: 999,
            station: "CP-1".into(),
            command: Command::ClearCache,
            outcome: CmdOutcome {
                result: Err(CmdError("boom".into())),
            },
        },
    );
    assert_eq!(
        app.results
            .get("CP-1")
            .map_or(0, std::collections::VecDeque::len),
        2
    );
}

#[test]
fn an_answer_removes_its_own_row_only() {
    let (mut app, first) = with_pending();
    let fx = press(&mut app, KeyCode::Char('C'));
    let [Effect::Send { id: second, .. }] = fx.as_slice() else {
        panic!("{fx:?}");
    };
    let second = *second;
    assert_ne!(first, second);
    update(
        &mut app,
        Msg::CommandDone {
            id: second,
            station: "CP-1".into(),
            command: Command::ClearCache,
            outcome: CmdOutcome {
                result: Ok(json!({"status": "Accepted"})),
            },
        },
    );
    assert_eq!(app.inflight.len(), 1);
    assert_eq!(app.inflight[0].id, first);
}

#[test]
fn the_palette_lists_pending_calls_even_without_a_station() {
    let mut app = App::new("ws://0.0.0.0:9000", at(0));
    press(&mut app, KeyCode::Char(':'));
    type_text(&mut app, "Pending calls");
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.mode, Mode::Pending(0)));
    let screen = screen(draw_app(&app, 100, 30).backend());
    assert!(screen.contains("nothing is waiting"), "{screen}");
}

#[tokio::test]
async fn cancel_abandons_a_call_the_station_never_answers() {
    let csms = CsmsServer::start(CsmsConfig::new("127.0.0.1:0".parse().unwrap()))
        .await
        .expect("engine starts");
    let dir = tempfile::tempdir().unwrap();
    let mut config = StationConfig::new(format!("{}/ocpp", csms.url()))
        .with_charge_point_id("CP-P")
        .with_call_timeout_ms(2_000)
        .with_reconnect(ReconnectConfig {
            enabled: false,
            initial_delay_ms: 30,
            max_delay_ms: 100,
            max_attempts: 0,
        })
        .with_state_dir(dir.path());
    config.ws_ping_interval_secs = 0;
    let (station, join) = Station::spawn(config, Arc::new(SystemClock));
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    station
        .send(StationCommand::SetResponseRule {
            rule: ResponseRule {
                action: "ClearCache".to_owned(),
                behavior: Behavior::Ignore,
                remaining: None,
            },
        })
        .await
        .expect("rule");

    let (mut exec, mut rx) = Exec::new();
    let send = Effect::Send {
        id: 5,
        station: "CP-P".into(),
        command: Command::ClearCache,
    };
    assert!(execute(send, &csms, &mut exec).await.is_empty());
    assert!(exec.calls.contains_key(&5));
    assert!(
        execute(Effect::CancelCall { id: 5 }, &csms, &mut exec)
            .await
            .is_empty()
    );
    assert!(exec.calls.is_empty());
    // The task is gone without an answer: it ends as cancelled, no message is sent.
    let ended = timeout(Duration::from_secs(10), exec.tasks.join_next())
        .await
        .expect("the call task ends after the cancel")
        .expect("one task");
    assert!(ended.is_err_and(|e| e.is_cancelled()));
    assert!(
        rx.try_recv().is_err(),
        "no answer is reported for a cancelled call"
    );

    station.shutdown().await.expect("station shutdown");
    join.await.expect("actor ends");
    csms.shutdown().await;
}

// ---- monochrome and clock ------------------------------------------------------------------

#[test]
fn monochrome_draws_no_color_and_marks_states_with_text() {
    let mut app = with_lines();
    app.mono = true;
    let terminal = draw_app(&app, 120, 30);
    let buf = terminal.backend().buffer();
    for cell in buf.content() {
        assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset), "{cell:?}");
    }
    let text = screen(terminal.backend());
    // Station log lines carry their level as a character, not only as a color.
    assert!(text.contains("+ connected"), "{text}");
    assert!(text.contains("! disconnected"), "{text}");
    // A replaced or disconnected station differs from a live one by its dot.
    assert!(text.contains("○ CP-1"), "{text}");
    // The "connected" chip is reverse video instead of a green fill.
    let chip = buf
        .content()
        .iter()
        .find(|c| c.symbol() == "c" && c.modifier.contains(Modifier::REVERSED));
    assert!(chip.is_some(), "no reversed chip in:\n{text}");
}

#[test]
fn color_mode_keeps_the_dot_marker_and_colors() {
    let app = with_lines();
    let terminal = draw_app(&app, 120, 30);
    let text = screen(terminal.backend());
    assert!(text.contains("● connected"), "{text}");
    assert!(text.contains("● disconnected"), "{text}");
    assert!(
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|c| c.fg == Color::Green || c.bg == Color::Green)
    );
}

#[test]
fn a_notice_carries_its_level_in_monochrome() {
    let mut app = with_lines();
    app.mono = true;
    app.notify(kirby_ocpp_tui::app::NoticeLevel::Error, "it broke");
    let text = screen(draw_app(&app, 120, 30).backend());
    assert!(text.contains(" x it broke"), "{text}");
}

#[test]
fn the_time_mode_changes_the_log_rows() {
    // The text in front of the log line `● connected`: a clock `HH:MM:SS.mmm ` or nothing.
    fn clock_before(screen: &str) -> Option<String> {
        let line = screen.lines().find(|l| l.contains("● connected"))?;
        let head: Vec<char> = line.split("● connected").next()?.chars().collect();
        let tail: String = head[head.len().checked_sub(13)?..].iter().collect();
        let shape = |i: usize, c: char| match i {
            2 | 5 => c == ':',
            8 => c == '.',
            12 => c == ' ',
            _ => c.is_ascii_digit(),
        };
        tail.chars()
            .enumerate()
            .all(|(i, c)| shape(i, c))
            .then_some(tail)
    }
    let mut app = with_lines();
    let utc = screen(draw_app(&app, 120, 30).backend());
    assert!(clock_before(&utc).is_some(), "{utc}");
    app.time = TimeMode::None;
    let none = screen(draw_app(&app, 120, 30).backend());
    assert!(none.contains("● connected"), "{none}");
    assert!(clock_before(&none).is_none(), "{none}");
}
