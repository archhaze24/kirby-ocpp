//! State file wiring: restart round trip, quarantine of corrupt files, newer versions.

mod station_support;

use std::fs;
use std::sync::Arc;

use kirby_ocpp_core::ids::ConnectorId;
use kirby_ocpp_core::ocpp::types::{AvailabilityType, ConnectorStatus};
use kirby_ocpp_core::station::Command;
use kirby_ocpp_core::store::{LoadOutcome, StationStore};
use kirby_ocpp_core::{ManualClock, Station};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms};
use serde_json::{Value, json};
use station_support::{Observed, Rig, STEP, incrementing, t0, test_config};

async fn payload(csms: &MockCsms, action: &str, request: Value) -> Value {
    match csms.send_call(action, request).await.unwrap() {
        kirby_ocpp_core::ocpp::Frame::CallResult { payload, .. } => payload,
        other => panic!("unexpected {}", other.to_json_string()),
    }
}

#[tokio::test]
async fn availability_configuration_connectors_and_list_survive_a_restart() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(csms.url(), &dir);

    // First run: change things, then shut down.
    let (station, join) = Station::spawn(config.clone(), Arc::new(ManualClock::new(t0())));
    station.connect().await.unwrap();
    csms.wait_for_call_count_after(0, "StatusNotification", 3, |_| true, STEP)
        .await
        .unwrap();
    station.send(Command::AddConnector).await.unwrap();
    for (action, request) in [
        (
            "ChangeConfiguration",
            json!({"key": "MeterValueSampleInterval", "value": "42"}),
        ),
        (
            "ChangeConfiguration",
            json!({"key": "AuthorizationCacheEnabled", "value": "false"}),
        ),
        (
            "ChangeAvailability",
            json!({"connectorId": 1, "type": "Inoperative"}),
        ),
        (
            "SendLocalList",
            json!({"listVersion": 3, "updateType": "Full",
                   "localAuthorizationList": [{"idTag": "TAG1", "idTagInfo": {"status": "Accepted"}}]}),
        ),
    ] {
        let reply = payload(&csms, action, request).await;
        assert!(
            matches!(reply["status"].as_str(), Some("Accepted")),
            "{action}: {reply}"
        );
    }
    station.shutdown().await.unwrap();
    join.await.unwrap();
    csms.wait_for_disconnect(STEP).await.unwrap();

    // Second run: the restored state is visible before anything is connected.
    let (station, join) = Station::spawn(config, Arc::new(ManualClock::new(t0())));
    let snapshot = station.snapshot();
    assert_eq!(snapshot.connectors.len(), 3, "connector count restored");
    let first = snapshot.connector(ConnectorId::new(1)).unwrap();
    assert_eq!(first.availability, AvailabilityType::Inoperative);
    assert_eq!(first.status, ConnectorStatus::Unavailable);
    assert_eq!(
        snapshot
            .connector(ConnectorId::new(2))
            .unwrap()
            .availability,
        AvailabilityType::Operative
    );
    assert_eq!(snapshot.local_list_version, 3);

    let mark = csms.mark();
    station.connect().await.unwrap();
    // The restored unavailable connector is what gets reported after the boot.
    csms.wait_for_call_after(
        mark,
        "StatusNotification",
        |p| p["connectorId"] == 1 && p["status"] == "Unavailable",
        STEP,
    )
    .await
    .unwrap();
    for (key, expected) in [
        ("MeterValueSampleInterval", "42"),
        ("AuthorizationCacheEnabled", "false"),
        ("NumberOfConnectors", "3"),
    ] {
        let reply = payload(&csms, "GetConfiguration", json!({"key": [key]})).await;
        assert_eq!(reply["configurationKey"][0]["value"], expected, "{key}");
    }
    assert_eq!(
        payload(&csms, "GetLocalListVersion", json!({})).await["listVersion"],
        3
    );
    station.shutdown().await.unwrap();
    join.await.unwrap();
    csms.shutdown().await;
}

