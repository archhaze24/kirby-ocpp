//! Model tests (pure `update`) and the run-loop plumbing against a real engine and station.

#![allow(
    clippy::assert_is_empty,
    clippy::field_reassign_with_default,
    reason = "test fixtures read better field by field"
)]

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyModifiers};
use futures_util::StreamExt;
use kirby_csms_core::{
    CsmsConfig, CsmsEvent, CsmsServer, Direction, DisconnectReason, Frame, FrameEntry, StationInfo,
};
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::types::RegistrationStatus;
use kirby_ocpp_core::{Station, StationConfig, SystemClock};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;
use tokio::sync::mpsc;

use crate::app::{App, Effect, Focus, LogView, Mode, Msg, update};
use crate::{TuiOptions, drive};

pub(crate) fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
}

pub(crate) fn info(id: &str, connected: bool) -> StationInfo {
    StationInfo {
        id: id.to_owned(),
        connected,
        connected_at: connected.then(|| at(0)),
        disconnected_at: None,
        remote_addr: connected.then(|| "10.0.0.5:4242".parse().unwrap()),
        subprotocol: connected.then(|| "ocpp1.6".to_owned()),
        received_calls: 3,
        received_by_action: std::collections::BTreeMap::new(),
    }
}

pub(crate) fn app_with(ids: &[&str]) -> (App, Vec<Effect>) {
    let mut app = App::new("ws://0.0.0.0:9000", at(0));
    let infos = ids.iter().map(|id| info(id, true)).collect();
    let fx = update(
        &mut app,
        Msg::Resynced {
            infos,
            states: Vec::new(),
        },
    );
    (app, fx)
}

fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
    update(app, Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

pub(crate) fn frame_msg(station: &str, dir: Direction, frame: Frame) -> Msg {
    Msg::Event(CsmsEvent::Frame {
        station: station.to_owned(),
        direction: dir,
        frame,
    })
}

fn authorize(id: &str) -> Frame {
    Frame::call(id, "Authorize", json!({"idTag": "ABC"}))
}

#[test]
fn resync_sorts_and_selects_the_first_station() {
    let (app, fx) = app_with(&["CP-2", "CP-1", "CP-3"]);
    let ids: Vec<_> = app.stations.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["CP-1", "CP-2", "CP-3"]);
    assert_eq!(app.selected.as_deref(), Some("CP-1"));
    assert_eq!(fx, [Effect::Load("CP-1".to_owned())]);
}

#[test]
fn cursor_keys_move_the_selection_and_load_the_station() {
    let (mut app, _) = app_with(&["A", "B", "C"]);
    assert_eq!(
        press(&mut app, KeyCode::Down),
        [Effect::Load("B".to_owned())]
    );
    assert_eq!(
        press(&mut app, KeyCode::Char('j')),
        [Effect::Load("C".to_owned())]
    );
    assert!(
        press(&mut app, KeyCode::Down).is_empty(),
        "stays on the last row"
    );
    assert_eq!(
        press(&mut app, KeyCode::Home),
        [Effect::Load("A".to_owned())]
    );
    assert_eq!(
        press(&mut app, KeyCode::End),
        [Effect::Load("C".to_owned())]
    );
    assert_eq!(
        press(&mut app, KeyCode::PageUp),
        [Effect::Load("A".to_owned())]
    );
    assert_eq!(app.cursor(), Some(0));
}

#[test]
fn station_filter_is_fuzzy_live_and_cleared_by_escape() {
    let (mut app, _) = app_with(&["alpha-1", "beta-2", "alpha-3"]);
    press(&mut app, KeyCode::Char('/'));
    assert!(matches!(app.mode, Mode::Input(_)));
    type_text(&mut app, "bt2");
    assert_eq!(app.visible_len(), 1);
    assert_eq!(
        app.selected.as_deref(),
        Some("beta-2"),
        "selection follows the filter"
    );
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.station_filter, "bt2");
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.station_filter, "");
    assert_eq!(app.visible_len(), 3);
    press(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "zzz");
    assert_eq!(app.visible_len(), 0);
    press(&mut app, KeyCode::Esc);
    assert_eq!(
        app.visible_len(),
        3,
        "Esc inside the input clears and closes it"
    );
}

