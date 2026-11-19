//! EVSE and connector status and availability of the OCPP 2.0.1 station.
//!
//! Quiet checks never sleep: a marker `Command2::StatusNotification` for a connector the test
//! does not care about is sent last, and because the station handles everything in order,
//! every notification caused by the earlier step precedes it.

use std::sync::Arc;

use chrono::{Duration, Utc};
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::frame::Frame;
use kirby_ocpp_core::ocpp::v2::messages::ConnectorStatus;
use kirby_ocpp_core::ocpp::v2::types::{IdTokenType, StopReason};
use kirby_ocpp_core::ws::ConnectionState;
use kirby_ocpp_core::{
    Command2, ManualClock, Outcome2, Snapshot2, Station2, Station2Handle, StationConfig,
    StationError,
};
use kirby_ocpp_testkit::MockCsms;
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use super::station2_support::{STEP, spawn_v201, v201_csms, wait_booted, wait_snapshot};

/// A started, booted station with its first status reports consumed.
struct Rig {
    csms: MockCsms,
    station: Station2Handle,
    task: JoinHandle<()>,
}

impl Rig {
    async fn new() -> Self {
        let csms = v201_csms().await;
        let (station, task) = spawn_v201(&csms);
        station.connect().await.expect("connect");
        wait_booted(&station).await;
        wait_statuses(&csms, 0, 2).await;
        Self {
            csms,
            station,
            task,
        }
    }

    async fn stop(self) {
        let _ = self.station.shutdown().await;
        let _ = self.task.await;
        self.csms.shutdown().await;
    }

    async fn send(&self, command: Command2) -> Outcome2 {
        self.station.send(command).await.expect("command accepted")
    }

    /// Sends the command and returns the error it fails with.
    async fn send_err(&self, command: Command2) -> StationError {
        self.station
            .send(command)
            .await
            .expect_err("command is refused")
    }

    /// The `(evse, connector, status)` of the notifications after `mark`, ending with a
    /// marker for EVSE 2 (which is dropped from the result).
    async fn statuses_after(&self, mark: usize) -> Vec<(i64, i64, String)> {
        self.send(Command2::StatusNotification {
            evse: 2,
            connector: 1,
        })
        .await;
        let mut all = statuses(&self.csms, mark);
        let marker = all.pop().expect("the marker was sent");
        assert_eq!((marker.0, marker.1), (2, 1), "marker is last: {all:?}");
        all
    }

    async fn change_availability(&self, body: Value) -> String {
        let frame = self
            .csms
            .send_call("ChangeAvailability", body)
            .await
            .expect("call answered");
        match frame {
            Frame::CallResult { payload, .. } => payload["status"]
                .as_str()
                .expect("status is a string")
                .to_owned(),
            other => panic!("unexpected reply {other:?}"),
        }
    }

    async fn availability_state(&self, component: Value) -> String {
        let frame = self
            .csms
            .send_call(
                "GetVariables",
                json!({"getVariableData": [{
                    "component": component,
                    "variable": {"name": "AvailabilityState"},
                }]}),
            )
            .await
            .expect("call answered");
        match frame {
            Frame::CallResult { payload, .. } => payload["getVariableResult"][0]["attributeValue"]
                .as_str()
                .expect("a value")
                .to_owned(),
            other => panic!("unexpected reply {other:?}"),
        }
    }

    /// Starts a transaction on EVSE 1 with a plugged EV and a central token.
    async fn start_transaction(&self) {
        self.send(Command2::PlugIn { evse: 1 }).await;
        self.send(Command2::PresentToken {
            evse: Some(1),
            id_token: "TOKEN-1".to_owned(),
            token_type: IdTokenType::Central,
        })
        .await;
        wait_snapshot(&self.station, "a transaction on EVSE 1", |s| {
            s.evse(1).is_some_and(|e| e.transaction.is_some())
        })
        .await;
    }

