//! Timer-driven `MeterValues` must stay bounded when the CSMS never answers.

mod station_support;

use std::time::Duration;

use kirby_ocpp_testkit::{CsmsOptions, Fault};
use station_support::Rig;

#[tokio::test]
async fn a_silent_csms_cannot_make_samples_pile_up() {
    // Ten connectors sampled every second: more than the in-flight limit within two ticks.
    let mut rig = Rig::with(CsmsOptions::default(), |config| {
        config.with_connectors(10).with_call_timeout_ms(60_000)
    })
    .await;
    rig.connect_booted().await;
    rig.csms.inject(Fault::silence("MeterValues").times(1_000));
    rig.cfg("ClockAlignedDataInterval", "1").await;

    let line = rig.observed.wait_log("dropping the oldest samples").await;
    assert!(line.contains("MeterValues"), "{line}");

    // Deliberate wait: more ticks overflow again, but the burst warns only once.
    tokio::time::sleep(Duration::from_millis(2_300)).await;
    rig.observed.drain();
    let warnings = rig
        .observed
        .logs
        .iter()
        .filter(|l| l.contains("dropping the oldest samples"))
        .count();
    assert_eq!(warnings, 1, "{:#?}", rig.observed.logs);

    // The connection is serialized and the first CALL is stuck: only it reached the CSMS.
    assert_eq!(rig.calls("MeterValues", 0).len(), 1);
    rig.shutdown().await;
}
