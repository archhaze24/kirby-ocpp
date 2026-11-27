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

pub(crate) fn render(app: &App, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| draw(f, app)).unwrap();
    screen(terminal.backend())
}

#[test]
fn v2_before_first_message_renders_at_small_sizes() {
    let (mut app, _) = app_with(&["CP-1"]);
    let mut station_info = info("CP-1", true);
    station_info.subprotocol = Some("ocpp2.0.1".into());
    app.info = Some(station_info);
    app.state = None;
    for (w, h) in [(24, 8), (110, 30), (160, 40)] {
        let screen = render(&app, w, h);
        assert!(!screen.contains("idTag"));
    }
}

pub(crate) fn tx2(id: &str, connector: Option<u32>, token: &str) -> kirby_csms_core::Tx2Record {
    use kirby_ocpp_core::ocpp::v2::messages::TriggerReason;
    kirby_csms_core::Tx2Record {
        id: id.into(),
        evse: Some(1),
        connector,
        id_token: Some(token.into()),
        id_token_type: None,
        id_token_status: None,
        status: TransactionStatus::Active,
        charging_state: None,
        remote_start_id: None,
        reservation_id: None,
        trigger_reason: TriggerReason::Authorized,
        started_at: at(0),
        stopped_at: None,
        stopped_reason: None,
        time_spent_charging: None,
        energy_wh: Some(1500.0),
        power_w: None,
        meter_start_wh: None,
        meter_stop_wh: None,
        last_meter_at: None,
        tariff_id: None,
        transaction_limit: None,
        cost: None,
        priority_charging: false,
        seq: kirby_csms_core::SeqTracker::default(),
    }
}

/// A 2.0.1 station with one running and one ended transaction, a duplicate event and a
/// report in progress; ids and tokens are hostile.
pub(crate) fn app_v2() -> App {
    app_v2_on("ocpp2.0.1")
}

/// The station of [`app_v2`] on the 2.x subprotocol `protocol`.
pub(crate) fn app_v2_on(protocol: &str) -> App {
    use kirby_ocpp_core::ocpp::v2::messages::{BootReason, ConnectorStatus as Status2};
    use kirby_ocpp_core::ocpp::v2::types::{ChargingState, StopReason};
    let (mut app, _) = app_with(&["CP-1"]);
    let mut station_info = info("CP-1", true);
    station_info.subprotocol = Some(protocol.into());
    app.stations[0].protocol = Some(protocol.into());
    app.info = Some(station_info);
    let mut running = tx2(
        "0123456789abcdef-running-transaction",
        None,
        "TOK\u{1b}[2J\u{202e}",
    );
    running.charging_state = Some(ChargingState::SuspendedEvse);
    let mut ended = tx2("fedcba98-ended", Some(1), "TOK-2");
    ended.status = TransactionStatus::Stopped;
    ended.stopped_at = Some(at(60));
    ended.stopped_reason = Some(StopReason::ImmediateReset);
    let mut evse = kirby_csms_core::EvseState::default();
    evse.connectors.insert(
        1,
        kirby_csms_core::Connector2State {
            status: Status2::Occupied,
            timestamp: at(0),
            received_at: at(0),
        },
    );
    let mut s = state();
    s.connectors.clear();
    s.transactions.clear();
    s.v2 = Some(Box::new(kirby_csms_core::StationState2 {
        protocol: protocol.into(),
        boot_reason: Some(BootReason::PowerUp),
        evses: [(1, evse)].into_iter().collect(),
        transactions: vec![ended, running],
        meters: std::collections::BTreeMap::new(),
        counters: kirby_csms_core::Counters2 {
            transaction_events: 4,
            duplicate_events: 1,
            ..kirby_csms_core::Counters2::default()
        },
        last_report: Some(kirby_csms_core::ReportInfo {
            request_id: 7,
            seq_no: 2,
            tbc: true,
            at: at(30),
        }),
    }));
    app.state = Some(s);
    app
}

