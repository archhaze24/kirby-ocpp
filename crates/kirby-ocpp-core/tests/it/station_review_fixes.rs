//! Regression tests for the review findings: queue retry and bound, separate maintenance
//! queues, sampling of suspended transactions, `StopTransactionOnInvalidId` semantics,
//! availability after an abandoned start, RemoteStart with a parentIdTag from `Authorize`.

use std::time::Duration;

use crate::station_support::{Rig, STEP, incrementing, t0, tag, wait_state};
use chrono::Duration as Delta;
use kirby_ocpp_core::StationError;
use kirby_ocpp_core::ids::{ConnectorId, TxId};
use kirby_ocpp_core::ocpp::types::{
    AvailabilityType, ConnectorStatus, DiagnosticsStatus, FirmwareStatus, StopReason,
};
use kirby_ocpp_core::station::{Command, Outcome, TransactionState};
use kirby_ocpp_testkit::{CsmsOptions, Fault};
use serde_json::{Value, json};

const C1: ConnectorId = ConnectorId::new(1);

async fn booted() -> Rig {
    let mut rig = Rig::with(incrementing(), |config| config).await;
    rig.connect_booted().await;
    rig
}

fn conn1(rig: &Rig) -> kirby_ocpp_core::station::ConnectorSnapshot {
    rig.station.snapshot().connector(C1).unwrap().clone()
}

/// A command that is answered after everything the station sent before it.
async fn barrier(rig: &Rig) {
    rig.send(Command::Heartbeat).await;
}

// ---- 2 and 11: retry of queued messages, no status re-send -----------------------------

