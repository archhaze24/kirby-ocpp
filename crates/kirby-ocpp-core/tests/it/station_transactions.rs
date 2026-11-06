//! Transactions, authorization, offline sync, metering and retries
//! (ports of the TS smoke scenarios baseline/status-transactions/negative-csms and of
//! `station-offline-transactions.test.mjs` / `station-reconnect.test.mjs`).

use std::sync::Arc;
use std::time::Duration;

use crate::station_support::{Rig, STEP, incrementing, t0, tag, test_config, wait_state};
use kirby_ocpp_core::StationError;
use kirby_ocpp_core::ids::{ConnectorId, TxId};
use kirby_ocpp_core::ocpp::types::{
    AuthorizationStatus, ConnectorStatus, RegistrationStatus, StopReason,
};
use kirby_ocpp_core::station::{Command, Outcome, TransactionState};
use kirby_ocpp_core::{ManualClock, Station};
use kirby_ocpp_testkit::{CsmsOptions, Fault};

const C1: ConnectorId = ConnectorId::new(1);

async fn booted() -> Rig {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    rig
}

fn conn1(rig: &Rig) -> kirby_ocpp_core::station::ConnectorSnapshot {
    rig.station.snapshot().connector(C1).unwrap().clone()
}

#[tokio::test]
async fn authorize_command_decisions() {
    let rig = booted().await;
    for (id_tag, accepted, status) in [
        ("TAG1", true, AuthorizationStatus::Accepted),
        ("BADTAG", false, AuthorizationStatus::Blocked),
        ("EXPIREDAUTH", false, AuthorizationStatus::Expired),
        ("CONCURRENTAUTH", false, AuthorizationStatus::ConcurrentTx),
    ] {
        let outcome = rig
            .send(Command::Authorize {
                id_tag: tag(id_tag),
            })
            .await;
        let Outcome::Authorized(decision) = outcome else {
            panic!("expected a decision for {id_tag}");
        };
        assert_eq!(decision.accepted, accepted, "{id_tag}");
        assert_eq!(decision.id_tag_info.status, status, "{id_tag}");
    }
    let Outcome::Authorized(parent) = rig
        .send(Command::Authorize {
            id_tag: tag("PARENTAUTH"),
        })
        .await
    else {
        panic!()
    };
    assert_eq!(parent.id_tag_info.parent_id_tag, Some(tag("PARENTTAG")));
    rig.shutdown().await;
}