#[test]
fn v2_station_shows_wire_names_short_ids_and_report_progress() {
    let app = app_v2();
    let screen = render(&app, 160, 40);
    for want in [
        "Occupied",
        "SuspendedEVSE",
        "ImmediateReset",
        "0123456…",
        "PowerUp",
        "1/-",
        "report #7 seqNo 2 tbc true",
    ] {
        assert!(screen.contains(want), "missing {want}:\n{screen}");
    }
    assert!(!screen.contains("0123456789abcdef-running"), "{screen}");
    for unwanted in ["idTag", "Preparing", "Finishing", "\u{1b}", "\u{202e}"] {
        assert!(!screen.contains(unwanted), "found {unwanted:?}:\n{screen}");
    }
    for (w, h) in [(24, 8), (80, 24), (110, 30)] {
        let screen = render(&app, w, h);
        assert!(!screen.contains('\u{1b}'));
    }
}

#[test]
fn every_charging_state_fits_the_table_at_every_width() {
    use kirby_ocpp_core::ocpp::v2::types::ChargingState;
    for (w, full) in [(80, false), (120, false), (140, true), (160, true)] {
        for (state, wire, short) in [
            (ChargingState::EvConnected, "EVConnected", "EVConn"),
            (ChargingState::SuspendedEv, "SuspendedEV", "SuspEV"),
            (ChargingState::SuspendedEvse, "SuspendedEVSE", "SuspEVSE"),
            (ChargingState::Charging, "Charging", "Charging"),
            (ChargingState::Idle, "Idle", "Idle"),
        ] {
            let mut app = app_v2();
            let mut other = app.state.as_ref().unwrap().clone();
            other.v2.as_mut().unwrap().transactions[1].charging_state = Some(state);
            app.state = Some(other);
            let screen = render(&app, w, 40);
            let want = if full { wire } else { short };
            assert!(
                screen.lines().any(|l| l.contains(&format!("{want} "))),
                "w={w} {wire}:\n{screen}"
            );
        }
    }
}

#[test]
fn suspended_charging_states_stay_apart_at_every_width() {
    use kirby_ocpp_core::ocpp::v2::types::ChargingState;
    for (w, full) in [(80, false), (120, false), (140, true), (160, true)] {
        let mut app = app_v2();
        let screen = render(&app, w, 40);
        assert_eq!(screen.contains("SuspendedEVSE"), full, "w={w}:\n{screen}");
        assert!(full || screen.contains("SuspEVSE"), "w={w}:\n{screen}");
        // A second transaction in the other suspended state.
        let mut other = app.state.as_ref().unwrap().clone();
        let v2 = other.v2.as_mut().unwrap();
        v2.transactions[1].charging_state = Some(ChargingState::SuspendedEv);
        app.state = Some(other);
        let screen = render(&app, w, 40);
        let want = if full { "SuspendedEV " } else { "SuspEV " };
        assert!(screen.lines().any(|l| l.contains(want)), "w={w}:\n{screen}");
    }
}

fn help_text(app: &App) -> String {
    crate::help::sections(app)
        .iter()
        .flat_map(|s| s.lines.iter().cloned())
        .collect::<Vec<_>>()
        .join("\n")
}

fn open_help(app: &mut App) {
    app.mode = crate::app::Mode::Help(kirby_ocpp_tui::help::HelpState::default());
}

#[test]
fn the_help_opens_on_the_focused_pane_then_tasks_then_general() {
    let mut app = loaded();
    let titles = |app: &App| -> Vec<String> {
        crate::help::sections(app)
            .into_iter()
            .map(|s| s.title)
            .collect()
    };
    assert_eq!(titles(&app), ["Stations", "Common tasks", "General"]);
    app.focus = crate::app::Focus::Details;
    assert_eq!(titles(&app)[0], "Details");
    app.focus = crate::app::Focus::Log;
    assert_eq!(titles(&app)[0], "Log");
    open_help(&mut app);
    let screen = render(&app, 100, 30);
    assert!(
        screen.contains("Help") && screen.contains("Common tasks"),
        "{screen}"
    );
    assert!(
        !screen.contains("First steps"),
        "no intro paragraph: {screen}"
    );
}

