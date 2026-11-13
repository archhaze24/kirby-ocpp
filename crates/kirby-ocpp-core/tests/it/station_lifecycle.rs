//! Boot flow, registration states, heartbeat, reconnect and shutdown of the station actor.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use crate::station_support::{Observed, Rig, STEP, dead_url, wait_state};
use kirby_ocpp_core::CallError;
use kirby_ocpp_core::StationError;
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::types::RegistrationStatus;
use kirby_ocpp_core::station::{Command, ConnectionState, Outcome};
use kirby_ocpp_testkit::{CsmsOptions, Reply};
use serde_json::json;

const SHORT: Duration = Duration::from_millis(300);

fn boot_reply(status: &str, interval: u32) -> Reply {
    Reply::Result(json!({
        "status": status,
        "currentTime": "2026-03-01T12:00:00Z",
        "interval": interval,
    }))
}

#[tokio::test]
async fn boot_accepted_sets_state_and_sends_all_statuses() {
    let mut rig = Rig::with(CsmsOptions::default(), |mut config| {
        config.serial_number = Some("SN-42".into());
        config.firmware_version = Some("1.2.3".into());
        config
    })
    .await;
    rig.connect_booted().await;

    let boot = rig
        .csms
        .wait_for_call("BootNotification", STEP)
        .await
        .unwrap();
    assert_eq!(boot["chargePointVendor"], "archhaze24");
    assert_eq!(boot["chargePointModel"], "kirby-ocpp");
    assert_eq!(boot["chargePointSerialNumber"], "SN-42");
    assert_eq!(boot["firmwareVersion"], "1.2.3");

    let snapshot = rig.station.snapshot();
    assert_eq!(snapshot.connection, ConnectionState::Connected);
    assert!(snapshot.booted());
    assert_eq!(
        snapshot.heartbeat_interval_secs, 5,
        "interval from the CSMS"
    );

    // Connector 0 and every connector, once each, all Available.
    let statuses = rig.calls("StatusNotification", 0);
    let mut ids: Vec<u64> = statuses
        .iter()
        .map(|p| p["connectorId"].as_u64().unwrap())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, [0, 1, 2]);
    assert!(statuses.iter().all(|p| p["status"] == "Available"));
    assert!(statuses.iter().all(|p| p["errorCode"] == "NoError"));
    assert!(statuses.iter().all(|p| p["timestamp"].is_string()));
    rig.shutdown().await;
}

#[tokio::test]
async fn optional_boot_fields_are_omitted_when_not_configured() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    let boot = rig
        .csms
        .wait_for_call("BootNotification", STEP)
        .await
        .unwrap();
    assert!(boot.get("chargePointSerialNumber").is_none());
    assert!(boot.get("firmwareVersion").is_none());
    rig.shutdown().await;
}

#[tokio::test]
async fn boot_command_returns_the_csms_response() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    let Outcome::Response { payload } = rig.send(Command::Boot).await else {
        panic!("expected a response");
    };
    assert_eq!(payload["status"], "Accepted");
    assert_eq!(payload["interval"], 5);
    rig.shutdown().await;
}

#[tokio::test]
async fn commands_that_need_a_connection_fail_while_disconnected() {
    let rig = Rig::start().await;
    for command in [Command::Boot, Command::Heartbeat] {
        let err = rig.station.send(command).await.unwrap_err();
        assert!(matches!(err, StationError::NotConnected), "{err:?}");
    }
    let err = rig
        .station
        .send(Command::DataTransfer {
            vendor_id: "x".into(),
            message_id: None,
            data: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StationError::NotConnected), "{err:?}");
    rig.shutdown().await;
}

