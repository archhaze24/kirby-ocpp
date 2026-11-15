//! State file and restore of the OCPP 2.0.1 station.
//!
//! The first group tests the store alone. The second one starts stations against a mock CSMS:
//! it writes a state file, starts the station on it and checks what the CSMS receives.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::v2::messages::TransactionEventKind;
use kirby_ocpp_core::ocpp::v2::types::{ChargingState, IdTokenType};
use kirby_ocpp_core::station2::{
    ActiveTx, AuthCachePersist, CacheEntry, DeviceModelPersist, EvseAvail, IdTokenRef,
    QueuePersist, QueuedEvent, TxId2, TxPersist,
};
use kirby_ocpp_core::store::{
    FileKind, Inspection2, LoadOutcome, LoadOutcome2, PersistedStation, PersistedStation2,
    STORE2_VERSION, StationSecrets2, StationStore, StoreError,
};
use kirby_ocpp_core::{ManualClock, Station2, Station2Handle, StationConfig};
use kirby_ocpp_testkit::MockCsms;
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::station2_support::{STEP, calls_of, v201_csms, wait_booted, wait_snapshot};

const ID: &str = "CP-S6";
/// A password that must never appear in the main state file.
const SENTINEL: &str = "s3ntinel-p4ssword-0042";

fn store(dir: &Path) -> StationStore {
    StationStore::new_for(FileKind::V2, dir, ID)
}

fn started_at() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH
}

fn active_tx(evse: u32, id: &str, next_seq_no: i32) -> ActiveTx {
    ActiveTx {
        id: TxId2(id.to_owned()),
        evse,
        next_seq_no,
        started_at: started_at(),
        charging_state: ChargingState::Charging,
        token: Some(IdTokenRef {
            id_token: "TAG-1".to_owned(),
            kind: IdTokenType::Iso14443,
        }),
        remote_start_id: Some(7),
        authorized: true,
        ev_connected: true,
        evse_sent: true,
        token_sent: true,
        meter_start_wh: 100,
        energy_wh: 250.5,
    }
}

fn queued(tx: &str, seq_no: i32, kind: TransactionEventKind) -> QueuedEvent {
    QueuedEvent {
        payload: json!({
            "eventType": kind,
            "timestamp": "2026-01-01T00:00:00.000Z",
            "triggerReason": "Trigger",
            "seqNo": seq_no,
            "offline": false,
            "transactionInfo": {"transactionId": tx},
        }),
        evse: 1,
        transaction_id: tx.to_owned(),
        kind,
        seq_no,
        attempts: 0,
        offline: false,
    }
}

fn sample() -> PersistedStation2 {
    let mut state = PersistedStation2::new(ID);
    state.station_operative = false;
    state.evse_availability = vec![
        EvseAvail {
            evse: 1,
            operative: true,
            connectors: vec![(1, true)],
        },
        EvseAvail {
            evse: 2,
            operative: false,
            connectors: vec![(1, false)],
        },
    ];
    state
        .device_model
        .values
        .insert("OCPPCommCtrlr.HeartbeatInterval".to_owned(), "9".to_owned());
    state.auth_cache = AuthCachePersist {
        entries: vec![(
            "ISO14443:TAG-1".to_owned(),
            CacheEntry {
                status: "Accepted".to_owned(),
                expires_at: Some(started_at()),
                group_id: Some("G".to_owned()),
            },
        )],
    };
    state.tx = TxPersist {
        active: vec![active_tx(1, "11111111-1111-4111-8111-111111111111", 3)],
        stopped_ids: vec!["22222222-2222-4222-8222-222222222222".to_owned()],
        meters: BTreeMap::from([(1, 350.5), (2, 12.0)]),
    };
    state.queue = QueuePersist {
        events: vec![queued(
            "22222222-2222-4222-8222-222222222222",
            4,
            TransactionEventKind::Ended,
        )],
    };
    state.boot_count = 3;
    state
}

fn loaded2(outcome: LoadOutcome2) -> PersistedStation2 {
    match outcome {
        LoadOutcome2::Loaded(state) => *state,
        other => panic!("expected Loaded, got {other:?}"),
    }
}

