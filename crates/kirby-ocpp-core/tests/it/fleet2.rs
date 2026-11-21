//! OCPP 2.x fleets against a shared mock CSMS.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use kirby_ocpp_core::fleet::{FleetItem, FleetSpec};
use kirby_ocpp_core::fleet2::Fleet2;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::station::Event;
use kirby_ocpp_core::station2::Snapshot2;
use kirby_ocpp_core::{Clock, StationConfig, SystemClock};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms, SubprotocolPolicy};

const STEP: Duration = Duration::from_secs(10);

async fn boots_and_merges_events(protocol: Protocol, count: u32) {
    let csms = MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Require(protocol.token().to_owned()),
        ..CsmsOptions::default()
    })
    .await
    .unwrap();
    let mut config = StationConfig::new(csms.url()).without_persistence();
    config.protocol = protocol;
    protocol.token().clone_into(&mut config.ws_subprotocol);
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let mut fleet = Fleet2::spawn(&config, &FleetSpec::new("CP-{n}", count), &clock).unwrap();
    let mut events = fleet.subscribe();

    assert!(
        tokio::time::timeout(STEP, fleet.connect_all())
            .await
            .unwrap()
            .is_empty()
    );
    tokio::time::timeout(STEP, async {
        for (_, handle) in fleet.stations() {
            handle
                .watch_snapshot()
                .wait_for(Snapshot2::booted)
                .await
                .unwrap();
        }
    })
    .await
    .expect("every station boots");
    let summary = fleet.summary();
    assert_eq!(summary.total, count as usize);
    assert_eq!(summary.connected, count as usize);
    assert_eq!(summary.registered, count as usize);
    assert_eq!(summary.active_transactions, 0);

    tokio::time::timeout(STEP, fleet.shutdown()).await.unwrap();
    assert_eq!(fleet.summary().disconnected, count as usize);
    csms.wait_for_disconnect(STEP).await.unwrap();

    events.stop();
    let boots = tokio::time::timeout(STEP, async {
        let mut boots = BTreeSet::new();
        while let Some(item) = events.recv().await {
            if let FleetItem::Event(item) = item
                && let Event::Frame(record) = &item.event
                && record.action.as_deref() == Some("BootNotification")
            {
                boots.insert(item.station_id);
            }
        }
        boots
    })
    .await
    .expect("merged events drain");
    assert_eq!(boots, (1..=count).map(|n| format!("CP-{n}")).collect());
    csms.shutdown().await;
}

#[tokio::test]
async fn fleet_2_0_1_boots_and_merges_events() {
    boots_and_merges_events(Protocol::V201, 3).await;
}

#[tokio::test]
async fn fleet_2_1_boots_and_merges_events() {
    boots_and_merges_events(Protocol::V21, 2).await;
}
