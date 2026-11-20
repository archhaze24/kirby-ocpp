//! `GetReport` (B08) of the OCPP 2.x station, over the wire on 2.0.1 and 2.1.
//!
//! Each test drives a running station from a mock CSMS and reads the `NotifyReport` CALLs the
//! station sends. Waits are on recorded CALLs and the log, with deadlines.

use std::sync::Arc;

use chrono::Utc;
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::frame::Frame;
use kirby_ocpp_core::station::Event;
use kirby_ocpp_core::{ManualClock, Station2, Station2Handle, StationConfig};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms, Reply, SubprotocolPolicy};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::station2_support::{STEP, calls_of, v201_csms, wait_booted};

const VERSIONS: [Protocol; 2] = [Protocol::V201, Protocol::V21];

/// Every component of the device model, in report order.
const ALL_COMPONENTS: [&str; 14] = [
    "AlignedDataCtrlr",
    "AuthCacheCtrlr",
    "AuthCtrlr",
    "ChargingStation",
    "ClockCtrlr",
    "Connector",
    "DeviceDataCtrlr",
    "EVSE",
    "LocalAuthListCtrlr",
    "OCPPCommCtrlr",
    "ReservationCtrlr",
    "SampledDataCtrlr",
    "SecurityCtrlr",
    "TxCtrlr",
];

async fn csms_for(protocol: Protocol) -> MockCsms {
    if protocol == Protocol::V201 {
        return v201_csms().await;
    }
    let csms = MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Require("ocpp2.1".to_owned()),
        ..CsmsOptions::default()
    })
    .await
    .expect("mock CSMS starts");
    csms.respond("BootNotification", |_| {
        Reply::Result(json!({
            "status": "Accepted",
            "currentTime": Utc::now().to_rfc3339(),
            "interval": 3600,
        }))
    });
    csms.respond("Heartbeat", |_| {
        Reply::Result(json!({"currentTime": Utc::now().to_rfc3339()}))
    });
    for action in ["StatusNotification", "NotifyReport"] {
        csms.respond(action, |_| Reply::Result(json!({})));
    }
    csms
}

async fn start(protocol: Protocol, csms: &MockCsms) -> (Station2Handle, JoinHandle<()>) {
    start_with(protocol, csms, 2).await
}

async fn start_with(
    protocol: Protocol,
    csms: &MockCsms,
    evses: u32,
) -> (Station2Handle, JoinHandle<()>) {
    let mut config = StationConfig::new(csms.url())
        .with_connectors(evses)
        .with_call_timeout_ms(2_000)
        .with_heartbeat_interval_secs(3600)
        .with_reconnect(ReconnectConfig {
            enabled: true,
            initial_delay_ms: 30,
            max_delay_ms: 100,
            max_attempts: 0,
        })
        .without_persistence();
    config.ws_ping_interval_secs = 0;
    protocol.token().clone_into(&mut config.ws_subprotocol);
    config.protocol = protocol;
    let (station, task) = Station2::spawn(config, Arc::new(ManualClock::new(Utc::now())));
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    (station, task)
}

async fn stop(station: &Station2Handle, task: JoinHandle<()>) {
    station.shutdown().await.expect("the station stops");
    timeout(STEP, task)
        .await
        .expect("the station task ends in time")
        .expect("the station task does not panic");
}

async fn ask(csms: &MockCsms, action: &str, payload: Value) -> Frame {
    csms.send_call(action, payload)
        .await
        .expect("the station answers")
}

async fn call(csms: &MockCsms, action: &str, payload: Value) -> Value {
    match ask(csms, action, payload).await {
        Frame::CallResult { payload, .. } => payload,
        other => panic!("{action} was not answered with a CALLRESULT: {other:?}"),
    }
}

