//! Drives the UI model against a real station and the mock CSMS, executing its
//! effects the way the event loop does: boot -> plug -> authorize -> start -> stop.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use kirby_ocpp_core::ocpp::types::RegistrationStatus;
use kirby_ocpp_core::station::{Command, Event, StationHandle, TransactionState};
use kirby_ocpp_core::ws::ConnectionState;
use kirby_ocpp_core::{Station, StationConfig, SystemClock};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms};
use kirby_ocpp_tui::app::{App, Effect, Mode, Msg, update};
use kirby_ocpp_tui::logview::FrameKind;
use tokio::sync::broadcast;
use tokio::time::timeout;

const STEP: Duration = Duration::from_secs(5);

struct Driver {
    app: App,
    handle: StationHandle,
    events: broadcast::Receiver<Event>,
}

impl Driver {
    fn feed_snapshot(&mut self) {
        update(
            &mut self.app,
            Msg::Snapshot(Box::new(self.handle.snapshot())),
        );
    }

    fn drain(&mut self) {
        while let Ok(e) = self.events.try_recv() {
            update(&mut self.app, Msg::Event(e));
        }
        self.feed_snapshot();
    }

    /// Waits (bounded) for the next station event and folds it in.
    async fn until(&mut self, what: &str, done: impl Fn(&App) -> bool) {
        timeout(STEP, async {
            loop {
                self.drain();
                if done(&self.app) {
                    return;
                }
                if let Ok(e) = self.events.recv().await {
                    update(&mut self.app, Msg::Event(e));
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    async fn run(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            if let Effect::Send {
                label,
                command,
                show_response,
            } = effect
            {
                let result = timeout(STEP, self.handle.send(command))
                    .await
                    .expect("command completes")
                    .map_err(|e| e.to_string());
                update(
                    &mut self.app,
                    Msg::CommandDone {
                        label,
                        show_response,
                        result,
                    },
                );
            }
        }
        self.drain();
    }

    async fn key(&mut self, code: KeyCode) {
        let msg = Msg::Key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        ));
        let fx = update(&mut self.app, msg);
        self.run(fx).await;
    }

    async fn keys(&mut self, s: &str) {
        for c in s.chars() {
            self.key(KeyCode::Char(c)).await;
        }
    }
}

#[tokio::test]
async fn boot_plug_authorize_start_stop_are_reflected_in_the_model() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let config = StationConfig::new(csms.charge_point_url("CP-TUI"))
        .with_connectors(2)
        .without_persistence();
    let (handle, join) = Station::spawn(config, Arc::new(SystemClock));
    let mut d = Driver {
        app: App::new(handle.snapshot(), "ws://mock", "Acme"),
        events: handle.subscribe(),
        handle: handle.clone(),
    };

    // Like the CLI: connect at startup.
    handle.connect().await.unwrap();
    d.until("connection", |a| {
        a.snapshot.connection == ConnectionState::Connected
    })
    .await;

    // Station tab: boot.
    d.keys("1b").await;
    d.until("registration", |a| {
        a.snapshot.registration == Some(RegistrationStatus::Accepted)
    })
    .await;

    // Connector tab: plug in connector 1.
    d.keys("2p").await;
    assert!(d.app.snapshot.connectors[0].ev_connected);

    // Transaction tab: authorize.
    d.keys("3a").await;
    assert!(matches!(d.app.mode, Mode::Form(_)));
    d.keys("TAG1").await;
    d.key(KeyCode::Enter).await;
    assert!(matches!(d.app.mode, Mode::Normal));
    assert!(
        d.app.notice.as_ref().unwrap().text.contains("accepted"),
        "{:?}",
        d.app.notice
    );
    d.until("Authorize answer in the log", |a| {
        a.log.visible().iter().any(|e| {
            e.kind == Some(FrameKind::Result)
                && e.frame().and_then(|f| f.action.as_deref()) == Some("Authorize")
        })
    })
    .await;
    let answer = d
        .app
        .log
        .visible()
        .into_iter()
        .find(|e| {
            e.kind == Some(FrameKind::Result)
                && e.frame().and_then(|f| f.action.as_deref()) == Some("Authorize")
        })
        .unwrap();
    assert!(answer.rtt_ms.is_some(), "round trip is correlated");

    // Start a transaction on the selected connector.
    d.keys("t").await;
    d.key(KeyCode::Down).await;
    d.keys("TAG1").await;
    d.key(KeyCode::Enter).await;
    d.until("active transaction", |a| {
        matches!(
            a.snapshot.connectors[0].transaction,
            TransactionState::Active(_)
        )
    })
    .await;
    assert!(d.app.notice.as_ref().unwrap().text.contains("started"));

    // Meter values advance the meter.
    d.keys("m").await;
    assert!(d.app.snapshot.connectors[0].meter_wh >= 120);

    // Stop with the first reason.
    d.keys("e").await;
    assert!(matches!(d.app.mode, Mode::Choice(_)));
    d.key(KeyCode::Enter).await;
    d.until("idle connector", |a| {
        a.snapshot.connectors[0].transaction.is_idle()
    })
    .await;

    // The CSMS saw what the UI sent.
    let calls: Vec<String> = csms.received().iter().map(|r| r.action.clone()).collect();
    for expected in [
        "BootNotification",
        "Authorize",
        "StartTransaction",
        "MeterValues",
        "StopTransaction",
    ] {
        assert!(
            calls.iter().any(|c| c == expected),
            "{expected} missing in {calls:?}"
        );
    }

    // The raw composer sends a schema-validated Heartbeat and shows the response.
    d.keys("5r").await;
    d.key(KeyCode::Enter).await;
    let Mode::Detail(detail) = &d.app.mode else {
        panic!("{:?}", d.app.mode)
    };
    assert!(detail.lines.join("\n").contains("currentTime"));

    d.keys("q").await;
    // `q` is swallowed by the detail view first, then quits.
    d.key(KeyCode::Esc).await;
    d.keys("q").await;
    assert!(d.app.quit);

    let _ = handle.send(Command::Shutdown).await;
    join.await.unwrap();
    csms.shutdown().await;
}

#[tokio::test]
async fn data_browser_edits_reach_the_station_and_come_back_in_the_snapshot() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let config = StationConfig::new(csms.charge_point_url("CP-DATA"))
        .with_connectors(2)
        .without_persistence();
    let (handle, join) = Station::spawn(config, Arc::new(SystemClock));
    let mut d = Driver {
        app: App::new(handle.snapshot(), "ws://mock", "Acme"),
        events: handle.subscribe(),
        handle: handle.clone(),
    };
    d.drain();

