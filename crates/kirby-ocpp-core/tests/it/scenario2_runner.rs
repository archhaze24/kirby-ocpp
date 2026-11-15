//! Running scenarios against an OCPP 2.0.1 station: every step kind, every `wait_for`
//! condition, `expect_call` in both directions, failure evidence and the shape of reports and
//! JUnit output.
//!
//! Steps are built from JSON so each test reads like the scenario file it stands for. Waits are
//! bounded by deadlines; nothing sleeps for a fixed time.

use std::collections::BTreeMap;
use std::time::Duration;

use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::scenario::{
    Action, Action2, CancelToken, Expect, JunitSuite, RunStatus, Scenario, ScenarioReport, Step,
    StepStatus, Vars, junit_xml, run_scenario, run_scenario_subscribed,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::timeout;

use super::station2_remote::Rig;

/// Steps that read and write like their OCPP 1.6 form; every other name is a 2.0.1 step.
const NEUTRAL: [&str; 9] = [
    "connect",
    "disconnect",
    "reconnect",
    "boot",
    "heartbeat",
    "sleep",
    "expect_call",
    "send_raw",
    "reply",
];

/// A step from its JSON form: the keys `name`, `expect` and `capture` belong to the step, the
/// rest to its action.
fn step(raw: Value) -> Step {
    let mut raw = raw;
    let fields = raw.as_object_mut().expect("a step is an object");
    let name = fields
        .remove("name")
        .map(|v| v.as_str().expect("a name").to_owned());
    let expect = fields
        .remove("expect")
        .map(|v| Expect::deserialize(v).expect("a valid expect"));
    let capture: BTreeMap<String, String> = fields
        .remove("capture")
        .map(|v| serde_json::from_value(v).expect("a valid capture"))
        .unwrap_or_default();
    let tag = fields["action"].as_str().expect("an action").to_owned();
    let action = if NEUTRAL.contains(&tag.as_str()) {
        Action::deserialize(raw).expect("a valid step")
    } else {
        Action::V201(Box::new(Action2::deserialize(raw).expect("a valid step")))
    };
    Step {
        name,
        expect,
        capture,
        action,
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "callers pass `json!` literals"
)]
fn scenario(steps: Value) -> Scenario {
    Scenario {
        name: "t".to_owned(),
        description: String::new(),
        vars: Vars::new(),
        steps: steps
            .as_array()
            .expect("a list of steps")
            .iter()
            .cloned()
            .map(step)
            .collect(),
        tags: Vec::new(),
        source: None,
        ocpp_version: Some(Protocol::V201),
    }
}

async fn run(rig: &Rig, steps: Value) -> ScenarioReport {
    let scenario = scenario(steps);
    timeout(
        Duration::from_secs(60),
        run_scenario(&rig.station, &scenario, &CancelToken::new(), None),
    )
    .await
    .expect("the scenario ends")
}

fn failure(report: &ScenarioReport, index: usize) -> &str {
    match &report.steps[index].status {
        StepStatus::Failed(reason) => reason,
        other => panic!("step {index} did not fail: {other:?}\n{report:#?}"),
    }
}

fn statuses(report: &ScenarioReport) -> Vec<&StepStatus> {
    report.steps.iter().map(|s| &s.status).collect()
}

fn sorted_keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

// ---- step kinds ------------------------------------------------------------------------

#[tokio::test]
async fn a_charging_session_runs_through_its_steps() {
    let rig = Rig::new().await;
    let report = run(
        &rig,
        json!([
            {"action": "plug", "evse": 1},
            {"action": "wait_for", "timeout_ms": 5000,
             "condition": {"kind": "connector_status", "evse": 1, "connector": 1, "status": "Occupied"}},
            {"action": "authorize", "id_token": "TAG-A", "capture": {"auth": "/status"}},
            {"action": "start_tx", "evse": 1, "id_token": "TAG-B",
             "capture": {"tx": "/transaction_id", "e": "/evse"}},
            {"action": "wait_for", "timeout_ms": 5000, "condition": {"kind": "tx_active", "evse": 1}},
            {"action": "expect_call", "call": "TransactionEvent", "direction": "out", "timeout_ms": 5000,
             "match": {"eventType": "Started", "transactionInfo": {"transactionId": "${tx}"}}},
            {"action": "charging_state", "evse": 1, "state": "Charging"},
            {"action": "wait_for", "timeout_ms": 5000,
             "condition": {"kind": "charging_state", "evse": 1, "state": "Charging"}},
            {"action": "wait_for", "timeout_ms": 5000, "condition": {"kind": "no_pending_messages"}},
            {"action": "stop_tx", "evse": 1, "reason": "DeAuthorized"},
            {"action": "wait_for", "timeout_ms": 5000, "condition": {"kind": "tx_idle", "evse": 1}},
            {"action": "expect_call", "call": "TransactionEvent", "direction": "out", "timeout_ms": 5000,
             "match": {"eventType": "Ended", "transactionInfo": {"transactionId": "${tx}", "stoppedReason": "DeAuthorized"}}},
            {"action": "unplug", "evse": 1},
            {"action": "wait_for", "timeout_ms": 5000,
             "condition": {"kind": "connector_status", "evse": 1, "connector": 1, "status": "Available"}},
        ]),
    )
    .await;
    assert!(report.passed, "{report:#?}");
    assert_eq!(report.status, RunStatus::Passed);
    assert_eq!(report.ocpp_version, "2.0.1");
    assert_eq!(report.steps.len(), 14);
    let started = rig.events_of("Started");
    assert_eq!(started.len(), 1);
    // `${tx}` was captured from the transaction the station started.
    assert_eq!(
        report.steps[5].detail.label,
        "expect_call TransactionEvent out"
    );
    let ended = rig.events_of("Ended");
    assert_eq!(ended[0]["transactionInfo"]["stoppedReason"], "DeAuthorized");
    assert_eq!(
        ended[0]["transactionInfo"]["transactionId"],
        started[0]["transactionInfo"]["transactionId"]
    );
    assert_eq!(
        rig.calls("Authorize")[0]["idToken"],
        json!({"idToken": "TAG-A", "type": "ISO14443"})
    );
    rig.stop().await;
}

#[tokio::test]
async fn a_scenario_file_runs_end_to_end() {
    let rig = Rig::new().await;
    let file = r#"
name = "file"
ocpp_version = "2.0.1"
tags = ["smoke"]

[vars]
token = "FILE-TAG"

[[steps]]
action = "plug"
evse = 2

[[steps]]
action = "start_session"
evse = 2
id_token = "${token}"
capture = { tx = "/transaction_id" }

[[steps]]
action = "wait_for"
condition = { kind = "tx_active", evse = 2 }

[[steps]]
action = "expect_call"
call = "TransactionEvent"
direction = "out"
match = { eventType = "Started", transactionInfo = { transactionId = "${tx}" } }

[[steps]]
action = "stop_session"
evse = 2

[[steps]]
action = "wait_for"
condition = { kind = "tx_idle", evse = 2 }
"#;
    let scenario = Scenario::from_toml(file).expect("a valid 2.0.1 file");
    assert_eq!(scenario.protocol(), Protocol::V201);
    let report = timeout(
        Duration::from_secs(60),
        run_scenario(&rig.station, &scenario, &CancelToken::new(), None),
    )
    .await
    .expect("the scenario ends");
    assert!(report.passed, "{report:#?}");
    assert_eq!(report.ocpp_version, "2.0.1");
    assert_eq!(
        report
            .steps
            .iter()
            .map(|s| s.detail.label.as_str())
            .collect::<Vec<_>>(),
        [
            "plug e2",
            // Labels keep the placeholder, so they stay stable across runs.
            "start_tx e2 ${token}",
            "wait_for tx_active e2",
            "expect_call TransactionEvent out",
            "stop_tx e2",
            "wait_for tx_idle e2",
        ]
    );
    assert_eq!(rig.calls("Authorize")[0]["idToken"]["idToken"], "FILE-TAG");
    rig.stop().await;
}

#[tokio::test]
async fn token_types_and_default_station_state_reach_the_csms() {
    let rig = Rig::new().await;
    let report = run(
        &rig,
        json!([
            {"action": "authorize", "id_token": "CENTRAL-1", "type": "Central"},
            {"action": "plug", "evse": 2},
            {"action": "present_token", "evse": 2, "id_token": "LOCAL-1", "type": "Local",
             "capture": {"started": "/transaction_id"}},
            {"action": "wait_for", "timeout_ms": 5000, "condition": {"kind": "tx_active", "evse": 2}},
        ]),
    )
    .await;
    assert!(report.passed, "{report:#?}");
    let tokens: Vec<Value> = rig
        .calls("Authorize")
        .iter()
        .map(|c| c["idToken"].clone())
        .collect();
    assert_eq!(
        tokens[0],
        json!({"idToken": "CENTRAL-1", "type": "Central"})
    );
    assert_eq!(tokens[1], json!({"idToken": "LOCAL-1", "type": "Local"}));
    rig.stop().await;
}

#[tokio::test]
async fn a_rejected_token_fails_start_tx_and_can_be_expected() {
    let rig = Rig::new().await;
    rig.csms.respond("Authorize", |_| {
        kirby_ocpp_testkit::Reply::Result(json!({"idTokenInfo": {"status": "Blocked"}}))
    });
    let report = run(
        &rig,
        json!([
            {"action": "plug", "evse": 1},
            {"action": "start_tx", "evse": 1, "id_token": "BAD"},
            {"action": "heartbeat"},
        ]),
    )
    .await;
    assert!(!report.passed, "{report:#?}");
    assert_eq!(
        failure(&report, 1),
        "start_tx: expected status Accepted, got Blocked"
    );
    assert_eq!(report.steps[1].detail.expected, Some(json!("Accepted")));
    assert_eq!(report.steps[1].detail.actual.as_deref(), Some("Blocked"));
    assert_eq!(report.steps[2].status, StepStatus::Skipped);

    // The same token passes when the scenario says what it expects; `present_token` expects
    // nothing; the answer of both reads `status`.
    let report = run(
        &rig,
        json!([
            {"action": "start_tx", "evse": 1, "id_token": "BAD",
             "expect": {"status": "Blocked"}, "capture": {"s": "/status"}},
            {"action": "present_token", "evse": 1, "id_token": "BAD", "capture": {"t": "/status"}},
            {"action": "expect_call", "call": "Authorize", "direction": "out",
             "match": {"idToken": {"idToken": "BAD"}}},
        ]),
    )
    .await;
    assert!(report.passed, "{report:#?}");
    rig.stop().await;
}

#[tokio::test]
async fn start_tx_fails_when_the_token_is_accepted_but_no_transaction_starts() {
    let rig = Rig::new().await;
    // No cable: the default start point (`PowerPathClosed`) is not reached.
    let report = run(
        &rig,
        json!([{"action": "start_tx", "evse": 1, "id_token": "TAG"}]),
    )
    .await;
    assert_eq!(
        failure(&report, 0),
        "start_tx: the token was accepted, but no transaction started"
    );
    assert_eq!(
        report.steps[0].detail.expected,
        Some(json!({"outcome": "transaction_started"}))
    );
    assert_eq!(report.steps[0].detail.actual.as_deref(), Some("authorized"));
    // `present_token` accepts the same outcome.
    let report = run(
        &rig,
        json!([{"action": "present_token", "evse": 2, "id_token": "TAG2"}]),
    )
    .await;
    assert!(report.passed, "{report:#?}");
    rig.stop().await;
}

#[tokio::test]
async fn an_unknown_evse_is_a_failed_step() {
    let rig = Rig::new().await;
    let report = run(
        &rig,
        json!([
            {"action": "plug", "evse": 9, "expect": {"error": true}},
            {"action": "unplug", "evse": 9},
        ]),
    )
    .await;
    assert_eq!(report.steps[0].status, StepStatus::Passed, "{report:#?}");
    assert!(failure(&report, 1).starts_with("unplug: "), "{report:#?}");
    rig.stop().await;
}

#[tokio::test]
async fn station_side_steps_change_state_and_send_notifications() {
    let rig = Rig::new().await;
    let report = run(
        &rig,
        json!([
            {"action": "fault", "evse": 1},
            {"action": "wait_for", "timeout_ms": 5000,
             "condition": {"kind": "connector_status", "evse": 1, "connector": 1, "status": "Faulted"}},
            {"action": "clear_fault", "evse": 1, "connector": 1},
            {"action": "wait_for", "timeout_ms": 5000,
             "condition": {"kind": "connector_status", "evse": 1, "connector": 1, "status": "Available"}},
            {"action": "status_notification", "evse": 2, "connector": 1},
            {"action": "expect_call", "call": "StatusNotification", "direction": "out", "timeout_ms": 5000,
             "match": {"evseId": 2, "connectorId": 1}},
            {"action": "set_variable", "component": {"name": "TxCtrlr"},
             "variable": {"name": "EVConnectionTimeOut"}, "value": "90"},
            {"action": "security_event", "event_type": "SettingSystemTime", "tech_info": "set by a test"},
            {"action": "data_transfer", "vendor_id": "vendor", "message_id": "m1",
             "data": {"nested": [1, 2]},
             "expect": {"status": "Accepted", "payload": {"data": "pong"}}, "capture": {"reply": "/data"}},
            {"action": "heartbeat"},
            {"action": "send_raw", "call": "Heartbeat", "payload": {}},
        ]),
    )
    .await;
    assert!(report.passed, "{report:#?}");
    assert_eq!(
        rig.calls("SecurityEventNotification")[0]["type"],
        "SettingSystemTime"
    );
    assert_eq!(
        rig.calls("DataTransfer")[0]["data"],
        json!({"nested": [1, 2]})
    );
    assert_eq!(rig.calls("Heartbeat").len(), 2);
    let variable = rig
        .station
        .snapshot()
        .variables
        .into_iter()
        .find(|v| v.component == "TxCtrlr" && v.variable == "EVConnectionTimeOut")
        .expect("the variable is listed");
    assert_eq!(variable.value.as_deref(), Some("90"));
    rig.stop().await;
}

#[tokio::test]
async fn link_steps_drive_the_connection() {
    let rig = Rig::new().await;
    let report = run(
        &rig,
        json!([
            {"action": "disconnect"},
            {"action": "wait_for", "timeout_ms": 5000, "condition": {"kind": "disconnected"}},
            {"action": "connect"},
            {"action": "wait_for", "timeout_ms": 5000, "condition": {"kind": "connected"}},
            {"action": "wait_for", "timeout_ms": 5000,
             "condition": {"kind": "registration", "status": "Accepted"}},
            {"action": "boot", "expect": {"status": "Accepted"}},
            {"action": "reconnect"},
            {"action": "wait_for", "timeout_ms": 5000, "condition": {"kind": "connected"}},
            {"action": "wait_for", "timeout_ms": 5000, "condition": {"kind": "no_pending_messages"}},
        ]),
    )
    .await;
    assert!(report.passed, "{report:#?}");
    rig.stop().await;
}

#[tokio::test]
async fn the_meter_step_advances_and_samples() {
    let rig = Rig::new().await;
    rig.start_transaction(1).await;
    let report = run(
        &rig,
        json!([
            {"action": "meter", "evse": 1, "wh_delta": 2500},
            {"action": "expect_call", "call": "TransactionEvent", "direction": "out", "timeout_ms": 5000,
             "match": {"eventType": "Updated", "triggerReason": "MeterValuePeriodic"}},
        ]),
    )
    .await;
    assert!(report.passed, "{report:#?}");
    let snapshot = rig.station.snapshot();
    let evse = snapshot.evse(1).expect("EVSE 1");
    assert_eq!(evse.transaction.as_ref().map(|t| t.energy_wh), Some(2500));
    rig.stop().await;
}

// ---- wait_for --------------------------------------------------------------------------

#[tokio::test]
async fn wait_for_succeeds_at_once_when_the_condition_holds() {
    let rig = Rig::new().await;
    let report = run(
        &rig,
        json!([
            {"action": "wait_for", "timeout_ms": 5000, "condition": {"kind": "connected"}},
            {"action": "wait_for", "timeout_ms": 5000,
             "condition": {"kind": "registration", "status": "Accepted"}},
            {"action": "wait_for", "timeout_ms": 5000,
             "condition": {"kind": "connector_status", "evse": 2, "connector": 1, "status": "Available"}},
            {"action": "wait_for", "timeout_ms": 5000, "condition": {"kind": "tx_idle", "evse": 2}},
            {"action": "wait_for", "timeout_ms": 5000, "condition": {"kind": "no_pending_messages"}},
        ]),
    )
    .await;
    assert!(report.passed, "{report:#?}");
    rig.stop().await;
}

#[tokio::test]
async fn an_unmet_wait_for_times_out_with_its_condition_as_evidence() {
    let rig = Rig::new().await;
    let conditions = [
        json!({"kind": "disconnected"}),
        json!({"kind": "registration", "status": "Rejected"}),
        json!({"kind": "connector_status", "evse": 1, "connector": 1, "status": "Faulted"}),
        json!({"kind": "connector_status", "evse": 1, "connector": 7, "status": "Available"}),
        json!({"kind": "tx_active", "evse": 1}),
        json!({"kind": "tx_idle", "evse": 9}),
        json!({"kind": "charging_state", "evse": 1, "state": "Charging"}),
    ];
    for condition in conditions {
        let report = run(
            &rig,
            json!([{"action": "wait_for", "timeout_ms": 100, "condition": condition.clone()}]),
        )
        .await;
        assert!(
            failure(&report, 0).starts_with("timed out after 100 ms waiting for "),
            "{condition}: {report:#?}"
        );
        assert_eq!(
            report.steps[0].detail.expected,
            Some(condition.clone()),
            "{condition}"
        );
        assert_eq!(
            report.steps[0].detail.params.as_ref().expect("params")["timeout_ms"],
            100
        );
    }
    rig.stop().await;
}

#[tokio::test]
async fn pending_messages_and_charging_state_conditions_follow_the_station() {
    let rig = Rig::new().await;
    rig.start_transaction(1).await;
    let report = run(
        &rig,
        json!([
            {"action": "charging_state", "evse": 1, "state": "SuspendedEV"},
            {"action": "wait_for", "timeout_ms": 5000,
             "condition": {"kind": "charging_state", "evse": 1, "state": "SuspendedEV"}},
            {"action": "wait_for", "timeout_ms": 5000, "condition": {"kind": "tx_active", "evse": 1}},
            {"action": "disconnect"},
            {"action": "wait_for", "timeout_ms": 5000, "condition": {"kind": "disconnected"}},
            {"action": "stop_tx", "evse": 1, "reason": "Local"},
            {"action": "wait_for", "timeout_ms": 100, "condition": {"kind": "no_pending_messages"}},
        ]),
    )
    .await;
    // Offline, the ended transaction waits in the queue.
    assert!(
        report.steps[..6]
            .iter()
            .all(|s| s.status == StepStatus::Passed),
        "{report:#?}"
    );
    assert!(
        failure(&report, 6).contains("NoPendingMessages"),
        "{report:#?}"
    );
    let report = run(
        &rig,
        json!([
            {"action": "connect"},
            {"action": "wait_for", "timeout_ms": 5000, "condition": {"kind": "no_pending_messages"}},
        ]),
    )
    .await;
    assert!(report.passed, "{report:#?}");
    rig.stop().await;
}

// ---- expect_call -----------------------------------------------------------------------

#[tokio::test]
async fn expect_call_sees_calls_in_both_directions() {
    let rig = Rig::new().await;
    // The subscription exists before the CSMS speaks, so the inbound CALL is not missed.
    let events = rig.station.subscribe();
    let answer = rig
        .call("TriggerMessage", json!({"requestedMessage": "Heartbeat"}))
        .await;
    assert_eq!(answer["status"], "Accepted");
    let scenario = scenario(json!([
        {"action": "expect_call", "call": "TriggerMessage", "direction": "in", "timeout_ms": 5000,
         "match": {"requestedMessage": "Heartbeat"}},
        {"action": "expect_call", "call": "Heartbeat", "direction": "out", "timeout_ms": 5000},
        {"action": "heartbeat"},
        {"action": "expect_call", "call": "Heartbeat", "timeout_ms": 5000},
        {"action": "expect_call", "call": "Heartbeat", "direction": "in", "timeout_ms": 100},
    ]));
    let report = timeout(
        Duration::from_secs(60),
        run_scenario_subscribed(
            &rig.station,
            &scenario,
            &CancelToken::new(),
            None,
            &Vars::new(),
            1,
            events,
        ),
    )
    .await
    .expect("the scenario ends");
    assert_eq!(
        statuses(&report)[..4],
        [&StepStatus::Passed; 4],
        "{report:#?}"
    );
    // No Heartbeat ever came in: the timeout names what was expected.
    assert!(
        failure(&report, 4).contains("timed out after 100 ms waiting for a Heartbeat CALL"),
        "{report:#?}"
    );
    assert_eq!(
        report.steps[4].detail.expected,
        Some(json!({"call": "Heartbeat", "direction": "in"}))
    );
    rig.stop().await;
}

// ---- reply -----------------------------------------------------------------------------

#[tokio::test]
async fn a_reply_step_is_checked_against_the_2_0_1_actions_and_schemas() {
    let rig = Rig::new().await;
    for (step, expected) in [
        (
            // A 1.6 action name.
            json!({"action": "reply", "call": "RemoteStartTransaction", "result": {"status": "Accepted"}}),
            "reply: unknown action `RemoteStartTransaction`",
        ),
        (
            // Sent by the station, never answered by it.
            json!({"action": "reply", "call": "Heartbeat", "result": {}}),
            "reply: unknown action `Heartbeat`",
        ),
        (
            json!({"action": "reply", "call": "Reset", "result": {"status": "Maybe"}}),
            "reply: `result` is not a valid Reset response",
        ),
        (
            json!({"action": "reply", "call": "Reset", "result": {"status": "Accepted"},
                   "error": {"code": "NotSupported"}}),
            "reply: needs exactly one of `result` and `error`",
        ),
        (
            json!({"action": "reply", "call": "Reset", "error": {"code": "Bogus"}}),
            "reply: unknown error code `Bogus`",
        ),
    ] {
        let report = run(&rig, json!([step])).await;
        assert!(
            failure(&report, 0).starts_with(expected),
            "{expected}: {report:#?}"
        );
    }
    rig.stop().await;
}

#[tokio::test]
async fn a_reply_step_answers_the_next_csms_call_and_is_disarmed_at_the_end() {
    let rig = Rig::new().await;
    let unlock = |evse: u32| json!({"evseId": evse, "connectorId": 1});
    let scenario = scenario(json!([
        {"action": "reply", "call": "unlockconnector", "result": {"status": "OngoingAuthorizedTransaction"},
         "must_fire": true},
        // Only the CALL for EVSE 2 releases the scenario, and the CSMS sends it after the
        // scripted answer was used.
        {"action": "expect_call", "call": "UnlockConnector", "direction": "in", "timeout_ms": 10000,
         "match": {"evseId": 2}},
    ]));
    let events = rig.station.subscribe();
    let (cancel, vars) = (CancelToken::new(), Vars::new());
    let runner = run_scenario_subscribed(&rig.station, &scenario, &cancel, None, &vars, 1, events);
    let csms = async {
        // The reply is armed by the first step; ask for EVSE 1 until it answers.
        loop {
            let answer = rig.call("UnlockConnector", unlock(1)).await;
            if answer["status"] == "OngoingAuthorizedTransaction" {
                break;
            }
        }
        rig.call("UnlockConnector", unlock(2)).await
    };
    let (report, second) = timeout(Duration::from_secs(60), async {
        tokio::join!(runner, csms)
    })
    .await
    .expect("both end");
    assert!(report.passed, "{report:#?}");
    // The scripted answer was used once; the next CALL gets the station's own answer.
    assert_ne!(second["status"], "OngoingAuthorizedTransaction");
    assert_ne!(
        rig.call("UnlockConnector", unlock(1)).await["status"],
        "OngoingAuthorizedTransaction"
    );
    rig.stop().await;
}

#[tokio::test]
async fn a_must_fire_reply_that_was_not_used_fails_the_run() {
    let rig = Rig::new().await;
    let report = run(
        &rig,
        json!([
            {"action": "reply", "call": "UnlockConnector", "result": {"status": "Unlocked"},
             "must_fire": true},
            {"action": "heartbeat"},
        ]),
    )
    .await;
    assert!(!report.passed, "{report:#?}");
    assert!(
        failure(&report, 0).contains("the scripted UnlockConnector reply was used 0 of 1 time(s)"),
        "{report:#?}"
    );
    rig.stop().await;
}

// ---- steps of the other protocol -------------------------------------------------------

#[tokio::test]
async fn steps_of_the_other_protocol_fail_with_a_clear_reason() {
    let rig = Rig::new().await;
    let one_six = |raw: Value| Step {
        name: None,
        expect: None,
        capture: BTreeMap::new(),
        action: Action::deserialize(raw).expect("a 1.6 step"),
    };
    for (raw, expected) in [
        (
            json!({"action": "plug", "connector": 1}),
            "`plug` is an OCPP 1.6 step; this station speaks OCPP 2.0.1",
        ),
        (
            json!({"action": "set_rule", "rule": "Reset=reject"}),
            "`set_rule` is not available for OCPP 2.0.1",
        ),
        (
            json!({"action": "request_certificate"}),
            "`request_certificate` is not available for OCPP 2.0.1",
        ),
    ] {
        let mut s = scenario(json!([]));
        s.steps.push(one_six(raw));
        let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
        assert_eq!(failure(&report, 0), expected, "{report:#?}");
    }
    rig.stop().await;
}

// ---- evidence and redaction ------------------------------------------------------------

const SENTINEL: &str = "sentinel-made-up-password-7c2e";

fn secret_payload() -> Value {
    json!({"setVariableData": [{
        "attributeValue": SENTINEL,
        "component": {"name": "SecurityCtrlr"},
        "variable": {"name": "BasicAuthPassword"},
    }]})
}

#[tokio::test]
async fn failure_evidence_hides_secrets_in_reports_and_junit() {
    let rig = Rig::new().await;
    let mut all = Vec::new();
    // A `set_variable` step that fails: its value is a secret by the names.
    all.push(
        run(
            &rig,
            json!([{"action": "set_variable", "component": {"name": "SecurityCtrlr"},
                    "variable": {"name": "BasicAuthPassword"}, "value": SENTINEL,
                    "expect": {"status": "Never"}}]),
        )
        .await,
    );
    // An expected payload that does not match, naming the secret.
    all.push(
        run(
            &rig,
            json!([{"action": "send_raw", "call": "DataTransfer",
                    "payload": {"vendorId": "v", "data": secret_payload()},
                    "expect": {"payload": secret_payload()}}]),
        )
        .await,
    );
    // An `expect_call` that times out next to a frame that carries the secret.
    all.push(
        run(
            &rig,
            json!([
                {"action": "data_transfer", "vendor_id": "v2", "data": secret_payload()},
                {"action": "expect_call", "call": "DataTransfer", "direction": "out", "timeout_ms": 200,
                 "match": {"vendorId": "never", "data": secret_payload()}},
            ]),
        )
        .await,
    );
    // A failed `capture`.
    all.push(
        run(
            &rig,
            json!([{"action": "send_raw", "call": "DataTransfer",
                    "payload": {"vendorId": "v", "data": secret_payload()},
                    "capture": {"x": "/missing"}}]),
        )
        .await,
    );
    for (index, report) in all.iter().enumerate() {
        assert!(!report.passed, "case {index}: {report:#?}");
        let text = serde_json::to_string(report).expect("serializes");
        assert!(!text.contains(SENTINEL), "case {index} report: {text}");
        let junit = junit_xml(&[JunitSuite {
            station: None,
            scenario: "t",
            outcome: Ok(report),
        }]);
        assert!(!junit.contains(SENTINEL), "case {index} junit: {junit}");
    }
    let first = all[0].steps[0].detail.params.as_ref().expect("params");
    assert_eq!(first["value"], "***");
    assert_eq!(first["component"]["name"], "SecurityCtrlr");
    let second = &all[1].steps[0].detail;
    assert!(second.expected.is_some() && second.actual.is_some());
    let third = &all[2].steps[1].detail;
    assert!(third.actual.as_deref().is_some_and(|a| a.contains("***")));
    rig.stop().await;
}

// ---- report and JUnit shapes -----------------------------------------------------------

#[tokio::test]
async fn report_and_junit_shapes_are_pinned() {
    let rig = Rig::new().await;
    let mut passing = scenario(json!([
        {"action": "heartbeat"},
        {"action": "plug", "evse": 1},
    ]));
    passing.source = Some("builtin:shape".to_owned());
    let report = run_scenario(&rig.station, &passing, &CancelToken::new(), None).await;
    assert!(report.passed, "{report:#?}");
    let value = serde_json::to_value(&report).expect("serializes");
    assert_eq!(
        sorted_keys(&value),
        [
            "cancelled",
            "duration_ms",
            "ocpp_version",
            "passed",
            "scenario",
            "schema",
            "source",
            "started_at",
            "station",
            "status",
            "steps",
            "tool_version",
        ]
    );
    assert_eq!(value["ocpp_version"], "2.0.1");
    assert_eq!(value["status"], "passed");
    assert_eq!(value["schema"], 1);
    assert_eq!(value["station"], rig.station.snapshot().charge_point_id);
    assert_eq!(value["source"], "builtin:shape");
    assert_eq!(
        sorted_keys(&value["steps"][1]),
        ["action", "duration_ms", "index", "label", "name", "status"]
    );
    assert_eq!(value["steps"][1]["action"], "plug");
    assert_eq!(value["steps"][1]["label"], "plug e1");

    let junit = junit_xml(&[JunitSuite {
        station: None,
        scenario: "t",
        outcome: Ok(&report),
    }]);
    assert!(
        junit.contains("<testsuites tests=\"2\" failures=\"0\" errors=\"0\" skipped=\"0\""),
        "{junit}"
    );
    assert!(
        junit.contains("<testcase classname=\"ocpp2.0.1.t\" name=\"heartbeat\""),
        "{junit}"
    );
    assert!(
        junit.contains("<testcase classname=\"ocpp2.0.1.t\" name=\"plug e1\""),
        "{junit}"
    );
    assert!(
        junit.contains("<property name=\"ocpp_version\" value=\"2.0.1\"/>"),
        "{junit}"
    );
    assert!(
        junit.contains("<property name=\"source\" value=\"builtin:shape\"/>"),
        "{junit}"
    );

    rig.stop().await;
}

#[tokio::test]
async fn failed_report_and_junit_shapes_are_pinned() {
    let rig = Rig::new().await;
    // The failing step carries its evidence, the rest is skipped.
    let failing = scenario(json!([
        {"action": "heartbeat"},
        {"action": "wait_for", "timeout_ms": 100, "condition": {"kind": "tx_active", "evse": 1}},
        {"action": "unplug", "evse": 1},
    ]));
    let report = run_scenario(&rig.station, &failing, &CancelToken::new(), None).await;
    let value = serde_json::to_value(&report).expect("serializes");
    assert_eq!(value["status"], "failed");
    assert_eq!(value["passed"], false);
    assert_eq!(
        sorted_keys(&value["steps"][1]),
        [
            "action",
            "duration_ms",
            "expected",
            "index",
            "label",
            "name",
            "params",
            "reason",
            "status"
        ]
    );
    assert_eq!(value["steps"][1]["status"], "failed");
    assert_eq!(
        sorted_keys(&value["steps"][2]),
        ["action", "duration_ms", "index", "label", "name", "status"]
    );
    assert_eq!(value["steps"][2]["status"], "skipped");
    assert_eq!(
        value["steps"][1]["expected"],
        json!({"kind": "tx_active", "evse": 1})
    );

    let junit = junit_xml(&[JunitSuite {
        station: Some("CP-1"),
        scenario: "t",
        outcome: Ok(&report),
    }]);
    assert!(
        junit.contains("<testsuites tests=\"3\" failures=\"1\" errors=\"0\" skipped=\"1\""),
        "{junit}"
    );
    assert!(
        junit.contains("<testcase classname=\"ocpp2.0.1.t [CP-1]\" name=\"wait_for tx_active e1\""),
        "{junit}"
    );
    assert!(junit.contains("<failure message="), "{junit}");
    assert!(junit.contains("<skipped"), "{junit}");
    rig.stop().await;
}

#[tokio::test]
async fn a_2_0_1_step_on_a_1_6_station_fails_and_the_report_says_1_6() {
    use crate::station_support::{Rig as Rig16, incrementing};
    let mut rig = Rig16::with(incrementing(), |c| c).await;
    rig.connect_booted().await;
    let s = scenario(json!([{"action": "plug", "evse": 1}]));
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    assert_eq!(
        failure(&report, 0),
        "`plug` is an OCPP 2.0.1 step; this station speaks OCPP 1.6"
    );
    assert_eq!(report.ocpp_version, "1.6");
    rig.shutdown().await;
}