#[test]
fn store2_roundtrips_every_part() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    store.save2(&sample()).unwrap();
    assert_eq!(loaded2(store.load2().unwrap()), sample());
    assert!(matches!(store.inspect2().unwrap(), Inspection2::Loaded(_)));
    assert!(store.path().ends_with("CP-S6.station2.json"));
    assert!(
        store
            .secrets_path()
            .ends_with("CP-S6.station2.secrets.json")
    );
    // A missing file is not an error.
    let other = StationStore::new_for(FileKind::V2, dir.path(), "other");
    assert!(matches!(other.load2().unwrap(), LoadOutcome2::Missing));
}

#[test]
fn store2_refuses_a_newer_file_and_leaves_it_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let newer = format!(
        r#"{{"version": {}, "charge_point_id": "{ID}"}}"#,
        STORE2_VERSION + 1
    );
    fs::write(store.path(), &newer).unwrap();
    for result in [store.load2().map(|_| ()), store.inspect2().map(|_| ())] {
        assert!(
            matches!(result, Err(StoreError::UnsupportedVersion { found, .. }) if found == u64::from(STORE2_VERSION) + 1)
        );
    }
    assert_eq!(fs::read_to_string(store.path()).unwrap(), newer);
    assert!(!store.path().with_extension("json.bak").exists());

    // A newer secrets file: refused and untouched.
    fs::remove_file(store.path()).unwrap();
    let newer_secrets = r#"{"version": 99, "basic_auth_password": "x"}"#;
    fs::write(store.secrets_path(), newer_secrets).unwrap();
    assert!(matches!(
        store.load_secrets2(),
        Err(StoreError::UnsupportedVersion { found: 99, .. })
    ));
    assert_eq!(
        fs::read_to_string(store.secrets_path()).unwrap(),
        newer_secrets
    );

    // A newer `.bak` copy stops `remove` before anything is deleted.
    store.save2(&sample()).unwrap();
    let backup = format!("{}.bak", store.path().display());
    fs::write(&backup, &newer).unwrap();
    assert!(matches!(
        store.remove(),
        Err(StoreError::UnsupportedVersion { .. })
    ));
    assert!(store.path().exists());
    assert!(store.secrets_path().exists());
    assert_eq!(fs::read_to_string(&backup).unwrap(), newer);
}

#[test]
fn store2_quarantines_a_corrupt_foreign_or_unversioned_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let foreign = serde_json::to_string(&PersistedStation2::new("someone-else")).unwrap();
    let cases = [
        ("{ torn", "not valid JSON"),
        (r#"{"charge_point_id": "CP-S6"}"#, "missing format version"),
        (foreign.as_str(), "belongs to charge point"),
    ];
    for (text, reason_part) in cases {
        fs::write(store.path(), text).unwrap();
        // `inspect2` reports and keeps the file.
        assert!(matches!(
            store.inspect2().unwrap(),
            Inspection2::Unusable { .. }
        ));
        assert!(store.path().exists());
        let LoadOutcome2::Quarantined { backup, reason } = store.load2().unwrap() else {
            panic!("expected quarantine for {text}");
        };
        assert!(reason.contains(reason_part), "{reason}");
        assert_eq!(fs::read_to_string(&backup).unwrap(), text);
        assert!(!store.path().exists());
        assert!(matches!(store.load2().unwrap(), LoadOutcome2::Missing));
    }
}

#[test]
fn a_station_id_has_independent_files_for_both_protocols() {
    let dir = tempfile::tempdir().unwrap();
    let v16 = StationStore::new(dir.path(), ID);
    let v2 = store(dir.path());
    assert_ne!(v16.path(), v2.path());
    assert_ne!(v16.secrets_path(), v2.secrets_path());
    assert_eq!(v16.kind(), FileKind::V16);
    assert_eq!(FileKind::for_protocol(Protocol::V201), FileKind::V2);

    let mut state16 = PersistedStation::new(ID);
    state16.connector_count = 2;
    v16.save(&state16).unwrap();
    v2.save2(&sample()).unwrap();
    let before16 = fs::read(v16.path()).unwrap();
    let before2 = fs::read(v2.path()).unwrap();

    // Neither kind reads, quarantines or rewrites the other one.
    assert!(matches!(
        v16.load().unwrap(),
        LoadOutcome::Loaded(state) if state.connector_count == 2
    ));
    assert_eq!(loaded2(v2.load2().unwrap()), sample());
    v2.save2(&sample()).unwrap();
    assert_eq!(fs::read(v16.path()).unwrap(), before16);
    assert_eq!(fs::read(v2.path()).unwrap(), before2);

    // Deleting one kind keeps the other.
    let removed = v2.remove().unwrap();
    assert_eq!(removed, [v2.path().to_path_buf()]);
    assert_eq!(fs::read(v16.path()).unwrap(), before16);
    assert!(matches!(v2.load2().unwrap(), LoadOutcome2::Missing));
    assert_eq!(v16.owned_files().len(), 4);
    assert!(
        v16.owned_files()
            .iter()
            .all(|p| !p.to_string_lossy().contains("station2"))
    );
}

