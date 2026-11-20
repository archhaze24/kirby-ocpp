//! Render tests: `draw2` into a `TestBackend` at several terminal sizes, and the device-model
//! browser.

use chrono::{TimeZone, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kirby_ocpp_core::StationConfig;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::v2::messages::{
    BootReason, ConnectorStatus, Mutability, RegistrationStatus,
};
use kirby_ocpp_core::ocpp::v2::types::{ChargingState, WireEnum};
use kirby_ocpp_core::station::{Direction, Event, FrameRecord, LogLevel, LogRecord};
use kirby_ocpp_core::station2::{
    ConnectorSnapshot2, EvseSnapshot, Snapshot2, TransactionSnapshot, VariableView,
};
use kirby_ocpp_core::text::is_unsafe_char;
use kirby_ocpp_core::ws::ConnectionState;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use serde_json::json;

use super::super::browser::{BrowserEvent, DeviceBrowser, row_text};
use super::super::ui::help_lines2;
use super::super::{App2, Mode2, Tab2, draw2};
use crate::app::{NoticeLevel, PendingCall};
use crate::palette::Palette;

const TX: &str = "0b9c1f3a-5d2e-4a77-9c61-7e8d2f4a1b30";
const STATUSES: [&str; 5] = [
    "Available",
    "Occupied",
    "Reserved",
    "Unavailable",
    "Faulted",
];
const SIZES: [(u16, u16); 3] = [(80, 24), (110, 30), (24, 8)];
/// Words of the 1.6 UI that must not describe 2.0.1 state.
const WORDS_16: [&str; 7] = [
    "Preparing",
    "Finishing",
    "idTag",
    "connector 0",
    "Soft",
    "Hard",
    "Suspended EV",
];

fn status(name: &str) -> ConnectorStatus {
    *ConnectorStatus::ALL
        .iter()
        .find(|s| s.as_str() == name)
        .expect("a connector status")
}

fn evse(id: u32, status_name: &str, plugged: bool, tx: Option<&str>) -> EvseSnapshot {
    EvseSnapshot {
        id,
        operative: true,
        connectors: vec![ConnectorSnapshot2 {
            id: 1,
            status: status(status_name),
            ev_connected: plugged,
            fault: status_name == "Faulted",
        }],
        transaction: tx.map(|id_token| TransactionSnapshot {
            transaction_id: TX.to_owned(),
            next_seq_no: 3,
            charging_state: *ChargingState::ALL
                .iter()
                .find(|s| s.as_str() == "SuspendedEVSE")
                .expect("a charging state"),
            id_token: Some(id_token.to_owned()),
            started_at: Utc.timestamp_millis_opt(1_700_000_000_000).unwrap(),
            energy_wh: 240,
            remote_start_id: Some(7),
        }),
        meter_wh: 1200,
    }
}

fn var(
    component: &str,
    instance: Option<&str>,
    variable: &str,
    value: Option<&str>,
    mutability: Mutability,
) -> VariableView {
    VariableView {
        component: component.to_owned(),
        instance: instance.map(str::to_owned),
        variable: variable.to_owned(),
        value: value.map(str::to_owned),
        mutability,
    }
}

fn app() -> App2 {
    let config = StationConfig::new("ws://csms.example/ocpp").with_charge_point_id("CP-2");
    let mut snapshot = Snapshot2::initial(&config);
    snapshot.connection = ConnectionState::Connected;
    snapshot.registration = Some(RegistrationStatus::Accepted);
    snapshot.boot_reason = Some(BootReason::ALL[0]);
    snapshot.evses = vec![
        evse(1, "Occupied", true, Some("TAG-1")),
        evse(2, "Available", false, None),
    ];
    snapshot.variables = vec![
        var(
            "OCPPCommCtrlr",
            None,
            "HeartbeatInterval",
            Some("30"),
            Mutability::ReadWrite,
        ),
        var(
            "ChargingStation",
            None,
            "SerialNumber",
            Some("X1"),
            Mutability::ReadOnly,
        ),
    ];
    let mut app = App2::new(snapshot, "ws://csms.example/ocpp", "Acme");
    let ts = Utc.timestamp_millis_opt(1_700_000_000_000).unwrap();
    app.log.push(Event::Frame(FrameRecord {
        ts,
        direction: Direction::Out,
        msg_id: Some("m1".into()),
        action: Some("BootNotification".into()),
        frame: json!([2, "m1", "BootNotification", {"reason": "PowerUp"}]),
    }));
    app.log.push(Event::Log(LogRecord {
        ts,
        level: LogLevel::Info,
        text: "connected".into(),
    }));
    app
}

fn draw_buffer(app: &App2, w: u16, h: u16) -> Buffer {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| draw2(f, app)).unwrap();
    term.backend().buffer().clone()
}

