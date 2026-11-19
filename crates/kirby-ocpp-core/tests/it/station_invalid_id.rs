//! `StopTransactionOnInvalidId=false`: a refused idTag in `StartTransaction.conf` only stops
//! the energy delivery (the transaction stays open, `SuspendedEVSE`); `MaxEnergyOnInvalidId`
//! is the energy that may still be delivered (`0`: none).

use crate::station_support::{Rig, incrementing};
use kirby_ocpp_core::ids::ConnectorId;
use kirby_ocpp_core::ocpp::types::ConnectorStatus;
use kirby_ocpp_core::station::{Command, Outcome, TransactionState};

const C1: ConnectorId = ConnectorId::new(1);

async fn booted() -> Rig {
    let mut rig = Rig::with(incrementing(), |config| config).await;
    rig.connect_booted().await;
    rig
}

fn conn1(rig: &Rig) -> kirby_ocpp_core::station::ConnectorSnapshot {
    rig.station.snapshot().connector(C1).unwrap().clone()
}

async fn barrier(rig: &Rig) {
    rig.send(Command::Heartbeat).await;
}

async fn meter(rig: &Rig, wh_delta: u64) {
    rig.send(Command::MeterValues {
        connector: C1,
        wh_delta,
        measurands: None,
    })
    .await;
}

#[tokio::test]
async fn without_an_energy_allowance_delivery_stops_at_once_and_the_transaction_stays_open() {
    let rig = booted().await;
    rig.cfg("StopTransactionOnInvalidId", "false").await;
    let mark = rig.csms.mark();
    let outcome = rig.begin(1, "BLOCKSTART").await;
    assert!(
        matches!(outcome, Outcome::TransactionStarted { .. }),
        "{outcome:?}"
    );
    rig.wait_status(mark, 1, "SuspendedEVSE").await;
    barrier(&rig).await;
    rig.assert_no_calls("StopTransaction", mark);
    let connector = conn1(&rig);
    assert_eq!(connector.status, ConnectorStatus::SuspendedEVSE);
    assert!(matches!(connector.transaction, TransactionState::Active(_)));
    // The refusal never showed up as a charging connector.
    assert!(
        !rig.calls("StatusNotification", mark)
            .iter()
            .any(|p| p["connectorId"] == 1 && p["status"] == "Charging"),
        "no Charging notification expected"
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn delivery_stops_after_the_allowance_and_the_transaction_stays_open() {
    let rig = booted().await;
    rig.cfg("StopTransactionOnInvalidId", "false").await;
    rig.cfg("MaxEnergyOnInvalidId", "50").await;
    let mark = rig.csms.mark();
    let outcome = rig.begin(1, "BLOCKSTART").await;
    assert!(
        matches!(outcome, Outcome::TransactionStarted { .. }),
        "{outcome:?}"
    );
    meter(&rig, 30).await;
    barrier(&rig).await;
    assert_eq!(
        conn1(&rig).status,
        ConnectorStatus::Charging,
        "below the allowance"
    );
    meter(&rig, 30).await;
    rig.wait_status(mark, 1, "SuspendedEVSE").await;
    barrier(&rig).await;
    rig.assert_no_calls("StopTransaction", mark);
    assert!(matches!(
        conn1(&rig).transaction,
        TransactionState::Active(_)
    ));
    rig.shutdown().await;
}

#[tokio::test]
async fn a_replug_does_not_resume_blocked_delivery() {
    let rig = booted().await;
    rig.cfg("StopTransactionOnInvalidId", "false").await;
    rig.cfg("StopTransactionOnEVSideDisconnect", "false").await;
    rig.begin(1, "BLOCKSTART").await;
    rig.send(Command::Unplug { connector: C1 }).await;
    rig.send(Command::PlugIn { connector: C1 }).await;
    assert_eq!(conn1(&rig).status, ConnectorStatus::SuspendedEVSE);
    rig.shutdown().await;
}

#[tokio::test]
async fn an_accepted_start_is_unaffected() {
    let rig = booted().await;
    rig.cfg("StopTransactionOnInvalidId", "false").await;
    rig.cfg("MaxEnergyOnInvalidId", "0").await;
    let mark = rig.csms.mark();
    rig.begin(1, "TAG1").await;
    rig.wait_status(mark, 1, "Charging").await;
    meter(&rig, 100_000).await;
    barrier(&rig).await;
    assert_eq!(conn1(&rig).status, ConnectorStatus::Charging);
    rig.shutdown().await;
}

#[tokio::test]
async fn with_stop_transaction_on_invalid_id_the_transaction_still_ends() {
    let rig = booted().await;
    rig.cfg("StopTransactionOnInvalidId", "true").await;
    let mark = rig.csms.mark();
    let outcome = rig.begin(1, "BLOCKSTART").await;
    assert!(
        matches!(outcome, Outcome::TransactionRejected { .. }),
        "{outcome:?}"
    );
    let stop = rig
        .csms
        .wait_for_call_after(
            mark,
            "StopTransaction",
            |_| true,
            crate::station_support::STEP,
        )
        .await
        .unwrap();
    assert_eq!(stop["reason"], "DeAuthorized");
    rig.shutdown().await;
}
