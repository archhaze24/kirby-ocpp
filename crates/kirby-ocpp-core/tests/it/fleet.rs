//! Fleet tests: several stations against one mock CSMS.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use kirby_ocpp_core::fleet::{Fleet, FleetError, FleetItem, FleetSpec};
use kirby_ocpp_core::ids::IdTag;
use kirby_ocpp_core::ocpp::types::AuthorizationStatus;
use kirby_ocpp_core::station::{Command, ConnectionState, Event, IdTagInfo, Snapshot};
use kirby_ocpp_core::{Clock, StationConfig, SystemClock};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms};

const STEP: Duration = Duration::from_secs(10);

fn clock() -> Arc<dyn Clock> {
    Arc::new(SystemClock)
}

fn base(csms: &MockCsms) -> StationConfig {
    StationConfig::new(csms.url()).without_persistence()
}

async fn wait_booted(fleet: &Fleet) {
    for (_, handle) in fleet.stations() {
        let mut snapshots = handle.watch_snapshot();
        tokio::time::timeout(STEP, snapshots.wait_for(Snapshot::booted))
            .await
            .expect("station boots in time")
            .expect("station alive");
    }
}

#[tokio::test]
async fn stations_connect_with_distinct_ids_and_shut_down_cleanly() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let spec = FleetSpec::new("CP-{n:03}", 4);
    let mut fleet = Fleet::spawn(&base(&csms), &spec, &clock()).unwrap();
    let ids: Vec<&str> = fleet.stations().iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["CP-001", "CP-002", "CP-003", "CP-004"]);

    assert!(fleet.connect_all().await.is_empty());
    wait_booted(&fleet).await;
    csms.wait_for_connection_count(4, STEP).await.unwrap();

    let seen: BTreeSet<String> = csms
        .connection_log()
        .into_iter()
        .filter_map(|c| c.charge_point_id)
        .collect();
    assert_eq!(seen.len(), 4, "{seen:?}");
    assert!(seen.contains("CP-003"));

    let summary = fleet.summary();
    assert_eq!(summary.total, 4);
    assert_eq!(summary.connected, 4);
    assert_eq!(summary.registered, 4);
    assert_eq!(summary.active_transactions, 0);
    assert_eq!(summary.stations.len(), 4);

    fleet.shutdown().await;
    assert_eq!(fleet.summary().disconnected, 4);
    assert_eq!(
        fleet.station("CP-002").unwrap().snapshot().connection,
        ConnectionState::Disconnected
    );
    csms.wait_for_disconnect(STEP).await.unwrap();
    csms.shutdown().await;
}

#[tokio::test]
async fn merged_events_carry_the_station_id() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let spec = FleetSpec {
        id_template: "T{n}".to_owned(),
        first_index: 10,
        count: 3,
        stagger_ms: 5,
    };
    let mut fleet = Fleet::spawn(&base(&csms), &spec, &clock()).unwrap();
    let mut events = fleet.subscribe();
    assert!(fleet.connect_all().await.is_empty());
    wait_booted(&fleet).await;
    fleet.shutdown().await;

    events.stop();
    let mut boots = BTreeSet::new();
    while let Some(item) = events.recv().await {
        if let FleetItem::Event(fleet_event) = item
            && let Event::Frame(record) = &fleet_event.event
            && record.action.as_deref() == Some("BootNotification")
        {
            boots.insert(fleet_event.station_id);
        }
    }
    assert_eq!(boots.into_iter().collect::<Vec<_>>(), ["T10", "T11", "T12"]);
    csms.shutdown().await;
}

#[tokio::test]
async fn each_station_has_its_own_state_file() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut config = StationConfig::new(csms.url());
    config.state_dir = Some(dir.path().to_owned());
    let mut fleet = Fleet::spawn(&config, &FleetSpec::new("S-{n}", 3), &clock()).unwrap();
    assert!(fleet.connect_all().await.is_empty());
    wait_booted(&fleet).await;
    // A local change makes every station write its state.
    for (_, handle) in fleet.stations() {
        handle
            .send(Command::SetLocalListEntry {
                id_tag: IdTag::new("TAG").unwrap(),
                info: IdTagInfo::with_status(AuthorizationStatus::Accepted),
            })
            .await
            .unwrap();
    }
    fleet.shutdown().await;

    let files: BTreeSet<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".station.json"))
        .collect();
    assert_eq!(
        files.into_iter().collect::<Vec<_>>(),
        ["S-1.station.json", "S-2.station.json", "S-3.station.json"]
    );
    csms.shutdown().await;
}

#[tokio::test]
async fn invalid_specs_spawn_nothing() {
    let config = StationConfig::new("ws://localhost:1/ocpp").without_persistence();
    let err = Fleet::spawn(&config, &FleetSpec::new("CP-001", 2), &clock()).unwrap_err();
    assert!(matches!(err, FleetError::NoPlaceholder(..)));
    let err = Fleet::spawn(&config, &FleetSpec::new("CP-{n}", 1001), &clock()).unwrap_err();
    assert_eq!(err, FleetError::CountOutOfRange(1001));
    let err = Fleet::spawn(&config, &FleetSpec::new("CP-{n}", 0), &clock()).unwrap_err();
    assert_eq!(err, FleetError::CountOutOfRange(0));
    let mut bad = config.clone();
    bad.phases = 9;
    let err = Fleet::spawn(&bad, &FleetSpec::new("CP-{n}", 2), &clock()).unwrap_err();
    assert!(matches!(err, FleetError::Config { .. }));
    for dots in ["..", "."] {
        let err = Fleet::spawn(&config, &FleetSpec::new(dots, 1), &clock()).unwrap_err();
        assert!(matches!(err, FleetError::Config { .. }), "{dots}: {err:?}");
    }
}