#[test]
fn v2_views_use_no_dark_gray_or_dim() {
    let mut a = app();
    for connection in [ConnectionState::Disconnected, ConnectionState::Connected] {
        a.snapshot.connection = connection;
        for (w, h) in [(60, 20), (110, 30), (160, 40)] {
            for c in draw_buffer(&a, w, h).content() {
                assert_ne!(c.fg, ratatui::style::Color::DarkGray, "{c:?}");
                assert_ne!(c.bg, ratatui::style::Color::DarkGray, "{c:?}");
                assert!(!c.modifier.contains(ratatui::style::Modifier::DIM), "{c:?}");
            }
        }
    }
}

fn render(app: &App2, w: u16, h: u16) -> String {
    let buf = draw_buffer(app, w, h);
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

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn assert_clean(text: &str) {
    assert!(
        !text.chars().any(|c| c != '\n' && is_unsafe_char(c)),
        "unsafe character on screen:\n{text:?}"
    );
}

fn assert_no_16_words(text: &str, what: &str) {
    for word in WORDS_16 {
        assert!(!text.contains(word), "`{word}` on {what}:\n{text}");
    }
}

#[test]
fn header_chips_come_from_the_snapshot() {
    let mut a = app();
    let text = render(&a, 110, 30);
    assert!(text.contains("CP-2"), "{text}");
    assert!(text.contains("connected"), "{text}");
    assert!(text.contains("Accepted"), "{text}");
    assert!(text.contains("ocpp2.0.1"), "{text}");
    for title in [
        "Station",
        "EVSE",
        "Transaction",
        "Data",
        "Logs",
        "Scenarios",
    ] {
        assert!(text.contains(title), "tab {title}:\n{text}");
    }
    assert!(!text.contains("Maintenance"), "{text}");
    // The chip is the snapshot's value, never a literal.
    a.snapshot.protocol = "ocpp2.1".to_owned();
    let text = render(&a, 110, 30);
    assert!(text.contains("ocpp2.1"), "{text}");
    assert!(!text.contains("ocpp2.0.1"), "{text}");
    a.snapshot.registration = None;
    assert!(render(&a, 110, 30).contains("not booted"));
}

#[test]
fn connector_status_is_one_of_the_five_wire_names() {
    for name in STATUSES {
        let mut a = app();
        a.snapshot.evses = vec![evse(1, name, name == "Occupied", None)];
        for (w, h) in [(80, 24), (110, 30)] {
            let text = render(&a, w, h);
            assert!(text.contains(name), "{name} at {w}x{h}:\n{text}");
            assert_no_16_words(&text, name);
            assert!(!text.contains("Charging "), "{text}");
        }
    }
}

#[test]
fn row_shows_eight_characters_of_the_transaction_id() {
    let mut a = app();
    a.set_tab(Tab2::Station);
    let text = render(&a, 80, 24);
    let row = text
        .lines()
        .find(|l| l.contains("Occupied") && l.contains(" P "))
        .expect("the EVSE row");
    assert!(row.contains(&TX[..8]), "{row}");
    assert!(!row.contains(&TX[..9]), "{row}");
    assert!(row.contains(" P "), "plug marker: {row}");
}

#[test]
fn details_show_the_full_transaction_id_and_wire_values() {
    for (w, h) in [(80, 24), (110, 30)] {
        let mut a = app();
        a.set_tab(Tab2::Transaction);
        let text = render(&a, w, h);
        assert!(text.contains(TX), "{w}x{h}:\n{text}");
        assert!(text.contains("SuspendedEVSE"), "{w}x{h}:\n{text}");
        assert!(text.contains("TAG-1"), "{w}x{h}:\n{text}");
        assert!(text.contains("running"), "{w}x{h}:\n{text}");
    }
    let mut a = app();
    a.set_tab(Tab2::Transaction);
    let text = render(&a, 110, 40);
    assert!(text.contains("remoteStartId"), "{text}");
    assert!(text.contains("energy"), "{text}");
    assert!(text.contains("240 Wh"), "{text}");
    assert!(text.contains("next seqNo"), "{text}");
}

#[test]
fn station_tab_shows_registration_boot_reason_and_event_counts() {
    let mut a = app();
    a.snapshot.queued_events = 4;
    a.security_events_sent = 2;
    a.snapshot.reconnect_stopped = Some("gave up".to_owned());
    let text = render(&a, 110, 40);
    assert!(text.contains("Accepted"), "{text}");
    assert!(text.contains(BootReason::ALL[0].as_str()), "{text}");
    assert!(text.contains("queued events"), "{text}");
    assert!(text.contains("2 sent (since UI start)"), "{text}");
    assert!(text.contains("stopped: gave up"), "{text}");
    assert!(text.contains("Operative"), "{text}");
}

#[test]
fn every_tab_is_free_of_1_6_words_at_every_size() {
    for (w, h) in SIZES {
        for tab in Tab2::ALL {
            let mut a = app();
            a.set_tab(tab);
            let text = render(&a, w, h);
            assert_no_16_words(&text, &format!("{} at {w}x{h}", tab.title()));
        }
    }
    // The overlays too.
    let mut a = app();
    a.mode = Mode2::Help;
    assert_no_16_words(&render(&a, 110, 60), "help");
    assert_no_16_words(&help_lines2(Protocol::V201).join("\n"), "help text");
    a.mode = Mode2::Palette(Palette::from_rows(&super::super::actions::palette_rows(
        &a.catalog,
    )));
    assert_no_16_words(&render(&a, 110, 60), "palette");
}

#[test]
fn small_and_narrow_terminals_do_not_panic() {
    for (w, h) in [
        (24, 8),
        (25, 9),
        (30, 10),
        (40, 12),
        (60, 16),
        (109, 30),
        (140, 20),
    ] {
        for tab in Tab2::ALL {
            let mut a = app();
            a.set_tab(tab);
            let _ = render(&a, w, h);
            a.mode = Mode2::Help;
            let _ = render(&a, w, h);
            a.mode = Mode2::DeviceModel(DeviceBrowser::new());
            let _ = render(&a, w, h);
            a.mode = Mode2::Pending(0);
            let _ = render(&a, w, h);
        }
    }
    let text = render(&app(), 20, 6);
    assert!(text.contains("need 24x8"), "{text}");
    // No EVSE, nothing selected.
    let mut a = app();
    a.snapshot.evses.clear();
    a.evse = 0;
    for tab in Tab2::ALL {
        a.set_tab(tab);
        let _ = render(&a, 80, 24);
    }
}

#[test]
fn hostile_station_and_csms_text_cannot_inject_escapes() {
    let evil = "\u{1b}[2J\u{1b}]0;pwn\u{7}\u{9b}31m\u{202e}X\u{85}Y";
    let mut a = app();
    a.snapshot.charge_point_id = format!("CP{evil}");
    a.snapshot.protocol = format!("ocpp{evil}");
    a.snapshot.reconnect_stopped = Some(evil.to_owned());
    a.snapshot.evses[0].transaction.as_mut().unwrap().id_token = Some(evil.to_owned());
    a.snapshot.evses[0]
        .transaction
        .as_mut()
        .unwrap()
        .transaction_id = format!("{evil}{TX}");
    a.snapshot.variables.push(var(
        &format!("Comp{evil}"),
        Some(evil),
        &format!("Var{evil}"),
        Some(evil),
        Mutability::ReadWrite,
    ));
    a.url = format!("ws://h/{evil}");
    a.vendor = evil.to_owned();
    let ts = Utc.timestamp_millis_opt(1_700_000_000_500).unwrap();
    a.log.push(Event::Frame(FrameRecord {
        ts,
        direction: Direction::In,
        msg_id: Some("m9".into()),
        action: Some(format!("Evil{evil}")),
        frame: json!([2, "m9", "SecurityEventNotification", {"type": evil, "techInfo": evil}]),
    }));
    a.log.push(Event::Log(LogRecord {
        ts,
        level: LogLevel::Error,
        text: evil.to_owned(),
    }));
    a.notify(NoticeLevel::Error, evil);
    a.pending.push(PendingCall {
        label: evil.to_owned(),
        action: None,
        since: std::time::Instant::now(),
    });
    for tab in Tab2::ALL {
        a.set_tab(tab);
        for (w, h) in [(80, 24), (110, 30)] {
            assert_clean(&render(&a, w, h));
        }
    }
    a.mode = Mode2::DeviceModel(DeviceBrowser::new());
    assert_clean(&render(&a, 110, 30));
    a.mode = Mode2::Pending(0);
    assert_clean(&render(&a, 110, 30));
}

#[test]
fn monochrome_keeps_text_markers() {
    use ratatui::style::{Color, Modifier};
    let mut a = app();
    a.log.push(Event::Log(LogRecord {
        ts: Utc.timestamp_millis_opt(1_700_000_000_100).unwrap(),
        level: LogLevel::Error,
        text: "it broke".into(),
    }));
    a.mono = true;
    a.notify(NoticeLevel::Error, "failed");
    let buf = draw_buffer(&a, 130, 30);
    for cell in buf.content() {
        assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset), "{cell:?}");
    }
    let text = render(&a, 130, 30);
    assert!(text.contains("x it broke"), "{text}");
    assert!(text.contains(" x failed"), "{text}");
    // States are words: the status name, the plug marker and the chips.
    assert!(text.contains("Occupied"), "{text}");
    assert!(text.contains("connected"), "{text}");
    assert!(
        buf.content()
            .iter()
            .any(|c| c.symbol() == "c" && c.modifier.contains(Modifier::REVERSED)),
        "the connection chip is reversed"
    );
    a.mono = false;
    assert!(render(&a, 130, 30).contains("● it broke"));
}