    async fn end_transaction(&self) {
        self.send(Command2::StopTransaction {
            evse: 1,
            reason: StopReason::Local,
        })
        .await;
        wait_snapshot(&self.station, "the transaction ended", |s| {
            s.evse(1).is_some_and(|e| e.transaction.is_none())
        })
        .await;
    }
}

fn statuses(csms: &MockCsms, mark: usize) -> Vec<(i64, i64, String)> {
    csms.calls_since(mark)
        .into_iter()
        .filter(|call| call.action == "StatusNotification")
        .map(|call| {
            (
                call.payload["evseId"].as_i64().expect("evseId"),
                call.payload["connectorId"].as_i64().expect("connectorId"),
                call.payload["connectorStatus"]
                    .as_str()
                    .expect("connectorStatus")
                    .to_owned(),
            )
        })
        .collect()
}

async fn wait_statuses(csms: &MockCsms, mark: usize, count: usize) {
    csms.wait_for_call_count_after(mark, "StatusNotification", count, |_| true, STEP)
        .await
        .expect("status notifications arrive");
}

fn note(evse: i64, connector: i64, status: &str) -> (i64, i64, String) {
    (evse, connector, status.to_owned())
}

fn connector_status(snapshot: &Snapshot2, evse: u32) -> ConnectorStatus {
    snapshot.evse(evse).expect("EVSE exists").connectors[0].status
}

#[tokio::test]
async fn boot_reports_every_connector_once() {
    let rig = Rig::new().await;
    assert_eq!(
        statuses(&rig.csms, 0),
        [note(1, 1, "Available"), note(2, 1, "Available")]
    );
    let snapshot = rig.station.snapshot();
    assert_eq!(snapshot.evses.len(), 2);
    assert!(snapshot.availability_operative);
    rig.stop().await;
}

#[tokio::test]
async fn plug_and_unplug_report_occupied_and_available() {
    let rig = Rig::new().await;
    let mark = rig.csms.mark();
    rig.send(Command2::PlugIn { evse: 2 }).await;
    assert_eq!(
        rig.station.snapshot().evse(2).expect("EVSE").connectors[0].status,
        ConnectorStatus::Occupied
    );
    rig.send(Command2::Unplug { evse: 2 }).await;
    // The marker for EVSE 2 is itself a notification here, so look at the whole list.
    rig.send(Command2::StatusNotification {
        evse: 1,
        connector: 1,
    })
    .await;
    wait_statuses(&rig.csms, mark, 3).await;
    assert_eq!(
        statuses(&rig.csms, mark),
        [
            note(2, 1, "Occupied"),
            note(2, 1, "Available"),
            note(1, 1, "Available")
        ]
    );
    rig.stop().await;
}

#[tokio::test]
async fn plug_state_errors_are_refused() {
    let rig = Rig::new().await;
    assert!(matches!(
        rig.send_err(Command2::Unplug { evse: 1 }).await,
        StationError::InvalidState { .. }
    ));
    rig.send(Command2::PlugIn { evse: 1 }).await;
    assert!(matches!(
        rig.send_err(Command2::PlugIn { evse: 1 }).await,
        StationError::InvalidState { .. }
    ));
    assert!(matches!(
        rig.send_err(Command2::PlugIn { evse: 9 }).await,
        StationError::InvalidArgument(_)
    ));
    rig.stop().await;
}

#[tokio::test]
async fn fault_and_clear_report_faulted_then_the_derived_state() {
    let rig = Rig::new().await;
    let mark = rig.csms.mark();
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.send(Command2::Fault {
        evse: 1,
        connector: None,
    })
    .await;
    assert_eq!(
        connector_status(&rig.station.snapshot(), 1),
        ConnectorStatus::Faulted
    );
    rig.send(Command2::ClearFault {
        evse: 1,
        connector: Some(1),
    })
    .await;
    assert_eq!(
        rig.statuses_after(mark).await,
        [
            note(1, 1, "Occupied"),
            note(1, 1, "Faulted"),
            note(1, 1, "Occupied")
        ]
    );
    assert!(matches!(
        rig.send_err(Command2::Fault {
            evse: 1,
            connector: Some(5)
        })
        .await,
        StationError::InvalidArgument(_)
    ));
    rig.stop().await;
}

