//! Drives the OCPP 2.x UI model against a real `Station2` and the mock CSMS, executing its
//! effects the way the event loop does: boot -> plug -> present token -> meter -> stop, a
//! device-model edit and a scenario run.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::v2::messages::RegistrationStatus;
use kirby_ocpp_core::scenario::{CancelToken, run_scenario};
use kirby_ocpp_core::station::Event;
use kirby_ocpp_core::ws::ConnectionState;
use kirby_ocpp_core::{Station2, Station2Handle, StationConfig, SystemClock};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms, SubprotocolPolicy};
use kirby_ocpp_tui::logview::FrameKind;
use kirby_ocpp_tui::v2::{App2, Effect2, Mode2, Msg2, Tab2, draw2, update2};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::broadcast;
use tokio::time::timeout;

const STEP: Duration = Duration::from_secs(8);

struct Driver {
    app: App2,
    handle: Station2Handle,
    events: broadcast::Receiver<Event>,
}

impl Driver {
    fn feed_snapshot(&mut self) {
        update2(
            &mut self.app,
            Msg2::Snapshot(Box::new(self.handle.snapshot())),
        );
    }

    fn drain(&mut self) {
        while let Ok(e) = self.events.try_recv() {
            update2(&mut self.app, Msg2::Event(e));
        }
        self.feed_snapshot();
    }

