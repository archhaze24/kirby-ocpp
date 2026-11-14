//! Transactions and `TransactionEvent` generation of the OCPP 2.0.1 station, over the wire.
//!
//! Sessions here start at the cable (`TxStartPoint=EVConnected`), which needs no token. The
//! paths that depend on authorization are unit tested next to the code and driven end to end
//! by the authorization tests. Waits are on recorded CALLs and snapshots with deadlines.

use std::sync::Arc;

use chrono::Utc;
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::frame::Frame;
use kirby_ocpp_core::ocpp::v2::types::{ChargingState, StopReason};
use kirby_ocpp_core::{Clock, Command2, ManualClock, Station2, Station2Handle, StationConfig};
use kirby_ocpp_testkit::{Fault, MockCsms};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::station2_support::{STEP, v201_csms, wait_booted, wait_snapshot};

/// A station against `csms` with the given clock; it does not connect by itself.
pub(crate) fn spawn_with_clock(
    csms: &MockCsms,
    clock: Arc<dyn Clock>,
) -> (Station2Handle, JoinHandle<()>) {
    let mut config = StationConfig::new(csms.url())
        .with_connectors(2)
        .with_call_timeout_ms(2_000)
        .with_heartbeat_interval_secs(30)
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
    Station2::spawn(config, clock)
}

/// Sets device-model variables through `SetVariables` and checks that all were accepted.
/// Each entry is `(component, variable, instance, value)`.
pub(crate) async fn set_variables(csms: &MockCsms, entries: &[(&str, &str, Option<&str>, &str)]) {
    let data: Vec<Value> = entries
        .iter()
        .map(|(component, variable, instance, value)| {
            let mut variable = json!({"name": variable});
            if let Some(instance) = instance {
                variable["instance"] = json!(instance);
            }
            json!({
                "attributeValue": value,
                "component": {"name": component},
                "variable": variable,
            })
        })
        .collect();
    let answer = call(csms, "SetVariables", json!({"setVariableData": data})).await;
    let results = answer["setVariableResult"].as_array().expect("results");
    for (result, entry) in results.iter().zip(entries) {
        assert_eq!(result["attributeStatus"], "Accepted", "{entry:?}: {result}");
    }
    assert_eq!(results.len(), entries.len());
}

/// Sends a CSMS CALL and returns the CALLRESULT payload.
pub(crate) async fn call(csms: &MockCsms, action: &str, payload: Value) -> Value {
    match csms
        .send_call(action, payload)
        .await
        .expect("the station answers")
    {
        Frame::CallResult { payload, .. } => payload,
        other => panic!("{action} was not answered with a CALLRESULT: {other:?}"),
    }
}

/// A connected, booted station with the clock frozen.
pub(crate) async fn booted(csms: &MockCsms) -> (Station2Handle, JoinHandle<()>) {
    let (station, task) = spawn_with_clock(csms, Arc::new(ManualClock::new(Utc::now())));
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    (station, task)
}

pub(crate) async fn stop(station: &Station2Handle, task: JoinHandle<()>) {
    station.shutdown().await.expect("the station stops");
    timeout(STEP, task)
        .await
        .expect("the station task ends in time")
        .expect("the station task does not panic");
}

/// Waits for the `count`-th `TransactionEvent` CALL and returns every one received so far.
pub(crate) async fn wait_events(csms: &MockCsms, count: usize) -> Vec<Value> {
    csms.wait_for_call_count_after(0, "TransactionEvent", count, |_| true, STEP)
        .await
        .expect("the TransactionEvents arrive")
}

/// Every `TransactionEvent` payload received so far.
pub(crate) fn events(csms: &MockCsms) -> Vec<Value> {
    csms.calls_since(0)
        .into_iter()
        .filter(|call| call.action == "TransactionEvent")
        .map(|call| call.payload)
        .collect()
}

/// Sends a command and expects it to succeed.
pub(crate) async fn send(station: &Station2Handle, command: Command2) {
    station.send(command).await.expect("command accepted");
}

/// Waits until every queued event was delivered and answered.
pub(crate) async fn wait_queue_empty(station: &Station2Handle) {
    wait_snapshot(station, "an empty transaction queue", |s| {
        s.queued_events == 0
    })
    .await;
}

/// The Wh value of the first sample of `meterValue[index]`.
fn sample_wh(event: &Value, index: usize) -> u64 {
    event["meterValue"][index]["sampledValue"][0]["value"]
        .as_u64()
        .unwrap_or_else(|| panic!("no energy sample at {index} in {event}"))
}