#[tokio::test]
async fn charge_point_availability_survives_a_restart() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(csms.url(), &dir);

    let (station, join) = Station::spawn(config.clone(), Arc::new(ManualClock::new(t0())));
    station.connect().await.unwrap();
    csms.wait_for_call_count_after(0, "StatusNotification", 3, |_| true, STEP)
        .await
        .unwrap();
    csms.expect_status(
        "ChangeAvailability",
        json!({"connectorId": 0, "type": "Inoperative"}),
        "Accepted",
    )
    .await
    .unwrap();
    station.shutdown().await.unwrap();
    join.await.unwrap();

    let (station, join) = Station::spawn(config, Arc::new(ManualClock::new(t0())));
    let snapshot = station.snapshot();
    assert_eq!(snapshot.availability, AvailabilityType::Inoperative);
    assert_eq!(snapshot.charge_point.status, ConnectorStatus::Unavailable);
    assert!(
        snapshot
            .connectors
            .iter()
            .all(|c| c.status == ConnectorStatus::Unavailable)
    );
    station.shutdown().await.unwrap();
    join.await.unwrap();
    csms.shutdown().await;
}

#[tokio::test]
async fn a_corrupt_state_file_is_quarantined_and_the_station_starts_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config("ws://127.0.0.1:9/ocpp", &dir);
    let store = StationStore::new(dir.path(), &config.charge_point_id);
    fs::write(store.path(), b"{ torn").unwrap();

    let (station, join) = Station::spawn(config, Arc::new(ManualClock::new(t0())));
    let mut observed = Observed::new(station.subscribe());
    let snapshot = station.snapshot();
    assert_eq!(snapshot.connectors.len(), 2);
    assert_eq!(snapshot.local_list_version, 0);
    observed.wait_log("starting fresh").await;
    let mut backup = store.path().as_os_str().to_owned();
    backup.push(".bak");
    assert_eq!(fs::read(&backup).unwrap(), b"{ torn");

    // The next change writes a valid file again.
    station.send(Command::AddConnector).await.unwrap();
    station.shutdown().await.unwrap();
    join.await.unwrap();
    let text = fs::read_to_string(store.path()).unwrap();
    let saved: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["connector_count"], 3);
}

#[tokio::test]
async fn a_newer_state_file_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config("ws://127.0.0.1:9/ocpp", &dir);
    let store = StationStore::new(dir.path(), &config.charge_point_id);
    let future = br#"{"version":99,"future":true}"#;
    fs::write(store.path(), future).unwrap();

    let (station, join) = Station::spawn(config, Arc::new(ManualClock::new(t0())));
    let mut observed = Observed::new(station.subscribe());
    observed.wait_log("not persisted").await;
    station.send(Command::AddConnector).await.unwrap();
    station.shutdown().await.unwrap();
    join.await.unwrap();
    assert_eq!(fs::read(store.path()).unwrap(), future);
}

#[tokio::test]
async fn persistence_can_be_switched_off() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config("ws://127.0.0.1:9/ocpp", &dir).without_persistence();
    let (station, join) = Station::spawn(config, Arc::new(ManualClock::new(t0())));
    station.send(Command::AddConnector).await.unwrap();
    station.shutdown().await.unwrap();
    join.await.unwrap();
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn changes_are_written_soon_without_waiting_for_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config("ws://127.0.0.1:9/ocpp", &dir);
    let store = StationStore::new(dir.path(), &config.charge_point_id);
    let (station, join) = Station::spawn(config, Arc::new(ManualClock::new(t0())));
    station.send(Command::AddConnector).await.unwrap();
    let expected = u32::try_from(station.snapshot().connectors.len()).unwrap();
    // AddConnector is an "important" change: the file shows it without a shutdown.
    // The write is debounced and fsynced (twice, with the directory), and a full fsync on a
    // shared macOS CI runner can take seconds. The point is that no shutdown is needed, not how
    // fast it is, so poll until the content is there under a deadline that only guards hangs.
    // `load` may briefly see nothing or an older file; the rename is atomic, never a torn one.
    let deadline = std::time::Instant::now() + STEP * 4;
    loop {
        let seen = match store.load() {
            Ok(LoadOutcome::Loaded(state)) => Some(state.connector_count),
            _ => None,
        };
        if seen == Some(expected) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "state file did not show {expected} connectors without a shutdown (last seen: {seen:?})"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    station.shutdown().await.unwrap();
    join.await.unwrap();
}

