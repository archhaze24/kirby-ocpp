//! Availability changes of the OCPP 2.0.1 station across a restart: an `Inoperative` request
//! answered `Scheduled` at EVSE or connector level waits for the transaction to end, also when
//! the station restarts in between. `Operative` cancels it and the cancellation persists.

use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::frame::Frame;
use kirby_ocpp_core::ocpp::v2::types::IdTokenType;
use kirby_ocpp_core::store::{FileKind, LoadOutcome2, PersistedStation2, StationStore};
use kirby_ocpp_core::{Command2, ManualClock, Station2, Station2Handle, StationConfig};
use kirby_ocpp_testkit::MockCsms;
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::station2_support::{STEP, v201_csms, wait_booted, wait_snapshot};

const ID: &str = "CP-AVAIL";

fn spawn(csms: &MockCsms, dir: &Path) -> (Station2Handle, JoinHandle<()>) {
    let mut config = StationConfig::new(csms.url())
        .with_charge_point_id(ID)
        .with_connectors(2)
        .with_call_timeout_ms(2_000)
        .with_heartbeat_interval_secs(1)
        .with_reconnect(ReconnectConfig {
            enabled: true,
            initial_delay_ms: 30,
            max_delay_ms: 100,
            max_attempts: 0,
        })
        .with_state_dir(dir);
    config.ws_ping_interval_secs = 0;
    Protocol::V201
        .token()
        .clone_into(&mut config.ws_subprotocol);
    config.protocol = Protocol::V201;
    Station2::spawn(config, Arc::new(ManualClock::new(Utc::now())))
}

async fn start(csms: &MockCsms, dir: &Path) -> (Station2Handle, JoinHandle<()>) {
    let (station, join) = spawn(csms, dir);
    station.send(Command2::Connect).await.expect("connect");
    wait_booted(&station).await;
    (station, join)
}

async fn stop(station: Station2Handle, join: JoinHandle<()>) {
    station.shutdown().await.expect("shutdown");
    timeout(STEP, join).await.expect("stops").expect("joins");
}

async fn change_availability(csms: &MockCsms, body: Value) -> String {
    match csms
        .send_call("ChangeAvailability", body)
        .await
        .expect("call answered")
    {
        Frame::CallResult { payload, .. } => payload["status"]
            .as_str()
            .expect("status is a string")
            .to_owned(),
        other => panic!("unexpected reply {other:?}"),
    }
}

/// A transaction on EVSE 1 with a plugged EV and a central token.
async fn start_transaction(station: &Station2Handle) {
    station
        .send(Command2::PlugIn { evse: 1 })
        .await
        .expect("plug in");
    station
        .send(Command2::PresentToken {
            evse: Some(1),
            id_token: "TOKEN-1".to_owned(),
            token_type: IdTokenType::Central,
        })
        .await
        .expect("token");
    wait_snapshot(station, "a transaction on EVSE 1", |s| {
        s.evse(1).is_some_and(|e| e.transaction.is_some())
    })
    .await;
}

fn load(dir: &Path) -> PersistedStation2 {
    match StationStore::new_for(FileKind::V2, dir, ID)
        .load2()
        .expect("state loads")
    {
        LoadOutcome2::Loaded(state) => *state,
        other => panic!("no state: {other:?}"),
    }
}

fn is_status(payload: &Value, evse: i64, status: &str) -> bool {
    payload["evseId"] == evse && payload["connectorStatus"] == status
}

/// Schedules `request` during a transaction, restarts, and returns once the restored
/// transaction ended. Returns the CSMS mark taken before the second run.
async fn schedule_and_restart(
    csms: &MockCsms,
    dir: &Path,
    request: Value,
    follow_up: Option<Value>,
) -> (Station2Handle, JoinHandle<()>, usize) {
    let (station, join) = start(csms, dir).await;
    start_transaction(&station).await;
    assert_eq!(change_availability(csms, request).await, "Scheduled");
    if let Some(body) = follow_up {
        assert_eq!(change_availability(csms, body).await, "Accepted");
    }
    stop(station, join).await;
    let mark = csms.mark();
    let (station, join) = start(csms, dir).await;
    (station, join, mark)
}

#[tokio::test]
async fn a_waiting_evse_request_survives_a_restart_and_applies_at_the_end() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let request = json!({"evse": {"id": 1}, "operationalStatus": "Inoperative"});
    let (station, join) = start(&csms, dir.path()).await;
    start_transaction(&station).await;
    assert_eq!(change_availability(&csms, request).await, "Scheduled");
    stop(station, join).await;
    let saved = load(dir.path());
    let entry = &saved.evse_availability[0];
    assert!(entry.operative && entry.evse_inoperative_pending);

    let mark = csms.mark();
    let (station, join) = start(&csms, dir.path()).await;
    // The restored transaction ends with the boot; the waiting request applies.
    csms.wait_for_call_after(
        mark,
        "StatusNotification",
        |p| is_status(p, 1, "Unavailable"),
        STEP,
    )
    .await
    .expect("EVSE 1 becomes unavailable");
    let snapshot = wait_snapshot(&station, "EVSE 1 inoperative", |s| {
        s.evse(1)
            .is_some_and(|e| !e.operative && e.transaction.is_none())
    })
    .await;
    assert!(snapshot.evse(2).is_some_and(|e| e.operative));
    stop(station, join).await;
    let saved = load(dir.path());
    let entry = &saved.evse_availability[0];
    assert!(!entry.operative && !entry.evse_inoperative_pending);
}

