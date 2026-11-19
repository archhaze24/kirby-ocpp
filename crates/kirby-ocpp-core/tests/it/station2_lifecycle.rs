//! Connection, boot, registration and heartbeat of the OCPP 2.0.1 station.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use chrono::Utc;
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::v2::messages::RegistrationStatus;
use kirby_ocpp_core::recording::{RecordOptions, Role};
use kirby_ocpp_core::station::Event;
use kirby_ocpp_core::{
    CallError, Command2, ManualClock, Outcome2, Station2, Station2Handle, StationConfig,
    StationError,
};
use kirby_ocpp_testkit::{ErrorCode, Frame, Reply};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::station2_support::{STEP, calls_of, spawn_v201, v201_csms, wait_booted, wait_snapshot};

fn boot_reply(status: &str, interval: u32) -> Reply {
    Reply::Result(json!({
        "status": status,
        "currentTime": "2026-03-01T12:00:00Z",
        "interval": interval,
    }))
}

/// A station configured like `spawn_v201`, with `tweak` applied. It does not connect by itself.
fn spawn_with(
    url: &str,
    tweak: impl FnOnce(&mut StationConfig),
    recording: Option<kirby_ocpp_core::recording::RecordSink>,
) -> (Station2Handle, JoinHandle<()>) {
    let mut config = StationConfig::new(url)
        .with_connectors(2)
        .with_call_timeout_ms(2_000)
        .with_heartbeat_interval_secs(1)
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
    tweak(&mut config);
    Station2::spawn_recorded(config, Arc::new(ManualClock::new(Utc::now())), recording)
}

/// Stops the station and waits until its task has ended.
async fn stop(station: &Station2Handle, task: JoinHandle<()>) {
    station.shutdown().await.expect("the station stops");
    timeout(STEP, task)
        .await
        .expect("the station task ends in time")
        .expect("the station task does not panic");
}

/// A URL nothing listens on. The port stays bound (without `listen`) while the guard lives, so
/// connections are refused and a parallel test cannot take the port over.
fn dead_url() -> (String, tokio::net::TcpSocket) {
    let socket = tokio::net::TcpSocket::new_v4().expect("a socket");
    socket
        .bind("127.0.0.1:0".parse().expect("an address"))
        .expect("a free port");
    let addr = socket.local_addr().expect("a local address");
    (format!("ws://{addr}/ocpp"), socket)
}

/// Waits for a log line containing `needle`.
async fn wait_log(events: &mut broadcast::Receiver<Event>, needle: &str) {
    let found = timeout(STEP, async {
        loop {
            match events.recv().await {
                Ok(Event::Log(record)) if record.text.contains(needle) => return true,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return false,
            }
        }
    })
    .await;
    assert_eq!(found, Ok(true), "no log line containing `{needle}`");
}

#[tokio::test]
async fn boot_accepted_sets_state() {
    let csms = v201_csms().await;
    let (station, task) = spawn_with(
        csms.url(),
        |config| {
            config.serial_number = Some("SN-42".into());
            config.firmware_version = Some("1.2.3".into());
        },
        None,
    );
    assert_eq!(station.snapshot().registration, None);
    station.connect().await.unwrap();
    let snapshot = wait_booted(&station).await;
    assert_eq!(snapshot.registration, Some(RegistrationStatus::Accepted));
    assert_eq!(
        snapshot.connection,
        kirby_ocpp_core::ws::ConnectionState::Connected
    );
    assert_eq!(
        snapshot.boot_reason.map(|reason| format!("{reason:?}")),
        Some("PowerUp".to_owned())
    );
    // The mock answers with an interval of 5 seconds, which replaces the configured 1.
    assert_eq!(snapshot.heartbeat_interval_secs, 5);

    let boots = calls_of(&csms, "BootNotification");
    assert_eq!(boots.len(), 1);
    assert_eq!(boots[0]["reason"], "PowerUp");
    let station_info = &boots[0]["chargingStation"];
    assert_eq!(station_info["vendorName"], station.config().vendor);
    assert_eq!(station_info["model"], station.config().model);
    assert_eq!(station_info["serialNumber"], "SN-42");
    assert_eq!(station_info["firmwareVersion"], "1.2.3");
    // The boot is the first message of the session.
    assert_eq!(csms.received()[0].action, "BootNotification");
    assert_eq!(csms.received()[0].subprotocol.as_deref(), Some("ocpp2.0.1"));
    stop(&station, task).await;
}