#[tokio::test]
async fn status_notification_command_resends_the_current_state() {
    let rig = Rig::new().await;
    let mark = rig.csms.mark();
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.send(Command2::StatusNotification {
        evse: 1,
        connector: 1,
    })
    .await;
    assert_eq!(
        rig.statuses_after(mark).await,
        [note(1, 1, "Occupied"), note(1, 1, "Occupied")]
    );
    assert!(matches!(
        rig.send_err(Command2::StatusNotification {
            evse: 1,
            connector: 3
        })
        .await,
        StationError::InvalidArgument(_)
    ));
    rig.stop().await;
}

#[tokio::test]
async fn connector_availability_changes_only_that_connector() {
    let rig = Rig::new().await;
    let mark = rig.csms.mark();
    let body =
        |status: &str| json!({"evse": {"id": 1, "connectorId": 1}, "operationalStatus": status});
    assert_eq!(
        rig.change_availability(body("Inoperative")).await,
        "Accepted"
    );
    assert_eq!(
        rig.change_availability(body("Inoperative")).await,
        "Accepted"
    );
    assert_eq!(
        rig.availability_state(json!({"name": "Connector", "evse": {"id": 1, "connectorId": 1}}))
            .await,
        "Unavailable"
    );
    assert_eq!(rig.change_availability(body("Operative")).await, "Accepted");
    assert_eq!(
        rig.statuses_after(mark).await,
        [note(1, 1, "Unavailable"), note(1, 1, "Available")]
    );
    rig.stop().await;
}

#[tokio::test]
async fn evse_availability_changes_its_connectors_and_leaves_siblings_quiet() {
    let rig = Rig::new().await;
    let mark = rig.csms.mark();
    let body = |status: &str| json!({"evse": {"id": 1}, "operationalStatus": status});
    assert_eq!(
        rig.change_availability(body("Inoperative")).await,
        "Accepted"
    );
    assert_eq!(
        rig.availability_state(json!({"name": "EVSE", "evse": {"id": 1}}))
            .await,
        "Unavailable"
    );
    assert_eq!(
        rig.availability_state(json!({"name": "EVSE", "evse": {"id": 2}}))
            .await,
        "Available"
    );
    let snapshot = rig.station.snapshot();
    assert!(!snapshot.evse(1).expect("EVSE").operative);
    assert!(snapshot.evse(2).expect("EVSE").operative);
    assert_eq!(rig.change_availability(body("Operative")).await, "Accepted");
    assert_eq!(
        rig.statuses_after(mark).await,
        [note(1, 1, "Unavailable"), note(1, 1, "Available")]
    );
    rig.stop().await;
}

#[tokio::test]
async fn station_availability_changes_every_connector() {
    let rig = Rig::new().await;
    let mark = rig.csms.mark();
    let body = |status: &str| json!({"operationalStatus": status});
    assert_eq!(
        rig.change_availability(body("Inoperative")).await,
        "Accepted"
    );
    assert_eq!(
        rig.availability_state(json!({"name": "ChargingStation"}))
            .await,
        "Unavailable"
    );
    assert!(!rig.station.snapshot().availability_operative);
    assert_eq!(
        rig.change_availability(body("Inoperative")).await,
        "Accepted"
    );
    assert_eq!(rig.change_availability(body("Operative")).await, "Accepted");
    wait_statuses(&rig.csms, mark, 4).await;
    let mut seen = statuses(&rig.csms, mark);
    seen.sort();
    assert_eq!(
        seen,
        [
            note(1, 1, "Available"),
            note(1, 1, "Unavailable"),
            note(2, 1, "Available"),
            note(2, 1, "Unavailable"),
        ]
    );
    rig.stop().await;
}