#[test]
fn the_password_is_only_in_the_secrets_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    store.save2(&sample()).unwrap();
    store
        .save_secrets2(&StationSecrets2 {
            version: 0,
            basic_auth_password: Some(SENTINEL.to_owned()),
        })
        .unwrap();
    assert!(!fs::read_to_string(store.path()).unwrap().contains(SENTINEL));
    let secrets = fs::read_to_string(store.secrets_path()).unwrap();
    assert!(secrets.contains(SENTINEL));
    assert_eq!(
        store
            .load_secrets2()
            .unwrap()
            .basic_auth_password
            .as_deref(),
        Some(SENTINEL)
    );
    assert!(!format!("{:?}", store.load_secrets2().unwrap()).contains(SENTINEL));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [store.path(), &store.secrets_path()] {
            let mode = fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{}", path.display());
        }
    }
    // No password: the file goes away.
    store.save_secrets2(&StationSecrets2::default()).unwrap();
    assert!(!store.secrets_path().exists());
}

#[test]
fn a_corrupt_secrets_file_is_moved_aside_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    fs::write(store.secrets_path(), "{ torn").unwrap();
    let Err(StoreError::Io { source, .. }) = store.load_secrets2() else {
        panic!("expected an error");
    };
    assert_eq!(source.kind(), std::io::ErrorKind::InvalidData);
    assert!(!store.secrets_path().exists());
    let backup = format!("{}.bak", store.secrets_path().display());
    assert_eq!(fs::read_to_string(backup).unwrap(), "{ torn");
}

#[test]
fn file_names_fit_the_budget_with_the_longest_suffix_and_backup() {
    let dir = Path::new("/state");
    let long = "ü".repeat(200);
    for id in ["CP-1", "", "a/b", long.as_str(), &"x".repeat(1000)] {
        for kind in [FileKind::V16, FileKind::V2] {
            for path in StationStore::new_for(kind, dir, id).owned_files() {
                assert_eq!(path.parent(), Some(dir));
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                assert!(name.len() <= 150, "{} bytes: {name}", name.len());
            }
        }
    }
    // A long id keeps its two kinds distinct and stable.
    let a = StationStore::new_for(FileKind::V2, dir, &"x".repeat(1000));
    let b = StationStore::new_for(FileKind::V2, dir, &"x".repeat(1001));
    assert_ne!(a.path(), b.path());
}

// Stations against a mock CSMS.

fn config(csms: &MockCsms, dir: &Path) -> StationConfig {
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
    config
}

fn spawn(csms: &MockCsms, dir: &Path) -> (Station2Handle, JoinHandle<()>) {
    Station2::spawn(config(csms, dir), Arc::new(ManualClock::new(Utc::now())))
}

/// Connects, waits for the boot and shuts the station down; the final write happens before
/// the actor ends.
async fn run_and_stop(csms: &MockCsms, dir: &Path) {
    let (station, join) = spawn(csms, dir);
    station
        .send(kirby_ocpp_core::Command2::Connect)
        .await
        .unwrap();
    wait_booted(&station).await;
    station.shutdown().await.unwrap();
    timeout(STEP, join).await.unwrap().unwrap();
}

#[tokio::test]
async fn a_newer_state_file_is_never_overwritten_by_a_running_station() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let newer = format!(
        r#"{{"version": {}, "charge_point_id": "{ID}"}}"#,
        STORE2_VERSION + 1
    );
    fs::write(store.path(), &newer).unwrap();
    run_and_stop(&csms, dir.path()).await;
    assert_eq!(fs::read_to_string(store.path()).unwrap(), newer);
    let names: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["CP-S6.station2.json"]);
}