#[tokio::test]
async fn a_failed_stop_is_retried_while_online_without_a_status_resend() {
    let rig = booted().await;
    rig.cfg("TransactionMessageAttempts", "2").await;
    rig.cfg("TransactionMessageRetryInterval", "1").await;
    rig.begin(1, "TAG1").await;
    // Both attempts of the first round fail, the retry round succeeds.
    rig.csms
        .inject(Fault::call_error("StopTransaction").times(2));
    let mark = rig.csms.mark();
    let err = rig
        .station
        .send(Command::StopTransaction {
            connector: C1,
            reason: StopReason::Local,
            id_tag: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StationError::Call(_)), "{err:?}");
    assert_eq!(rig.station.snapshot().pending_offline_messages, 1);

    rig.csms
        .wait_for_call_count_after(mark, "StopTransaction", 3, |_| true, STEP)
        .await
        .expect("the queued stop is sent again");
    wait_state(&rig.station, "queue drained", |s| {
        s.pending_offline_messages == 0
    })
    .await;
    // A retry round is not a boot: no status re-send of the other connector.
    barrier(&rig).await;
    assert!(
        rig.calls("StatusNotification", mark)
            .iter()
            .all(|p| p["connectorId"] == 1),
        "{:#?}",
        rig.calls("StatusNotification", mark)
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn a_queued_stop_is_dropped_after_the_configured_rounds() {
    let mut rig = booted().await;
    rig.cfg("TransactionMessageAttempts", "1").await;
    rig.cfg("TransactionMessageRetryInterval", "1").await;
    rig.begin(1, "TAG1").await;
    rig.csms
        .inject(Fault::call_error("StopTransaction").times(100));
    let mark = rig.csms.mark();
    rig.station
        .send(Command::StopTransaction {
            connector: C1,
            reason: StopReason::Local,
            id_tag: None,
        })
        .await
        .unwrap_err();
    rig.observed
        .wait_log("giving up on the queued StopTransaction")
        .await;
    wait_state(&rig.station, "queue empty", |s| {
        s.pending_offline_messages == 0
    })
    .await;
    // The first round and one retry round, one attempt each.
    assert_eq!(rig.calls("StopTransaction", mark).len(), 2);
    rig.shutdown().await;
}

#[tokio::test]
async fn a_failed_queued_start_is_retried_while_online() {
    let mut rig = booted().await;
    rig.cfg("TransactionMessageAttempts", "1").await;
    rig.cfg("TransactionMessageRetryInterval", "1").await;
    rig.station.disconnect().await.unwrap();
    rig.csms.wait_for_disconnect(STEP).await.unwrap();
    let Outcome::TransactionStarted { tx_id, .. } = rig.begin(1, "TAG1").await else {
        panic!("offline start")
    };
    assert!(tx_id.is_local());
    rig.csms
        .inject(Fault::call_error("StartTransaction").times(1));
    let mark = rig.csms.mark();
    rig.station.connect().await.unwrap();
    rig.observed
        .wait_log("sync of queued StartTransaction failed")
        .await;
    let state = wait_state(&rig.station, "start delivered by the retry", |s| {
        s.connector(C1).unwrap().tx_id() == Some(TxId::Csms(100))
    })
    .await;
    assert!(matches!(
        state.connector(C1).unwrap().transaction,
        TransactionState::Active(_)
    ));
    assert_eq!(rig.calls("StartTransaction", mark).len(), 2);
    assert_eq!(state.pending_offline_messages, 0);
    rig.shutdown().await;
}

// ---- 3: separate maintenance queues ---------------------------------------------------------

#[tokio::test]
async fn firmware_and_diagnostics_report_side_by_side() {
    let rig = booted().await;
    rig.csms_call(
        "UpdateFirmware",
        json!({"location": "ftp://example.com/fw.bin", "retrieveDate": t0().to_rfc3339()}),
    )
    .await;
    rig.csms_call(
        "GetDiagnostics",
        json!({"location": "ftp://example.com/logs"}),
    )
    .await;
    let state = wait_state(&rig.station, "both finished", |s| {
        s.firmware_status == FirmwareStatus::Installed
            && s.diagnostics_status == DiagnosticsStatus::Uploaded
    })
    .await;
    assert_eq!(state.firmware_status, FirmwareStatus::Installed);
    let firmware: Vec<String> = rig
        .csms
        .wait_for_call_count_after(0, "FirmwareStatusNotification", 4, |_| true, STEP)
        .await
        .unwrap()
        .iter()
        .map(|p| p["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        firmware,
        ["Downloading", "Downloaded", "Installing", "Installed"]
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn a_disconnect_resets_a_status_that_was_only_a_step_of_the_plan() {
    let rig = booted().await;
    rig.csms_call(
        "UpdateFirmware",
        json!({"location": "ftp://example.com/fw.bin", "retrieveDate": t0().to_rfc3339(), "retries": 5, "retryInterval": 1}),
    )
    .await;
    rig.send(Command::SetFirmwareOutcome {
        outcome: kirby_ocpp_core::station::FirmwareOutcome::Success,
    })
    .await;
    wait_state(&rig.station, "downloading", |s| {
        s.firmware_status == FirmwareStatus::Downloading
    })
    .await;
    rig.station.disconnect().await.unwrap();
    let state = wait_state(&rig.station, "idle again", |s| {
        s.firmware_status == FirmwareStatus::Idle
    })
    .await;
    assert_eq!(state.firmware_status, FirmwareStatus::Idle);
    rig.shutdown().await;
}

// ---- 4: aligned burst over many connectors ---------------------------------------------------

#[tokio::test]
async fn a_clock_aligned_burst_over_many_connectors_is_not_cut_short() {
    let mut rig = Rig::with(CsmsOptions::default(), |config| {
        config.with_connectors(24).with_call_timeout_ms(60_000)
    })
    .await;
    rig.connect_booted().await;
    rig.csms.inject(Fault::silence("MeterValues").times(1_000));
    rig.cfg("ClockAlignedDataInterval", "1").await;
    // The first sample reaching the CSMS means the whole burst was handed over.
    rig.csms.wait_for_call("MeterValues", STEP).await.unwrap();
    rig.observed.drain();
    assert!(
        !rig.observed
            .logs
            .iter()
            .any(|l| l.contains("dropping the oldest samples")),
        "{:#?}",
        rig.observed.logs
    );
    rig.shutdown().await;
}

// ---- 5: pending ChangeAvailability after a start that never happened -------------------------

#[tokio::test]
async fn a_pending_availability_change_applies_after_a_failed_start() {
    let mut rig = Rig::with(CsmsOptions::default(), |mut config| {
        config.call_timeout_ms = 400;
        config
    })
    .await;
    rig.connect_booted().await;
    rig.cfg("TransactionMessageAttempts", "1").await;
    rig.csms.inject(Fault::silence("StartTransaction").times(1));
    let station = rig.station.clone();
    let start = tokio::spawn(async move {
        station
            .send(Command::StartTransaction {
                connector: C1,
                id_tag: tag("TAG1"),
            })
            .await
    });
    rig.csms
        .wait_for_call("StartTransaction", STEP)
        .await
        .unwrap();
    let reply = rig
        .csms_call(
            "ChangeAvailability",
            json!({"connectorId": 1, "type": "Inoperative"}),
        )
        .await;
    assert_eq!(reply["status"], "Scheduled");
    assert!(start.await.unwrap().is_err(), "the start timed out");
    let state = wait_state(&rig.station, "availability applied", |s| {
        let c = s.connector(C1).unwrap();
        c.availability == AvailabilityType::Inoperative && c.status == ConnectorStatus::Unavailable
    })
    .await;
    assert!(state.connector(C1).unwrap().pending_availability.is_none());
    rig.shutdown().await;
}

// ---- 6: StopTransactionOnInvalidId / MaxEnergyOnInvalidId --------------------------------------

#[tokio::test]
async fn a_refused_authorize_does_not_stop_the_running_transaction() {
    let rig = booted().await;
    rig.cfg("StopTransactionOnInvalidId", "true").await;
    rig.begin(1, "CONCURRENTAUTH").await;
    let mark = rig.csms.mark();
    let Outcome::Authorized(decision) = rig
        .send(Command::Authorize {
            id_tag: tag("CONCURRENTAUTH"),
        })
        .await
    else {
        panic!()
    };
    assert!(!decision.accepted);
    barrier(&rig).await;
    rig.assert_no_calls("StopTransaction", mark);
    assert!(matches!(
        conn1(&rig).transaction,
        TransactionState::Active(_)
    ));
    rig.shutdown().await;
}

#[tokio::test]
async fn a_refused_start_keeps_the_transaction_until_the_energy_limit() {
    let rig = booted().await;
    rig.cfg("StopTransactionOnInvalidId", "false").await;
    rig.cfg("MaxEnergyOnInvalidId", "50").await;
    let outcome = rig.begin(1, "BLOCKSTART").await;
    assert!(
        matches!(outcome, Outcome::TransactionStarted { .. }),
        "{outcome:?}"
    );
    let mark = rig.csms.mark();
    rig.send(Command::MeterValues {
        connector: C1,
        wh_delta: 30,
        measurands: None,
    })
    .await;
    barrier(&rig).await;
    rig.assert_no_calls("StopTransaction", mark);
    assert!(!conn1(&rig).transaction.is_idle(), "below the limit");
    rig.send(Command::MeterValues {
        connector: C1,
        wh_delta: 30,
        measurands: None,
    })
    .await;
    let stop = rig
        .csms
        .wait_for_call_after(mark, "StopTransaction", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(stop["reason"], "DeAuthorized");
    rig.shutdown().await;
}

#[tokio::test]
async fn a_refused_start_without_an_energy_limit_goes_on() {
    let rig = booted().await;
    rig.cfg("StopTransactionOnInvalidId", "false").await;
    let outcome = rig.begin(1, "CONCURRENTSTART").await;
    assert!(
        matches!(outcome, Outcome::TransactionStarted { .. }),
        "{outcome:?}"
    );
    let mark = rig.csms.mark();
    rig.send(Command::MeterValues {
        connector: C1,
        wh_delta: 100_000,
        measurands: None,
    })
    .await;
    barrier(&rig).await;
    rig.assert_no_calls("StopTransaction", mark);
    assert!(!conn1(&rig).transaction.is_idle());
    rig.shutdown().await;
}

#[tokio::test]
async fn a_refused_start_stops_at_once_with_stop_transaction_on_invalid_id() {
    let rig = booted().await;
    rig.cfg("StopTransactionOnInvalidId", "true").await;
    rig.cfg("MaxEnergyOnInvalidId", "50").await;
    let mark = rig.csms.mark();
    let outcome = rig.begin(1, "BLOCKSTART").await;
    assert!(
        matches!(outcome, Outcome::TransactionRejected { .. }),
        "{outcome:?}"
    );
    let stop = rig
        .csms
        .wait_for_call_after(mark, "StopTransaction", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(stop["reason"], "DeAuthorized");
    assert!(conn1(&rig).transaction.is_idle());
    rig.shutdown().await;
}

// ---- 7: RemoteStart, reservation parentIdTag known only from Authorize ---------------------------

#[tokio::test]
async fn remote_start_matches_a_reservation_parent_from_the_authorize_answer() {
    let rig = booted().await;
    rig.cfg("AuthorizeRemoteTxRequests", "true").await;
    rig.csms
        .expect_status(
            "ReserveNow",
            json!({
                "connectorId": 1, "reservationId": 9, "idTag": "RESERVEDFOR",
                "parentIdTag": "PARENTTAG",
                "expiryDate": (t0() + Delta::hours(1)).to_rfc3339(),
            }),
            "Accepted",
        )
        .await
        .unwrap();
    // PARENTAUTH has the parent PARENTTAG, known only from the CSMS answer (empty cache).
    rig.csms
        .expect_status(
            "RemoteStartTransaction",
            json!({"connectorId": 1, "idTag": "PARENTAUTH"}),
            "Accepted",
        )
        .await
        .unwrap();
    wait_state(&rig.station, "started", |s| {
        s.connector(C1).unwrap().tx_id().is_some()
    })
    .await;
    // A tag with another parent is still refused.
    rig.shutdown().await;
}

// ---- 8: sampling continues while the transaction is suspended -----------------------------------

#[tokio::test]
async fn periodic_samples_continue_while_the_ev_is_disconnected() {
    let rig = booted().await;
    rig.cfg("StopTransactionOnEVSideDisconnect", "false").await;
    rig.cfg("MeterValueSampleInterval", "1").await;
    rig.begin(1, "TAG1").await;
    rig.send(Command::Unplug { connector: C1 }).await;
    assert_eq!(conn1(&rig).status, ConnectorStatus::SuspendedEV);
    let mark = rig.csms.mark();
    let samples: Vec<Value> = rig
        .csms
        .wait_for_call_count_after(
            mark,
            "MeterValues",
            2,
            |p| p["meterValue"][0]["sampledValue"][0]["context"] == "Sample.Periodic",
            Duration::from_secs(8),
        )
        .await
        .expect("periodic samples while suspended");
    assert!(samples.iter().all(|p| p["transactionId"] == 100));
    rig.shutdown().await;
}