#[test]
fn frames_go_to_the_all_log_and_to_the_selected_station_log_only() {
    let (mut app, _) = app_with(&["A", "B"]);
    update(&mut app, frame_msg("A", Direction::In, authorize("1")));
    update(&mut app, frame_msg("B", Direction::In, authorize("1")));
    update(
        &mut app,
        frame_msg(
            "A",
            Direction::Out,
            Frame::call_result("1", json!({"idTagInfo": {"status": "Accepted"}})),
        ),
    );
    assert_eq!((app.frames_in, app.frames_out), (2, 1));
    assert_eq!(app.station_log.len(), 2);
    assert_eq!(app.all_log.len(), 3);
    // Message ids collide across stations; the correlation must not mix them.
    app.view = LogView::All;
    let all = app.all_log.visible();
    assert_eq!(all[0].peer, Some(2));
    assert_eq!(all[1].peer, None);
    assert_eq!(app.entry_station(all[1]), Some("B"));
}

#[test]
fn results_carry_the_action_of_their_call_so_search_finds_them() {
    let (mut app, _) = app_with(&["A"]);
    update(&mut app, frame_msg("A", Direction::In, authorize("1")));
    update(
        &mut app,
        frame_msg(
            "A",
            Direction::Out,
            Frame::call_result("1", json!({"x": 1})),
        ),
    );
    app.station_log.search = "authorize".to_owned();
    assert_eq!(app.station_log.visible_count(), 2);
    assert_eq!(app.station_log.visible()[1].rtt_ms, Some(0));
}

#[test]
fn history_load_replaces_the_log_and_drops_duplicates_of_its_tail() {
    let (mut app, _) = app_with(&["A"]);
    let entry = FrameEntry {
        ts: at(1),
        direction: Direction::In,
        frame: authorize("h1"),
    };
    update(
        &mut app,
        Msg::Loaded {
            id: "A".to_owned(),
            history: vec![entry],
            info: None,
            state: None,
        },
    );
    assert_eq!(app.station_log.len(), 1);
    // The same frame arrives live (it was queued before the history was read).
    update(&mut app, frame_msg("A", Direction::In, authorize("h1")));
    assert_eq!(app.station_log.len(), 1);
    assert_eq!(app.all_log.len(), 1, "the all-stations log is live only");
    update(&mut app, frame_msg("A", Direction::In, authorize("h2")));
    assert_eq!(app.station_log.len(), 2);
    // A load for a station that is no longer selected is ignored.
    update(
        &mut app,
        Msg::Loaded {
            id: "Z".to_owned(),
            history: vec![],
            info: None,
            state: None,
        },
    );
    assert_eq!(app.station_log.len(), 2);
}

#[test]
fn state_changes_are_coalesced_into_one_refresh_per_tick() {
    let (mut app, _) = app_with(&["A", "B"]);
    for station in ["B", "A", "B"] {
        let fx = update(
            &mut app,
            Msg::Event(CsmsEvent::StateChanged {
                station: station.to_owned(),
            }),
        );
        assert!(fx.is_empty(), "no engine access per event");
    }
    let fx = update(&mut app, Msg::Tick(at(1)));
    assert_eq!(
        fx,
        [Effect::Refresh {
            states: vec!["A".to_owned(), "B".to_owned()],
            info: None
        }]
    );
    assert!(update(&mut app, Msg::Tick(at(2))).is_empty());
}

#[test]
fn inbound_frames_of_the_selected_station_refresh_its_info_on_the_next_tick() {
    let (mut app, _) = app_with(&["A", "B"]);
    update(&mut app, frame_msg("B", Direction::In, authorize("1")));
    assert!(update(&mut app, Msg::Tick(at(1))).is_empty());
    update(&mut app, frame_msg("A", Direction::In, authorize("2")));
    assert_eq!(
        update(&mut app, Msg::Tick(at(2))),
        [Effect::Refresh {
            states: vec![],
            info: Some("A".to_owned())
        }]
    );
}