#[tokio::test]
async fn availability_survives_a_restart() {
    // Needs the actor loop (S1) and the station constructor wiring.
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let mut state = PersistedStation2::new(ID);
    state.evse_availability = vec![EvseAvail {
        evse: 2,
        operative: false,
        connectors: vec![(1, true)],
    }];
    store(dir.path()).save2(&state).unwrap();
    let (station, join) = spawn(&csms, dir.path());
    let snapshot = wait_snapshot(&station, "the restored availability", |s| {
        s.evses.iter().any(|e| e.id == 2 && !e.operative)
    })
    .await;
    assert!(snapshot.evses.iter().any(|e| e.id == 1 && e.operative));
    station.shutdown().await.unwrap();
    timeout(STEP, join).await.unwrap().unwrap();
    // The write at shutdown keeps it.
    let saved = loaded2(store(dir.path()).load2().unwrap());
    assert!(
        saved
            .evse_availability
            .iter()
            .any(|e| e.evse == 2 && !e.operative)
    );
}

#[tokio::test]
async fn every_start_counts_as_one_boot_in_the_state_file() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().unwrap();
    run_and_stop(&csms, dir.path()).await;
    assert_eq!(loaded2(store(dir.path()).load2().unwrap()).boot_count, 1);
    run_and_stop(&csms, dir.path()).await;
    assert_eq!(loaded2(store(dir.path()).load2().unwrap()).boot_count, 2);
}

#[tokio::test]
async fn a_restored_transaction_ends_with_power_loss_and_seq_no_continues() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let tx = "11111111-1111-4111-8111-111111111111";
    let mut state = PersistedStation2::new(ID);
    state.tx.active = vec![active_tx(1, tx, 3)];
    store(dir.path()).save2(&state).unwrap();
    let (station, join) = spawn(&csms, dir.path());
    station
        .send(kirby_ocpp_core::Command2::Connect)
        .await
        .unwrap();
    wait_booted(&station).await;
    csms.wait_for_call("TransactionEvent", STEP).await.unwrap();
    let events = calls_of(&csms, "TransactionEvent");
    let first = &events[0];
    assert_eq!(first["eventType"], "Ended");
    assert_eq!(first["triggerReason"], "AbnormalCondition");
    assert_eq!(first["seqNo"], 3);
    assert_eq!(first["transactionInfo"]["transactionId"], tx);
    assert_eq!(first["transactionInfo"]["stoppedReason"], "PowerLoss");
    station.shutdown().await.unwrap();
    timeout(STEP, join).await.unwrap().unwrap();
}

#[tokio::test]
async fn queued_events_survive_flagged_offline_and_arrive_in_order_after_boot() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let tx = "33333333-3333-4333-8333-333333333333";
    let mut state = PersistedStation2::new(ID);
    state.queue.events = vec![
        queued(tx, 0, TransactionEventKind::Started),
        queued(tx, 1, TransactionEventKind::Updated),
        queued(tx, 2, TransactionEventKind::Ended),
    ];
    store(dir.path()).save2(&state).unwrap();
    let (station, join) = spawn(&csms, dir.path());
    station
        .send(kirby_ocpp_core::Command2::Connect)
        .await
        .unwrap();
    wait_booted(&station).await;
    let events = csms
        .wait_for_call_count_after(0, "TransactionEvent", 3, |_| true, STEP)
        .await
        .unwrap();
    let seq: Vec<Value> = events.iter().map(|e| e["seqNo"].clone()).collect();
    assert_eq!(seq, [json!(0), json!(1), json!(2)]);
    assert!(events.iter().all(|e| e["offline"] == json!(true)));
    station.shutdown().await.unwrap();
    timeout(STEP, join).await.unwrap().unwrap();
}

#[tokio::test]
async fn the_password_set_by_the_csms_reaches_only_the_secrets_file() {
    // Needs the device model (S2).
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let (station, join) = spawn(&csms, dir.path());
    station
        .send(kirby_ocpp_core::Command2::Connect)
        .await
        .unwrap();
    wait_booted(&station).await;
    csms.send_call(
        "SetVariables",
        json!({"setVariableData": [{
            "attributeValue": SENTINEL,
            "component": {"name": "SecurityCtrlr"},
            "variable": {"name": "BasicAuthPassword"},
        }]}),
    )
    .await
    .unwrap();
    station.shutdown().await.unwrap();
    timeout(STEP, join).await.unwrap().unwrap();
    let store = store(dir.path());
    assert!(!fs::read_to_string(store.path()).unwrap().contains(SENTINEL));
    assert!(
        fs::read_to_string(store.secrets_path())
            .unwrap()
            .contains(SENTINEL)
    );
}

