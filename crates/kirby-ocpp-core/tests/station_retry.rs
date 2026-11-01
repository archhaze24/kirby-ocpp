//! Retry of transaction messages (`TransactionMessageAttempts` / `...RetryInterval`) and its
//! cancellation on disconnect. A CSMS-triggered `MeterValues` is the vehicle: it goes through
//! the same outbound path as the periodic/transaction MeterValues of stage 2b.

mod station_support;

use std::time::Duration;

use kirby_ocpp_testkit::Fault;
use serde_json::json;
use station_support::{Rig, STEP};

async fn configure(rig: &Rig, attempts: &str, interval: &str) {
    for (key, value) in [
        ("TransactionMessageAttempts", attempts),
        ("TransactionMessageRetryInterval", interval),
    ] {
        rig.csms
            .expect_status(
                "ChangeConfiguration",
                json!({"key": key, "value": value}),
                "Accepted",
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_failed_transaction_message_is_retried_after_the_interval() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    configure(&rig, "2", "1").await;
    rig.csms.inject(Fault::call_error("MeterValues").times(1));

    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "TriggerMessage",
            json!({"requestedMessage": "MeterValues", "connectorId": 1}),
            "Accepted",
        )
        .await
        .unwrap();
    rig.csms
        .wait_for_call_count_after(mark, "MeterValues", 2, |_| true, STEP)
        .await
        .expect("the message was sent twice");
    rig.observed
        .wait_log("MeterValues attempt 1/2 failed")
        .await;
    rig.shutdown().await;
}

#[tokio::test]
async fn without_extra_attempts_nothing_is_retried() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    rig.csms.inject(Fault::call_error("MeterValues").times(1));
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "TriggerMessage",
            json!({"requestedMessage": "MeterValues", "connectorId": 1}),
            "Accepted",
        )
        .await
        .unwrap();
    rig.csms
        .wait_for_call_after(mark, "MeterValues", |_| true, STEP)
        .await
        .unwrap();
    // Deliberate negative wait: with one attempt there is nothing more to arrive.
    assert!(
        rig.csms
            .wait_for_call_count_after(mark, "MeterValues", 2, |_| true, Duration::from_millis(300))
            .await
            .is_err()
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn pending_retries_are_cancelled_when_the_connection_goes_away() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    configure(&rig, "3", "3600").await;
    rig.csms.inject(Fault::call_error("MeterValues").times(1));

    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "TriggerMessage",
            json!({"requestedMessage": "MeterValues", "connectorId": 1}),
            "Accepted",
        )
        .await
        .unwrap();
    rig.observed
        .wait_log("MeterValues attempt 1/3 failed")
        .await;

    rig.station.disconnect().await.unwrap();
    rig.csms.wait_for_disconnect(STEP).await.unwrap();
    rig.station.connect().await.unwrap();
    rig.csms
        .wait_for_call_count_after(mark, "BootNotification", 1, |_| true, STEP)
        .await
        .unwrap();
    rig.csms
        .wait_for_call_count_after(mark, "StatusNotification", 3, |_| true, STEP)
        .await
        .unwrap();
    // The retry sleep belonged to the old connection and was aborted with it; the new
    // connection never sees a second MeterValues. (That the task itself is aborted is
    // internal to `fail_pending_calls`; externally only this absence is visible.)
    assert_eq!(rig.calls("MeterValues", mark).len(), 1);
    rig.shutdown().await;
}