#[tokio::test]
async fn unknown_targets_are_rejected() {
    let rig = Rig::new().await;
    let mark = rig.csms.mark();
    for evse in [
        json!({"id": 9}),
        json!({"id": 1, "connectorId": 4}),
        json!({"id": 0, "connectorId": 1}),
    ] {
        let body = json!({"evse": evse, "operationalStatus": "Inoperative"});
        assert_eq!(rig.change_availability(body).await, "Rejected");
    }
    assert_eq!(rig.statuses_after(mark).await, []);
    rig.stop().await;
}

#[tokio::test]
async fn faulted_connector_stays_faulted_when_made_inoperative() {
    let rig = Rig::new().await;
    let mark = rig.csms.mark();
    rig.send(Command2::Fault {
        evse: 1,
        connector: None,
    })
    .await;
    let body = json!({"evse": {"id": 1}, "operationalStatus": "Inoperative"});
    assert_eq!(rig.change_availability(body).await, "Accepted");
    assert_eq!(
        connector_status(&rig.station.snapshot(), 1),
        ConnectorStatus::Faulted
    );
    assert_eq!(rig.statuses_after(mark).await, [note(1, 1, "Faulted")]);
    rig.stop().await;
}

#[tokio::test]
async fn inoperative_during_a_transaction_is_scheduled_and_applied_when_it_ends() {
    let rig = Rig::new().await;
    rig.start_transaction().await;
    let mark = rig.csms.mark();
    let body = json!({"evse": {"id": 1}, "operationalStatus": "Inoperative"});
    assert_eq!(rig.change_availability(body).await, "Scheduled");
    assert!(rig.station.snapshot().evse(1).expect("EVSE").operative);
    rig.end_transaction().await;
    wait_snapshot(&rig.station, "the scheduled change applied", |s| {
        s.evse(1).is_some_and(|e| !e.operative)
    })
    .await;
    assert_eq!(
        connector_status(&rig.station.snapshot(), 1),
        ConnectorStatus::Unavailable
    );
    let seen = rig.statuses_after(mark).await;
    assert_eq!(seen.last(), Some(&note(1, 1, "Unavailable")));
    rig.stop().await;
}

#[tokio::test]
async fn station_inoperative_during_a_transaction_waits_for_it() {
    let rig = Rig::new().await;
    rig.start_transaction().await;
    let body = json!({"operationalStatus": "Inoperative"});
    assert_eq!(rig.change_availability(body).await, "Scheduled");
    assert!(rig.station.snapshot().availability_operative);
    rig.end_transaction().await;
    wait_snapshot(&rig.station, "the station is inoperative", |s| {
        !s.availability_operative
    })
    .await;
    rig.stop().await;
}