#[test]
fn actions_are_dimmed_and_explained_when_disabled() {
    let mut a = app();
    a.snapshot.connection = ConnectionState::Disconnected;
    let text = render(&a, 110, 30);
    assert!(text.contains("BootNotification"), "{text}");
    assert!(
        text.contains("BootNotification: disabled, not connected"),
        "{text}"
    );
    let buf = draw_buffer(&a, 110, 30);
    assert!(
        buf.content()
            .iter()
            .any(|c| c.fg == crate::theme::SECONDARY),
        "a dimmed row"
    );
    a.snapshot.connection = ConnectionState::Connected;
    let text = render(&a, 110, 30);
    assert!(text.contains("Send BootNotification."), "{text}");
}

#[test]
fn scenarios_tab_lists_the_built_in_scenarios() {
    let mut a = app();
    a.set_tab(Tab2::Scenarios);
    let text = render(&a, 110, 30);
    assert!(text.contains("Actions: Scenarios"), "{text}");
    assert!(text.contains("No scenario run yet"), "{text}");
    assert!(
        text.contains(&a.catalog[a.tab_actions()[0]].label),
        "{text}"
    );
}

#[test]
fn a_2_1_station_shows_the_2_1_version_where_2_0_1_shows_its_own() {
    let mut snapshot = app().snapshot;
    snapshot.protocol = "ocpp2.1".to_owned();
    let mut a = App2::new(snapshot, "ws://csms.example/ocpp", "Kirby");
    assert_eq!(a.protocol, Protocol::V21);
    let text = render(&a, 110, 30);
    assert!(text.contains("ocpp2.1"), "{text}");
    a.mode = Mode2::Help;
    let text = render(&a, 110, 60);
    assert!(text.contains("Not available for OCPP 2.1"), "{text}");
    assert!(!text.contains("2.0.1"), "{text}");
    for def in &a.catalog {
        assert!(!def.desc.contains("2.0.1"), "{}: {}", def.label, def.desc);
    }
    assert!(
        a.catalog
            .iter()
            .any(|def| def.desc.contains("validated against the OCPP 2.1 schema"))
    );
    // The 2.0.1 station keeps its own text.
    let old = app();
    assert_eq!(old.protocol, Protocol::V201);
    assert!(
        old.catalog
            .iter()
            .any(|def| def.desc.contains("validated against the OCPP 2.0.1 schema"))
    );
}

