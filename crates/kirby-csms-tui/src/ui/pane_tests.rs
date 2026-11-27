//! The pane layout at a small and a wide terminal: details, scroll markers, zoom, the shared
//! first line of the details, and the request-first frame detail.

use crossterm::event::KeyCode;

use super::tests::{app_v2, loaded, render, tx2};
use crate::app::{App, Focus, Mode};
use crate::tests::press;

fn many_transactions(app: &mut App, count: usize) {
    let v2 = app
        .state
        .as_mut()
        .and_then(|s| s.v2.as_mut())
        .expect("a 2.x station");
    v2.transactions.clear();
    for i in 0..count {
        v2.transactions
            .push(tx2(&format!("tx{i:04}-id"), Some(1), &format!("TOK{i}")));
    }
}

fn sized(mut app: App, w: u16, h: u16) -> App {
    app.size = (w, h);
    app
}

/// The line of `screen` that holds `needle`.
fn line_with<'a>(screen: &'a str, needle: &str) -> &'a str {
    screen
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?}:\n{screen}"))
}

#[test]
fn the_details_pane_gets_most_of_a_small_terminal_and_starts_with_a_summary() {
    let app = sized(app_v2(), 80, 24);
    let screen = render(&app, 80, 24);
    assert!(
        line_with(&screen, "1 EVSE").contains("2 tx") || screen.contains("1 EVSE \u{b7} 1 tx"),
        "{screen}"
    );
    // The list shrinks to the selected row and a count; the log to a few rows.
    let rows: Vec<&str> = screen.lines().collect();
    let station_title = rows
        .iter()
        .position(|l| l.contains("Station CP-1"))
        .expect("details title");
    let log_title = rows
        .iter()
        .position(|l| l.contains("Log: CP-1"))
        .expect("log title");
    assert!(
        log_title - station_title >= 11,
        "the details have at least 11 rows:\n{screen}"
    );
    assert!(station_title <= 6, "{screen}");
    assert!(screen.contains("Stations 1/1"), "{screen}");
    // The 1.6 station summarizes the same way.
    let screen = render(&sized(loaded(), 80, 24), 80, 24);
    assert!(
        screen.contains("1 connector \u{b7} 1 tx \u{b7} 7.2 kW"),
        "{screen}"
    );
}

#[test]
fn every_cut_pane_says_how_much_is_cut_and_the_selection_stays_visible() {
    let mut app = sized(app_v2(), 80, 24);
    many_transactions(&mut app, 20);
    let screen = render(&app, 80, 24);
    assert!(
        screen.contains("\u{25bc}"),
        "rows below are marked:\n{screen}"
    );
    assert!(screen.contains("tx0019"), "the newest row:\n{screen}");
    // Move to the last row: it scrolls into view and the rows above are marked.
    press(&mut app, KeyCode::Right);
    assert_eq!(app.focus, Focus::Details);
    for _ in 0..19 {
        press(&mut app, KeyCode::Down);
    }
    let screen = render(&app, 80, 24);
    assert!(screen.contains("tx0000"), "the selected row:\n{screen}");
    assert!(
        screen.contains("\u{25b2}"),
        "rows above are marked:\n{screen}"
    );
    // The station list is cut too: its marker sits on the border.
    let (mut many, _) = crate::tests::app_with(&["A", "B", "C", "D", "E"]);
    many.size = (80, 24);
    let screen = render(&many, 80, 24);
    assert!(
        line_with(&screen, "Stations 5/5").len() > 10 && screen.contains("\u{25bc} 4 more"),
        "{screen}"
    );
}

#[test]
fn enter_acts_only_on_a_row_that_is_on_screen() {
    let mut app = sized(app_v2(), 80, 24);
    press(&mut app, KeyCode::Right);
    assert_eq!(app.focus, Focus::Details);
    assert!(crate::ui::visible_transaction_rows(&app) > 0);
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.mode, Mode::Detail(_)), "{:?}", app.mode);
    // A terminal with no room for the table: Enter says so, and `z` makes room.
    let mut cramped = sized(app_v2(), 40, 12);
    press(&mut cramped, KeyCode::Right);
    assert_eq!(crate::ui::visible_transaction_rows(&cramped), 0);
    press(&mut cramped, KeyCode::Enter);
    assert!(matches!(cramped.mode, Mode::Normal));
    assert!(
        cramped
            .notice
            .as_ref()
            .is_some_and(|n| n.text.contains("z zooms")),
        "{:?}",
        cramped.notice
    );
    press(&mut cramped, KeyCode::Char('z'));
    assert!(crate::ui::visible_transaction_rows(&cramped) > 0);
    press(&mut cramped, KeyCode::Enter);
    assert!(matches!(cramped.mode, Mode::Detail(_)));
}

