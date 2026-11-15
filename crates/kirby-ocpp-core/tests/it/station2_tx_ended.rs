//! `TxEndedInterval` sampling and the contents of the `Ended` event, over the wire.
//!
//! The sample interval is the smallest the device model allows (1 s), set through
//! `SetVariables`; the tests wait on recorded CALLs, so the transaction lasts as long as the
//! CSMS needs to see two periodic updates.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use kirby_ocpp_core::{Command2, StationError, SystemClock};
use serde_json::Value;

use crate::station2_support::{STEP, v201_csms, wait_booted};
use crate::station2_transactions::{send, set_variables, spawn_with_clock, stop};

/// `(context of the first sampled value, timestamp)` of each `meterValue` entry.
fn meter_entries(event: &Value) -> Vec<(String, DateTime<Utc>)> {
    event["meterValue"]
        .as_array()
        .expect("the event has meter values")
        .iter()
        .map(|value| {
            let timestamp = value["timestamp"]
                .as_str()
                .and_then(|text| text.parse().ok())
                .expect("a timestamp");
            let context = value["sampledValue"][0]["context"]
                .as_str()
                .expect("a context")
                .to_owned();
            (context, timestamp)
        })
        .collect()
}

#[tokio::test]
async fn the_ended_event_carries_a_sample_per_interval_between_begin_and_end() {
    let csms = v201_csms().await;
    let (station, task) = spawn_with_clock(&csms, Arc::new(SystemClock));
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    set_variables(
        &csms,
        &[
            ("TxCtrlr", "TxStartPoint", None, "EVConnected"),
            ("SampledDataCtrlr", "TxUpdatedInterval", None, "1"),
            ("SampledDataCtrlr", "TxEndedInterval", None, "1"),
            (
                "SampledDataCtrlr",
                "TxEndedMeasurands",
                None,
                "Energy.Active.Import.Register,Voltage",
            ),
        ],
    )
    .await;

    send(&station, Command2::PlugIn { evse: 1 }).await;
    // Two periodic updates prove that at least two seconds passed.
    csms.wait_for_call_count_after(
        0,
        "TransactionEvent",
        2,
        |p| p["triggerReason"] == "MeterValuePeriodic",
        STEP,
    )
    .await
    .expect("two periodic updates");
    send(&station, Command2::Unplug { evse: 1 }).await;
    let ended = csms
        .wait_for_call_after(0, "TransactionEvent", |p| p["eventType"] == "Ended", STEP)
        .await
        .expect("the Ended event");

    let entries = meter_entries(&ended);
    let contexts: Vec<&str> = entries
        .iter()
        .map(|(context, _)| context.as_str())
        .collect();
    assert_eq!(contexts.first(), Some(&"Transaction.Begin"));
    assert_eq!(contexts.last(), Some(&"Transaction.End"));
    let periodic = contexts.iter().filter(|c| **c == "Sample.Periodic").count();
    assert!(periodic >= 1, "no intermediate sample: {contexts:?}");
    assert_eq!(
        periodic + 2,
        contexts.len(),
        "only begin, periodic and end samples: {contexts:?}"
    );
    // Oldest first; the intermediate samples are one interval apart.
    let times: Vec<DateTime<Utc>> = entries.iter().map(|(_, at)| *at).collect();
    assert!(times.windows(2).all(|pair| pair[0] <= pair[1]), "{times:?}");
    let sampled: Vec<DateTime<Utc>> = entries
        .iter()
        .filter(|(context, _)| context == "Sample.Periodic")
        .map(|(_, at)| *at)
        .collect();
    for pair in sampled.windows(2) {
        let gap = pair[1] - pair[0];
        assert!(gap.num_milliseconds() >= 900, "samples {gap} apart");
    }
    // Every sample has the configured measurands and nothing else.
    for value in ended["meterValue"].as_array().expect("meter values") {
        let measurands: Vec<&str> = value["sampledValue"]
            .as_array()
            .expect("sampled values")
            .iter()
            .map(|s| {
                s["measurand"]
                    .as_str()
                    .unwrap_or("Energy.Active.Import.Register")
            })
            .collect();
        assert_eq!(
            measurands,
            ["Energy.Active.Import.Register", "Voltage"],
            "{value}"
        );
    }
    // The intermediate samples are not sent anywhere else: an update has at most one
    // meter value.
    let updates: Vec<Value> = csms
        .calls_since(0)
        .into_iter()
        .filter(|call| call.action == "TransactionEvent" && call.payload["eventType"] == "Updated")
        .map(|call| call.payload)
        .collect();
    assert_ne!(updates.len(), 0, "the periodic updates were sent");
    for update in updates {
        assert!(
            update["meterValue"].as_array().map_or(0, Vec::len) <= 1,
            "{update}"
        );
    }
    stop(&station, task).await;
}