#[test]
fn overlays_draw_for_each_mode() {
    let mut a = app();
    a.mode = Mode2::Help;
    let text = render(&a, 110, 60);
    assert!(text.contains("Not available for OCPP 2.0.1"), "{text}");
    assert!(text.contains("d DataTransfer"), "{text}");
    assert!(text.contains("r raw CALL"), "{text}");
    assert!(text.contains("K device"), "{text}");

    a.mode = Mode2::Palette(Palette::from_rows(&super::super::actions::palette_rows(
        &a.catalog,
    )));
    let text = render(&a, 110, 40);
    assert!(text.contains("Command palette"), "{text}");
    assert!(text.contains("Station: BootNotification"), "{text}");

    a.pending.push(PendingCall {
        label: "boot notification".to_owned(),
        action: Some("BootNotification".to_owned()),
        since: std::time::Instant::now(),
    });
    a.mode = Mode2::Pending(0);
    let text = render(&a, 110, 30);
    assert!(text.contains("Pending calls (1)"), "{text}");
    assert!(text.contains("boot notification"), "{text}");
    assert!(!text.contains("(no cancel)"), "{text}");
}

// ---- device-model browser ----

fn model() -> Vec<VariableView> {
    vec![
        var(
            "OCPPCommCtrlr",
            None,
            "HeartbeatInterval",
            Some("30"),
            Mutability::ReadWrite,
        ),
        var(
            "SecurityCtrlr",
            None,
            "BasicAuthPassword",
            Some("sentinel-Pw-0123"),
            Mutability::ReadWrite,
        ),
        var(
            "SecurityCtrlr",
            Some("Slot2"),
            "Key",
            None,
            Mutability::WriteOnly,
        ),
        var(
            "ChargingStation",
            None,
            "SerialNumber",
            Some("X1"),
            Mutability::ReadOnly,
        ),
    ]
}