    /// Waits (bounded) for the model to satisfy `done`, folding in station events.
    async fn until(&mut self, what: &str, done: impl Fn(&App2) -> bool) {
        timeout(STEP, async {
            loop {
                self.drain();
                if done(&self.app) {
                    return;
                }
                if let Ok(e) = self.events.recv().await {
                    update2(&mut self.app, Msg2::Event(e));
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    async fn run(&mut self, effects: Vec<Effect2>) {
        for effect in effects {
            match effect {
                Effect2::Send {
                    label,
                    commands,
                    show_response,
                } => {
                    let mut result = Ok(kirby_ocpp_core::Outcome2::Done);
                    for command in commands {
                        match timeout(STEP, self.handle.send(command))
                            .await
                            .expect("command completes")
                        {
                            Ok(outcome) => result = Ok(outcome),
                            Err(e) => {
                                result = Err(e.to_string());
                                break;
                            }
                        }
                    }
                    update2(
                        &mut self.app,
                        Msg2::CommandDone {
                            label,
                            show_response,
                            result,
                        },
                    );
                }
                Effect2::RunScenario(scenario) => {
                    let report = timeout(
                        Duration::from_secs(60),
                        run_scenario(&self.handle, &scenario, &CancelToken::new(), None),
                    )
                    .await
                    .expect("the scenario ends");
                    update2(&mut self.app, Msg2::ScenarioDone(Box::new(report)));
                }
                other => panic!("effect not expected in this flow: {other:?}"),
            }
        }
        self.drain();
    }

    async fn key(&mut self, code: KeyCode) {
        let msg = Msg2::Key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        ));
        let fx = update2(&mut self.app, msg);
        self.run(fx).await;
    }

    async fn keys(&mut self, s: &str) {
        for c in s.chars() {
            self.key(KeyCode::Char(c)).await;
        }
    }

    fn screen(&self, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw2(f, &self.app)).unwrap();
        let buf = term.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

async fn v201_csms() -> MockCsms {
    MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Require("ocpp2.0.1".to_owned()),
        ..CsmsOptions::default()
    })
    .await
    .expect("mock CSMS starts")
}

/// A connected, booted station and the driver on top of it.
async fn booted(csms: &MockCsms) -> Driver {
    let mut config = StationConfig::new(csms.url())
        .with_connectors(2)
        .with_call_timeout_ms(2_000)
        .with_heartbeat_interval_secs(30)
        .with_reconnect(ReconnectConfig {
            enabled: true,
            initial_delay_ms: 30,
            max_delay_ms: 100,
            max_attempts: 0,
        })
        .without_persistence();
    config.ws_ping_interval_secs = 0;
    Protocol::V201
        .token()
        .clone_into(&mut config.ws_subprotocol);
    config.protocol = Protocol::V201;
    let (handle, _join) = Station2::spawn(config, Arc::new(SystemClock));
    let mut d = Driver {
        app: App2::new(handle.snapshot(), "ws://mock", "Acme"),
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
    d
}

fn transaction_event_types(d: &Driver) -> Vec<String> {
    d.app
        .log
        .visible()
        .iter()
        .filter(|e| e.kind == Some(FrameKind::Call))
        .filter_map(|e| e.frame())
        .filter(|f| f.action.as_deref() == Some("TransactionEvent"))
        .filter_map(|f| {
            f.frame
                .get(3)?
                .get("eventType")?
                .as_str()
                .map(str::to_owned)
        })
        .collect()
}

#[tokio::test]
async fn boot_plug_present_token_meter_stop_are_reflected_in_the_model() {
    let csms = v201_csms().await;
    let mut d = booted(&csms).await;
    assert!(d.app.snapshot.booted());
    assert!(d.screen(110, 30).contains("ocpp2.0.1"));

    // EVSE tab: plug in the selected EVSE.
    d.keys("2p").await;
    d.until("EV connected", |a| {
        a.selected_evse()
            .is_some_and(|e| e.connectors.iter().any(|c| c.ev_connected))
    })
    .await;
    assert!(d.screen(110, 30).contains("Occupied"));

    // Transaction tab: present a token (the target EVSE is the selected one).
    d.keys("3t").await;
    assert!(matches!(d.app.mode, Mode2::Form(_)));
    d.key(KeyCode::Down).await;
    d.keys("TAG1").await;
    d.key(KeyCode::Enter).await;
    assert!(matches!(d.app.mode, Mode2::Normal), "{:?}", d.app.mode);
    d.until("a running transaction", |a| {
        a.selected_evse().is_some_and(|e| e.transaction.is_some())
    })
    .await;
    let tx = d
        .app
        .selected_evse()
        .and_then(|e| e.transaction.clone())
        .unwrap();
    assert_eq!(tx.transaction_id.len(), 36, "{tx:?}");
    assert_eq!(tx.id_token.as_deref(), Some("TAG1"));
    let screen = d.screen(110, 30);
    assert!(screen.contains(&tx.transaction_id), "{screen}");
    assert!(screen.contains("TAG1"), "{screen}");
    assert!(screen.contains(&tx.transaction_id[..8]));

    // Meter: +120 Wh and a sample.
    d.keys("m").await;
    d.until("the meter to advance", |a| {
        a.selected_evse().is_some_and(|e| e.meter_wh >= 120)
    })
    .await;

    // Stop with the first reason of the list.
    d.keys("e").await;
    assert!(matches!(d.app.mode, Mode2::Choice(_)));
    d.key(KeyCode::Enter).await;
    assert!(matches!(d.app.mode, Mode2::Normal));
    d.until("the transaction to end", |a| {
        a.selected_evse().is_some_and(|e| e.transaction.is_none())
    })
    .await;
    d.until("the Ended event in the log", |a| {
        a.log
            .visible()
            .iter()
            .filter(|e| e.kind == Some(FrameKind::Call))
            .filter_map(|e| e.frame())
            .any(|f| {
                f.action.as_deref() == Some("TransactionEvent")
                    && f.frame
                        .get(3)
                        .and_then(|p| p.get("eventType"))
                        .and_then(|t| t.as_str())
                        == Some("Ended")
            })
    })
    .await;
    let types = transaction_event_types(&d);
    assert!(types.iter().any(|t| t == "Started"), "{types:?}");
    assert!(types.iter().any(|t| t == "Ended"), "{types:?}");

    // The CSMS saw what the UI sent.
    let calls: Vec<String> = csms.calls_since(0).into_iter().map(|c| c.action).collect();
    for expected in ["BootNotification", "TransactionEvent"] {
        assert!(
            calls.iter().any(|c| c == expected),
            "{expected} missing in {calls:?}"
        );
    }
    let screen = d.screen(110, 30);
    for word in ["Preparing", "Finishing", "idTag"] {
        assert!(!screen.contains(word), "{word} on screen:\n{screen}");
    }
}

#[tokio::test]
async fn a_device_model_edit_comes_back_in_the_snapshot() {
    let csms = v201_csms().await;
    let mut d = booted(&csms).await;

    // Station tab: open the browser, filter, edit the heartbeat interval.
    d.keys("K").await;
    assert!(matches!(d.app.mode, Mode2::DeviceModel(_)));
    let screen = d.screen(110, 30);
    assert!(screen.contains("HeartbeatInterval"), "{screen}");
    d.keys("/HeartbeatInterval").await;
    d.key(KeyCode::Enter).await;
    d.key(KeyCode::Enter).await;
    let Mode2::Form(form) = &d.app.mode else {
        panic!("the editor did not open: {:?}", d.app.mode);
    };
    assert!(form.title.contains("HeartbeatInterval"), "{}", form.title);
    // Component, instance and variable are prefilled; the value is the last field.
    for _ in 0..3 {
        d.key(KeyCode::Tab).await;
    }
    for _ in 0..10 {
        d.key(KeyCode::Backspace).await;
    }
    d.keys("45").await;
    d.key(KeyCode::Enter).await;
    d.until("the new value in the snapshot", |a| {
        a.snapshot
            .variables
            .iter()
            .any(|v| v.variable == "HeartbeatInterval" && v.value.as_deref() == Some("45"))
    })
    .await;
    // The browser is back (it was suspended behind the form).
    assert!(
        matches!(d.app.mode, Mode2::DeviceModel(_)),
        "{:?}",
        d.app.mode
    );
    d.key(KeyCode::Esc).await;
    assert!(matches!(d.app.mode, Mode2::Normal), "{:?}", d.app.mode);
}

#[tokio::test]
async fn a_scenario_run_from_the_scenarios_tab_reports_ocpp_2_0_1() {
    let csms = v201_csms().await;
    let mut d = booted(&csms).await;
    d.app.set_tab(Tab2::Scenarios);
    d.keys("o").await;
    let report = d
        .app
        .scenario
        .report
        .clone()
        .expect("the scenario produced a report");
    assert_eq!(report.ocpp_version, "2.0.1");
    assert!(!d.app.scenario.running);
    assert!(d.screen(110, 30).contains("scenario"));
}
