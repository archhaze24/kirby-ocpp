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
    fn palette_lists_actions_with_keys_and_reasons() {
        let (mut app, _) = app_with(&[]);
        open(&mut app, ':');
        let screen = render(&app, 100, 30);
        assert!(screen.contains("Actions"), "{screen}");
        assert!(screen.contains("t  Remote start transaction (no station selected)"));
        assert!(screen.contains("Response rule"));
        assert!(!screen.contains("Response rule ("), "rules need no station");
        type_text(&mut app, "reset");
        let screen = render(&app, 100, 30);
        assert!(screen.contains(": reset"), "{screen}");
        assert!(screen.contains("Reset"));

        let mut app = loaded();
        open(&mut app, ':');
        let screen = render(&app, 100, 30);
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
        assert!(screen.contains("RemoteStartTransaction"), "{screen}");
        assert!(screen.contains("idTag"));
        assert!(screen.contains("connector (optional)"));
        assert!(screen.contains("idTag is required"), "{screen}");
        assert!(screen.contains("Enter send"), "{screen}");

        let mut app = loaded();
        open(&mut app, 'P');
        let screen = render(&app, 120, 30);
        assert!(screen.contains("csChargingProfiles (JSON)"), "{screen}");
        assert!(screen.contains("chargingSchedulePeriod"), "{screen}");
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Backspace);
        let screen = render(&app, 120, 30);
        assert!(screen.contains("invalid JSON"), "{screen}");

        let mut app = loaded();
        open(&mut app, 'R');
        let screen = render(&app, 100, 30);
        assert!(screen.contains("◀ Soft ▶"), "{screen}");
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
            faults: 2,
        };
        let screen = render(&app, 120, 30);
        assert!(screen.contains("3 pending"), "{screen}");
        assert!(
            screen.contains("faults 2 rules: Authorize,Heartbeat"),
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
        let screen = render(&app, 100, 50);
        assert!(screen.contains("command palette"), "{screen}");
        assert!(screen.contains("t remote start"), "{screen}");
        assert!(screen.contains("m response rule"), "{screen}");
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
                };
                update(
                    &mut app,
                    Msg::CommandDone {
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