#[tokio::test]
async fn pending_registration_retries_the_boot_and_gates_other_messages() {
    let accept = Arc::new(AtomicBool::new(false));
    let rig = Rig::start().await;
    let flag = Arc::clone(&accept);
    rig.csms.respond("BootNotification", move |_| {
        if flag.load(Ordering::SeqCst) {
            boot_reply("Accepted", 5)
        } else {
            boot_reply("Pending", 1)
        }
    });
    rig.station.connect().await.unwrap();
    rig.wait_registration(RegistrationStatus::Pending).await;

    // While Pending: no heartbeat timer, no StatusNotification, user messages are refused.
    let err = rig.station.send(Command::Heartbeat).await.unwrap_err();
    assert!(
        matches!(err, StationError::Call(CallError::NotRegistered { .. })),
        "{err:?}"
    );
    let mark = rig.csms.mark();
    // ... but a message the CSMS asked for is allowed.
    rig.csms
        .expect_status(
            "TriggerMessage",
            json!({"requestedMessage": "Heartbeat"}),
            "Accepted",
        )
        .await
        .unwrap();
    rig.csms
        .wait_for_call_after(mark, "Heartbeat", |_| true, STEP)
        .await
        .unwrap();
    // Other triggered messages needing registration are allowed in Pending too.
    rig.csms
        .expect_status(
            "TriggerMessage",
            json!({"requestedMessage": "StatusNotification", "connectorId": 1}),
            "Accepted",
        )
        .await
        .unwrap();
    rig.csms
        .wait_for_call_after(mark, "StatusNotification", |_| true, STEP)
        .await
        .unwrap();

    // The boot is repeated after the CSMS's interval (1 s); then the station registers.
    accept.store(true, Ordering::SeqCst);
    rig.wait_registration(RegistrationStatus::Accepted).await;
    assert!(rig.calls("BootNotification", 0).len() >= 2);
    rig.shutdown().await;
}