#[tokio::test]
async fn a_waiting_connector_request_survives_a_restart_and_applies_at_the_end() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let request = json!({
        "evse": {"id": 1, "connectorId": 1},
        "operationalStatus": "Inoperative",
    });
    let (station, join) = start(&csms, dir.path()).await;
    start_transaction(&station).await;
    assert_eq!(change_availability(&csms, request).await, "Scheduled");
    stop(station, join).await;
    let saved = load(dir.path());
    let entry = &saved.evse_availability[0];
    assert!(entry.operative && !entry.evse_inoperative_pending);
    assert_eq!(entry.connectors_inoperative_pending, [1]);

    let mark = csms.mark();
    let (station, join) = start(&csms, dir.path()).await;
    csms.wait_for_call_after(
        mark,
        "StatusNotification",
        |p| is_status(p, 1, "Unavailable"),
        STEP,
    )
    .await
    .expect("the connector becomes unavailable");
    stop(station, join).await;
    let saved = load(dir.path());
    let entry = &saved.evse_availability[0];
    assert!(entry.operative);
    assert_eq!(entry.connectors, [(1, false)]);
    assert_eq!(entry.connectors_inoperative_pending, Vec::<u32>::new());
}

#[tokio::test]
async fn operative_cancels_a_waiting_evse_request_for_good() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let request = json!({"evse": {"id": 1}, "operationalStatus": "Inoperative"});
    let cancel = json!({"evse": {"id": 1}, "operationalStatus": "Operative"});
    let (station, join, mark) =
        schedule_and_restart(&csms, dir.path(), request, Some(cancel)).await;
    assert!(
        load(dir.path())
            .evse_availability
            .iter()
            .all(|e| !e.evse_inoperative_pending)
    );
    let snapshot = wait_snapshot(&station, "the restored transaction ended", |s| {
        s.evse(1).is_some_and(|e| e.transaction.is_none())
    })
    .await;
    assert!(snapshot.evse(1).is_some_and(|e| e.operative));
    // Marker: everything the end caused precedes this report.
    station
        .send(Command2::StatusNotification {
            evse: 2,
            connector: 1,
        })
        .await
        .expect("marker");
    csms.wait_for_call_after(
        mark,
        "StatusNotification",
        |p| p["evseId"] == 2 && p["connectorStatus"] == "Available",
        STEP,
    )
    .await
    .expect("marker arrives");
    assert!(
        !csms
            .calls_since(mark)
            .iter()
            .any(|c| c.action == "StatusNotification" && is_status(&c.payload, 1, "Unavailable"))
    );
    stop(station, join).await;
}

#[tokio::test]
async fn operative_cancels_a_waiting_connector_request_for_good() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let scope = json!({"id": 1, "connectorId": 1});
    let request = json!({"evse": scope, "operationalStatus": "Inoperative"});
    let cancel = json!({"evse": scope, "operationalStatus": "Operative"});
    let (station, join, _mark) =
        schedule_and_restart(&csms, dir.path(), request, Some(cancel)).await;
    assert!(
        load(dir.path())
            .evse_availability
            .iter()
            .all(|e| e.connectors_inoperative_pending.is_empty())
    );
    let snapshot = wait_snapshot(&station, "the restored transaction ended", |s| {
        s.evse(1).is_some_and(|e| e.transaction.is_none())
    })
    .await;
    assert!(snapshot.evse(1).is_some_and(|e| e.operative));
    stop(station, join).await;
    let entry = &load(dir.path()).evse_availability[0];
    assert_eq!(entry.connectors, [(1, true)]);
}

#[tokio::test]
async fn applied_evse_and_connector_inoperative_survive_a_restart() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let (station, join) = start(&csms, dir.path()).await;
    let evse = json!({"evse": {"id": 1}, "operationalStatus": "Inoperative"});
    assert_eq!(change_availability(&csms, evse).await, "Accepted");
    let connector = json!({
        "evse": {"id": 2, "connectorId": 1},
        "operationalStatus": "Inoperative",
    });
    assert_eq!(change_availability(&csms, connector).await, "Accepted");
    stop(station, join).await;
    let saved = load(dir.path());
    assert!(!saved.evse_availability[0].operative);
    assert_eq!(saved.evse_availability[1].connectors, [(1, false)]);

    let (station, join) = start(&csms, dir.path()).await;
    let snapshot = wait_snapshot(&station, "the restored availability", |s| {
        s.evse(1).is_some_and(|e| !e.operative)
    })
    .await;
    let evse2 = snapshot.evse(2).expect("EVSE 2");
    assert_eq!(
        evse2.connectors[0].status,
        kirby_ocpp_core::ocpp::v2::messages::ConnectorStatus::Unavailable
    );
    stop(station, join).await;
}
