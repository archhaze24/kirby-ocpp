//! Device model of the OCPP 2.0.1 station: variables, reports and secrets, over the wire.
//!
//! The table, the `SetVariables` and `GetVariables` rules and the chunk arithmetic are unit
//! tested next to the code; these tests drive a running station from a mock CSMS.

use std::sync::Arc;

use chrono::Utc;
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::frame::Frame;
use kirby_ocpp_core::station::Event;
use kirby_ocpp_core::{ManualClock, Station2, Station2Handle, StationConfig};
use kirby_ocpp_testkit::{MockCsms, Reply};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::station2_support::{STEP, calls_of, spawn_v201, v201_csms, wait_booted, wait_snapshot};

const SECRET: &str = "sentinel-Password-0123456789-xyz";

/// Sends a CSMS CALL and returns the CALLRESULT payload.
async fn call(csms: &MockCsms, action: &str, payload: Value) -> Value {
    match csms
        .send_call(action, payload)
        .await
        .expect("the station answers")
    {
        Frame::CallResult { payload, .. } => payload,
        other => panic!("{action} was not answered with a CALLRESULT: {other:?}"),
    }
}

async fn booted_station(csms: &MockCsms) -> (Station2Handle, JoinHandle<()>) {
    let (station, task) = spawn_v201(csms);
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    (station, task)
}

/// A station with `evses` EVSEs, so that a full report needs several chunks.
fn spawn_with_evses(csms: &MockCsms, evses: u32) -> (Station2Handle, JoinHandle<()>) {
    let mut config = StationConfig::new(csms.url())
        .with_connectors(evses)
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
    Station2::spawn(config, Arc::new(ManualClock::new(Utc::now())))
}

async fn stop(station: &Station2Handle, task: JoinHandle<()>) {
    station.shutdown().await.expect("the station stops");
    timeout(STEP, task)
        .await
        .expect("the station task ends in time")
        .expect("the station task does not panic");
}

async fn wait_log(events: &mut broadcast::Receiver<Event>, needle: &str) {
    let found = timeout(STEP, async {
        loop {
            match events.recv().await {
                Ok(Event::Log(record)) if record.text.contains(needle) => return true,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return false,
            }
        }
    })
    .await;
    assert_eq!(found, Ok(true), "no log line containing `{needle}`");
}

fn item(component: &str, variable: &str) -> Value {
    json!({"component": {"name": component}, "variable": {"name": variable}})
}

#[tokio::test]
async fn get_variables_answers_in_request_order() {
    let csms = v201_csms().await;
    let (station, task) = booted_station(&csms).await;
    let answer = call(
        &csms,
        "GetVariables",
        json!({"getVariableData": [
            item("OCPPCommCtrlr", "HeartbeatInterval"),
            item("NoSuchComponent", "X"),
            item("SecurityCtrlr", "BasicAuthPassword"),
            {"attributeType": "Target", "component": {"name": "TxCtrlr"},
             "variable": {"name": "TxStartPoint"}},
            {"component": {"name": "Connector", "evse": {"id": 2, "connectorId": 1}},
             "variable": {"name": "ConnectorType"}},
        ]}),
    )
    .await;
    let results = answer["getVariableResult"].as_array().expect("results");
    let statuses: Vec<&str> = results
        .iter()
        .map(|r| r["attributeStatus"].as_str().expect("status"))
        .collect();
    assert_eq!(
        statuses,
        [
            "Accepted",
            "UnknownComponent",
            "Rejected",
            "NotSupportedAttributeType",
            "Accepted"
        ]
    );
    // The booted interval is the variable's value.
    let interval = station.snapshot().heartbeat_interval_secs.to_string();
    assert_eq!(results[0]["attributeValue"], json!(interval));
    assert_eq!(results[0]["attributeType"], json!("Actual"));
    assert_eq!(results[4]["attributeValue"], json!("cType2"));
    assert!(results[1].get("attributeValue").is_none());
    assert!(results[2].get("attributeValue").is_none());
    stop(&station, task).await;
}