#[tokio::test]
async fn optional_boot_fields_are_omitted_when_not_configured() {
    let csms = v201_csms().await;
    let (station, task) = spawn_v201(&csms);
    station.connect().await.unwrap();
    wait_booted(&station).await;
    let boots = calls_of(&csms, "BootNotification");
    let station_info = boots[0]["chargingStation"].as_object().unwrap();
    assert!(!station_info.contains_key("serialNumber"));
    assert!(!station_info.contains_key("firmwareVersion"));
    assert!(!station_info.contains_key("modem"));
    stop(&station, task).await;
}

#[tokio::test]
async fn heartbeats_repeat_at_the_booted_interval() {
    let csms = v201_csms().await;
    csms.respond("BootNotification", |_| boot_reply("Accepted", 1));
    let (station, task) = spawn_v201(&csms);
    station.connect().await.unwrap();
    wait_booted(&station).await;
    csms.wait_for_call_count_after(0, "Heartbeat", 3, |_| true, STEP)
        .await
        .unwrap();
    let times: Vec<_> = csms
        .received()
        .into_iter()
        .filter(|call| call.action == "Heartbeat")
        .map(|call| call.ts)
        .collect();
    assert!(times.windows(2).all(|pair| pair[0] <= pair[1]));
    wait_snapshot(&station, "a heartbeat time", |s| {
        s.last_heartbeat_at.is_some()
    })
    .await;
    assert_eq!(station.snapshot().heartbeat_interval_secs, 1);
    stop(&station, task).await;
}