#[tokio::test]
async fn cable_session_numbers_events_and_merges_optional_fields() {
    let csms = v201_csms().await;
    let (station, task) = booted(&csms).await;
    set_variables(
        &csms,
        &[
            ("TxCtrlr", "TxStartPoint", None, "EVConnected"),
            ("SampledDataCtrlr", "TxUpdatedInterval", None, "1"),
        ],
    )
    .await;

    send(&station, Command2::PlugIn { evse: 1 }).await;
    let started = wait_events(&csms, 1).await.remove(0);
    assert_eq!(started["eventType"], "Started");
    assert_eq!(started["triggerReason"], "CablePluggedIn");
    assert_eq!(started["seqNo"], 0);
    assert_eq!(started["transactionInfo"]["chargingState"], "EVConnected");
    assert_eq!(started["evse"], json!({"id": 1, "connectorId": 1}));
    assert!(started.get("idToken").is_none(), "no token, no idToken");
    assert!(
        started.get("offline").is_none(),
        "online events are not flagged"
    );
    let id = started["transactionInfo"]["transactionId"]
        .as_str()
        .expect("transaction id")
        .to_owned();
    assert_eq!(id.len(), 36);
    assert_eq!(
        started["meterValue"][0]["sampledValue"][0]["context"],
        "Transaction.Begin"
    );
    assert_eq!(
        started["meterValue"][0]["sampledValue"][0]["measurand"],
        "Energy.Active.Import.Register"
    );
    let begin = sample_wh(&started, 0);

    let snapshot = wait_snapshot(&station, "the transaction in the snapshot", |s| {
        s.evse(1).is_some_and(|e| e.transaction.is_some())
    })
    .await;
    let running = snapshot
        .evse(1)
        .and_then(|e| e.transaction.clone())
        .expect("transaction");
    assert_eq!(running.transaction_id, id);
    assert_eq!(running.charging_state, ChargingState::EvConnected);
    assert!(snapshot.evse(2).is_some_and(|e| e.transaction.is_none()));

    send(
        &station,
        Command2::AdvanceMeter {
            evse: 1,
            wh_delta: 2_500,
        },
    )
    .await;
    // The periodic sample (1 s) shows the advanced register.
    let periodic = csms
        .wait_for_call_after(
            0,
            "TransactionEvent",
            |p| p["triggerReason"] == "MeterValuePeriodic",
            STEP,
        )
        .await
        .expect("a periodic event");
    assert_eq!(periodic["eventType"], "Updated");
    assert_eq!(
        periodic["meterValue"][0]["sampledValue"][0]["context"],
        "Sample.Periodic"
    );
    assert!(sample_wh(&periodic, 0) >= begin + 2_500);

    send(&station, Command2::Unplug { evse: 1 }).await;
    let ended = csms
        .wait_for_call_after(0, "TransactionEvent", |p| p["eventType"] == "Ended", STEP)
        .await
        .expect("the Ended event");
    assert_eq!(ended["triggerReason"], "EVCommunicationLost");
    assert_eq!(ended["transactionInfo"]["stoppedReason"], "EVDisconnected");
    assert_eq!(ended["transactionInfo"]["chargingState"], "Idle");
    assert_eq!(
        ended["meterValue"][0]["sampledValue"][0]["context"],
        "Transaction.Begin"
    );
    assert_eq!(
        ended["meterValue"][1]["sampledValue"][0]["context"],
        "Transaction.End"
    );
    assert!(sample_wh(&ended, 1) >= begin + 2_500);

    // Every event of the transaction, in order: seqNo 0, 1, 2, ...; `evse` only in the first.
    let all = events(&csms);
    let seqs: Vec<i64> = all.iter().filter_map(|e| e["seqNo"].as_i64()).collect();
    assert_eq!(seqs, (0..).take(all.len()).collect::<Vec<i64>>());
    assert!(all.len() >= 3);
    for (index, event) in all.iter().enumerate() {
        assert_eq!(event["transactionInfo"]["transactionId"], id.as_str());
        assert_eq!(event.get("evse").is_some(), index == 0, "{event}");
    }
    wait_snapshot(&station, "the transaction gone", |s| {
        s.evse(1).is_some_and(|e| e.transaction.is_none())
    })
    .await;
    stop(&station, task).await;
}