#[test]
fn connection_events_update_flags_and_lines() {
    let (mut app, _) = app_with(&["A"]);
    update(
        &mut app,
        Msg::Event(CsmsEvent::Disconnected {
            station: "A".to_owned(),
            reason: DisconnectReason::Replaced,
        }),
    );
    assert!(!app.stations[0].connected && app.stations[0].replaced);
    update(
        &mut app,
        Msg::Event(CsmsEvent::Connected {
            station: "A".to_owned(),
        }),
    );
    assert!(app.stations[0].connected && app.stations[0].replaced);
    update(
        &mut app,
        Msg::Event(CsmsEvent::Disconnected {
            station: "A".to_owned(),
            reason: DisconnectReason::Closed,
        }),
    );
    assert!(!app.stations[0].replaced);
    assert_eq!(app.station_log.len(), 3);
    // A station we never saw appears on Connected.
    let fx = update(
        &mut app,
        Msg::Event(CsmsEvent::Connected {
            station: "0".to_owned(),
        }),
    );
    assert!(fx.is_empty());
    assert_eq!(app.stations[0].id, "0");
    assert_eq!(
        app.selected.as_deref(),
        Some("A"),
        "the selection does not jump"
    );
}

#[test]
fn forgetting_the_selected_station_selects_a_neighbour() {
    let (mut app, _) = app_with(&["A", "B"]);
    let fx = update(
        &mut app,
        Msg::Event(CsmsEvent::Forgotten {
            station: "A".to_owned(),
        }),
    );
    assert_eq!(app.selected.as_deref(), Some("B"));
    assert_eq!(fx, [Effect::Load("B".to_owned())]);
    let fx = update(
        &mut app,
        Msg::Event(CsmsEvent::Forgotten {
            station: "B".to_owned(),
        }),
    );
    assert!(fx.is_empty());
    assert_eq!(app.selected, None);
}

#[test]
fn lag_resyncs_and_says_so() {
    let (mut app, _) = app_with(&["A"]);
    let fx = update(&mut app, Msg::Lagged(7));
    assert_eq!(fx, [Effect::Resync]);
    assert!(app.notice.as_ref().unwrap().text.contains('7'));
    // The resync reselects the same station and reloads it.
    let fx = update(
        &mut app,
        Msg::Resynced {
            infos: vec![info("A", true), info("B", true)],
            states: vec![],
        },
    );
    assert_eq!(fx, [Effect::Load("A".to_owned())]);
    assert_eq!(app.stations.len(), 2);
}

#[test]
fn quit_help_view_and_pane_keys() {
    let (mut app, _) = app_with(&["A"]);
    press(&mut app, KeyCode::Char('?'));
    assert!(matches!(app.mode, Mode::Help));
    press(&mut app, KeyCode::Char('x'));
    assert!(matches!(app.mode, Mode::Help));
    press(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode::Normal));
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(app.view, LogView::All);
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(app.view, LogView::Station);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.focus, Focus::Log);
    press(&mut app, KeyCode::Left);
    assert_eq!(app.focus, Focus::Stations);
    press(&mut app, KeyCode::Char('q'));
    assert!(app.quit);
    let (mut app, _) = app_with(&["A"]);
    update(
        &mut app,
        Msg::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
    );
    assert!(app.quit);
}