#[test]
fn the_common_tasks_show_the_keys_of_the_selected_stations_protocol() {
    let task_keys = |app: &App| -> Vec<char> {
        let sections = crate::help::sections(app);
        let tasks = sections.iter().find(|s| s.title == "Common tasks").unwrap();
        tasks
            .lines
            .iter()
            .filter_map(|l| l.chars().next().filter(|c| c.is_alphabetic()))
            .collect()
    };
    for key in ['t', 'x', 'R', 'T', 'A', 'u', 'c', 'm'] {
        assert!(task_keys(&loaded()).contains(&key), "1.6: {key}");
    }
    let v2 = app_v2();
    for key in ['t', 'x', 'R', 'T', 'A', 'u', 'c', 'm'] {
        assert!(task_keys(&v2).contains(&key), "2.x: {key}");
    }
    // The text names what the 2.x pair does.
    assert!(help_text(&v2).contains("Change / read the variables"));
    assert!(help_text(&loaded()).contains("Change / read the configuration"));
}

#[test]
fn every_key_the_help_lists_does_something() {
    let app = loaded();
    for line in crate::help::sections(&app)[1..]
        .iter()
        .flat_map(|s| s.lines.iter())
    {
        // Rows are `key  text` with the keys padded to eleven cells; notes have no key.
        let keys = line.get(..11).unwrap_or("").trim();
        if keys.is_empty() || line.as_bytes().get(11) != Some(&b' ') {
            continue;
        }
        for key in keys.split(' ').filter(|k| k.chars().count() == 1) {
            let c = key.chars().next().unwrap();
            let handled =
                app.action_for_key(c).is_some() || matches!(c, '?' | 'q' | 'z' | 'a' | ':');
            assert!(handled, "the help lists `{c}` but no action has it: {line}");
        }
    }
}

#[test]
fn the_help_scrolls_and_fits_a_small_terminal() {
    let mut app = loaded();
    open_help(&mut app);
    for (w, h) in [(80, 24), (24, 8)] {
        let screen = render(&app, w, h);
        assert!(screen.contains("Help"), "{w}x{h}\n{screen}");
        for line in screen.lines() {
            assert!(
                kirby_ocpp_tui::text::width(line) <= usize::from(w),
                "{line}"
            );
        }
    }
    // Down walks to the last row of the section and stops there.
    for _ in 0..50 {
        crate::tests::press(&mut app, crossterm::event::KeyCode::Down);
    }
    let screen = render(&app, 80, 10);
    assert!(screen.contains('▲'), "{screen}");
    assert!(!screen.contains('▼'), "{screen}");
    crate::tests::press(&mut app, crossterm::event::KeyCode::Right);
    let screen = render(&app, 80, 24);
    assert!(screen.contains("Start a transaction"), "{screen}");
}

#[test]
fn a_2_1_station_uses_the_v2_views_and_shows_its_protocol() {
    let app = app_v2_on("ocpp2.1");
    let screen = render(&app, 160, 40);
    for want in [
        " 2.1",
        "Occupied",
        "SuspendedEVSE",
        "ImmediateReset",
        "0123456…",
        "PowerUp",
        "1/-",
        "report #7 seqNo 2 tbc true",
    ] {
        assert!(screen.contains(want), "missing {want}:\n{screen}");
    }
    for unwanted in ["idTag", "Preparing", "\u{1b}", "\u{202e}"] {
        assert!(!screen.contains(unwanted), "found {unwanted:?}:\n{screen}");
    }
    // The 2.0.1 station of the same fixture is marked `2.0.1`, not `2.1`.
    let older = render(&app_v2(), 160, 40);
    assert!(older.contains(" 2.0.1"), "{older}");
    assert!(!older.contains(" 2.1"), "{older}");
    for (w, h) in [(24, 8), (80, 24), (110, 30)] {
        assert!(!render(&app, w, h).contains('\u{1b}'));
    }
}

pub(crate) fn state() -> StationState {
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

pub(crate) fn loaded() -> App {
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
        s.contains("booted Acme Charger9") && s.contains("fw 1.2.3") && s.contains("sn SN-77"),
        "{s}"
    );
    assert!(
        s.contains("remote 10.0.0.5:4242") && s.contains("OCPP 1.6"),
        "{s}"
    );
    assert!(s.contains("no heartbeat"), "{s}");
    assert!(s.contains("1 connector \u{b7} 1 tx \u{b7} 7.2 kW"), "{s}");
    assert!(
        s.contains("failed 2") && s.contains("unknown stops 1"),
        "{s}"
    );
    assert!(
        s.contains("Charging") && s.contains("NoError") && s.contains("plugged"),
        "{s}"
    );
    assert!(
        s.contains("4242") && s.contains("TAG-XYZ") && s.contains("1400") && s.contains("7200"),
        "{s}"
    );
    assert!(s.contains("CALL Authorize idTag=ABC"), "{s}");
    assert!(s.contains("RES  Authorize status=Accepted"), "{s}");
}