#[tokio::test]
async fn get_variables_over_the_item_limit_is_a_call_error() {
    let csms = v201_csms().await;
    let (station, task) = booted_station(&csms).await;
    let items: Vec<Value> = (0..101)
        .map(|_| item("OCPPCommCtrlr", "HeartbeatInterval"))
        .collect();
    let frame = csms
        .send_call("GetVariables", json!({"getVariableData": items}))
        .await
        .expect("the station answers");
    match frame {
        Frame::CallError { code, .. } => assert_eq!(code.as_str(), "OccurrenceConstraintViolation"),
        other => panic!("expected a CALLERROR, got {other:?}"),
    }
    stop(&station, task).await;
}

#[tokio::test]
async fn set_variables_changes_the_value_and_the_snapshot() {
    let csms = v201_csms().await;
    let (station, task) = booted_station(&csms).await;
    let answer = call(
        &csms,
        "SetVariables",
        json!({"setVariableData": [
            {"attributeValue": "5", "component": {"name": "OCPPCommCtrlr"},
             "variable": {"name": "HeartbeatInterval"}},
            {"attributeValue": "0", "component": {"name": "OCPPCommCtrlr"},
             "variable": {"name": "ResetRetries"}},
            {"attributeValue": "x", "component": {"name": "OCPPCommCtrlr"},
             "variable": {"name": "ResetRetries"}},
            {"attributeValue": "ParkingBayOccupancy", "component": {"name": "TxCtrlr"},
             "variable": {"name": "TxStartPoint"}},
            {"attributeValue": "1", "component": {"name": "ClockCtrlr"},
             "variable": {"name": "Nope"}},
        ]}),
    )
    .await;
    let statuses: Vec<&str> = answer["setVariableResult"]
        .as_array()
        .expect("results")
        .iter()
        .map(|r| r["attributeStatus"].as_str().expect("status"))
        .collect();
    assert_eq!(
        statuses,
        [
            "Accepted",
            "Accepted",
            "Rejected",
            "Rejected",
            "UnknownVariable"
        ]
    );
    let snapshot = wait_snapshot(&station, "the new heartbeat interval", |s| {
        s.heartbeat_interval_secs == 5
    })
    .await;
    let view = |name: &str| {
        snapshot
            .variables
            .iter()
            .find(|v| v.variable == name)
            .and_then(|v| v.value.clone())
    };
    assert_eq!(view("HeartbeatInterval").as_deref(), Some("5"));
    assert_eq!(view("ResetRetries").as_deref(), Some("0"));
    let read = call(
        &csms,
        "GetVariables",
        json!({"getVariableData": [item("OCPPCommCtrlr", "HeartbeatInterval")]}),
    )
    .await;
    assert_eq!(read["getVariableResult"][0]["attributeValue"], json!("5"));
    stop(&station, task).await;
}