#[test]
fn z_zooms_the_focused_pane_and_esc_or_z_restores_it() {
    for (w, h) in [(80, 24), (200, 50)] {
        let mut app = sized(loaded(), w, h);
        let normal = render(&app, w, h);
        assert!(normal.contains("Stations") && normal.contains("Log: CP-1"));
        // The list is focused: it fills the body.
        press(&mut app, KeyCode::Char('z'));
        assert!(app.zoom);
        let screen = render(&app, w, h);
        assert!(screen.contains("Stations 2/2"), "{screen}");
        assert!(!screen.contains("Log: CP-1") && !screen.contains("Station CP-1"));
        assert!(screen.contains("z unzoom"), "{screen}");
        press(&mut app, KeyCode::Esc);
        assert!(!app.zoom);
        assert_eq!(render(&app, w, h), normal);
        // The log.
        press(&mut app, KeyCode::BackTab);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Log);
        press(&mut app, KeyCode::Char('z'));
        let screen = render(&app, w, h);
        assert!(screen.contains("Log: CP-1"), "{screen}");
        assert!(!screen.contains("Stations 2/2") && !screen.contains("Station CP-1"));
        press(&mut app, KeyCode::Char('z'));
        assert!(!app.zoom);
        // The details.
        press(&mut app, KeyCode::Left);
        assert_eq!(app.focus, Focus::Details);
        press(&mut app, KeyCode::Char('z'));
        let screen = render(&app, w, h);
        assert!(screen.contains("Station CP-1"), "{screen}");
        assert!(!screen.contains("Stations 2/2") && !screen.contains("Log: CP-1"));
    }
}

#[test]
fn both_protocols_start_the_details_with_the_same_line() {
    for app in [loaded(), app_v2()] {
        let screen = render(&sized(app, 120, 30), 120, 30);
        let line = line_with(&screen, "OCPP ");
        for part in ["connected for", "booted", "heartbeat"] {
            assert!(line.contains(part), "{part} is missing in {line:?}");
        }
    }
    let screen = render(&sized(app_v2(), 120, 30), 120, 30);
    assert!(screen.contains("events 4  duplicates 1"), "{screen}");
    let screen = render(&sized(loaded(), 120, 30), 120, 30);
    assert!(screen.contains("calls received"), "{screen}");
}

#[test]
fn log_rows_carry_the_key_fields_of_the_command() {
    use crate::app::update;
    use crate::tests::frame_msg;
    use kirby_csms_core::{Direction, Frame};
    use serde_json::json;
    let mut app = loaded();
    for (id, action, payload) in [
        ("c1", "Reset", json!({"type": "Soft"})),
        ("c2", "RemoteStartTransaction", json!({"idTag": "TAG1"})),
        (
            "c3",
            "RequestStartTransaction",
            json!({"remoteStartId": 1, "idToken": {"idToken": "TAG1", "type": "Central"}}),
        ),
        (
            "c4",
            "TriggerMessage",
            json!({"requestedMessage": "StatusNotification"}),
        ),
    ] {
        update(
            &mut app,
            frame_msg("CP-1", Direction::Out, Frame::call(id, action, payload)),
        );
    }
    let screen = render(&sized(app, 200, 50), 200, 50);
    for want in [
        "Reset type=Soft",
        "RemoteStartTransaction idTag=TAG1",
        "RequestStartTransaction remoteStartId=1 idToken=TAG1",
        "TriggerMessage requestedMessage=StatusNotification",
    ] {
        assert!(screen.contains(want), "{want}:\n{screen}");
    }
}

#[test]
fn the_frame_detail_shows_the_request_before_the_answer() {
    let mut app = loaded();
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.focus, Focus::Log);
    // Select the answer row (the last): the request still comes first.
    press(&mut app, KeyCode::End);
    press(&mut app, KeyCode::Enter);
    let Mode::Detail(detail) = &app.mode else {
        panic!("frame detail: {:?}", app.mode);
    };
    let at = |needle: &str| {
        detail
            .lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no {needle}: {:?}", detail.lines))
    };
    assert!(at("--- request ---") < at("--- answer ---"));
    assert!(at("\"idTag\"") < at("\"idTagInfo\""));
}