#[tokio::test]
async fn start_and_stop_a_transaction() {
    let rig = booted().await;
    let mark = rig.csms.mark();
    let Outcome::TransactionStarted { connector, tx_id } = rig.begin(1, "TAG1").await else {
        panic!("expected a started transaction");
    };
    assert_eq!((connector, tx_id), (C1, TxId::Csms(123)));
    let sent = rig
        .csms
        .wait_for_call_after(mark, "StartTransaction", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(sent["idTag"], "TAG1");
    assert_eq!(sent["meterStart"], 0);
    // Auto-plug: Preparing, then Charging.
    rig.wait_status(mark, 1, "Preparing").await;
    rig.wait_status(mark, 1, "Charging").await;
    let c = conn1(&rig);
    assert!(matches!(c.transaction, TransactionState::Active(_)));
    assert!(c.ev_connected);

    // A second start is refused without side effects.
    let err = rig
        .station
        .send(Command::StartTransaction {
            connector: C1,
            id_tag: tag("TAG1"),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StationError::InvalidState { .. }), "{err:?}");

    let mark = rig.csms.mark();
    rig.stop(1, StopReason::Local).await;
    let stop = rig
        .csms
        .wait_for_call_after(mark, "StopTransaction", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(stop["transactionId"], 123);
    assert_eq!(stop["reason"], "Local");
    let c = conn1(&rig);
    assert!(c.transaction.is_idle());
    assert_eq!(c.status, ConnectorStatus::Finishing, "EV still connected");
    rig.wait_status(mark, 1, "Finishing").await;
    // A stop without a transaction is an error.
    let err = rig
        .station
        .send(Command::StopTransaction {
            connector: C1,
            reason: StopReason::Local,
            id_tag: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StationError::InvalidState { .. }), "{err:?}");
    rig.shutdown().await;
}

#[tokio::test]
async fn csms_rejected_id_tags_leave_no_state_and_close_the_remote_transaction() {
    let rig = booted().await;
    for (id_tag, status) in [
        ("BLOCKSTART", AuthorizationStatus::Blocked),
        ("EXPIREDSTART", AuthorizationStatus::Expired),
        ("CONCURRENTSTART", AuthorizationStatus::ConcurrentTx),
    ] {
        let mark = rig.csms.mark();
        let outcome = rig.begin(1, id_tag).await;
        assert_eq!(outcome, Outcome::TransactionRejected { status }, "{id_tag}");
        assert!(conn1(&rig).transaction.is_idle(), "{id_tag}");
        let stop = rig
            .csms
            .wait_for_call_after(mark, "StopTransaction", |_| true, STEP)
            .await
            .unwrap();
        assert_eq!(stop["reason"], "DeAuthorized");
        assert_eq!(stop["transactionId"], 123);
        rig.send(Command::Unplug { connector: C1 }).await;
    }
    rig.shutdown().await;
}

#[tokio::test]
async fn local_pre_authorize_blocks_unknown_tags_before_any_side_effect() {
    let rig = booted().await;
    rig.cfg("LocalPreAuthorize", "true").await;
    let mark = rig.csms.mark();
    let outcome = rig.begin(1, "UNKNOWNLOCAL").await;
    assert!(
        matches!(outcome, Outcome::TransactionRejected { .. }),
        "{outcome:?}"
    );
    rig.assert_no_calls("StartTransaction", mark);
    let c = conn1(&rig);
    assert!(!c.ev_connected, "validation happens before the auto-plug");
    assert_eq!(c.status, ConnectorStatus::Available);
    rig.shutdown().await;
}

#[tokio::test]
async fn unplug_and_fault_stop_a_running_transaction() {
    let rig = booted().await;
    rig.begin(1, "TAG1").await;
    let mark = rig.csms.mark();
    rig.send(Command::Unplug { connector: C1 }).await;
    let stop = rig
        .csms
        .wait_for_call_after(mark, "StopTransaction", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(stop["reason"], "EVDisconnected");
    let c = conn1(&rig);
    assert!(!c.ev_connected);
    assert_eq!(c.status, ConnectorStatus::Available);

    rig.begin(1, "TAG1").await;
    let mark = rig.csms.mark();
    rig.send(Command::Fault {
        connector: C1,
        error_code: kirby_ocpp_core::ocpp::types::ChargePointErrorCode::OtherError,
        info: None,
        vendor_error_code: None,
    })
    .await;
    let stop = rig
        .csms
        .wait_for_call_after(mark, "StopTransaction", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(stop["reason"], "Other");
    rig.wait_status(mark, 1, "Faulted").await;
    assert_eq!(conn1(&rig).status, ConnectorStatus::Faulted);
    rig.shutdown().await;
}

#[tokio::test]
async fn ev_side_disconnect_can_suspend_instead_of_stopping() {
    let rig = booted().await;
    rig.cfg("StopTransactionOnEVSideDisconnect", "false").await;
    rig.begin(1, "TAG1").await;
    let mark = rig.csms.mark();
    rig.send(Command::Unplug { connector: C1 }).await;
    rig.wait_status(mark, 1, "SuspendedEV").await;
    assert!(matches!(
        conn1(&rig).transaction,
        TransactionState::Active(_)
    ));
    rig.assert_no_calls("StopTransaction", mark);
    rig.station.plug_in(C1).await.unwrap();
    rig.wait_status(mark, 1, "Charging").await;
    rig.shutdown().await;
}

#[tokio::test]
async fn a_second_stop_while_one_is_in_flight_is_refused_and_a_failed_stop_is_queued() {
    let mut rig = Rig::with(CsmsOptions::default(), |mut c| {
        c.call_timeout_ms = 500;
        c
    })
    .await;
    rig.connect_booted().await;
    rig.begin(1, "TAG1").await;
    rig.csms.inject(Fault::silence("StopTransaction"));

    let station = rig.station.clone();
    let first = tokio::spawn(async move {
        station
            .send(Command::StopTransaction {
                connector: C1,
                reason: StopReason::Local,
                id_tag: None,
            })
            .await
    });
    wait_state(&rig.station, "stopping", |s| {
        matches!(
            s.connector(C1).unwrap().transaction,
            TransactionState::Stopping(_)
        )
    })
    .await;
    let err = rig
        .station
        .send(Command::StopTransaction {
            connector: C1,
            reason: StopReason::Local,
            id_tag: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StationError::InvalidState { .. }), "{err:?}");

    // The silent CSMS makes the first stop time out: it is queued, nothing is lost.
    assert!(first.await.unwrap().is_err());
    let snapshot = wait_state(&rig.station, "queued stop", |s| {
        s.pending_offline_messages == 1
    })
    .await;
    assert!(snapshot.connector(C1).unwrap().transaction.is_idle());

    rig.send(Command::Reconnect).await;
    rig.csms
        .wait_for_call_count_after(0, "StopTransaction", 2, |_| true, STEP)
        .await
        .unwrap();
    wait_state(&rig.station, "queue drained", |s| {
        s.pending_offline_messages == 0
    })
    .await;
    rig.shutdown().await;
}

#[tokio::test]
async fn a_disconnect_during_a_pending_start_leaves_no_phantom_transaction() {
    let rig = booted().await;
    rig.csms.inject(Fault::silence("StartTransaction"));
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
    rig.station.disconnect().await.unwrap();
    assert!(start.await.unwrap().is_err());
    let snapshot = rig.station.snapshot();
    assert!(snapshot.connector(C1).unwrap().transaction.is_idle());
    assert_eq!(snapshot.pending_offline_messages, 0);

    rig.station.connect().await.unwrap();
    rig.wait_registration(RegistrationStatus::Accepted).await;
    rig.csms
        .wait_for_call_count_after(0, "StatusNotification", 6, |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(rig.calls("StartTransaction", 0).len(), 1, "never re-sent");
    rig.shutdown().await;
}

#[tokio::test]
async fn transaction_messages_are_retried_after_a_callerror() {
    let rig = booted().await;
    rig.cfg("TransactionMessageAttempts", "2").await;
    rig.cfg("TransactionMessageRetryInterval", "1").await;
    rig.csms.inject(Fault::call_error("StartTransaction"));
    let outcome = rig.begin(1, "TAG1").await;
    assert!(
        matches!(outcome, Outcome::TransactionStarted { .. }),
        "{outcome:?}"
    );
    assert_eq!(rig.calls("StartTransaction", 0).len(), 2);
    rig.shutdown().await;
}

#[tokio::test]
async fn transaction_messages_are_retried_after_a_timeout() {
    let mut rig = Rig::with(CsmsOptions::default(), |mut c| {
        c.call_timeout_ms = 400;
        c
    })
    .await;
    rig.connect_booted().await;
    rig.cfg("TransactionMessageAttempts", "2").await;
    rig.cfg("TransactionMessageRetryInterval", "1").await;
    rig.csms.inject(Fault::silence("StartTransaction"));
    let outcome = rig.begin(1, "TAG1").await;
    assert!(
        matches!(outcome, Outcome::TransactionStarted { .. }),
        "{outcome:?}"
    );
    assert_eq!(rig.calls("StartTransaction", 0).len(), 2);
    rig.shutdown().await;
}

async fn allow_offline(rig: &Rig) {
    rig.cfg("LocalAuthorizeOffline", "true").await;
    rig.cfg("AllowOfflineTxForUnknownId", "true").await;
}

#[tokio::test]
async fn offline_transactions_sync_in_order_before_the_status_resend() {
    let mut rig = Rig::with(incrementing(), |c| c).await;
    rig.connect_booted().await;
    allow_offline(&rig).await;
    rig.station.disconnect().await.unwrap();
    rig.csms.wait_for_disconnect(STEP).await.unwrap();

    // Offline: start, stop (queued, connector idle again), start again.
    let Outcome::TransactionStarted { tx_id: first, .. } = rig.begin(1, "OFFTAG").await else {
        panic!("offline start");
    };
    assert!(first.is_local());
    assert_eq!(conn1(&rig).status, ConnectorStatus::Charging);
    assert_eq!(rig.station.snapshot().pending_offline_messages, 1);
    rig.stop(1, StopReason::Local).await;
    assert!(conn1(&rig).transaction.is_idle());
    assert_eq!(rig.station.snapshot().pending_offline_messages, 2);
    rig.send(Command::Unplug { connector: C1 }).await;
    let Outcome::TransactionStarted { tx_id: second, .. } = rig.begin(1, "OFFTAG2").await else {
        panic!("second offline start");
    };
    assert!(second.is_local() && second != first);

    let mark = rig.csms.mark();
    rig.station.connect().await.unwrap();
    wait_state(&rig.station, "synced", |s| {
        s.pending_offline_messages == 0
            && matches!(
                s.connector(C1).unwrap().transaction,
                TransactionState::Active(_)
            )
    })
    .await;
    assert_eq!(conn1(&rig).tx_id(), Some(TxId::Csms(101)));
    rig.csms
        .wait_for_call_count_after(mark, "StatusNotification", 3, |_| true, STEP)
        .await
        .unwrap();

    let order: Vec<String> = rig
        .csms
        .calls_since(mark)
        .into_iter()
        .map(|c| c.action)
        .filter(|a| a != "BootNotification")
        .collect();
    let first_status = order
        .iter()
        .position(|a| a == "StatusNotification")
        .unwrap();
    let last_tx = order
        .iter()
        .rposition(|a| a == "StartTransaction" || a == "StopTransaction")
        .unwrap();
    assert!(
        last_tx < first_status,
        "sync before status re-send: {order:?}"
    );
    let tx_order: Vec<&str> = order
        .iter()
        .map(String::as_str)
        .filter(|a| *a == "StartTransaction" || *a == "StopTransaction")
        .collect();
    assert_eq!(
        tx_order,
        ["StartTransaction", "StopTransaction", "StartTransaction"],
        "chronological, one entry at a time"
    );
    let stop = rig.calls("StopTransaction", mark);
    assert_eq!(stop.len(), 1);
    assert_eq!(
        stop[0]["transactionId"], 100,
        "the real id of the first transaction"
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn offline_transactions_survive_a_station_restart() {
    let mut rig = Rig::with(incrementing(), |c| c).await;
    rig.connect_booted().await;
    allow_offline(&rig).await;
    rig.station.disconnect().await.unwrap();
    rig.csms.wait_for_disconnect(STEP).await.unwrap();
    rig.begin(1, "OFFTAG").await;
    rig.send(Command::MeterValues {
        connector: C1,
        wh_delta: 40,
        measurands: None,
    })
    .await;

    // Restart the station on the same state directory.
    rig.station.shutdown().await.unwrap();
    rig.join.take().unwrap().await.unwrap();
    let config = test_config(rig.csms.url(), &rig.dir);
    let (station, join) = Station::spawn(config, Arc::new(ManualClock::new(t0())));
    let restored = station.snapshot();
    let c = restored.connector(C1).unwrap();
    assert!(
        matches!(c.transaction, TransactionState::PendingStart(_)),
        "{:?}",
        c.transaction
    );
    assert_eq!(c.status, ConnectorStatus::Charging);
    assert_eq!(c.meter_wh, 40);
    assert_eq!(restored.pending_offline_messages, 1);

    let mark = rig.csms.mark();
    station.connect().await.unwrap();
    let started = rig
        .csms
        .wait_for_call_after(mark, "StartTransaction", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(started["idTag"], "OFFTAG");
    assert_eq!(started["meterStart"], 0);
    wait_state(&station, "active", |s| {
        s.connector(C1).unwrap().tx_id() == Some(TxId::Csms(100)) && s.pending_offline_messages == 0
    })
    .await;
    station.shutdown().await.unwrap();
    join.await.unwrap();
    rig.csms.shutdown().await;
}

#[tokio::test]
async fn offline_authorization_gates_offline_starts() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    // With the offline switches off nothing allows an unknown tag offline.
    rig.cfg("LocalAuthorizeOffline", "false").await;
    rig.cfg("AllowOfflineTxForUnknownId", "false").await;
    rig.station.disconnect().await.unwrap();
    rig.csms.wait_for_disconnect(STEP).await.unwrap();
    let outcome = rig.begin(1, "STRANGER").await;
    assert!(
        matches!(outcome, Outcome::TransactionRejected { .. }),
        "{outcome:?}"
    );
    assert!(conn1(&rig).transaction.is_idle());
    assert_eq!(rig.station.snapshot().pending_offline_messages, 0);
    rig.shutdown().await;
}

#[tokio::test]
async fn stock_station_starts_an_unknown_tag_offline() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    rig.station.disconnect().await.unwrap();
    rig.csms.wait_for_disconnect(STEP).await.unwrap();
    let outcome = rig.begin(1, "STRANGER").await;
    assert!(
        matches!(outcome, Outcome::TransactionStarted { .. }),
        "{outcome:?}"
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn periodic_meter_values_advance_the_meter_and_carry_the_transaction_id() {
    let rig = booted().await;
    rig.begin(1, "TAG1").await;
    // The command: 120 Wh more, one sample with the transaction id.
    let mark = rig.csms.mark();
    rig.send(Command::MeterValues {
        connector: C1,
        wh_delta: 120,
        measurands: None,
    })
    .await;
    let sent = rig
        .csms
        .wait_for_call_after(mark, "MeterValues", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(sent["transactionId"], 123);
    assert_eq!(conn1(&rig).meter_wh, 120);

    rig.cfg("MeterValueSampleInterval", "1").await;
    let mark = rig.csms.mark();
    let periodic = rig
        .csms
        .wait_for_call_after(
            mark,
            "MeterValues",
            |p| p["meterValue"][0]["sampledValue"][0]["context"] == "Sample.Periodic",
            STEP,
        )
        .await
        .unwrap();
    assert_eq!(periodic["connectorId"], 1);
    // 7.2 kW for about a second is about 2 Wh on top of the 120 Wh, counted once.
    let wh = conn1(&rig).meter_wh;
    assert!((122..130).contains(&wh), "{wh}");
    rig.shutdown().await;
}

#[tokio::test]
async fn the_meter_advances_offline_but_samples_are_dropped() {
    let rig = booted().await;
    rig.begin(1, "TAG1").await;
    rig.cfg("MeterValueSampleInterval", "1").await;
    rig.station.disconnect().await.unwrap();
    rig.csms.wait_for_disconnect(STEP).await.unwrap();
    let mark = rig.csms.mark();
    wait_state(&rig.station, "meter advanced offline", |s| {
        s.connector(C1).unwrap().meter_wh >= 2
    })
    .await;
    rig.assert_no_calls("MeterValues", mark);
    rig.shutdown().await;
}

#[tokio::test]
async fn clock_aligned_samples_cover_idle_connectors_and_add_no_energy() {
    let rig = booted().await;
    rig.cfg("ClockAlignedDataInterval", "1").await;
    let samples = rig
        .csms
        .wait_for_call_count_after(
            0,
            "MeterValues",
            2,
            |p| p["meterValue"][0]["sampledValue"][0]["context"] == "Sample.Clock",
            STEP,
        )
        .await
        .unwrap();
    let mut ids: Vec<u64> = samples
        .iter()
        .map(|p| p["connectorId"].as_u64().unwrap())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, [1, 2]);
    assert!(samples.iter().all(|p| p.get("transactionId").is_none()));
    assert_eq!(conn1(&rig).meter_wh, 0);
    rig.shutdown().await;
}

#[tokio::test]
async fn stop_transaction_carries_the_configured_transaction_data() {
    let rig = booted().await;
    rig.cfg("StopTxnSampledData", "Energy.Active.Import.Register")
        .await;
    rig.begin(1, "TAG1").await;
    rig.send(Command::MeterValues {
        connector: C1,
        wh_delta: 30,
        measurands: None,
    })
    .await;
    let mark = rig.csms.mark();
    rig.stop(1, StopReason::Local).await;
    let stop = rig
        .csms
        .wait_for_call_after(mark, "StopTransaction", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(stop["meterStop"], 30);
    assert_eq!(
        stop["transactionData"][0]["sampledValue"][0]["context"],
        "Transaction.End"
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn silence_is_not_mistaken_for_a_started_transaction() {
    // Start times out with a single attempt: an error, and the connector is free again.
    let mut rig = Rig::with(CsmsOptions::default(), |mut c| {
        c.call_timeout_ms = 300;
        c
    })
    .await;
    rig.connect_booted().await;
    rig.csms.inject(Fault::silence("StartTransaction"));
    let err = rig
        .station
        .send(Command::StartTransaction {
            connector: C1,
            id_tag: tag("TAG1"),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StationError::Call(_)), "{err:?}");
    let c = conn1(&rig);
    assert!(c.transaction.is_idle());
    assert_eq!(
        c.status,
        ConnectorStatus::Preparing,
        "the EV stays plugged in"
    );
    // And a new start works.
    let outcome = rig.begin(1, "TAG1").await;
    assert!(matches!(outcome, Outcome::TransactionStarted { .. }));
    let _ = Duration::ZERO;
    rig.shutdown().await;
}

#[tokio::test]
async fn a_full_offline_queue_never_drops_the_entry_of_a_running_transaction() {
    use kirby_ocpp_core::ids::IdTag;
    use kirby_ocpp_core::station::offline::{MAX_QUEUED_TRANSACTIONS, OfflineTx};
    use kirby_ocpp_core::station::transaction::PendingStart;
    use kirby_ocpp_core::store::{PersistedConnector, PersistedStation, StationStore};

    let dir = tempfile::tempdir().unwrap();
    let config = test_config("ws://127.0.0.1:9/ocpp", &dir);
    let store = StationStore::new(dir.path(), &config.charge_point_id);
    let mut state = PersistedStation::new(config.charge_point_id.clone());
    state.connector_count = 2;
    state
        .configuration
        .insert("AllowOfflineTxForUnknownId".to_owned(), "true".to_owned());
    state
        .configuration
        .insert("LocalAuthorizeOffline".to_owned(), "true".to_owned());
    let pending = PendingStart {
        tx_id: TxId::Local(1),
        id_tag: IdTag::new("TAG1").unwrap(),
        started_at: t0(),
        meter_start_wh: 0,
        reservation_id: None,
    };
    state.connectors.push(PersistedConnector {
        id: C1,
        status: ConnectorStatus::Charging,
        availability: kirby_ocpp_core::ocpp::types::AvailabilityType::Operative,
        pending_availability: None,
        ev_connected: true,
        meter_wh: 0,
        last_id_tag: None,
        reservation: None,
        transaction: TransactionState::PendingStart(pending.clone()),
    });
    // The running transaction owns the oldest entry; the queue is already full.
    for key in 1..=MAX_QUEUED_TRANSACTIONS as u64 {
        let connector = if key == 1 { C1 } else { ConnectorId::new(2) };
        let dropped = state.offline_queue.push(OfflineTx {
            key,
            connector,
            tx_id: TxId::Local(key),
            id_tag: pending.id_tag.clone(),
            started_at: t0(),
            meter_start_wh: 0,
            reservation_id: None,
            start_pending: true,
            stop: None,
        });
        assert!(dropped.is_none(), "not over the bound yet");
    }
    store.save(&state).unwrap();

    let (station, join) = Station::spawn(config, Arc::new(ManualClock::new(t0())));
    let snapshot = station.snapshot();
    assert!(!snapshot.connector(C1).unwrap().transaction.is_idle());
    // A new offline transaction on connector 2 overflows the queue.
    let outcome = station
        .send(Command::StartTransaction {
            connector: ConnectorId::new(2),
            id_tag: tag("TAG2"),
        })
        .await;
    assert!(
        matches!(outcome, Ok(Outcome::TransactionStarted { .. })),
        "{outcome:?}"
    );

    // The running transaction on connector 1 keeps its entry, so it can still be stopped
    // and delivered later; an entry of nobody's connector went instead.
    let one = station.snapshot().connector(C1).unwrap().clone();
    assert_eq!(one.transaction.tx_id(), Some(TxId::Local(1)));
    assert_eq!(
        station.snapshot().pending_offline_messages as usize,
        MAX_QUEUED_TRANSACTIONS
    );
    let stopped = station
        .send(Command::StopTransaction {
            connector: C1,
            reason: StopReason::Local,
            id_tag: None,
        })
        .await;
    assert!(stopped.is_ok(), "{stopped:?}");
    assert!(
        station
            .snapshot()
            .connector(C1)
            .unwrap()
            .transaction
            .is_idle()
    );
    station.shutdown().await.unwrap();
    join.await.unwrap();
}

/// The CSMS-side order of transaction messages after `mark`: `(action, transactionId)`.
fn tx_messages(rig: &Rig, mark: usize) -> Vec<(String, Option<i64>)> {
    rig.csms
        .calls_since(mark)
        .into_iter()
        .filter(|c| {
            ["StartTransaction", "MeterValues", "StopTransaction"].contains(&c.action.as_str())
        })
        .map(|c| (c.action, c.payload["transactionId"].as_i64()))
        .collect()
}

async fn offline_sample(rig: &Rig, wh: u64) {
    rig.clock.advance(chrono::TimeDelta::seconds(1));
    rig.send(Command::MeterValues {
        connector: C1,
        wh_delta: wh,
        measurands: None,
    })
    .await;
}

#[tokio::test]
async fn offline_meter_values_follow_their_start_and_precede_their_stop() {
    let mut rig = Rig::with(incrementing(), |c| c).await;
    rig.connect_booted().await;
    allow_offline(&rig).await;
    rig.station.disconnect().await.unwrap();
    rig.csms.wait_for_disconnect(STEP).await.unwrap();
    rig.begin(1, "OFFTAG").await;
    offline_sample(&rig, 10).await;
    offline_sample(&rig, 20).await;
    rig.stop(1, StopReason::Local).await;

    let mark = rig.csms.mark();
    rig.station.connect().await.unwrap();
    rig.csms
        .wait_for_call_after(mark, "StopTransaction", |_| true, STEP)
        .await
        .unwrap();
    // Start (real id assigned), the samples under the real id, in the order taken, then stop.
    assert_eq!(
        tx_messages(&rig, mark),
        vec![
            ("StartTransaction".to_owned(), None),
            ("MeterValues".to_owned(), Some(100)),
            ("MeterValues".to_owned(), Some(100)),
            ("StopTransaction".to_owned(), Some(100)),
        ]
    );
    let samples = rig.calls("MeterValues", mark);
    let stamp =
        |v: &serde_json::Value| v["meterValue"][0]["timestamp"].as_str().unwrap().to_owned();
    assert!(stamp(&samples[0]) < stamp(&samples[1]));
    rig.shutdown().await;
}

#[tokio::test]
async fn offline_meter_values_of_an_online_transaction_survive_a_restart() {
    let mut rig = Rig::with(incrementing(), |c| c).await;
    rig.connect_booted().await;
    rig.begin(1, "TAG1").await;
    wait_state(&rig.station, "active", |s| {
        s.connector(C1).unwrap().tx_id() == Some(TxId::Csms(100))
    })
    .await;
    rig.station.disconnect().await.unwrap();
    rig.csms.wait_for_disconnect(STEP).await.unwrap();
    offline_sample(&rig, 10).await;
    offline_sample(&rig, 20).await;

    rig.station.shutdown().await.unwrap();
    rig.join.take().unwrap().await.unwrap();
    let config = test_config(rig.csms.url(), &rig.dir);
    let (station, join) = Station::spawn(config, Arc::new(rig.clock.clone()));
    let mark = rig.csms.mark();
    station.connect().await.unwrap();
    rig.csms
        .wait_for_call_count_after(mark, "MeterValues", 2, |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(
        tx_messages(&rig, mark),
        vec![
            ("MeterValues".to_owned(), Some(100)),
            ("MeterValues".to_owned(), Some(100)),
        ]
    );
    // Delivered samples are gone from the persisted queue (the last answer may still be on its
    // way; the shutdown then keeps its sample, so wait until the CSMS answered both).
    let read_state = || -> serde_json::Value {
        let state = std::fs::read_dir(rig.dir.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.to_string_lossy().ends_with(".station.json"))
            .unwrap();
        serde_json::from_str(&std::fs::read_to_string(state).unwrap()).unwrap()
    };
    let deadline = std::time::Instant::now() + STEP;
    loop {
        // A flush writes the file once the queue changed.
        if read_state()["offline_samples"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
            || std::time::Instant::now() > deadline
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    station.shutdown().await.unwrap();
    join.await.unwrap();
    let value = read_state();
    assert_eq!(value["version"], 8);
    assert_eq!(
        value["offline_samples"]["items"].as_array().unwrap().len(),
        0
    );
    rig.csms.shutdown().await;
}

#[tokio::test]
async fn offline_meter_values_without_a_transaction_are_dropped() {
    let mut rig = Rig::with(incrementing(), |c| c).await;
    rig.connect_booted().await;
    rig.station.disconnect().await.unwrap();
    rig.csms.wait_for_disconnect(STEP).await.unwrap();
    offline_sample(&rig, 10).await;
    let mark = rig.csms.mark();
    rig.station.connect().await.unwrap();
    rig.wait_registration(RegistrationStatus::Accepted).await;
    rig.csms
        .wait_for_call_after(mark, "StatusNotification", |_| true, STEP)
        .await
        .unwrap();
    let sent = tx_messages(&rig, mark);
    assert_eq!(sent.len(), 0, "{sent:?}");
    rig.shutdown().await;
}