#[tokio::test]
async fn without_an_interval_the_ended_event_has_the_begin_and_end_samples_only() {
    let csms = v201_csms().await;
    let (station, task) = spawn_with_clock(&csms, Arc::new(SystemClock));
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    set_variables(&csms, &[("TxCtrlr", "TxStartPoint", None, "EVConnected")]).await;
    send(&station, Command2::PlugIn { evse: 1 }).await;
    csms.wait_for_call_after(0, "TransactionEvent", |p| p["eventType"] == "Started", STEP)
        .await
        .expect("the Started event");
    send(&station, Command2::Unplug { evse: 1 }).await;
    let ended = csms
        .wait_for_call_after(0, "TransactionEvent", |p| p["eventType"] == "Ended", STEP)
        .await
        .expect("the Ended event");
    let contexts: Vec<String> = meter_entries(&ended).into_iter().map(|e| e.0).collect();
    assert_eq!(contexts, ["Transaction.Begin", "Transaction.End"]);
    stop(&station, task).await;
}

#[tokio::test]
async fn sample_meter_inside_a_transaction_sends_a_periodic_update() {
    let csms = v201_csms().await;
    let (station, task) = spawn_with_clock(&csms, Arc::new(SystemClock));
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    set_variables(&csms, &[("TxCtrlr", "TxStartPoint", None, "EVConnected")]).await;
    send(&station, Command2::PlugIn { evse: 1 }).await;
    csms.wait_for_call_after(0, "TransactionEvent", |p| p["eventType"] == "Started", STEP)
        .await
        .expect("the Started event");
    // The default interval is 30 s: the update below comes from the command.
    send(&station, Command2::SampleMeter { evse: 1 }).await;
    let update = csms
        .wait_for_call_after(
            0,
            "TransactionEvent",
            |p| p["triggerReason"] == "MeterValuePeriodic",
            STEP,
        )
        .await
        .expect("a periodic update");
    assert_eq!(update["eventType"], "Updated");
    assert_eq!(
        update["meterValue"][0]["sampledValue"][0]["context"],
        "Sample.Periodic"
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn sample_meter_outside_a_transaction_sends_meter_values() {
    let csms = v201_csms().await;
    let (station, task) = spawn_with_clock(&csms, Arc::new(SystemClock));
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    send(&station, Command2::SampleMeter { evse: 2 }).await;
    let sent = csms
        .wait_for_call_after(0, "MeterValues", |_| true, STEP)
        .await
        .expect("MeterValues");
    assert_eq!(sent["evseId"], 2);
    assert_eq!(
        sent["meterValue"][0]["sampledValue"][0]["measurand"],
        "Energy.Active.Import.Register"
    );
    // An EVSE that does not exist is refused.
    let error = station
        .send(Command2::SampleMeter { evse: 9 })
        .await
        .expect_err("no such EVSE");
    assert!(matches!(error, StationError::InvalidArgument(_)));
    stop(&station, task).await;
}
