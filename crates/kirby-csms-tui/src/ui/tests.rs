#![allow(
    clippy::assert_is_empty,
    clippy::field_reassign_with_default,
    reason = "test fixtures read better field by field"
)]

use std::time::{Duration, Instant};

use kirby_csms_core::{
    BootInfo, ConnectorState, Direction, Frame, StationState, TransactionRecord, TransactionStatus,
};
use kirby_ocpp_core::ocpp::types::{
    AuthorizationStatus, ChargePointErrorCode, ConnectorStatus, RegistrationStatus,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

use super::*;
use crate::app::{LogView, Msg, update};
use crate::tests::{app_with, at, frame_msg, info};

/// The buffer as text, one line per row.
pub(crate) fn screen(backend: &TestBackend) -> String {
    let buf = backend.buffer();
    let w = usize::from(buf.area.width);
    let cells: Vec<&str> = buf
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    cells
        .chunks(w.max(1))
        .map(|row| row.concat().trim_end().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

fn render(app: &App, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| draw(f, app)).unwrap();
    screen(terminal.backend())
}

fn state() -> StationState {
    let mut s = StationState::default();
    s.boot = Some(BootInfo {
        vendor: "Acme".into(),
        model: "Charger9".into(),
        charge_point_serial_number: Some("SN-77".into()),
        charge_box_serial_number: None,
        firmware_version: Some("1.2.3".into()),
        iccid: None,
        imsi: None,
        meter_type: None,
        meter_serial_number: None,
        booted_at: at(0),
        registration: Some(RegistrationStatus::Accepted),
        interval: Some(300),
    });
    s.connectors.insert(
        1,
        ConnectorState {
            status: ConnectorStatus::Charging,
            error_code: ChargePointErrorCode::NoError,
            info: Some("plugged".into()),
            vendor_id: None,
            vendor_error_code: None,
            timestamp: None,
            received_at: at(0),
        },
    );
    s.transactions.push(TransactionRecord {
        id: 4242,
        connector: 1,
        id_tag: "TAG-XYZ".into(),
        id_tag_status: AuthorizationStatus::Accepted,
        meter_start: 100,
        started_at: at(0),
        status: TransactionStatus::Active,
        meter_stop: None,
        stopped_at: None,
        stop_reason: None,
        energy_wh: Some(1500.0),
        power_w: Some(7200.0),
        last_meter_at: None,
    });
    s.failed_calls = 2;
    s.unknown_stops = 1;
    s
}

fn loaded() -> App {
    let (mut app, _) = app_with(&["CP-1", "CP-2"]);
    update(
        &mut app,
        Msg::Loaded {
            id: "CP-1".into(),
            history: vec![],
            info: Some(Box::new(info("CP-1", true))),
            state: Some(Box::new(state())),
        },
    );
    update(
        &mut app,
        frame_msg(
            "CP-1",
            Direction::In,
            Frame::call("m1", "Authorize", json!({"idTag": "ABC"})),
        ),
    );
    update(
        &mut app,
        frame_msg(
            "CP-1",
            Direction::Out,
            Frame::call_result(
                "m1",
                json!({"idTagInfo": {"status": "Accepted"}, "status": "Accepted"}),
            ),
        ),
    );
    update(&mut app, Msg::Tick(at(5)));
    app
}

#[test]
fn list_details_and_log_are_drawn() {
    let app = loaded();
    let s = render(&app, 140, 36);
    assert!(s.contains("listening on ws://0.0.0.0:9000"), "{s}");
    assert!(s.contains("2/2 connected"), "{s}");
    assert!(s.contains("frames in 1 out 1"), "{s}");
    assert!(
        s.contains("● CP-1") && s.contains("Charger9") && s.contains("tx1"),
        "{s}"
    );
    assert!(s.contains("● CP-2"), "{s}");
    assert!(
        s.contains("boot Acme Charger9") && s.contains("fw 1.2.3") && s.contains("sn SN-77"),
        "{s}"
    );
    assert!(
        s.contains("remote 10.0.0.5:4242") && s.contains("subprotocol ocpp1.6"),
        "{s}"
    );
    assert!(s.contains("heartbeat none"), "{s}");
    assert!(
        s.contains("failed 2") && s.contains("unknown stops 1"),
        "{s}"
    );
    assert!(
        s.contains("Charging") && s.contains("NoError") && s.contains("plugged"),
        "{s}"
    );
    assert!(
        s.contains("4242") && s.contains("TAG-XYZ") && s.contains("1500") && s.contains("7200"),
        "{s}"
    );
    assert!(s.contains("CALL Authorize idTag=ABC"), "{s}");
    assert!(s.contains("RES  Authorize status=Accepted"), "{s}");
}

#[test]
fn disconnected_and_replaced_stations_are_marked() {
    let mut app = loaded();
    app.stations[1].connected = false;
    let s = render(&app, 140, 36);
    assert!(s.contains("○ CP-2"), "{s}");
    assert!(s.contains("1/2 connected"), "{s}");
}

#[test]
fn all_stations_log_tags_rows_with_the_station() {
    let mut app = loaded();
    update(
        &mut app,
        frame_msg(
            "CP-2",
            Direction::In,
            Frame::call("m9", "Heartbeat", json!({})),
        ),
    );
    app.view = LogView::All;
    let s = render(&app, 140, 36);
    assert!(s.contains("all stations"), "{s}");
    assert!(s.contains("CP-2") && s.contains("Heartbeat"), "{s}");
    assert!(s.contains("Authorize"), "{s}");
}

#[test]
fn filter_input_help_and_detail_overlays() {
    let mut app = loaded();
    app.mode = Mode::Input(crate::app::Input {
        target: crate::app::InputTarget::Stations,
        text: kirby_ocpp_tui::text::TextInput::new("cp-2"),
    });
    assert!(render(&app, 100, 30).contains("Filter stations"));
    app.mode = Mode::Help;
    let s = render(&app, 100, 30);
    assert!(s.contains("Help") && s.contains("all stations"), "{s}");
    app.mode = Mode::Normal;
    app.focus = Focus::Log;
    app.open_detail();
    let s = render(&app, 100, 30);
    assert!(
        s.contains("Authorize") && s.contains("--- request ---"),
        "{s}"
    );
    assert!(
        s.contains("station -> CSMS") || s.contains("CSMS -> station"),
        "{s}"
    );
}

#[test]
fn titles_show_filter_search_and_pause() {
    let mut app = loaded();
    app.station_filter = "cp".into();
    app.station_log.search = "auth".into();
    app.station_log.toggle_follow();
    let s = render(&app, 140, 36);
    assert!(s.contains("Stations /cp"), "{s}");
    assert!(s.contains("/auth") && s.contains("PAUSED"), "{s}");
}

#[test]
fn hostile_station_ids_cannot_inject_escapes() {
    let (mut app, _) = app_with(&["bad\u{1b}[2Jid"]);
    update(&mut app, Msg::Tick(at(1)));
    assert!(!render(&app, 100, 30).contains('\u{1b}'));
}

#[test]
fn tiny_and_narrow_terminals_do_not_panic() {
    let mut app = loaded();
    for mode in [Mode::Normal, Mode::Help] {
        app.mode = mode;
        for (w, h) in [
            (0, 0),
            (1, 1),
            (10, 3),
            (24, 8),
            (30, 10),
            (60, 20),
            (99, 30),
            (100, 12),
            (200, 60),
        ] {
            let _ = render(&app, w, h);
        }
    }
    app.mode = Mode::Normal;
    app.open_detail();
    for (w, h) in [(24, 8), (40, 10), (120, 40)] {
        let _ = render(&app, w, h);
    }
    app.mode = Mode::Input(crate::app::Input {
        target: crate::app::InputTarget::LogSearch,
        text: kirby_ocpp_tui::text::TextInput::new("x".repeat(500)),
    });
    for (w, h) in [(24, 8), (40, 10), (120, 40)] {
        let _ = render(&app, w, h);
    }
    assert!(render(&app, 20, 5).contains("too small"));
}

#[test]
fn a_thousand_stations_render_only_the_visible_rows() {
    let ids: Vec<String> = (0..1000).map(|i| format!("ST-{i:04}")).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let (mut app, _) = app_with(&refs);
    let mut fx = Vec::new();
    app.select_row(500, &mut fx);
    let started = Instant::now();
    let mut s = String::new();
    for _ in 0..20 {
        s = render(&app, 120, 30);
    }
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "drawing stays cheap"
    );
    assert!(s.contains("ST-0500"), "the selected row is visible");
    assert!(!s.contains("ST-0000") && !s.contains("ST-0999"));
    assert!(s.contains("1000/1000"), "{s}");
    // Filtering narrows the count.
    let mut fx = Vec::new();
    app.set_station_filter("0777", &mut fx);
    let s = render(&app, 120, 30);
    assert!(s.contains("ST-0777") && s.contains("1/1000"), "{s}");
}