/// Sends a `GetReport`; on `Accepted` waits for the last `NotifyReport` of the report and
/// returns the answer status with every reported item, in order.
async fn report(csms: &MockCsms, request: Value) -> (String, Vec<Value>) {
    let before = calls_of(csms, "NotifyReport").len();
    let mark = csms.calls_since(0).len();
    // The station frees its report slot when it handles the answer to the last chunk, which can
    // come after the CSMS recorded that chunk: poll while it is busy.
    let answer = timeout(STEP, async {
        loop {
            let answer = call(csms, "GetReport", request.clone()).await;
            if answer["status"] != json!("Rejected") {
                return answer;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the station accepts or answers the request in time");
    let status = answer["status"].as_str().expect("a status").to_owned();
    if status != "Accepted" {
        assert_eq!(calls_of(csms, "NotifyReport").len(), before, "{answer}");
        return (status, Vec::new());
    }
    csms.wait_for_call_after(mark, "NotifyReport", |p| p.get("tbc").is_none(), STEP)
        .await
        .expect("the last chunk");
    let items = calls_of(csms, "NotifyReport")
        .into_iter()
        .skip(before)
        .flat_map(|chunk| chunk["reportData"].as_array().cloned().unwrap_or_default())
        .collect();
    (status, items)
}

fn names(items: &[Value], key: &str) -> Vec<String> {
    let mut found: Vec<String> = items
        .iter()
        .filter_map(|item| item[key]["name"].as_str().map(str::to_owned))
        .collect();
    found.sort();
    found.dedup();
    found
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
    assert_eq!(found, Ok(true), "no log line with {needle:?}");
}

#[tokio::test]
async fn a_component_without_a_variable_reports_all_its_variables() {
    for protocol in VERSIONS {
        let csms = csms_for(protocol).await;
        let (station, task) = start(protocol, &csms).await;
        let (status, items) = report(
            &csms,
            json!({"requestId": 7, "componentVariable": [
                {"component": {"name": "EVSE", "evse": {"id": 1}}}
            ]}),
        )
        .await;
        assert_eq!(status, "Accepted", "{protocol:?}");
        assert!(items.len() > 2, "{items:?}");
        for item in &items {
            assert_eq!(item["component"]["name"], json!("EVSE"));
            assert_eq!(item["component"]["evse"]["id"], json!(1));
            assert!(item["component"]["evse"].get("connectorId").is_none());
            assert!(item["variableCharacteristics"].is_object());
        }
        let variables = names(&items, "variable");
        assert!(variables.contains(&"AvailabilityState".to_owned()));
        for chunk in calls_of(&csms, "NotifyReport") {
            assert_eq!(chunk["requestId"], json!(7));
        }
        stop(&station, task).await;
    }
}

#[tokio::test]
async fn a_named_variable_reports_only_that_variable() {
    for protocol in VERSIONS {
        let csms = csms_for(protocol).await;
        let (station, task) = start(protocol, &csms).await;
        let (status, items) = report(
            &csms,
            json!({"requestId": 1, "componentVariable": [
                {"component": {"name": "ChargingStation"},
                 "variable": {"name": "AvailabilityState"}},
                {"component": {"name": "Connector", "evse": {"id": 2, "connectorId": 1}},
                 "variable": {"name": "Available"}}
            ]}),
        )
        .await;
        assert_eq!(status, "Accepted");
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(items[0]["component"]["name"], json!("ChargingStation"));
        assert_eq!(items[0]["variable"]["name"], json!("AvailabilityState"));
        assert_eq!(items[1]["component"]["evse"]["id"], json!(2));
        assert_eq!(items[1]["component"]["evse"]["connectorId"], json!(1));
        stop(&station, task).await;
    }
}

#[tokio::test]
async fn instances_and_evses_match_exactly() {
    let csms = csms_for(Protocol::V201).await;
    let (station, task) = start(Protocol::V201, &csms).await;
    let device = |variable: Value| {
        json!({"requestId": 2, "componentVariable": [
            {"component": {"name": "DeviceDataCtrlr"}, "variable": variable}
        ]})
    };
    // The variable instance picks one of the instanced rows.
    let (status, items) = report(
        &csms,
        device(json!({"name": "ItemsPerMessage", "instance": "GetReport"})),
    )
    .await;
    assert_eq!(status, "Accepted");
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["variable"]["instance"], json!("GetReport"));
    // An instance that does not exist, or none for an instanced variable, matches nothing.
    for variable in [
        json!({"name": "ItemsPerMessage", "instance": "Nope"}),
        json!({"name": "ItemsPerMessage"}),
    ] {
        let (status, _) = report(&csms, device(variable)).await;
        assert_eq!(status, "EmptyResultSet");
    }
    // A component instance, an unknown EVSE, a connector without a row and a name at the
    // wrong level match nothing.
    for component in [
        json!({"name": "ChargingStation", "instance": "x"}),
        json!({"name": "EVSE", "evse": {"id": 3}}),
        json!({"name": "EVSE"}),
        json!({"name": "ChargingStation", "evse": {"id": 1}}),
        json!({"name": "Connector", "evse": {"id": 1, "connectorId": 2}}),
        json!({"name": "NoSuchComponent"}),
    ] {
        let (status, _) = report(
            &csms,
            json!({"requestId": 3, "componentVariable": [{"component": component}]}),
        )
        .await;
        assert_eq!(status, "EmptyResultSet", "{component}");
    }
    stop(&station, task).await;
}

#[tokio::test]
async fn the_available_criterion_selects_components_with_available_true() {
    for protocol in VERSIONS {
        let csms = csms_for(protocol).await;
        let (station, task) = start(protocol, &csms).await;
        let (status, items) = report(
            &csms,
            json!({"requestId": 4, "componentCriteria": ["Available"]}),
        )
        .await;
        assert_eq!(status, "Accepted");
        // `Available` is true where it exists, and a component without it meets the criterion
        // too (B08.FR.08).
        assert_eq!(names(&items, "component"), ALL_COMPONENTS);
        // Whole components are reported, not only the variable that matched.
        assert!(items.iter().any(
            |item| item["variable"]["name"] == json!("AvailabilityState")
                && item["component"]["name"] == json!("EVSE")
        ));
        // Both EVSEs and their connectors are covered.
        let evses: Vec<_> = items
            .iter()
            .filter(|i| i["component"]["name"] == json!("Connector"))
            .map(|i| i["component"]["evse"]["id"].clone())
            .collect();
        assert!(evses.contains(&json!(1)) && evses.contains(&json!(2)));
        stop(&station, task).await;
    }
}

#[tokio::test]
async fn a_missing_variable_meets_active_but_never_problem() {
    for protocol in VERSIONS {
        let csms = csms_for(protocol).await;
        let (station, task) = start(protocol, &csms).await;
        // No component has a `Problem` variable (B08.FR.10).
        let (status, items) = report(
            &csms,
            json!({"requestId": 5, "componentCriteria": ["Problem"]}),
        )
        .await;
        assert_eq!(status, "EmptyResultSet");
        assert_eq!(items, Vec::<Value>::new());
        assert_eq!(calls_of(&csms, "NotifyReport"), Vec::<Value>::new());
        // No component has an `Active` variable either, so all of them meet it (B08.FR.07).
        let (status, items) = report(
            &csms,
            json!({"requestId": 6, "componentCriteria": ["Active"]}),
        )
        .await;
        assert_eq!(status, "Accepted");
        assert_eq!(names(&items, "component"), ALL_COMPONENTS);
        stop(&station, task).await;
    }
}

#[tokio::test]
async fn the_enabled_criterion_follows_the_variable_value() {
    for protocol in VERSIONS {
        let csms = csms_for(protocol).await;
        let (station, task) = start(protocol, &csms).await;
        let (_, items) = report(
            &csms,
            json!({"requestId": 6, "componentCriteria": ["Enabled"]}),
        )
        .await;
        // Every component is enabled or has no `Enabled` variable (B08.FR.09).
        let enabled = names(&items, "component");
        assert_eq!(enabled, ALL_COMPONENTS);
        let set = call(
            &csms,
            "SetVariables",
            json!({"setVariableData": [{
                "attributeValue": "false",
                "component": {"name": "SampledDataCtrlr"},
                "variable": {"name": "Enabled"}
            }]}),
        )
        .await;
        assert_eq!(
            set["setVariableResult"][0]["attributeStatus"],
            json!("Accepted")
        );
        let (_, items) = report(
            &csms,
            json!({"requestId": 8, "componentCriteria": ["Enabled"]}),
        )
        .await;
        let without: Vec<&str> = ALL_COMPONENTS
            .iter()
            .copied()
            .filter(|name| *name != "SampledDataCtrlr")
            .collect();
        assert_eq!(names(&items, "component"), without);
        stop(&station, task).await;
    }
}

#[tokio::test]
async fn criteria_combine_with_or_and_with_the_component_selection() {
    let csms = csms_for(Protocol::V201).await;
    let (station, task) = start(Protocol::V201, &csms).await;
    let (_, available) = report(
        &csms,
        json!({"requestId": 1, "componentCriteria": ["Available"]}),
    )
    .await;
    let (status, either) = report(
        &csms,
        json!({"requestId": 2, "componentCriteria": ["Problem", "Available", "Active"]}),
    )
    .await;
    assert_eq!(status, "Accepted");
    assert_eq!(either, available);
    // Criteria and component variables both limit the report.
    let (status, items) = report(
        &csms,
        json!({"requestId": 3, "componentCriteria": ["Available"], "componentVariable": [
            {"component": {"name": "EVSE", "evse": {"id": 1}}},
            {"component": {"name": "AuthCtrlr"}}
        ]}),
    )
    .await;
    assert_eq!(status, "Accepted");
    // `AuthCtrlr` has no `Available` variable, which meets the criterion (B08.FR.08).
    assert_eq!(names(&items, "component"), ["AuthCtrlr", "EVSE"]);
    assert!(
        items
            .iter()
            .filter(|i| i["component"]["name"] == json!("EVSE"))
            .all(|i| i["component"]["evse"]["id"] == json!(1))
    );
    let (status, _) = report(
        &csms,
        json!({"requestId": 4, "componentCriteria": ["Problem"], "componentVariable": [
            {"component": {"name": "EVSE", "evse": {"id": 1}}}
        ]}),
    )
    .await;
    assert_eq!(status, "EmptyResultSet");
    // No criteria and no component variables: the whole model.
    let (status, all) = report(&csms, json!({"requestId": 5})).await;
    assert_eq!(status, "Accepted");
    // No component reports `Available` false, so the criterion keeps the whole model.
    assert_eq!(all.len(), available.len());
    stop(&station, task).await;
}

#[tokio::test]
async fn write_only_variables_are_reported_without_a_value() {
    let csms = csms_for(Protocol::V201).await;
    let (station, task) = start(Protocol::V201, &csms).await;
    let (status, items) = report(
        &csms,
        json!({"requestId": 1, "componentVariable": [
            {"component": {"name": "SecurityCtrlr"}, "variable": {"name": "BasicAuthPassword"}}
        ]}),
    )
    .await;
    assert_eq!(status, "Accepted");
    assert_eq!(items.len(), 1);
    assert!(items[0]["variableAttribute"][0].get("value").is_none());
    stop(&station, task).await;
}

#[tokio::test]
async fn a_second_report_while_one_is_sent_is_rejected() {
    for protocol in VERSIONS {
        let csms = csms_for(protocol).await;
        // The first chunk is never answered, so the report stays in progress.
        csms.respond("NotifyReport", |_| Reply::Silence);
        let (station, task) = start(protocol, &csms).await;
        let mut events = station.subscribe();
        let first = call(
            &csms,
            "GetReport",
            json!({"requestId": 1, "componentCriteria": ["Available"]}),
        )
        .await;
        assert_eq!(first["status"], json!("Accepted"));
        csms.wait_for_call("NotifyReport", STEP)
            .await
            .expect("the first chunk");
        let second = call(
            &csms,
            "GetReport",
            json!({"requestId": 2, "componentCriteria": ["Available"]}),
        )
        .await;
        assert_eq!(second["status"], json!("Rejected"));
        assert!(second["statusInfo"]["reasonCode"].is_string());
        // A base report is refused too while this one runs.
        let base = call(
            &csms,
            "GetBaseReport",
            json!({"requestId": 3, "reportBase": "SummaryInventory"}),
        )
        .await;
        assert_eq!(base["status"], json!("Rejected"));
        wait_log(&mut events, "report is abandoned").await;
        csms.respond("NotifyReport", |_| Reply::Result(json!({})));
        let (status, _) = report(
            &csms,
            json!({"requestId": 4, "componentCriteria": ["Available"]}),
        )
        .await;
        assert_eq!(status, "Accepted");
        stop(&station, task).await;
    }
}

#[tokio::test]
async fn the_report_is_chunked_by_items_per_message_and_the_request_is_limited_by_it() {
    for protocol in VERSIONS {
        let csms = csms_for(protocol).await;
        // 40 EVSEs make a report far longer than the default 100 items per message.
        let (station, task) = start_with(protocol, &csms, 40).await;
        let (status, items) = report(
            &csms,
            json!({"requestId": 9, "componentCriteria": ["Available"]}),
        )
        .await;
        assert_eq!(status, "Accepted");
        let chunks = calls_of(&csms, "NotifyReport");
        assert!(chunks.len() >= 2, "{} chunks", chunks.len());
        assert_eq!(
            items.len(),
            chunks
                .iter()
                .map(|c| c["reportData"].as_array().map_or(0, Vec::len))
                .sum::<usize>()
        );
        for (index, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk["requestId"], json!(9));
            assert_eq!(chunk["seqNo"], json!(index));
            let count = chunk["reportData"].as_array().expect("data").len();
            assert!(count <= 100);
            if index + 1 < chunks.len() {
                assert_eq!(count, 100);
                assert_eq!(chunk["tbc"], json!(true));
            } else {
                assert!(chunk.get("tbc").is_none());
            }
        }
        // More component variables than `ItemsPerMessage` (100): the request is refused.
        let evse = json!({"component": {"name": "EVSE", "evse": {"id": 1}}});
        let frame = ask(
            &csms,
            "GetReport",
            json!({"requestId": 10, "componentVariable": vec![evse; 101]}),
        )
        .await;
        match frame {
            Frame::CallError { code, .. } => {
                assert_eq!(code.as_str(), "OccurrenceConstraintViolation");
            }
            other => panic!("expected a CALLERROR, got {other:?}"),
        }
        stop(&station, task).await;
    }
}