#[tokio::test]
async fn station_inoperative_during_a_transaction_holds_the_idle_evses_at_once() {
    let rig = Rig::new().await;
    rig.start_transaction().await;
    // Barrier: everything the start sent has arrived before the mark.
    rig.statuses_after(0).await;
    let mark = rig.csms.mark();
    let body = json!({"operationalStatus": "Inoperative"});
    assert_eq!(rig.change_availability(body).await, "Scheduled");
    // The EVSE without a transaction is unavailable immediately; the busy one is left alone.
    assert_eq!(rig.statuses_after(mark).await, [note(2, 1, "Unavailable")]);
    let snapshot = rig.station.snapshot();
    assert_eq!(connector_status(&snapshot, 2), ConnectorStatus::Unavailable);
    assert_eq!(connector_status(&snapshot, 1), ConnectorStatus::Occupied);
    assert!(snapshot.availability_operative);
    // The EVSE snapshot agrees with the status: a held EVSE is not operative.
    assert!(!snapshot.evse(2).expect("EVSE").operative);
    assert!(snapshot.evse(1).expect("EVSE").operative);

    // Nothing new starts: plug and token on EVSE 2 do not open a transaction.
    let mark = rig.csms.mark();
    rig.send(Command2::PlugIn { evse: 2 }).await;
    let _ = rig
        .station
        .send(Command2::PresentToken {
            evse: Some(2),
            id_token: "TOKEN-2".to_owned(),
            token_type: IdTokenType::Central,
        })
        .await;
    assert_eq!(rig.statuses_after(mark).await, []);
    assert!(
        !rig.csms
            .calls_since(mark)
            .iter()
            .any(|call| call.action == "TransactionEvent"),
        "no transaction event after the request"
    );
    let snapshot = rig.station.snapshot();
    assert!(snapshot.evse(2).is_some_and(|e| e.transaction.is_none()));
    assert_eq!(connector_status(&snapshot, 2), ConnectorStatus::Unavailable);

    // The busy EVSE turns unavailable when its own transaction ends.
    let mark = rig.csms.mark();
    rig.end_transaction().await;
    wait_snapshot(&rig.station, "the station is inoperative", |s| {
        !s.availability_operative
    })
    .await;
    assert_eq!(
        connector_status(&rig.station.snapshot(), 1),
        ConnectorStatus::Unavailable
    );
    assert_eq!(
        rig.statuses_after(mark).await.last(),
        Some(&note(1, 1, "Unavailable"))
    );

    // Operative restores both EVSEs (both have an EV plugged in).
    let body = json!({"operationalStatus": "Operative"});
    assert_eq!(rig.change_availability(body).await, "Accepted");
    let snapshot = rig.station.snapshot();
    assert!(snapshot.availability_operative);
    assert_eq!(connector_status(&snapshot, 1), ConnectorStatus::Occupied);
    assert_eq!(connector_status(&snapshot, 2), ConnectorStatus::Occupied);
    rig.stop().await;
}

impl Rig {
    /// Makes the cable alone a start point, so no token and no energy offer is needed.
    async fn start_at_the_cable(&self) {
        let frame = self
            .csms
            .send_call(
                "SetVariables",
                json!({"setVariableData": [{
                    "attributeValue": "EVConnected",
                    "component": {"name": "TxCtrlr"},
                    "variable": {"name": "TxStartPoint"},
                }]}),
            )
            .await
            .expect("call answered");
        match frame {
            Frame::CallResult { payload, .. } => {
                assert_eq!(
                    payload["setVariableResult"][0]["attributeStatus"],
                    "Accepted"
                );
            }
            other => panic!("unexpected reply {other:?}"),
        }
    }

    /// Whether the CSMS received a `TransactionEvent` after `mark`, once everything the
    /// station did so far has been reported.
    async fn transaction_events_after(&self, mark: usize) -> usize {
        self.statuses_after(0).await;
        self.csms
            .calls_since(mark)
            .iter()
            .filter(|call| call.action == "TransactionEvent")
            .count()
    }
}

#[tokio::test]
async fn a_held_evse_starts_no_transaction_at_the_cable() {
    let rig = Rig::new().await;
    rig.start_at_the_cable().await;
    rig.send(Command2::PlugIn { evse: 1 }).await;
    wait_snapshot(&rig.station, "a transaction on EVSE 1", |s| {
        s.evse(1).is_some_and(|e| e.transaction.is_some())
    })
    .await;
    // The start of the first transaction has reached the CSMS before the mark.
    rig.csms
        .wait_for_call("TransactionEvent", STEP)
        .await
        .expect("the start is reported");
    let body = json!({"operationalStatus": "Inoperative"});
    assert_eq!(rig.change_availability(body).await, "Scheduled");
    let mark = rig.csms.mark();
    rig.send(Command2::PlugIn { evse: 2 }).await;
    assert_eq!(rig.transaction_events_after(mark).await, 0);
    assert!(
        rig.station
            .snapshot()
            .evse(2)
            .is_some_and(|e| e.transaction.is_none()),
        "the held EVSE stays without a transaction"
    );
    rig.end_transaction().await;
    let snapshot = wait_snapshot(&rig.station, "the station is inoperative", |s| {
        !s.availability_operative
    })
    .await;
    assert!(snapshot.evses.iter().all(|e| e.transaction.is_none()));
    rig.stop().await;
}