#[test]
fn device_model_values_are_plain_strings_in_the_file() {
    // Pins the shape `state show` and a hand-edited file rely on.
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let mut state = PersistedStation2::new(ID);
    state.device_model = DeviceModelPersist::default();
    state
        .device_model
        .values
        .insert("K".to_owned(), "v".to_owned());
    store.save2(&state).unwrap();
    let value: Value = serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
    assert_eq!(value["device_model"]["values"]["K"], "v");
    assert_eq!(value["version"], STORE2_VERSION);
    assert_eq!(value["protocol"], "ocpp2.0.1");
}

#[test]
fn a_file_from_before_the_registers_still_loads() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    store.save2(&sample()).unwrap();
    let mut value: Value = serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
    assert_eq!(value["tx"]["meters"]["1"], 350.5);
    value["tx"].as_object_mut().unwrap().remove("meters");
    fs::write(store.path(), serde_json::to_vec(&value).unwrap()).unwrap();
    let state = loaded2(store.load2().unwrap());
    assert!(state.tx.meters.is_empty());
    assert_eq!(state.tx.active.len(), 1);
    assert_eq!(state.version, STORE2_VERSION);
}

fn evse_meter(snapshot: &kirby_ocpp_core::Snapshot2, evse: u32) -> u64 {
    snapshot.evse(evse).expect("the EVSE exists").meter_wh
}

#[tokio::test]
async fn the_meter_register_survives_a_restart() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let (station, join) = spawn(&csms, dir.path());
    station
        .send(kirby_ocpp_core::Command2::Connect)
        .await
        .unwrap();
    wait_booted(&station).await;
    station
        .send(kirby_ocpp_core::Command2::AdvanceMeter {
            evse: 2,
            wh_delta: 321,
        })
        .await
        .unwrap();
    station.shutdown().await.unwrap();
    timeout(STEP, join).await.unwrap().unwrap();
    let saved = loaded2(store(dir.path()).load2().unwrap());
    assert!(saved.tx.meters.get(&2).copied().unwrap_or_default() >= 321.0);

    let (station, join) = spawn(&csms, dir.path());
    let snapshot = wait_snapshot(&station, "the restored register", |s| {
        evse_meter(s, 2) >= 321
    })
    .await;
    assert_eq!(evse_meter(&snapshot, 1), 0);
    station.shutdown().await.unwrap();
    timeout(STEP, join).await.unwrap().unwrap();
}

#[tokio::test]
async fn a_restored_register_wins_over_the_start_value_of_a_transaction() {
    let csms = v201_csms().await;
    let tx = "11111111-1111-4111-8111-111111111111";

    // Without a stored register the transaction supplies it: start 100 + 250.5 Wh.
    let dir = tempfile::tempdir().unwrap();
    let mut state = PersistedStation2::new(ID);
    state.tx.active = vec![active_tx(1, tx, 3)];
    store(dir.path()).save2(&state).unwrap();
    let (station, join) = spawn(&csms, dir.path());
    let snapshot = wait_snapshot(&station, "the restored transaction register", |s| {
        evse_meter(s, 1) > 0
    })
    .await;
    assert_eq!(evse_meter(&snapshot, 1), 350);
    station.shutdown().await.unwrap();
    timeout(STEP, join).await.unwrap().unwrap();

    // With one, that value is used.
    let dir = tempfile::tempdir().unwrap();
    state.tx.meters = BTreeMap::from([(1, 900.5)]);
    store(dir.path()).save2(&state).unwrap();
    let (station, join) = spawn(&csms, dir.path());
    let snapshot = wait_snapshot(&station, "the stored register", |s| evse_meter(s, 1) > 0).await;
    assert_eq!(evse_meter(&snapshot, 1), 900);
    station.shutdown().await.unwrap();
    timeout(STEP, join).await.unwrap().unwrap();
}