#[test]
fn stacked_tables_give_every_connector_a_row_at_110x30() {
    let mut app = loaded();
    let mut st = state();
    let template = st.connectors.get(&1).cloned().unwrap();
    st.connectors.insert(0, template.clone());
    st.connectors.insert(2, template);
    update(
        &mut app,
        Msg::Loaded {
            id: "CP-1".into(),
            history: vec![],
            info: Some(Box::new(info("CP-1", true))),
            state: Some(Box::new(st)),
        },
    );
    let s = render(&app, 110, 30);
    let rows = s.lines().filter(|l| l.contains("plugged")).count();
    assert_eq!(rows, 3, "{s}");
    assert!(s.contains("4242"), "{s}");
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
    open_help(&mut app);
    let s = render(&app, 100, 30);
    assert!(s.contains("Help") && s.contains("Common tasks"), "{s}");
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
    for mode in [
        Mode::Normal,
        Mode::Help(kirby_ocpp_tui::help::HelpState::default()),
    ] {
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
    assert!(render(&app, 20, 5).contains("need 24x8, have 20x5"));
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

// ---- commands -------------------------------------------------------------------------------

mod commands {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use serde_json::json;

    use super::*;
    use crate::actions::Rules;
    use crate::command::{CmdOutcome, Command};
    use crate::tests::{press, type_text};

    fn open(app: &mut App, key: char) {
        update(
            app,
            Msg::Key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE)),
        );
    }

    #[test]
    fn long_ids_with_a_shared_prefix_differ_in_the_list() {
        let a = format!("{}-A", "L".repeat(60));
        let b = format!("{}-B", "L".repeat(60));
        let (app, _) = app_with(&[a.as_str(), b.as_str()]);
        let screen = render(&app, 100, 30);
        assert!(screen.contains("-A") && screen.contains("-B"), "{screen}");
        assert!(screen.contains('…'), "{screen}");
    }

    #[test]
    fn authorization_key_value_is_drawn_as_stars() {
        let mut app = loaded();
        open(&mut app, 'c');
        type_text(&mut app, "AuthorizationKey");
        update(
            &mut app,
            Msg::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        );
        update(&mut app, Msg::Paste("supersecretkey12345".into()));
        let screen = render(&app, 100, 30);
        assert!(!screen.contains("supersecret"), "{screen}");
        assert!(screen.contains(&"*".repeat(19)), "{screen}");
        assert!(screen.contains("hidden"), "{screen}");
        // The key field itself and other keys stay readable.
        assert!(screen.contains("AuthorizationKey"), "{screen}");
        let mut app = loaded();
        open(&mut app, 'c');
        type_text(&mut app, "HeartbeatInterval");
        update(
            &mut app,
            Msg::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        );
        update(&mut app, Msg::Paste("60".into()));
        let screen = render(&app, 100, 30);
        assert!(
            screen.contains("60") && !screen.contains("hidden"),
            "{screen}"
        );
    }

    #[test]
    fn schema_errors_wrap_onto_more_lines_in_the_raw_form() {
        let mut app = loaded();
        open(&mut app, 'w');
        update(
            &mut app,
            Msg::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        );
        let untouched = render(&app, 100, 30);
        assert!(!untouched.contains("violates"), "{untouched}");
        assert!(untouched.contains("empty sends {}"), "{untouched}");
        // Replace the template with a payload that breaks the schema.
        update(
            &mut app,
            Msg::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
        );
        type_text(&mut app, r#"{"x":1}"#);
        for (w, h) in [(60, 30), (80, 30), (120, 40)] {
            let screen = render(&app, w, h);
            assert!(screen.contains("violates"), "{w}x{h}\n{screen}");
            // The cause after the first clause is visible too.
            assert!(
                screen.contains("Constraint") || screen.contains("required"),
                "{w}x{h}\n{screen}"
            );
        }
    }

    #[test]
    fn palette_lists_actions_with_keys_and_reasons() {
        let (mut app, _) = app_with(&[]);
        open(&mut app, ':');
        let screen = render(&app, 100, 40);
        assert!(screen.contains("Actions"), "{screen}");
        assert!(screen.contains("t  Remote start transaction (no station selected)"));
        assert!(screen.contains("Set response rule"));
        assert!(
            !screen.contains("Set response rule ("),
            "rules need no station"
        );
        type_text(&mut app, "reset");
        let screen = render(&app, 100, 30);
        assert!(screen.contains(": reset"), "{screen}");
        assert!(screen.contains("Reset"));

        let mut app = loaded();
        open(&mut app, ':');
        let screen = render(&app, 100, 40);
        assert!(screen.contains("t  Remote start transaction"), "{screen}");
        assert!(!screen.contains("Remote start transaction ("), "{screen}");
        assert!(screen.contains("Forget station (station is connected)"));
    }

    #[test]
    fn form_shows_its_fields_and_the_inline_error() {
        let mut app = loaded();
        open(&mut app, 't');
        press(&mut app, KeyCode::Enter);
        let screen = render(&app, 100, 30);
        assert!(
            screen.contains("Remote start transaction \u{2192} CP-1 (1.6)"),
            "{screen}"
        );
        assert!(screen.contains("idTag"));
        assert!(screen.contains("connector (optional)"));
        assert!(screen.contains("idTag is required"), "{screen}");
        assert!(screen.contains("Enter send"), "{screen}");

        let mut app = loaded();
        open(&mut app, 'P');
        let screen = render(&app, 120, 30);
        assert!(screen.contains("csChargingProfiles (JSON)"), "{screen}");
        // A long template opens at its start, at 80x24 too.
        assert!(screen.contains("{\"chargingProfileId\":1"), "{screen}");
        let small = render(&app, 80, 24);
        assert!(small.contains("{\"chargingProfileId\":1"), "{small}");
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::End);
        assert!(
            render(&app, 120, 30).contains("chargingSchedulePeriod"),
            "End shows the end of the template"
        );
        press(&mut app, KeyCode::Backspace);
        let screen = render(&app, 120, 30);
        assert!(screen.contains("invalid JSON"), "{screen}");

        let mut app = loaded();
        open(&mut app, 'R');
        let screen = render(&app, 100, 30);
        assert!(screen.contains("◀ Soft ▶"), "{screen}");
    }

    #[test]
    fn main_views_use_no_dark_gray_or_dim() {
        let apps = [loaded(), app_v2()];
        for app in &apps {
            for (w, h) in [(60, 20), (100, 30), (160, 40)] {
                let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                terminal.draw(|f| draw(f, app)).unwrap();
                for c in terminal.backend().buffer().content() {
                    assert_ne!(c.fg, Color::DarkGray, "{c:?}");
                    assert_ne!(c.bg, Color::DarkGray, "{c:?}");
                    assert!(!c.modifier.contains(Modifier::DIM), "{c:?}");
                }
            }
        }
    }

    #[test]
    fn confirm_and_results_overlays() {
        let mut app = loaded();
        open(&mut app, 'R');
        press(&mut app, KeyCode::Enter);
        let screen = render(&app, 100, 30);
        assert!(screen.contains("Confirm"), "{screen}");
        assert!(
            screen.contains("Send Reset (Soft) to CP-1?  [y/N]"),
            "{screen}"
        );

        let mut app = loaded();
        for outcome in [
            CmdOutcome {
                result: Ok(json!({"status": "Accepted"})),
            },
            CmdOutcome {
                result: Err(crate::command::CmdError(
                    "CALLERROR InternalError: boom".into(),
                )),
            },
        ] {
            update(
                &mut app,
                Msg::CommandDone {
                    id: 0,
                    station: "CP-1".into(),
                    command: Command::ClearCache,
                    outcome,
                },
            );
        }
        open(&mut app, 'r');
        let screen = render(&app, 100, 30);
        assert!(screen.contains("Results of CP-1"), "{screen}");
        assert!(
            screen.contains("ClearCache: CALLERROR InternalError: boom"),
            "{screen}"
        );
        assert!(screen.contains("ClearCache: Accepted"), "{screen}");
        press(&mut app, KeyCode::Enter);
        let screen = render(&app, 100, 30);
        assert!(screen.contains("--- request ---"), "{screen}");
        assert!(screen.contains("--- error ---"), "{screen}");
    }

    #[test]
    fn header_shows_pending_commands_and_active_rules() {
        let mut app = loaded();
        assert!(!render(&app, 120, 30).contains("pending"));
        app.pending = 3;
        app.rules = Rules {
            actions: vec!["Authorize".into(), "Heartbeat".into()],
            scoped: vec![crate::actions::ScopedRule {
                station: Some("CP-1".into()),
                action: "Reset".into(),
                protocol: None,
            }],
            faults: 2,
        };
        let screen = render(&app, 120, 30);
        assert!(screen.contains("3 pending"), "{screen}");
        assert!(
            screen.contains("faults 2 rules: Authorize,Heartbeat,Reset@CP-1"),
            "{screen}"
        );
        // many rules never overflow a narrow header
        app.rules.actions = (0..40).map(|i| format!("Action{i}")).collect();
        let _ = render(&app, 40, 12);
    }

    #[test]
    fn help_lists_the_action_keys() {
        let mut app = loaded();
        open(&mut app, '?');
        // The tasks section, then the general one, on a wide and a narrow terminal.
        press(&mut app, KeyCode::Right);
        for (w, h) in [(100, 50), (80, 40)] {
            let screen = render(&app, w, h);
            // Wrapped text: compare the words, not the line breaks and borders.
            let words = screen
                .replace('│', " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            assert!(words.contains("Command palette"), "{w}x{h}\n{screen}");
            assert!(words.contains("t Start a transaction"), "{w}x{h}\n{screen}");
            assert!(
                words.contains("m i M Scripted behavior"),
                "{w}x{h}\n{screen}"
            );
            assert!(screen.lines().all(|l| l.chars().count() <= usize::from(w)));
        }
        press(&mut app, KeyCode::Right);
        let screen = render(&app, 100, 40);
        assert!(screen.contains("Copy the station id"), "{screen}");
        assert!(screen.contains("Forget a disconnected station"), "{screen}");
    }

    #[test]
    fn overlays_survive_every_terminal_size() {
        let mut makers: Vec<Box<dyn Fn() -> App>> = Vec::new();
        for key in [
            't', 'x', 'R', 'c', 'g', 'T', 'u', 'A', 'P', 'D', 'w', 'm', 'i', ':', 'F', 'M', 'r',
            '?',
        ] {
            makers.push(Box::new(move || {
                let mut app = loaded();
                app.rules = Rules {
                    actions: vec!["Authorize".into()],
                    faults: 1,
                    ..Rules::default()
                };
                update(
                    &mut app,
                    Msg::CommandDone {
                        id: 0,
                        station: "CP-1".into(),
                        command: Command::ClearCache,
                        outcome: CmdOutcome {
                            result: Ok(json!({"status": "Accepted"})),
                        },
                    },
                );
                open(&mut app, key);
                if key == 'R' {
                    press(&mut app, KeyCode::Enter);
                }
                if key == 't' {
                    press(&mut app, KeyCode::Enter);
                }
                app
            }));
        }
        for make in &makers {
            let app = make();
            for (w, h) in [(24, 8), (30, 9), (40, 12), (60, 16), (80, 24), (200, 60)] {
                let _ = render(&app, w, h);
            }
        }
    }
}

#[test]
fn notices_with_bidi_and_line_separators_are_drawn_inert() {
    let mut a = app_with(&[]).0;
    a.notify(
        kirby_ocpp_tui::app::NoticeLevel::Error,
        "bad \u{202e}evil\u{2028}end",
    );
    let t = render(&a, 120, 40);
    assert!(t.contains("evil") && !t.contains('\u{202e}') && !t.contains('\u{2028}'));
}

#[test]
fn empty_state_shows_url_example_and_profile_zero_note() {
    let mut app = App::new("ws://0.0.0.0:9000", at(0));
    let text = render(&app, 120, 30);
    assert!(text.contains("Listening on ws://0.0.0.0:9000"), "{text}");
    assert!(
        text.contains("ws://<this-host>:9000/<station id>"),
        "{text}"
    );
    assert!(text.contains("any peer can connect"), "{text}");
    app.security_profile = 2;
    assert!(!render(&app, 120, 30).contains("any peer can connect"));
}

#[test]
fn station_url_example_keeps_specific_hosts() {
    assert_eq!(
        station_url_example("ws://192.168.1.5:9000", false),
        "ws://192.168.1.5:9000/<station id>"
    );
    assert_eq!(
        station_url_example("wss://[::]:9443", true),
        "wss://<this-host>:9443/<prefix>/<station id>"
    );
}

#[test]
fn station_url_example_handles_missing_ports_and_ipv6() {
    for (listen, expected) in [
        ("ws://example.org", "ws://example.org/<station id>"),
        ("ws://[::1]:9000", "ws://[::1]:9000/<station id>"),
        ("ws://[::1]", "ws://[::1]/<station id>"),
        ("ws://::1", "ws://[::1]/<station id>"),
        ("ws://[::]", "ws://<this-host>/<station id>"),
        ("ws://0.0.0.0:9000", "ws://<this-host>:9000/<station id>"),
        ("ws://host:9000/", "ws://host:9000/<station id>"),
    ] {
        assert_eq!(station_url_example(listen, false), expected, "{listen}");
    }
}

#[test]
fn footer_keeps_help_and_palette_hints_in_narrow_terminals() {
    let (app, _) = app_with(&["CP-1"]);
    for w in [40_u16, 30, 24] {
        let text = render(&app, w, 12);
        let footer = text.lines().last().unwrap().to_owned();
        assert!(
            footer.contains("? help") && footer.contains(": actions"),
            "w={w}: {footer:?}"
        );
    }
}

/// The keys a footer hint stands for.
fn hint_codes(hint: &str) -> Vec<crossterm::event::KeyCode> {
    use crossterm::event::KeyCode;
    match hint {
        "Enter" => vec![KeyCode::Enter],
        "↑↓" => vec![KeyCode::Up, KeyCode::Down],
        "←→" => vec![KeyCode::Left, KeyCode::Right],
        other => other.chars().map(KeyCode::Char).collect(),
    }
}

fn fingerprint(a: &App) -> String {
    format!(
        "{:?} {} {:?} {} {} {:?} {}",
        a.focus,
        a.station_filter,
        a.view,
        a.log().follow,
        a.quit,
        a.log().selected_index(),
        a.notice.as_ref().map_or("", |n| n.text.as_str()),
    ) + &format!("{:?}", std::mem::discriminant(&a.mode))
}

#[test]
fn every_footer_hint_key_does_what_its_label_says() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let fresh = |focus: crate::app::Focus| {
        let mut a = loaded();
        a.focus = focus;
        a.inflight.push(crate::app::Inflight {
            id: 1,
            station: "CP-1".into(),
            command: crate::command::Command::ClearCache,
            since: a.now,
        });
        a
    };
    for focus in [crate::app::Focus::Stations, crate::app::Focus::Log] {
        for (k, label) in hints(&fresh(focus)) {
            let did_something = hint_codes(k).into_iter().any(|code: KeyCode| {
                let mut a = fresh(focus);
                let before = fingerprint(&a);
                let fx = update(&mut a, Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
                !fx.is_empty() || fingerprint(&a) != before
            });
            assert!(
                did_something,
                "{focus:?}: the hint `{k} {label}` does nothing"
            );
        }
    }
}

#[test]
fn the_slash_hint_names_the_station_filter_only_on_the_station_list() {
    let keys = |a: &App| hints(a).into_iter().map(|(k, _)| k).collect::<Vec<_>>();
    let mut a = loaded();
    a.focus = crate::app::Focus::Stations;
    assert!(keys(&a).contains(&"/"));
    a.focus = crate::app::Focus::Log;
    assert!(!keys(&a).contains(&"/") && keys(&a).contains(&"s"));
}

#[test]
fn a_palette_query_drops_weak_scattered_matches() {
    let mut app = app_v2();
    crate::tests::press(&mut app, crossterm::event::KeyCode::Char(':'));
    crate::tests::type_text(&mut app, "tariff");
    let screen = render(&app, 120, 40);
    assert!(
        screen.contains("tariff") || screen.contains("Tariff"),
        "{screen}"
    );
    assert!(!screen.contains("Get variables"), "{screen}");
}

#[test]
fn the_help_names_the_station_setup_for_a_new_charge_point() {
    let app = loaded();
    assert!(
        help_text(&app).contains("ws://<host>:<port>/<prefix>/<station id>"),
        "{}",
        help_text(&app)
    );
}
