//! Shared helpers for the `station2_*` integration tests.
//!
//! A test starts a mock CSMS that speaks OCPP 2.0.1 and spawns a station against it. Waits are
//! event driven (snapshots, recorded CALLs) and bounded by [`STEP`]; nothing sleeps for a fixed
//! time.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of the helpers"
)]
#![allow(
    clippy::match_wild_err_arm,
    reason = "a timeout is the only error these waits can produce"
)]

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::{ManualClock, Snapshot2, Station2, Station2Handle, StationConfig};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms, SubprotocolPolicy};
use serde_json::Value;
use tokio::task::JoinHandle;
use tokio::time::timeout;

/// Upper bound for any single wait. Only reached when something is broken.
pub const STEP: Duration = Duration::from_secs(8);

/// A mock CSMS that accepts only the `ocpp2.0.1` subprotocol.
pub async fn v201_csms() -> MockCsms {
    MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Require("ocpp2.0.1".to_owned()),
        ..CsmsOptions::default()
    })
    .await
    .expect("mock CSMS starts")
}

/// Spawns a station against `csms`: two EVSEs, no persistence, fast reconnect, a heartbeat
/// every second. The station does not connect by itself.
pub fn spawn_v201(csms: &MockCsms) -> (Station2Handle, JoinHandle<()>) {
    let mut config = StationConfig::new(csms.url())
        .with_connectors(2)
        .with_call_timeout_ms(2_000)
        .with_heartbeat_interval_secs(1)
        .with_reconnect(ReconnectConfig {
            enabled: true,
            initial_delay_ms: 30,
            max_delay_ms: 100,
            max_attempts: 0,
        })
        .without_persistence();
    config.ws_ping_interval_secs = 0;
    Protocol::V201
        .token()
        .clone_into(&mut config.ws_subprotocol);
    config.protocol = Protocol::V201;
    Station2::spawn(config, Arc::new(ManualClock::new(Utc::now())))
}

/// Waits until `pred` holds for the station's snapshot.
pub async fn wait_snapshot(
    station: &Station2Handle,
    what: &str,
    pred: impl Fn(&Snapshot2) -> bool,
) -> Snapshot2 {
    let mut rx = station.watch_snapshot();
    match timeout(STEP, rx.wait_for(|s| pred(s))).await {
        Ok(Ok(snapshot)) => snapshot.clone(),
        Ok(Err(_)) => panic!("station stopped while waiting for {what}"),
        Err(_) => panic!(
            "timed out waiting for {what}; last snapshot: {:#?}",
            station.snapshot()
        ),
    }
}

/// Waits until the CSMS accepted the station's boot.
pub async fn wait_booted(station: &Station2Handle) -> Snapshot2 {
    wait_snapshot(station, "an accepted boot", Snapshot2::booted).await
}

/// The payloads of every CALL of `action` the CSMS has recorded, oldest first.
pub fn calls_of(csms: &MockCsms, action: &str) -> Vec<Value> {
    csms.calls_since(0)
        .into_iter()
        .filter(|call| call.action == action)
        .map(|call| call.payload)
        .collect()
}