#[tokio::test]
async fn a_stop_in_flight_at_a_crash_is_restored_as_a_queued_stop() {
    use kirby_ocpp_core::ids::{IdTag, TxId};
    use kirby_ocpp_core::ocpp::types::StopReason;
    use kirby_ocpp_core::station::transaction::{StoppingTransaction, TransactionState};
    use kirby_ocpp_core::store::{PersistedConnector, PersistedStation};

    let dir = tempfile::tempdir().unwrap();
    let config = test_config("ws://127.0.0.1:9/ocpp", &dir);
    let store = StationStore::new(dir.path(), &config.charge_point_id);
    let mut state = PersistedStation::new(config.charge_point_id.clone());
    state.connector_count = 2;
    state.connectors.push(PersistedConnector {
        id: ConnectorId::new(1),
        status: ConnectorStatus::Finishing,
        availability: AvailabilityType::Operative,
        pending_availability: None,
        ev_connected: true,
        meter_wh: 42,
        last_id_tag: None,
        reservation: None,
        transaction: TransactionState::Stopping(StoppingTransaction {
            tx_id: TxId::Csms(7),
            id_tag: Some(IdTag::new("TAG1").unwrap()),
            reason: StopReason::Local,
        }),
    });
    // Connector 2: an online start that was in flight at the crash (no queue entry).
    state.connectors.push(PersistedConnector {
        id: ConnectorId::new(2),
        status: ConnectorStatus::Preparing,
        availability: AvailabilityType::Operative,
        pending_availability: None,
        ev_connected: true,
        meter_wh: 0,
        last_id_tag: None,
        reservation: None,
        transaction: TransactionState::PendingStart(
            kirby_ocpp_core::station::transaction::PendingStart {
                tx_id: TxId::Local(99),
                id_tag: IdTag::new("TAG2").unwrap(),
                started_at: t0(),
                meter_start_wh: 0,
                reservation_id: None,
            },
        ),
    });
    store.save(&state).unwrap();

    let (station, join) = Station::spawn(config, Arc::new(ManualClock::new(t0())));
    let snapshot = station.snapshot();
    let two = snapshot.connector(ConnectorId::new(2)).unwrap();
    assert!(two.transaction.is_idle());
    assert_eq!(two.status, ConnectorStatus::Preparing);
    assert_eq!(snapshot.pending_offline_messages, 1);
    assert!(
        snapshot
            .connector(ConnectorId::new(1))
            .unwrap()
            .transaction
            .is_idle()
    );
    station.shutdown().await.unwrap();
    join.await.unwrap();
}

#[tokio::test]
async fn pending_availability_survives_a_restart_and_is_applied_when_the_transaction_ends() {
    use kirby_ocpp_core::ocpp::types::StopReason;

    let mut rig = Rig::with(incrementing(), |config| config).await;
    rig.connect_booted().await;
    rig.begin(1, "TAG1").await;
    rig.csms
        .expect_status(
            "ChangeAvailability",
            json!({"connectorId": 1, "type": "Inoperative"}),
            "Scheduled",
        )
        .await
        .unwrap();
    let one = ConnectorId::new(1);
    assert_eq!(
        rig.station
            .snapshot()
            .connector(one)
            .unwrap()
            .pending_availability,
        Some(AvailabilityType::Inoperative)
    );

    // Restart on the same state directory (shutdown writes the final state).
    rig.station.shutdown().await.unwrap();
    rig.join.take().unwrap().await.unwrap();
    let config = test_config(rig.csms.url(), &rig.dir);
    let (station, join) = Station::spawn(config, Arc::new(ManualClock::new(t0())));
    let restored = station.snapshot();
    let connector = restored.connector(one).unwrap();
    assert_eq!(
        connector.pending_availability,
        Some(AvailabilityType::Inoperative)
    );
    assert_eq!(connector.availability, AvailabilityType::Operative);
    assert!(!connector.transaction.is_idle());

    // The transaction ends: the restored pending change is applied.
    station
        .send(Command::StopTransaction {
            connector: one,
            reason: StopReason::Local,
            id_tag: None,
        })
        .await
        .unwrap();
    let snapshot = station_support::wait_state(&station, "availability applied", |s| {
        s.connector(one)
            .is_some_and(|c| c.availability == AvailabilityType::Inoperative)
    })
    .await;
    let connector = snapshot.connector(one).unwrap();
    assert_eq!(connector.pending_availability, None);
    assert_eq!(connector.status, ConnectorStatus::Unavailable);

    station.shutdown().await.unwrap();
    join.await.unwrap();
    rig.csms.shutdown().await;
}