#[tokio::test]
async fn password_is_write_only_everywhere() {
    let csms = v201_csms().await;
    let (station, task) = booted_station(&csms).await;
    let answer = call(
        &csms,
        "SetVariables",
        json!({"setVariableData": [
            {"attributeValue": SECRET, "component": {"name": "SecurityCtrlr"},
             "variable": {"name": "BasicAuthPassword"}},
        ]}),
    )
    .await;
    assert_eq!(
        answer["setVariableResult"][0]["attributeStatus"],
        json!("Accepted")
    );
    let read = call(
        &csms,
        "GetVariables",
        json!({"getVariableData": [item("SecurityCtrlr", "BasicAuthPassword")]}),
    )
    .await;
    assert_eq!(
        read["getVariableResult"][0]["attributeStatus"],
        json!("Rejected")
    );
    assert!(!read.to_string().contains(SECRET));
    let snapshot = station.snapshot();
    let password = snapshot
        .variables
        .iter()
        .find(|v| v.variable == "BasicAuthPassword")
        .expect("the password row");
    assert_eq!(password.value, None);
    assert!(
        !serde_json::to_string(&snapshot)
            .expect("json")
            .contains(SECRET)
    );

    let report = call(
        &csms,
        "GetBaseReport",
        json!({"requestId": 1, "reportBase": "ConfigurationInventory"}),
    )
    .await;
    assert_eq!(report["status"], json!("Accepted"));
    let chunk = csms
        .wait_for_call("NotifyReport", STEP)
        .await
        .expect("a report chunk");
    let rows = chunk["reportData"].as_array().expect("report data");
    assert!(
        rows.iter()
            .any(|r| r["variable"]["name"] == json!("BasicAuthPassword"))
    );
    assert!(
        !calls_of(&csms, "NotifyReport")
            .iter()
            .any(|c| c.to_string().contains(SECRET))
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn base_report_is_sent_in_chunks_after_the_reply() {
    let csms = v201_csms().await;
    let (station, task) = spawn_with_evses(&csms, 40);
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    let answer = call(
        &csms,
        "GetBaseReport",
        json!({"requestId": 9, "reportBase": "FullInventory"}),
    )
    .await;
    assert_eq!(answer["status"], json!("Accepted"));
    // 40 EVSEs give far more than one chunk of 100 items; wait for the last one.
    let last = csms
        .wait_for_call_after(0, "NotifyReport", |p| p.get("tbc").is_none(), STEP)
        .await
        .expect("the last chunk");
    assert!(last["reportData"].as_array().is_some_and(|d| !d.is_empty()));
    let chunks = calls_of(&csms, "NotifyReport");
    assert!(chunks.len() >= 2, "{} chunks", chunks.len());
    for (index, chunk) in chunks.iter().enumerate() {
        assert_eq!(chunk["requestId"], json!(9));
        assert_eq!(chunk["seqNo"], json!(index));
        let count = chunk["reportData"].as_array().expect("data").len();
        assert!(count <= 100);
        if index + 1 < chunks.len() {
            assert_eq!(chunk["tbc"], json!(true));
            assert_eq!(count, 100);
        } else {
            assert!(chunk.get("tbc").is_none());
        }
        assert!(chunk["generatedAt"].is_string());
    }
    let first = &chunks[0]["reportData"][0];
    assert!(first["variableCharacteristics"].is_object());
    // A new report is accepted once the previous one is complete.
    let second = call(
        &csms,
        "GetBaseReport",
        json!({"requestId": 10, "reportBase": "SummaryInventory"}),
    )
    .await;
    assert_eq!(second["status"], json!("Accepted"));
    stop(&station, task).await;
}

#[tokio::test]
async fn second_base_report_while_chunking_is_rejected() {
    let csms = v201_csms().await;
    // The first chunk is never answered, so the report stays in progress.
    csms.respond("NotifyReport", |_| Reply::Silence);
    let (station, task) = booted_station(&csms).await;
    let mut events = station.subscribe();
    let first = call(
        &csms,
        "GetBaseReport",
        json!({"requestId": 1, "reportBase": "FullInventory"}),
    )
    .await;
    assert_eq!(first["status"], json!("Accepted"));
    csms.wait_for_call("NotifyReport", STEP)
        .await
        .expect("the first chunk");
    let second = call(
        &csms,
        "GetBaseReport",
        json!({"requestId": 2, "reportBase": "FullInventory"}),
    )
    .await;
    assert_eq!(second["status"], json!("Rejected"));
    // The call timeout abandons the report; then a new request is accepted.
    wait_log(&mut events, "report is abandoned").await;
    csms.respond("NotifyReport", |_| Reply::Result(json!({})));
    let third = call(
        &csms,
        "GetBaseReport",
        json!({"requestId": 3, "reportBase": "SummaryInventory"}),
    )
    .await;
    assert_eq!(third["status"], json!("Accepted"));
    stop(&station, task).await;
}

#[tokio::test]
async fn get_report_is_not_supported() {
    let csms = v201_csms().await;
    let (station, task) = booted_station(&csms).await;
    let answer = call(&csms, "GetReport", json!({"requestId": 4})).await;
    assert_eq!(answer["status"], json!("NotSupported"));
    assert_eq!(calls_of(&csms, "NotifyReport").len(), 0);
    stop(&station, task).await;
}