#[test]
fn browser_rows_have_the_documented_shape() {
    let vars = model();
    assert_eq!(
        row_text(&vars[0]),
        "OCPPCommCtrlr / HeartbeatInterval = 30 (ReadWrite)"
    );
    assert_eq!(
        row_text(&vars[2]),
        "SecurityCtrlr.Slot2 / Key = (hidden) (WriteOnly)"
    );
    assert_eq!(
        row_text(&vars[3]),
        "ChargingStation / SerialNumber = X1 (ReadOnly)"
    );
}

#[test]
fn browser_hides_secret_values_everywhere() {
    let vars = model();
    // A password that is ReadWrite is hidden too.
    assert!(row_text(&vars[1]).contains("(hidden)"));
    for v in &vars {
        assert!(!row_text(v).contains("sentinel"));
    }
    let mut a = app();
    a.snapshot.variables = vars;
    a.mode = Mode2::DeviceModel(DeviceBrowser::new());
    let text = render(&a, 110, 30);
    assert!(!text.contains("sentinel"), "{text}");
    assert!(text.contains("(hidden)"), "{text}");
    assert!(text.contains("station-level variables"), "{text}");
    assert!(text.contains("GetVariables"), "{text}");
}

#[test]
fn browser_filters_with_slash() {
    let vars = model();
    let mut b = DeviceBrowser::new();
    assert_eq!(b.rows(&vars).len(), vars.len());
    assert_eq!(
        b.handle_key(press(KeyCode::Char('/')), &vars),
        BrowserEvent::None
    );
    assert!(b.filtering);
    for c in "serial".chars() {
        b.handle_key(press(KeyCode::Char(c)), &vars);
    }
    let rows = b.rows(&vars);
    assert_eq!(rows.first().map(|r| r.0), Some(3), "{rows:?}");
    // `q` and `j` are text while typing.
    b.handle_key(press(KeyCode::Char('q')), &vars);
    assert!(b.filtering);
    assert_eq!(b.rows(&vars).len(), 0);
    b.handle_key(press(KeyCode::Backspace), &vars);
    // Enter keeps the filter; the row can then be opened.
    b.handle_key(press(KeyCode::Enter), &vars);
    assert!(!b.filtering);
    assert_eq!(b.query.text(), "serial");
    assert_eq!(
        b.handle_key(press(KeyCode::Enter), &vars),
        BrowserEvent::Show(3)
    );
    // Esc while typing clears the filter and stays open.
    b.handle_key(press(KeyCode::Char('/')), &vars);
    assert_eq!(b.handle_key(press(KeyCode::Esc), &vars), BrowserEvent::None);
    assert!(b.query.is_empty() && !b.filtering);
    assert_eq!(b.rows(&vars).len(), vars.len());
    // The filter line is drawn with the count.
    let mut a = app();
    a.snapshot.variables = vars;
    let mut b = DeviceBrowser::new();
    b.query.insert_str("heart");
    a.mode = Mode2::DeviceModel(b);
    let text = render(&a, 110, 30);
    assert!(text.contains("/ heart"), "{text}");
    assert!(text.contains("of 4"), "{text}");
}