    // Config key: set MeterValueSampleInterval to 45.
    d.keys("K/SampleInterval").await;
    d.key(KeyCode::Enter).await;
    d.key(KeyCode::Enter).await;
    assert!(matches!(d.app.mode, Mode::Form(_)), "{}", d.app.mode.name());
    for _ in 0..8 {
        d.key(KeyCode::Backspace).await;
    }
    d.keys("45").await;
    d.key(KeyCode::Enter).await;
    d.until("new value in the snapshot", |a| {
        a.snapshot
            .config
            .iter()
            .any(|i| i.key == "MeterValueSampleInterval" && i.value == "45")
    })
    .await;
    assert!(matches!(d.app.mode, Mode::Browser(_)));

    // Local list: add an entry.
    d.key(KeyCode::Tab).await;
    d.keys("aKIRBY-OCPP").await;
    d.key(KeyCode::Enter).await;
    d.until("entry in the snapshot", |a| {
        a.snapshot
            .local_list
            .entries
            .iter()
            .any(|e| e.id_tag == "KIRBY-OCPP")
    })
    .await;
    assert_eq!(d.app.snapshot.local_list.version, 1);

    // Charging profile: install the template, then clear all.
    d.key(KeyCode::Tab).await;
    d.keys("i").await;
    d.key(KeyCode::Enter).await;
    d.until("profile in the snapshot", |a| {
        !a.snapshot.charging_profiles.is_empty()
    })
    .await;
    d.keys("cy").await;
    d.until("no profiles", |a| a.snapshot.charging_profiles.is_empty())
        .await;

    handle.shutdown().await.unwrap();
    join.await.unwrap();
    csms.shutdown().await;
}