#[tokio::test]
async fn an_inoperative_evse_starts_no_transaction_at_the_cable() {
    let rig = Rig::new().await;
    rig.start_at_the_cable().await;
    let body = json!({"evse": {"id": 1}, "operationalStatus": "Inoperative"});
    assert_eq!(rig.change_availability(body).await, "Accepted");
    let mark = rig.csms.mark();
    rig.send(Command2::PlugIn { evse: 1 }).await;
    assert_eq!(rig.transaction_events_after(mark).await, 0);
    assert!(
        rig.station
            .snapshot()
            .evse(1)
            .is_some_and(|e| e.transaction.is_none())
    );
    rig.stop().await;
}

#[tokio::test]
async fn an_unplug_that_ends_the_waiting_transaction_reports_unavailable_once() {
    let rig = Rig::new().await;
    rig.start_transaction().await;
    let body = json!({"operationalStatus": "Inoperative"});
    assert_eq!(rig.change_availability(body).await, "Scheduled");
    // Barrier: the hold of EVSE 2 has been reported before the mark.
    rig.statuses_after(0).await;
    let mark = rig.csms.mark();
    rig.send(Command2::Unplug { evse: 1 }).await;
    wait_snapshot(&rig.station, "the station is inoperative", |s| {
        !s.availability_operative
    })
    .await;
    assert_eq!(rig.statuses_after(mark).await, [note(1, 1, "Unavailable")]);
    rig.stop().await;
}

/// A station whose clock the test advances.
fn spawn_with_clock(csms: &MockCsms) -> (Station2Handle, JoinHandle<()>, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(Utc::now()));
    let mut config = StationConfig::new(csms.url())
        .with_connectors(2)
        .with_call_timeout_ms(2_000)
        .with_heartbeat_interval_secs(1)
        .with_reconnect(ReconnectConfig {
            enabled: true,
            initial_delay_ms: 1_000,
            max_delay_ms: 1_000,
            max_attempts: 0,
        })
        .without_persistence();
    config.ws_ping_interval_secs = 0;
    Protocol::V201
        .token()
        .clone_into(&mut config.ws_subprotocol);
    config.protocol = Protocol::V201;
    let (station, task) = Station2::spawn(config, clock.clone());
    (station, task, clock)
}

/// Drops the link, plugs an EV into EVSE 1 while offline, lets `offline` pass on the station
/// clock, waits for the reconnect and returns the notifications that followed.
async fn statuses_after_offline_change(offline: Duration) -> Vec<(i64, i64, String)> {
    let csms = v201_csms().await;
    let (station, task, clock) = spawn_with_clock(&csms);
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    wait_statuses(&csms, 0, 2).await;
    // The clock is frozen: move it so the boot-time state is older than the outage.
    clock.advance(Duration::seconds(1));
    let mark = csms.mark();
    csms.drop_connection().expect("drop the connection");
    // The first reconnect attempt is delayed, so the change happens while the link is down.
    wait_snapshot(&station, "the link to drop", |s| {
        s.connection != ConnectionState::Connected
    })
    .await;
    station
        .send(Command2::PlugIn { evse: 1 })
        .await
        .expect("plug in");
    clock.advance(offline);
    // The heartbeat follows the status reports (B04 step 5).
    csms.wait_for_call_after(mark, "Heartbeat", |_| true, STEP)
        .await
        .expect("heartbeat after reconnect");
    let seen = statuses(&csms, mark);
    let _ = station.shutdown().await;
    let _ = task.await;
    csms.shutdown().await;
    seen
}

#[tokio::test]
async fn short_outage_reports_only_the_connector_that_changed() {
    let seen = statuses_after_offline_change(Duration::seconds(5)).await;
    assert_eq!(seen, [note(1, 1, "Occupied")]);
}

#[tokio::test]
async fn long_outage_reports_every_connector() {
    let seen = statuses_after_offline_change(Duration::seconds(3_600)).await;
    assert_eq!(seen, [note(1, 1, "Occupied"), note(2, 1, "Available")]);
}