#[tokio::test]
async fn an_accepted_boot_sends_the_statuses_before_the_first_heartbeat() {
    let csms = v201_csms().await;
    csms.respond("BootNotification", |_| boot_reply("Accepted", 1));
    let (station, task) = spawn_v201(&csms);
    station.connect().await.unwrap();
    csms.wait_for_call("Heartbeat", STEP).await.unwrap();
    let actions: Vec<String> = csms
        .received()
        .into_iter()
        .map(|call| call.action)
        .collect();
    let first_heartbeat = actions
        .iter()
        .position(|action| action == "Heartbeat")
        .expect("a heartbeat was sent");
    // B01.FR.05: the statuses follow the boot, the heartbeat timer starts after them. The
    // security event of the startup may sit between them (A04.FR.01).
    assert_eq!(actions[0], "BootNotification");
    let between = &actions[1..first_heartbeat];
    assert!(!between.is_empty(), "{actions:?}");
    assert!(
        between
            .iter()
            .all(|action| action == "StatusNotification" || action == "SecurityEventNotification"),
        "{actions:?}"
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn heartbeat_interval_zero_keeps_the_configured_period() {
    let csms = v201_csms().await;
    csms.respond("BootNotification", |_| boot_reply("Accepted", 0));
    let (station, task) = spawn_v201(&csms);
    station.connect().await.unwrap();
    let snapshot = wait_booted(&station).await;
    // The configured heartbeat of one second stays; a zero period is never armed.
    assert_eq!(snapshot.heartbeat_interval_secs, 1);
    csms.wait_for_call("Heartbeat", STEP).await.unwrap();
    stop(&station, task).await;
}

#[tokio::test]
async fn cancel_heartbeat_and_refuse_invalid_cancellations() {
    let csms = v201_csms().await;
    csms.respond("Heartbeat", |_| Reply::Silence);
    let (station, task) = spawn_v201(&csms);
    station.connect().await.unwrap();
    wait_booted(&station).await;
    let sender = station.clone();
    let heartbeat = tokio::spawn(async move { sender.send(Command2::Heartbeat).await });
    csms.wait_for_call("Heartbeat", STEP).await.unwrap();
    station
        .send(Command2::CancelCall {
            action: "Heartbeat".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        timeout(STEP, heartbeat).await.unwrap().unwrap(),
        Err(StationError::Call(CallError::Cancelled { .. }))
    ));
    assert!(
        station
            .send(Command2::CancelCall {
                action: "Heartbeat".into()
            })
            .await
            .is_err()
    );
    assert!(
        station
            .send(Command2::CancelCall {
                action: "TransactionEvent".into()
            })
            .await
            .is_err()
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn heartbeat_command_returns_the_csms_time() {
    let csms = v201_csms().await;
    let (station, task) = spawn_v201(&csms);
    station.connect().await.unwrap();
    wait_booted(&station).await;
    let outcome = station.send(Command2::Heartbeat).await.unwrap();
    let Outcome2::Response { payload } = outcome else {
        panic!("expected a response, got {outcome:?}");
    };
    assert!(payload["currentTime"].is_string());
    assert!(station.snapshot().last_heartbeat_at.is_some());
    stop(&station, task).await;
}

#[tokio::test]
async fn pending_registration_retries_the_boot_and_gates_other_messages() {
    let accept = Arc::new(AtomicBool::new(false));
    let csms = v201_csms().await;
    let flag = Arc::clone(&accept);
    csms.respond("BootNotification", move |_| {
        if flag.load(Ordering::SeqCst) {
            boot_reply("Accepted", 5)
        } else {
            boot_reply("Pending", 1)
        }
    });
    let (station, task) = spawn_v201(&csms);
    station.connect().await.unwrap();
    wait_snapshot(&station, "a pending registration", |s| {
        s.registration == Some(RegistrationStatus::Pending)
    })
    .await;

    // User messages are refused while Pending ...
    let error = station.send(Command2::Heartbeat).await.unwrap_err();
    assert!(
        matches!(error, StationError::Call(CallError::NotRegistered { .. })),
        "{error:?}"
    );
    // ... except a raw CALL, which bypasses the gate.
    let outcome = station
        .send(Command2::SendRaw {
            action: "Heartbeat".to_owned(),
            payload: json!({}),
            validate: true,
        })
        .await
        .unwrap();
    assert!(matches!(outcome, Outcome2::Response { .. }));
    // The channel stays open (B02.FR.06).
    assert_eq!(csms.connections(), 1);
    assert_eq!(csms.open_connections(), 1);

    // The boot repeats after the interval of the answer (1 s); then the station registers.
    csms.wait_for_call_count_after(0, "BootNotification", 2, |_| true, STEP)
        .await
        .unwrap();
    accept.store(true, Ordering::SeqCst);
    wait_booted(&station).await;
    // Both boots carry the reason of the first one, and nothing but the raw heartbeat left
    // before the registration.
    let boots = calls_of(&csms, "BootNotification");
    assert!(boots.iter().all(|boot| boot["reason"] == "PowerUp"));
    let actions: Vec<String> = csms
        .received()
        .into_iter()
        .map(|call| call.action)
        .collect();
    let last_boot = actions
        .iter()
        .rposition(|action| action == "BootNotification")
        .expect("boots were sent");
    let others: Vec<_> = actions[..last_boot]
        .iter()
        .filter(|action| *action != "BootNotification")
        .collect();
    assert_eq!(others, ["Heartbeat"]);
    stop(&station, task).await;
}

#[tokio::test]
async fn rejected_registration_sends_nothing_but_boot_and_refuses_the_csms() {
    let accept = Arc::new(AtomicBool::new(false));
    let csms = v201_csms().await;
    let flag = Arc::clone(&accept);
    csms.respond("BootNotification", move |_| {
        if flag.load(Ordering::SeqCst) {
            boot_reply("Accepted", 5)
        } else {
            boot_reply("Rejected", 1)
        }
    });
    let (station, task) = spawn_v201(&csms);
    station.connect().await.unwrap();
    wait_snapshot(&station, "a rejected registration", |s| {
        s.registration == Some(RegistrationStatus::Rejected)
    })
    .await;

    // B03.FR.08: every CALL of the CSMS is answered with SecurityError.
    csms.expect_call_error(
        "GetVariables",
        json!({"getVariableData": [{
            "component": {"name": "OCPPCommCtrlr"},
            "variable": {"name": "HeartbeatInterval"},
        }]}),
        &ErrorCode::SecurityError,
    )
    .await
    .unwrap();
    // The retry arrives and nothing but boots left the station in the meantime.
    csms.wait_for_call_count_after(0, "BootNotification", 2, |_| true, STEP)
        .await
        .unwrap();
    assert!(
        csms.received()
            .iter()
            .all(|call| call.action == "BootNotification"),
        "{:?}",
        csms.received()
    );
    assert_eq!(station.snapshot().last_heartbeat_at, None);

    accept.store(true, Ordering::SeqCst);
    wait_booted(&station).await;
    // Accepted: the CSMS may talk to the station again.
    let frame = csms
        .send_call(
            "GetVariables",
            json!({"getVariableData": [{
                "component": {"name": "OCPPCommCtrlr"},
                "variable": {"name": "HeartbeatInterval"},
            }]}),
        )
        .await
        .unwrap();
    assert!(
        !matches!(
            frame,
            Frame::CallError {
                code: ErrorCode::SecurityError,
                ..
            }
        ),
        "{frame:?}"
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn a_failed_boot_call_is_retried_with_the_same_reason() {
    let calls = Arc::new(AtomicUsize::new(0));
    let csms = v201_csms().await;
    let counter = Arc::clone(&calls);
    csms.respond("BootNotification", move |_| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            Reply::Error(ErrorCode::InternalError, "first boot fails".into())
        } else {
            boot_reply("Accepted", 5)
        }
    });
    let (station, task) = spawn_v201(&csms);
    station.connect().await.unwrap();
    // The failed boot is retried after the heartbeat interval (1 s) and then succeeds.
    wait_booted(&station).await;
    let boots = calls_of(&csms, "BootNotification");
    assert_eq!(boots.len(), 2);
    assert_eq!(boots[0]["reason"], boots[1]["reason"]);
    stop(&station, task).await;
}

#[tokio::test]
async fn reconnect_keeps_the_registration_and_does_not_boot_again() {
    let csms = v201_csms().await;
    let (station, task) = spawn_v201(&csms);
    station.connect().await.unwrap();
    wait_booted(&station).await;
    let mark = csms.mark();

    csms.drop_connection().unwrap();
    csms.wait_for_connection_count(2, STEP).await.unwrap();
    // B04: the station resumes with a heartbeat, without a new boot.
    csms.wait_for_call_after(mark, "Heartbeat", |_| true, STEP)
        .await
        .unwrap();
    let snapshot = wait_booted(&station).await;
    assert_eq!(snapshot.registration, Some(RegistrationStatus::Accepted));
    let after: Vec<_> = csms
        .calls_since(mark)
        .into_iter()
        .map(|call| call.action)
        .collect();
    assert!(
        !after.iter().any(|action| action == "BootNotification"),
        "{after:?}"
    );
    assert_eq!(calls_of(&csms, "BootNotification").len(), 1);
    stop(&station, task).await;
}

#[tokio::test]
async fn boot_command_sends_the_unknown_reason() {
    let csms = v201_csms().await;
    let (station, task) = spawn_v201(&csms);
    // Not connected: nothing can be sent.
    assert!(matches!(
        station.send(Command2::Boot).await,
        Err(StationError::NotConnected)
    ));
    station.connect().await.unwrap();
    wait_booted(&station).await;
    let outcome = station.send(Command2::Boot).await.unwrap();
    let Outcome2::Response { payload } = outcome else {
        panic!("expected a response, got {outcome:?}");
    };
    assert_eq!(payload["status"], "Accepted");
    let boots = calls_of(&csms, "BootNotification");
    assert_eq!(boots.len(), 2);
    assert_eq!(boots[1]["reason"], "Unknown");
    stop(&station, task).await;
}

#[tokio::test]
async fn disconnect_clears_the_registration_and_the_next_connect_boots() {
    let csms = v201_csms().await;
    let (station, task) = spawn_v201(&csms);
    station.connect().await.unwrap();
    wait_booted(&station).await;
    station.disconnect().await.unwrap();
    let snapshot = wait_snapshot(&station, "a closed link", |s| s.registration.is_none()).await;
    assert_eq!(
        snapshot.connection,
        kirby_ocpp_core::ws::ConnectionState::Disconnected
    );
    csms.wait_for_disconnect(STEP).await.unwrap();
    // No automatic reconnect follows a deliberate disconnect.
    assert_eq!(csms.connections(), 1);

    station.connect().await.unwrap();
    wait_booted(&station).await;
    let boots = calls_of(&csms, "BootNotification");
    assert_eq!(boots.len(), 2);
    assert_eq!(boots[1]["reason"], "PowerUp");
    stop(&station, task).await;
}

#[tokio::test]
async fn backoff_doubles_and_stops_after_max_attempts() {
    let (url, _port) = dead_url();
    let (station, task) = spawn_with(
        &url,
        |config| {
            config.reconnect = ReconnectConfig {
                enabled: true,
                initial_delay_ms: 30,
                max_delay_ms: 100,
                max_attempts: 3,
            };
        },
        None,
    );
    let mut events = station.subscribe();
    station.connect().await.unwrap();
    wait_log(&mut events, "connection failed: ").await;
    wait_log(&mut events, "reconnect attempt 1/3 in 30ms").await;
    wait_log(&mut events, "reconnect attempt 2/3 in 60ms").await;
    wait_log(&mut events, "reconnect attempt 3/3 in 100ms").await; // 120 capped at 100
    wait_log(&mut events, "reconnect stopped").await;
    let snapshot = wait_snapshot(&station, "a stopped loop", |s| {
        s.reconnect_stopped.is_some()
    })
    .await;
    assert!(!snapshot.booted());
    assert_eq!(
        snapshot.reconnect_stopped.as_deref(),
        Some("after 3 failed attempt(s)")
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn disconnect_and_connect_reset_the_stopped_reason() {
    let (url, _port) = dead_url();
    let (station, task) = spawn_with(
        &url,
        |config| {
            config.reconnect = ReconnectConfig {
                enabled: true,
                initial_delay_ms: 20,
                max_delay_ms: 20,
                max_attempts: 2,
            };
        },
        None,
    );
    assert_eq!(station.snapshot().reconnect_stopped, None);
    station.connect().await.unwrap();
    wait_snapshot(&station, "a stopped loop", |s| {
        s.reconnect_stopped.is_some()
    })
    .await;
    // A manual connect restarts the loop, and a deliberate disconnect clears the reason.
    station.connect().await.unwrap();
    wait_snapshot(&station, "a loop stopped again", |s| {
        s.reconnect_stopped.is_some()
    })
    .await;
    station.disconnect().await.unwrap();
    wait_snapshot(&station, "a cleared reason", |s| {
        s.reconnect_stopped.is_none()
    })
    .await;
    stop(&station, task).await;
}

#[tokio::test]
async fn disabled_reconnect_sets_the_stopped_reason() {
    let csms = v201_csms().await;
    let (station, task) = spawn_with(csms.url(), |config| config.reconnect.enabled = false, None);
    station.connect().await.unwrap();
    wait_booted(&station).await;
    assert_eq!(station.snapshot().reconnect_stopped, None);
    csms.drop_connection().unwrap();
    let snapshot = wait_snapshot(&station, "a stopped loop", |s| {
        s.reconnect_stopped.is_some()
    })
    .await;
    assert_eq!(
        snapshot.reconnect_stopped.as_deref(),
        Some("automatic reconnect is disabled")
    );
    assert_eq!(
        snapshot.connection,
        kirby_ocpp_core::ws::ConnectionState::Disconnected
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn the_security_profile_is_derived_from_the_url() {
    let csms = v201_csms().await;
    let (plain, plain_task) = spawn_v201(&csms);
    assert_eq!(plain.snapshot().security_profile, 0);
    stop(&plain, plain_task).await;

    let url = csms.url().replacen("ws://", "ws://CP-1:secret@", 1);
    let (basic, basic_task) = spawn_with(&url, |_| {}, None);
    assert_eq!(basic.snapshot().security_profile, 1);
    stop(&basic, basic_task).await;
}

#[tokio::test]
async fn security_events_are_sent_once_registered() {
    let csms = v201_csms().await;
    let (station, task) = spawn_v201(&csms);
    station.connect().await.unwrap();
    wait_booted(&station).await;
    let outcome = station
        .send(Command2::SendSecurityEvent {
            event_type: "StartupOfDevice".to_owned(),
            tech_info: Some("test".to_owned()),
        })
        .await
        .unwrap();
    assert!(matches!(outcome, Outcome2::Response { .. }));
    // The station reports its own startup too; only the command's event is checked here.
    let events: Vec<Value> = calls_of(&csms, "SecurityEventNotification")
        .into_iter()
        .filter(|event| event["type"] == "StartupOfDevice")
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["techInfo"], "test");
    assert!(events[0]["timestamp"].is_string());
    stop(&station, task).await;
}

/// The `type` of every `SecurityEventNotification` the CSMS has recorded, oldest first.
fn security_event_types(csms: &kirby_ocpp_testkit::MockCsms) -> Vec<String> {
    calls_of(csms, "SecurityEventNotification")
        .into_iter()
        .map(|event| event["type"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[tokio::test]
async fn the_startup_is_reported_once_after_the_boot_was_accepted() {
    let csms = v201_csms().await;
    let (station, task) = spawn_v201(&csms);
    station.connect().await.unwrap();
    wait_booted(&station).await;
    let event = csms
        .wait_for_call("SecurityEventNotification", STEP)
        .await
        .unwrap();
    assert_eq!(event["type"], "StartupOfTheDevice");
    assert!(event["timestamp"].is_string());
    assert!(event.get("techInfo").is_none());
    let actions: Vec<String> = csms
        .received()
        .into_iter()
        .map(|call| call.action)
        .collect();
    let position = |name: &str| actions.iter().position(|action| action == name);
    assert!(
        position("BootNotification") < position("SecurityEventNotification"),
        "{actions:?}"
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn a_reconnect_without_a_reboot_does_not_report_the_startup_again() {
    let csms = v201_csms().await;
    let (station, task) = spawn_v201(&csms);
    station.connect().await.unwrap();
    wait_booted(&station).await;
    csms.wait_for_call("SecurityEventNotification", STEP)
        .await
        .unwrap();
    // The answer to the event reaches the station before the answer to the heartbeat; a link
    // that closes earlier would make the station send the event again.
    station.send(Command2::Heartbeat).await.unwrap();
    let mark = csms.mark();

    csms.drop_connection().unwrap();
    csms.wait_for_connection_count(2, STEP).await.unwrap();
    // The resume sends its security events before the heartbeat.
    csms.wait_for_call_after(mark, "Heartbeat", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(security_event_types(&csms), ["StartupOfTheDevice"]);
    stop(&station, task).await;
}

#[tokio::test]
async fn the_startup_waits_for_an_accepted_boot() {
    let accept = Arc::new(AtomicBool::new(false));
    let csms = v201_csms().await;
    let flag = Arc::clone(&accept);
    csms.respond("BootNotification", move |_| {
        if flag.load(Ordering::SeqCst) {
            boot_reply("Accepted", 5)
        } else {
            boot_reply("Pending", 1)
        }
    });
    let (station, task) = spawn_v201(&csms);
    station.connect().await.unwrap();
    wait_snapshot(&station, "a pending registration", |s| {
        s.registration == Some(RegistrationStatus::Pending)
    })
    .await;
    assert_eq!(calls_of(&csms, "SecurityEventNotification").len(), 0);
    accept.store(true, Ordering::SeqCst);
    wait_booted(&station).await;
    let event = csms
        .wait_for_call("SecurityEventNotification", STEP)
        .await
        .unwrap();
    assert_eq!(event["type"], "StartupOfTheDevice");
    stop(&station, task).await;
}

#[tokio::test]
async fn shutdown_closes_the_websocket() {
    let csms = v201_csms().await;
    let (station, task) = spawn_v201(&csms);
    station.connect().await.unwrap();
    wait_booted(&station).await;
    stop(&station, task).await;
    csms.wait_for_disconnect(STEP).await.unwrap();
    assert!(matches!(
        station.send(Command2::Heartbeat).await,
        Err(StationError::Stopped)
    ));
}

#[tokio::test]
async fn a_recorded_session_has_the_2_0_1_header() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let (sink, guard) = kirby_ocpp_core::recording::start(
        &path,
        "2.0.1",
        Role::Emulator,
        "CP-REC",
        &RecordOptions::default(),
    )
    .unwrap();
    let (station, task) = spawn_with(csms.url(), |_| {}, Some(sink));
    station.connect().await.unwrap();
    wait_booted(&station).await;
    stop(&station, task).await;
    let summary = guard.finish();
    assert_eq!(summary.incomplete, None);

    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines[0]["type"], "header");
    assert_eq!(lines[0]["ocpp_version"], "2.0.1");
    assert_eq!(lines[0]["role"], "emulator");
    let boot = lines
        .iter()
        .find(|line| line["type"] == "frame" && line["action"] == "BootNotification")
        .expect("the boot frame is recorded");
    assert_eq!(boot["direction"], "out");
    assert_eq!(boot["session"], 1);
    assert_eq!(boot["frame"][3]["reason"], "PowerUp");
    let command = lines
        .iter()
        .find(|line| line["type"] == "command")
        .expect("the connect command is recorded");
    assert_eq!(command["command"]["command"], "connect");
}

#[tokio::test]
async fn log_lines_that_tools_parse_keep_their_prefixes() {
    let csms = v201_csms().await;
    let (station, task) = spawn_v201(&csms);
    let mut events = station.subscribe();
    station.connect().await.unwrap();
    wait_booted(&station).await;
    csms.drop_connection().unwrap();
    wait_log(&mut events, "disconnected").await;
    wait_log(&mut events, "reconnect attempt 1 in 30ms").await;
    stop(&station, task).await;
}

#[tokio::test]
async fn an_unanswered_heartbeat_is_not_followed_by_a_backlog() {
    let csms = v201_csms().await;
    csms.respond("BootNotification", |_| boot_reply("Accepted", 1));
    let answering = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&answering);
    csms.respond("Heartbeat", move |_| {
        if flag.load(Ordering::SeqCst) {
            Reply::Result(json!({"currentTime": "2026-03-01T12:00:00Z"}))
        } else {
            Reply::Silence
        }
    });
    let (station, task) = spawn_with(csms.url(), |c| c.call_timeout_ms = 4_000, None);
    station.connect().await.unwrap();
    wait_booted(&station).await;
    csms.wait_for_call("Heartbeat", STEP).await.unwrap();
    // Three periods pass while the first heartbeat waits for its answer.
    let more = csms
        .wait_for_call_count_after(
            0,
            "Heartbeat",
            2,
            |_| true,
            std::time::Duration::from_secs(3),
        )
        .await;
    assert!(more.is_err(), "a second heartbeat was sent");

    answering.store(true, Ordering::SeqCst);
    csms.wait_for_call_count_after(0, "Heartbeat", 3, |_| true, STEP)
        .await
        .unwrap();
    let times: Vec<_> = csms
        .received()
        .into_iter()
        .filter(|call| call.action == "Heartbeat")
        .map(|call| call.ts)
        .collect();
    // The heartbeats of the stalled periods are not sent in a row once the CSMS answers.
    let gap = times[2] - times[1];
    assert!(gap >= chrono::Duration::milliseconds(500), "{times:?}");
    stop(&station, task).await;
}
