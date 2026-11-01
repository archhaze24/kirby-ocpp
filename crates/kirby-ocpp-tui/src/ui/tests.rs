use chrono::{TimeZone, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use kirby_ocpp_core::StationConfig;
use kirby_ocpp_core::station::{Direction, Event, FrameRecord, LogLevel, LogRecord, Snapshot};
use kirby_ocpp_core::ws::ConnectionState;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

use super::*;
use crate::app::{Msg, update};

fn app() -> App {
    let config = StationConfig::new("ws://csms.example/ocpp")
        .with_connectors(2)
        .with_charge_point_id("CP-1");
    let mut app = App::new(Snapshot::initial(&config), "ws://csms.example/ocpp", "Acme");
    app.snapshot.connection = ConnectionState::Connected;
    let ts = |ms: i64| Utc.timestamp_millis_opt(1_700_000_000_000 + ms).unwrap();
    let rec = |ms, dir, frame: serde_json::Value, action: &str| {
        Event::Frame(FrameRecord {
            ts: ts(ms),
            direction: dir,
            msg_id: Some("m1".into()),
            action: Some(action.into()),
            frame,
        })
    };
    app.log.push(rec(
        0,
        Direction::Out,
        json!([2, "m1", "Authorize", {"idTag": "ABC"}]),
        "Authorize",
    ));
    app.log.push(rec(
        31,
        Direction::In,
        json!([3, "m1", {"idTagInfo": {"status": "Accepted"}}]),
        "Authorize",
    ));
    app.log.push(Event::Log(LogRecord {
        ts: ts(40),
        level: LogLevel::Info,
        text: "connected".into(),
    }));
    app
}

fn render(app: &App, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| draw(f, app)).unwrap();
    let buf = term.backend().buffer();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn key(app: &mut App, c: KeyCode) {
    update(
        app,
        Msg::Key(KeyEvent::new_with_kind(
            c,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )),
    );
}

fn keys(app: &mut App, s: &str) {
    for c in s.chars() {
        key(app, KeyCode::Char(c));
    }
}

#[test]
fn wide_layout_shows_three_columns() {
    let text = render(&app(), 130, 30);
    let row = text.lines().find(|l| l.contains("Connectors")).unwrap();
    let (c, a, l) = (
        row.find("Connectors"),
        row.find("Station"),
        row.find("Log ["),
    );
    assert!(c < a && a < l, "{row}");
    assert!(text.contains("CP-1") && text.contains("connected"));
    assert!(text.contains("Actions: Station"));
    assert!(text.contains("→ CALL Authorize idTag=ABC"));
    assert!(
        text.contains("← RES  Authorize") && text.contains("31ms"),
        "{text}"
    );
    assert!(text.contains("● connected"));
}

#[test]
fn compact_layout_stacks_the_panes() {
    let text = render(&app(), 80, 30);
    let pos = |needle: &str| text.lines().position(|l| l.contains(needle)).unwrap();
    assert!(pos("Connectors") < pos("Actions: Station") + 1);
    assert!(pos("Actions: Station") < pos("Log ["));
}

#[test]
fn hint_bar_shows_whole_hints_only() {
    let all = [
        ("Enter", "run"),
        ("↑↓", "move"),
        ("1-7", "tab"),
        ("/", "palette"),
        ("q", "quit"),
    ];
    for w in 1..80 {
        let line = hint_line(&all, w);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(width(text.trim_end()) <= w, "w={w}: {text:?}");
        let prefixes: Vec<String> = (0..=all.len())
            .map(|n| {
                all[..n]
                    .iter()
                    .map(|(k, l)| format!("{k} {l}"))
                    .collect::<Vec<_>>()
                    .join("  ")
            })
            .collect();
        assert!(prefixes.iter().any(|p| text.trim() == p), "w={w}: {text:?}");
    }
    let narrow = hint_line(&all, 20);
    let t: String = narrow.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(t.contains("Enter run") && !t.contains("quit"));
}

#[test]
fn footer_shows_disabled_reason_for_the_selected_action() {
    let mut a = app();
    a.snapshot.connection = ConnectionState::Disconnected;
    let text = render(&a, 100, 30);
    assert!(
        text.contains("BootNotification: disabled, not connected to the CSMS"),
        "{text}"
    );
}

#[test]
fn overlays_render_at_several_sizes() {
    for (w, h) in [(120, 40), (80, 24), (40, 14)] {
        let mut a = app();
        keys(&mut a, "/auth");
        let t = render(&a, w, h);
        assert!(
            t.contains("Command palette") && t.contains("Authorize"),
            "{w}x{h}\n{t}"
        );

        let mut a = app();
        keys(&mut a, "3t");
        key(&mut a, KeyCode::Enter);
        let t = render(&a, w, h);
        assert!(
            t.contains("Start transaction") && t.contains("idTag is required"),
            "{w}x{h}\n{t}"
        );

        let mut a = app();
        keys(&mut a, "3e");
        keys(&mut a, "?");
        assert!(render(&a, w, h).contains("Help"));

        let mut a = app();
        keys(&mut a, "4d");
        assert!(render(&a, w, h).contains("iagnostics"), "{w}x{h}");

        let mut a = app();
        keys(&mut a, "6c");
        assert!(render(&a, w, h).contains("[y/N]"));
    }
}

#[test]
fn log_detail_shows_pretty_json_and_the_correlated_answer() {
    let mut a = app();
    keys(&mut a, "6");
    key(&mut a, KeyCode::Up);
    key(&mut a, KeyCode::Up);
    key(&mut a, KeyCode::Enter);
    let t = render(&a, 100, 30);
    assert!(t.contains("\"idTag\": \"ABC\""), "{t}");
    assert!(
        t.contains("--- answer ---") && t.contains("\"status\": \"Accepted\""),
        "{t}"
    );
}

#[test]
fn form_shows_errors_inline_and_a_wide_char_cursor() {
    let mut a = app();
    keys(&mut a, "3a");
    update(&mut a, Msg::Paste("日本語".into()));
    let t = render(&a, 80, 24);
    assert!(t.replace(' ', "").contains("日本語"), "{t}");
    update(&mut a, Msg::Paste("x".repeat(30)));
    assert!(render(&a, 80, 24).contains("33 characters"));
}

#[test]
fn raw_form_shows_schema_error_for_the_payload() {
    let mut a = app();
    keys(&mut a, "5r");
    key(&mut a, KeyCode::Tab);
    let t = render(&a, 100, 24);
    assert!(t.contains("Raw CALL") && t.contains("Heartbeat"), "{t}");
}

#[test]
fn tiny_and_odd_sizes_never_panic_in_any_mode() {
    let setups: [&str; 9] = ["", "?", "/a", "3t", "3a", "5r", "6c", "4d", "6"];
    for setup in setups {
        let mut a = app();
        keys(&mut a, setup);
        if setup == "6" {
            key(&mut a, KeyCode::Enter);
        }
        for w in [0, 1, 2, 5, 10, 23, 24, 25, 40, 79, 109, 110, 111, 200] {
            for h in [0, 1, 3, 7, 8, 9, 12, 24, 50] {
                let _ = render(&a, w, h);
            }
        }
    }
}

#[test]
fn too_small_terminal_says_so() {
    assert!(render(&app(), 20, 6).contains("too small"));
}

#[test]
fn payload_text_is_displayed_inertly() {
    let mut a = app();
    a.log.push(Event::Log(LogRecord {
        ts: Utc.timestamp_millis_opt(0).unwrap(),
        level: LogLevel::Warn,
        text: "[red]x[/red] \x1b[2J{bold}".into(),
    }));
    let t = render(&a, 110, 30);
    assert!(t.contains("[red]x[/red]") && !t.contains('\x1b'));
}

#[test]
fn log_is_virtualised_and_follows_the_tail() {
    let mut a = app();
    for i in 0..500 {
        a.log.push(Event::Log(LogRecord {
            ts: Utc.timestamp_millis_opt(i).unwrap(),
            level: LogLevel::Info,
            text: format!("line-{i}"),
        }));
    }
    let t = render(&a, 130, 20);
    assert!(t.contains("line-499") && !t.contains("line-10 "));
    a.log.set_filter(crate::logview::LogFilter::Calls);
    assert!(!render(&a, 130, 20).contains("line-499"));
}

#[test]
fn connector_status_and_detail_labels_are_not_cut_or_glued() {
    for w in [110u16, 130] {
        let mut a = app();
        a.snapshot.connectors[0].status = kirby_ocpp_core::ocpp::types::ConnectorStatus::Available;
        keys(&mut a, "3");
        let t = render(&a, w, 30);
        assert!(t.contains("Available"), "{w}\n{t}");
        assert!(!t.contains("availabilityOperative"), "{t}");
    }
}