#[test]
fn browser_enter_edits_writable_rows_and_shows_read_only_ones() {
    let vars = model();
    let mut b = DeviceBrowser::new();
    assert_eq!(
        b.handle_key(press(KeyCode::Enter), &vars),
        BrowserEvent::Edit(0)
    );
    b.handle_key(press(KeyCode::Down), &vars);
    assert_eq!(
        b.handle_key(press(KeyCode::Enter), &vars),
        BrowserEvent::Edit(1)
    );
    b.handle_key(press(KeyCode::Char('j')), &vars);
    assert_eq!(
        b.handle_key(press(KeyCode::Enter), &vars),
        BrowserEvent::Edit(2)
    );
    b.handle_key(press(KeyCode::Down), &vars);
    assert_eq!(
        b.handle_key(press(KeyCode::Enter), &vars),
        BrowserEvent::Show(3)
    );
    // The selection stops at the ends.
    for _ in 0..10 {
        b.handle_key(press(KeyCode::Down), &vars);
    }
    assert_eq!(b.selected, 3);
    b.handle_key(press(KeyCode::Home), &vars);
    assert_eq!(b.selected, 0);
    b.handle_key(press(KeyCode::Char('k')), &vars);
    assert_eq!(b.selected, 0);
    // No rows: Enter does nothing.
    assert_eq!(b.handle_key(press(KeyCode::Enter), &[]), BrowserEvent::None);
}

#[test]
fn browser_closes_with_escape_or_q() {
    let vars = model();
    let mut b = DeviceBrowser::new();
    assert_eq!(
        b.handle_key(press(KeyCode::Esc), &vars),
        BrowserEvent::Close
    );
    assert_eq!(
        b.handle_key(press(KeyCode::Char('q')), &vars),
        BrowserEvent::Close
    );
}

#[test]
fn browser_selection_follows_the_filtered_rows() {
    let vars = model();
    let mut b = DeviceBrowser::new();
    b.query.insert_str("Key");
    let rows = b.rows(&vars);
    assert_eq!(rows.first().map(|r| r.0), Some(2), "{rows:?}");
    assert_eq!(
        b.handle_key(press(KeyCode::Enter), &vars),
        BrowserEvent::Edit(2)
    );
}