#[tokio::test]
async fn rejected_registration_sends_nothing_but_boot_retries() {
    let accept = Arc::new(AtomicBool::new(false));
    let rig = Rig::start().await;
    let flag = Arc::clone(&accept);
    rig.csms.respond("BootNotification", move |_| {
        if flag.load(Ordering::SeqCst) {
            boot_reply("Accepted", 5)
        } else {
            boot_reply("Rejected", 1)
        }
    });
    rig.station.connect().await.unwrap();
    rig.wait_registration(RegistrationStatus::Rejected).await;

    // Triggered messages other than a boot are refused while Rejected.
    rig.csms
        .expect_status(
            "TriggerMessage",
            json!({"requestedMessage": "Heartbeat"}),
            "Rejected",
        )
        .await
        .unwrap();
    // The retry arrives (second boot) and nothing else was sent in the meantime.
    rig.csms
        .wait_for_call_count_after(0, "BootNotification", 2, |_| true, STEP)
        .await
        .unwrap();
    rig.assert_no_calls("StatusNotification", 0);
    rig.assert_no_calls("Heartbeat", 0);

    accept.store(true, Ordering::SeqCst);
    rig.wait_registration(RegistrationStatus::Accepted).await;
    rig.csms
        .wait_for_call("StatusNotification", STEP)
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn heartbeat_interval_zero_never_arms_a_zero_timer() {
    let mut rig = Rig::start().await;
    rig.csms
        .respond("BootNotification", |_| boot_reply("Accepted", 0));
    rig.connect_booted().await;
    // interval 0 = the charge point chooses: the configured 30 s stays.
    assert_eq!(rig.station.snapshot().heartbeat_interval_secs, 30);
    // A 0 ms timer would have produced a flood of heartbeats by now.
    assert!(
        rig.csms.wait_for_call("Heartbeat", SHORT).await.is_err(),
        "no heartbeat expected"
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn heartbeats_are_sent_at_the_csms_interval() {
    let options = CsmsOptions {
        heartbeat_interval: 1,
        ..CsmsOptions::default()
    };
    let mut rig = Rig::with(options, |config| config).await;
    rig.connect_booted().await;
    rig.csms.wait_for_call("Heartbeat", STEP).await.unwrap();
    wait_state(&rig.station, "last heartbeat time", |s| {
        s.last_heartbeat_at.is_some()
    })
    .await;
    rig.shutdown().await;
}

#[tokio::test]
async fn heartbeat_command_returns_the_csms_time() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    let Outcome::Response { payload } = rig.send(Command::Heartbeat).await else {
        panic!("expected a response");
    };
    assert!(payload["currentTime"].is_string());
    assert!(rig.station.snapshot().last_heartbeat_at.is_some());
    rig.shutdown().await;
}

#[tokio::test]
async fn reconnects_and_boots_again_after_the_server_closes() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    rig.csms.drop_connection().unwrap();
    rig.csms
        .wait_for_connection_count(2, STEP)
        .await
        .expect("a second connection");
    rig.csms
        .wait_for_call_count_after(0, "BootNotification", 2, |_| true, STEP)
        .await
        .unwrap();
    rig.wait_registration(RegistrationStatus::Accepted).await;
    // The statuses are re-sent with force after the new boot.
    rig.csms
        .wait_for_call_count_after(0, "StatusNotification", 6, |_| true, STEP)
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn backoff_doubles_and_stops_after_max_attempts() {
    let url = dead_url().await;
    let rig = Rig::with(CsmsOptions::default(), |mut config| {
        config.url = url;
        config.reconnect = ReconnectConfig {
            enabled: true,
            initial_delay_ms: 30,
            max_delay_ms: 100,
            max_attempts: 3,
        };
        config
    })
    .await;
    let mut observed = Observed::new(rig.station.subscribe());
    rig.station.connect().await.unwrap();
    observed.wait_log("reconnect attempt 1/3 in 30ms").await;
    observed.wait_log("reconnect attempt 2/3 in 60ms").await;
    observed.wait_log("reconnect attempt 3/3 in 100ms").await; // 120 capped at 100
    observed.wait_log("reconnect stopped").await;
    let snapshot = wait_state(&rig.station, "disconnected", |s| {
        s.connection == ConnectionState::Disconnected
    })
    .await;
    assert!(!snapshot.booted());
    rig.shutdown().await;
}

#[tokio::test]
async fn snapshot_records_that_the_reconnect_loop_stopped() {
    let url = dead_url().await;
    let rig = Rig::with(CsmsOptions::default(), |mut config| {
        config.url = url;
        config.reconnect = ReconnectConfig {
            enabled: true,
            initial_delay_ms: 20,
            max_delay_ms: 20,
            max_attempts: 2,
        };
        config
    })
    .await;
    assert_eq!(rig.station.snapshot().reconnect_stopped, None);
    rig.station.connect().await.unwrap();
    let snapshot = wait_state(&rig.station, "reconnect stopped", |s| {
        s.reconnect_stopped.is_some()
    })
    .await;
    assert_eq!(
        snapshot.reconnect_stopped.as_deref(),
        Some("after 2 failed attempt(s)")
    );
    assert_eq!(snapshot.connection, ConnectionState::Disconnected);
    // A manual connect restarts the loop, and a deliberate disconnect clears the reason.
    rig.station.connect().await.unwrap();
    wait_state(&rig.station, "stopped again", |s| {
        s.reconnect_stopped.is_some()
    })
    .await;
    rig.station.disconnect().await.unwrap();
    wait_state(&rig.station, "cleared by disconnect", |s| {
        s.reconnect_stopped.is_none()
    })
    .await;
    rig.shutdown().await;
}

#[tokio::test]
async fn disabled_reconnect_sets_the_stopped_reason() {
    let mut rig = Rig::with(CsmsOptions::default(), |mut config| {
        config.reconnect.enabled = false;
        config
    })
    .await;
    rig.connect_booted().await;
    assert_eq!(rig.station.snapshot().reconnect_stopped, None);
    rig.csms.drop_connection().unwrap();
    let snapshot = wait_state(&rig.station, "reconnect stopped", |s| {
        s.reconnect_stopped.is_some()
    })
    .await;
    assert_eq!(
        snapshot.reconnect_stopped.as_deref(),
        Some("automatic reconnect is disabled")
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn manual_connect_resets_the_attempt_counter() {
    let url = dead_url().await;
    let rig = Rig::with(CsmsOptions::default(), |mut config| {
        config.url = url;
        config.reconnect = ReconnectConfig {
            enabled: true,
            initial_delay_ms: 20,
            max_delay_ms: 20,
            max_attempts: 2,
        };
        config
    })
    .await;
    let mut observed = Observed::new(rig.station.subscribe());
    rig.station.connect().await.unwrap();
    observed.wait_log("reconnect stopped").await;
    // A manual Connect starts counting from 1 again.
    rig.station.connect().await.unwrap();
    observed.wait_log("reconnect attempt 1/2 in 20ms").await;
    observed.wait_log("reconnect stopped").await;
    rig.shutdown().await;
}

#[tokio::test]
async fn disabled_reconnect_stays_disconnected() {
    let mut rig = Rig::with(CsmsOptions::default(), |mut config| {
        config.reconnect.enabled = false;
        config
    })
    .await;
    rig.connect_booted().await;
    let mut observed = Observed::new(rig.station.subscribe());
    rig.csms.drop_connection().unwrap();
    observed.wait_log("reconnect stopped").await;
    assert_eq!(rig.csms.connections(), 1);
    rig.shutdown().await;
}

#[tokio::test]
async fn disconnect_closes_the_socket_and_stops_reconnecting() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    rig.station.disconnect().await.unwrap();
    rig.csms.wait_for_disconnect(STEP).await.unwrap();
    let snapshot = rig.station.snapshot();
    assert_eq!(snapshot.connection, ConnectionState::Disconnected);
    assert_eq!(snapshot.registration, None);
    // No automatic reconnect follows a manual disconnect.
    assert!(rig.csms.wait_for_connection_count(2, SHORT).await.is_err());
    rig.shutdown().await;
}

#[tokio::test]
async fn disconnect_cancels_a_pending_backoff() {
    let url = dead_url().await;
    let rig = Rig::with(CsmsOptions::default(), |mut config| {
        config.url = url;
        config.reconnect.initial_delay_ms = 60_000;
        config.reconnect.max_delay_ms = 60_000;
        config
    })
    .await;
    rig.station.connect().await.unwrap();
    wait_state(&rig.station, "backoff", |s| {
        s.connection == ConnectionState::Backoff { attempt: 1 }
    })
    .await;
    rig.station.disconnect().await.unwrap();
    assert_eq!(
        rig.station.snapshot().connection,
        ConnectionState::Disconnected
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn reconnect_command_opens_a_fresh_connection_and_boots() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    rig.send(Command::Reconnect).await;
    rig.csms.wait_for_connection_count(2, STEP).await.unwrap();
    rig.csms
        .wait_for_call_count_after(0, "BootNotification", 2, |_| true, STEP)
        .await
        .unwrap();
    rig.wait_registration(RegistrationStatus::Accepted).await;
    rig.shutdown().await;
}

#[tokio::test]
async fn shutdown_closes_the_websocket() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    rig.station.shutdown().await.unwrap();
    rig.join.take().unwrap().await.unwrap();
    rig.csms.wait_for_disconnect(STEP).await.unwrap();
    assert_eq!(rig.csms.open_connections(), 0);
    rig.csms.shutdown().await;
}

#[tokio::test]
async fn a_failed_boot_call_is_retried() {
    let calls = Arc::new(AtomicUsize::new(0));
    let rig = Rig::with(CsmsOptions::default(), |mut config| {
        config.heartbeat_interval_secs = 1;
        config
    })
    .await;
    let counter = Arc::clone(&calls);
    rig.csms.respond("BootNotification", move |_| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            Reply::Error(
                kirby_ocpp_testkit::ErrorCode::InternalError,
                "first boot fails".into(),
            )
        } else {
            boot_reply("Accepted", 5)
        }
    });
    rig.station.connect().await.unwrap();
    // The failed boot is retried after the heartbeat interval (1 s) and then succeeds.
    rig.wait_registration(RegistrationStatus::Accepted).await;
    assert!(calls.load(Ordering::SeqCst) >= 2);
    rig.shutdown().await;
}