#[tokio::test]
async fn interval_changed_during_a_transaction_takes_effect() {
    let csms = v201_csms().await;
    let (station, task) = booted(&csms).await;
    set_variables(&csms, &[("TxCtrlr", "TxStartPoint", None, "EVConnected")]).await;
    send(&station, Command2::PlugIn { evse: 1 }).await;
    wait_events(&csms, 1).await;

    // The default interval is 30 s; the change re-arms the running timer.
    set_variables(
        &csms,
        &[("SampledDataCtrlr", "TxUpdatedInterval", None, "1")],
    )
    .await;
    csms.wait_for_call_after(
        0,
        "TransactionEvent",
        |p| p["triggerReason"] == "MeterValuePeriodic",
        STEP,
    )
    .await
    .expect("a periodic event after the change");

    // Back to 0: no further periodic events while the transaction goes on.
    set_variables(
        &csms,
        &[("SampledDataCtrlr", "TxUpdatedInterval", None, "0")],
    )
    .await;
    send(&station, Command2::Unplug { evse: 1 }).await;
    let ended = csms
        .wait_for_call_after(0, "TransactionEvent", |p| p["eventType"] == "Ended", STEP)
        .await
        .expect("the Ended event");
    assert_eq!(ended["transactionInfo"]["stoppedReason"], "EVDisconnected");
    stop(&station, task).await;
}