#[test]
fn log_pane_keys_detail_copy_filter_pause() {
    let (mut app, _) = app_with(&["A"]);
    update(&mut app, frame_msg("A", Direction::In, authorize("1")));
    update(
        &mut app,
        frame_msg(
            "A",
            Direction::Out,
            Frame::call_result("1", json!({"status": "Accepted"})),
        ),
    );
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Enter);
    let Mode::Detail(d) = &app.mode else {
        panic!("detail open");
    };
    assert!(d.lines.iter().any(|l| l.contains("--- request ---")));
    assert!(d.lines.iter().any(|l| l.contains("idTag")));
    let fx = press(&mut app, KeyCode::Char('c'));
    assert!(matches!(fx.as_slice(), [Effect::Copy { .. }]));
    press(&mut app, KeyCode::Esc);
    let fx = press(&mut app, KeyCode::Char('y'));
    assert!(matches!(fx.as_slice(), [Effect::Copy { what, .. }] if what == "row"));
    let fx = press(&mut app, KeyCode::Char('Y'));
    assert!(matches!(&fx[..], [Effect::Copy { text, .. }] if text.lines().count() == 2));
    press(&mut app, KeyCode::Char('p'));
    assert!(!app.station_log.follow);
    press(&mut app, KeyCode::Char('p'));
    assert!(app.station_log.follow);
    press(&mut app, KeyCode::Char('f'));
    assert_eq!(
        app.station_log.filter,
        kirby_ocpp_tui::logview::LogFilter::Calls
    );
    press(&mut app, KeyCode::Char('s'));
    type_text(&mut app, "accepted");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.station_log.visible_count(), 1);
    press(&mut app, KeyCode::Esc);
    assert!(app.station_log.search.is_empty());
}

#[test]
fn station_switch_keeps_the_log_filter() {
    let (mut app, _) = app_with(&["A", "B"]);
    press(&mut app, KeyCode::Char('f'));
    press(&mut app, KeyCode::Down);
    assert_eq!(
        app.station_log.filter,
        kirby_ocpp_tui::logview::LogFilter::Calls
    );
    assert!(app.station_log.is_empty());
}

// ---- run loop with a real engine and a real station ---------------------------------------

#[tokio::test]
async fn run_loop_shows_a_connecting_station() {
    let csms = CsmsServer::start(CsmsConfig::new("127.0.0.1:0".parse().unwrap()))
        .await
        .expect("engine starts");

    let dir = tempfile::tempdir().unwrap();
    let mut config = StationConfig::new(format!("{}/ocpp", csms.url()))
        .with_charge_point_id("CP-TUI")
        .with_connectors(2)
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

    // Keys come from a channel; the observer presses `q` once the model has what we wait for.
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<std::io::Result<TermEvent>>();
    let keys = Box::pin(futures_util::stream::poll_fn(move |cx| {
        key_rx.poll_recv(cx)
    }));
    let quit = key_tx.clone();
    let mut sent = false;
    let observe = move |app: &App| {
        let booted = app
            .state
            .as_ref()
            .and_then(|s| s.boot.as_ref())
            .is_some_and(|b| b.registration == Some(RegistrationStatus::Accepted));
        if !sent && booted && app.station_log.len() >= 2 && app.info.is_some() {
            sent = true;
            let _ = quit.send(Ok(TermEvent::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
            ))));
        }
    };

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let options = TuiOptions {
        tick_rate: Duration::from_millis(10),
    };
    station.connect().await.expect("connect");
    let result = tokio::time::timeout(
        Duration::from_secs(10),
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
    .expect("the UI saw the booted station in time");
    result.expect("loop ends cleanly");

    let screen = crate::ui::tests::screen(terminal.backend());
    assert!(screen.contains("CP-TUI"), "{screen}");
    assert!(screen.contains("BootNotification"), "{screen}");
    assert!(screen.contains("1/1 connected"), "{screen}");

    drop(key_tx);
    station.shutdown().await.expect("station shutdown");
    join.await.expect("actor ends");
    csms.shutdown().await;
}

#[tokio::test]
async fn run_loop_ends_when_the_key_stream_ends() {
    let csms = CsmsServer::start(CsmsConfig::new("127.0.0.1:0".parse().unwrap()))
        .await
        .expect("engine starts");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let keys = futures_util::stream::empty::<std::io::Result<TermEvent>>().boxed();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        drive(
            &mut terminal,
            &csms,
            &TuiOptions::default(),
            keys,
            std::future::pending::<()>(),
            |_| {},
        ),
    )
    .await
    .expect("ends");
    result.expect("clean");
    csms.shutdown().await;
}