#[tokio::test]
async fn clock_aligned_samples_are_sent_during_a_transaction() {
    let csms = v201_csms().await;
    let (station, task) = booted(&csms).await;
    set_variables(
        &csms,
        &[
            ("TxCtrlr", "TxStartPoint", None, "EVConnected"),
            ("AlignedDataCtrlr", "Interval", None, "1"),
            (
                "AlignedDataCtrlr",
                "Measurands",
                None,
                "Energy.Active.Import.Register",
            ),
        ],
    )
    .await;
    send(&station, Command2::PlugIn { evse: 1 }).await;
    let clock = csms
        .wait_for_call_after(
            0,
            "TransactionEvent",
            |p| p["triggerReason"] == "MeterValueClock",
            STEP,
        )
        .await
        .expect("a clock-aligned event");
    assert_eq!(clock["eventType"], "Updated");
    assert_eq!(
        clock["meterValue"][0]["sampledValue"][0]["context"],
        "Sample.Clock"
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn local_commands_change_state_and_end_transactions() {
    let csms = v201_csms().await;
    let (station, task) = booted(&csms).await;
    set_variables(&csms, &[("TxCtrlr", "TxStartPoint", None, "EVConnected")]).await;

    // SetChargingState sends an event; StopTransaction(Local) ends with StopAuthorized.
    send(&station, Command2::PlugIn { evse: 1 }).await;
    wait_events(&csms, 1).await;
    send(
        &station,
        Command2::SetChargingState {
            evse: 1,
            state: ChargingState::SuspendedEv,
        },
    )
    .await;
    let suspended = wait_events(&csms, 2).await.remove(1);
    assert_eq!(suspended["eventType"], "Updated");
    assert_eq!(suspended["triggerReason"], "ChargingStateChanged");
    assert_eq!(suspended["transactionInfo"]["chargingState"], "SuspendedEV");
    assert_eq!(suspended["seqNo"], 1);
    send(
        &station,
        Command2::StopTransaction {
            evse: 1,
            reason: StopReason::Local,
        },
    )
    .await;
    let ended = wait_events(&csms, 3).await.remove(2);
    assert_eq!(ended["eventType"], "Ended");
    assert_eq!(ended["triggerReason"], "StopAuthorized");
    assert_eq!(ended["transactionInfo"]["stoppedReason"], "Local");
    assert_eq!(ended["seqNo"], 2);
    // Nothing left to stop or to suspend.
    assert!(
        station
            .send(Command2::StopTransaction {
                evse: 1,
                reason: StopReason::Local
            })
            .await
            .is_err()
    );
    assert!(
        station
            .send(Command2::SetChargingState {
                evse: 1,
                state: ChargingState::Charging
            })
            .await
            .is_err()
    );

    // A fault ends the transaction of the other EVSE.
    send(&station, Command2::PlugIn { evse: 2 }).await;
    csms.wait_for_call_after(
        0,
        "TransactionEvent",
        |p| p["evse"]["id"] == 2 && p["eventType"] == "Started",
        STEP,
    )
    .await
    .expect("Started on EVSE 2");
    send(
        &station,
        Command2::Fault {
            evse: 2,
            connector: None,
        },
    )
    .await;
    let faulted = csms
        .wait_for_call_after(
            0,
            "TransactionEvent",
            |p| p["eventType"] == "Ended" && p["triggerReason"] == "AbnormalCondition",
            STEP,
        )
        .await
        .expect("Ended by the fault");
    assert_eq!(faulted["transactionInfo"]["stoppedReason"], "Other");
    assert_eq!(faulted["seqNo"], 1, "the counter restarts per transaction");
    stop(&station, task).await;
}

#[tokio::test]
async fn meter_values_outside_a_transaction_are_not_queued_offline() {
    let csms = v201_csms().await;
    let (station, task) = booted(&csms).await;

    send(
        &station,
        Command2::AdvanceMeter {
            evse: 1,
            wh_delta: 700,
        },
    )
    .await;
    let first = csms
        .wait_for_call_after(0, "MeterValues", |_| true, STEP)
        .await
        .expect("MeterValues");
    assert_eq!(first["evseId"], 1);
    assert_eq!(
        first["meterValue"][0]["sampledValue"][0]["measurand"],
        "Energy.Active.Import.Register"
    );
    assert!(first["meterValue"][0]["sampledValue"][0]["value"].as_u64() >= Some(700));

    // While the link is down the reading is dropped, not queued.
    station.disconnect().await.expect("disconnect");
    send(
        &station,
        Command2::AdvanceMeter {
            evse: 1,
            wh_delta: 300,
        },
    )
    .await;
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    send(
        &station,
        Command2::AdvanceMeter {
            evse: 1,
            wh_delta: 100,
        },
    )
    .await;
    let readings = csms
        .wait_for_call_count_after(0, "MeterValues", 2, |_| true, STEP)
        .await
        .expect("two readings");
    let wh: Vec<u64> = readings
        .iter()
        .filter_map(|p| p["meterValue"][0]["sampledValue"][0]["value"].as_u64())
        .collect();
    assert!(wh[0] >= 700 && wh[0] < 1_000, "{wh:?}");
    // The second reading is the one after the reconnect: 700 + 300 + 100.
    assert!(wh[1] >= 1_100, "{wh:?}");
    assert_eq!(
        csms.calls_since(0)
            .iter()
            .filter(|c| c.action == "MeterValues")
            .count(),
        2
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn transaction_status_answers_for_known_stopped_and_unknown_ids() {
    let csms = v201_csms().await;
    let (station, task) = booted(&csms).await;
    set_variables(&csms, &[("TxCtrlr", "TxStartPoint", None, "EVConnected")]).await;
    send(&station, Command2::PlugIn { evse: 1 }).await;
    let started = wait_events(&csms, 1).await.remove(0);
    let id = started["transactionInfo"]["transactionId"]
        .as_str()
        .expect("id")
        .to_owned();
    wait_queue_empty(&station).await;

    // Ongoing, nothing queued.
    let ongoing = call(&csms, "GetTransactionStatus", json!({"transactionId": id})).await;
    assert_eq!(
        ongoing,
        json!({"ongoingIndicator": true, "messagesInQueue": false})
    );

    // Unknown id: both false (E14.FR.01).
    let unknown = call(
        &csms,
        "GetTransactionStatus",
        json!({"transactionId": "no-such-id"}),
    )
    .await;
    assert_eq!(
        unknown,
        json!({"ongoingIndicator": false, "messagesInQueue": false})
    );

    // Without an id: only `messagesInQueue` (E14.FR.06).
    let overall = call(&csms, "GetTransactionStatus", json!({})).await;
    assert_eq!(overall, json!({"messagesInQueue": false}));

    send(&station, Command2::Unplug { evse: 1 }).await;
    wait_events(&csms, 2).await;
    wait_queue_empty(&station).await;
    let stopped = call(&csms, "GetTransactionStatus", json!({"transactionId": id})).await;
    assert_eq!(
        stopped,
        json!({"ongoingIndicator": false, "messagesInQueue": false})
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn transaction_status_reports_queued_messages() {
    let csms = v201_csms().await;
    let (station, task) = booted(&csms).await;
    set_variables(
        &csms,
        &[
            ("TxCtrlr", "TxStartPoint", None, "EVConnected"),
            (
                "OCPPCommCtrlr",
                "MessageAttemptInterval",
                Some("TransactionEvent"),
                "1",
            ),
        ],
    )
    .await;
    // The CSMS does not answer the first event: it stays at the head of the queue.
    csms.inject(Fault::silence("TransactionEvent"));
    send(&station, Command2::PlugIn { evse: 1 }).await;
    let started = wait_events(&csms, 1).await.remove(0);
    let id = started["transactionInfo"]["transactionId"]
        .as_str()
        .expect("id")
        .to_owned();

    let queued = call(&csms, "GetTransactionStatus", json!({"transactionId": id})).await;
    assert_eq!(
        queued,
        json!({"ongoingIndicator": true, "messagesInQueue": true})
    );
    let other = call(
        &csms,
        "GetTransactionStatus",
        json!({"transactionId": "other"}),
    )
    .await;
    assert_eq!(
        other,
        json!({"ongoingIndicator": false, "messagesInQueue": false})
    );
    let overall = call(&csms, "GetTransactionStatus", json!({})).await;
    assert_eq!(overall, json!({"messagesInQueue": true}));

    // The retry is answered and the queue drains.
    wait_queue_empty(&station).await;
    let drained = call(&csms, "GetTransactionStatus", json!({"transactionId": id})).await;
    assert_eq!(drained["messagesInQueue"], false);
    stop(&station, task).await;
}
