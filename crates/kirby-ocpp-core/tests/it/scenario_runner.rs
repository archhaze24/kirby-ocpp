//! Scenario runner against the mock CSMS.

use crate::station_support::{Rig, incrementing};
use kirby_ocpp_core::scenario::{
    CancelToken, Scenario, ScenarioError, ScenarioRunner, StepStatus, builtin, run_scenario,
};

fn scenario(steps: &str) -> Scenario {
    Scenario::from_toml(&format!("name = \"t\"\n{steps}")).expect("valid scenario")
}

async fn booted_rig() -> Rig {
    let mut rig = Rig::with(incrementing(), |c| c).await;
    rig.connect_booted().await;
    rig
}

fn statuses(report: &kirby_ocpp_core::scenario::ScenarioReport) -> Vec<&StepStatus> {
    report.steps.iter().map(|s| &s.status).collect()
}

#[tokio::test]
async fn all_builtins_pass_in_sequence() {
    let rig = booted_rig().await;
    // Stock station: no offline-authorization keys overridden (offline-sync relies on the defaults).
    let cancel = CancelToken::new();
    for scenario in builtin::all() {
        let report = run_scenario(&rig.station, &scenario, &cancel, None).await;
        assert!(report.passed, "{}: {report:#?}", scenario.name);
        assert_eq!(report.steps.len(), scenario.steps.len());
    }
    rig.shutdown().await;
}

#[tokio::test]
async fn rejected_authorize_stops_the_scenario() {
    let rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "authorize"
id_tag = "BADTAG"
[[steps]]
action = "start_tx"
connector = 1
id_tag = "BADTAG"
"#,
    );
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    assert!(!report.passed);
    match &report.steps[0].status {
        StepStatus::Failed(reason) => assert!(reason.contains("Blocked"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(report.steps[1].status, StepStatus::Skipped);
    rig.assert_no_calls("StartTransaction", 0);
    rig.shutdown().await;
}

#[tokio::test]
async fn expect_status_can_assert_a_rejection() {
    let rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "authorize"
id_tag = "BADTAG"
expect = { status = "Blocked" }
[[steps]]
action = "heartbeat"
expect = { payload = { currentTime = "x" } }
"#,
    );
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    assert_eq!(report.steps[0].status, StepStatus::Passed, "{report:#?}");
    assert!(matches!(report.steps[1].status, StepStatus::Failed(_)));
    rig.shutdown().await;
}

#[tokio::test]
async fn failing_command_fails_the_step_unless_error_is_expected() {
    let rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "unplug"
connector = 1
expect = { error = true }
[[steps]]
action = "unplug"
connector = 1
[[steps]]
action = "connect"
"#,
    );
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    assert_eq!(report.steps[0].status, StepStatus::Passed, "{report:#?}");
    assert!(matches!(report.steps[1].status, StepStatus::Failed(_)));
    assert_eq!(report.steps[2].status, StepStatus::Skipped);
    rig.shutdown().await;
}

#[tokio::test]
async fn wait_for_and_expect_call_time_out() {
    let rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "wait_for"
condition = { kind = "connector_status", connector = 1, status = "Charging" }
timeout_ms = 100
"#,
    );
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    assert!(matches!(&report.steps[0].status, StepStatus::Failed(r) if r.contains("timed out")));

    let s = scenario(
        r#"
[[steps]]
action = "expect_call"
call = "DiagnosticsStatusNotification"
timeout_ms = 100
"#,
    );
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    assert!(matches!(&report.steps[0].status, StepStatus::Failed(r) if r.contains("timed out")));
    rig.shutdown().await;
}

#[tokio::test]
async fn expect_call_sees_csms_initiated_calls_and_matches_payloads() {
    let rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "heartbeat"
[[steps]]
action = "expect_call"
call = "Heartbeat"
direction = "out"
[[steps]]
action = "expect_call"
call = "GetConfiguration"
direction = "in"
match = { key = ["HeartbeatInterval"] }
timeout_ms = 4000
"#,
    );
    let csms = &rig.csms;
    let cancel = CancelToken::new();
    // `join!` polls the runner first, which subscribes to events before any step runs.
    let (report, ()) = tokio::join!(run_scenario(&rig.station, &s, &cancel, None), async {
        // The inbound CALL must come after the station's own Heartbeat (steps are ordered).
        csms.wait_for_call("Heartbeat", crate::station_support::STEP)
            .await
            .unwrap();
        let _ = csms
            .send_call(
                "GetConfiguration",
                serde_json::json!({"key": ["HeartbeatInterval"]}),
            )
            .await;
    });
    assert!(report.passed, "{report:#?}");
    rig.shutdown().await;
}

#[tokio::test]
async fn cancel_stops_a_sleeping_scenario() {
    let rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "sleep"
ms = 60000
[[steps]]
action = "heartbeat"
"#,
    );
    let cancel = CancelToken::new();
    let canceller = cancel.clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (report, ()) = tokio::join!(
        run_scenario(&rig.station, &s, &cancel, Some(&tx)),
        async move {
            // Cancel as soon as the sleep step has started.
            rx.recv().await.expect("progress");
            canceller.cancel();
        }
    );
    assert!(report.cancelled && !report.passed);
    assert!(matches!(&report.steps[0].status, StepStatus::Failed(r) if r == "cancelled"));
    assert_eq!(report.steps[1].status, StepStatus::Skipped);
    assert_eq!(statuses(&report).len(), 2);
    rig.shutdown().await;
}

#[tokio::test]
async fn runner_refuses_two_scenarios_at_once() {
    let rig = booted_rig().await;
    let runner = ScenarioRunner::new(rig.station.clone());
    let slow = scenario("[[steps]]\naction = \"sleep\"\nms = 60000\n");
    let cancel = CancelToken::new();
    let (first, ()) = tokio::join!(runner.run(&slow, &cancel, None), async {
        while !runner.is_running() {
            tokio::task::yield_now().await;
        }
        let second = runner.run(&slow, &CancelToken::new(), None).await;
        assert!(matches!(second, Err(ScenarioError::AlreadyRunning)));
        cancel.cancel();
    });
    assert!(first.expect("first run").cancelled);
    assert!(!runner.is_running());
    rig.shutdown().await;
}

#[tokio::test]
async fn expect_call_fails_loudly_when_the_event_subscription_lagged() {
    use kirby_ocpp_core::station::Event;
    use tokio::sync::broadcast;

    let rig = booted_rig().await;
    // A subscription with room for 2 events that already lost 8: frames may be among them.
    let (tx, rx) = broadcast::channel(2);
    for _ in 0..10 {
        tx.send(Event::StateChanged).unwrap();
    }
    let s = scenario(
        r#"
[[steps]]
action = "expect_call"
call = "Heartbeat"
timeout_ms = 60000
"#,
    );
    let report = kirby_ocpp_core::scenario::run_scenario_with_events(
        &rig.station,
        &s,
        &CancelToken::new(),
        None,
        rx,
    )
    .await;
    assert!(!report.passed);
    match &report.steps[0].status {
        StepStatus::Failed(reason) => {
            assert!(reason.contains("fell behind"), "{reason}");
            assert!(reason.contains("dropped"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    drop(tx);
    rig.shutdown().await;
}

#[tokio::test]
async fn set_rule_and_clear_rules_steps_drive_response_rules() {
    let rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "set_rule"
rule = "ClearCache=reject*1"
[[steps]]
action = "set_rule"
rule = "Reset=drop"
[[steps]]
action = "clear_rules"
call = "Reset"
"#,
    );
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    assert!(report.passed, "{report:#?}");
    let rules: Vec<String> = rig
        .station
        .snapshot()
        .response_rules
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(rules, ["ClearCache=reject*1"]);
    rig.csms
        .expect_status("ClearCache", serde_json::json!({}), "Rejected")
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn security_steps_send_a_certificate_request_and_an_event() {
    let rig = booted_rig().await;
    rig.csms.respond("SignCertificate", |_| {
        kirby_ocpp_testkit::Reply::Result(serde_json::json!({"status": "Accepted"}))
    });
    rig.csms.respond("SecurityEventNotification", |_| {
        kirby_ocpp_testkit::Reply::Result(serde_json::json!({}))
    });
    let s = scenario(
        r#"
[[steps]]
action = "request_certificate"
[[steps]]
action = "security_event"
event_type = "TamperDetectionActivated"
tech_info = "lid opened"
"#,
    );
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    assert!(report.passed, "{report:#?}");
    let csr = rig
        .csms
        .wait_for_call("SignCertificate", crate::station_support::STEP)
        .await
        .unwrap();
    assert!(
        csr["csr"]
            .as_str()
            .is_some_and(|c| c.contains("CERTIFICATE REQUEST"))
    );
    rig.csms
        .wait_for_call_after(
            0,
            "SecurityEventNotification",
            |p| p["type"] == "TamperDetectionActivated" && p["techInfo"] == "lid opened",
            crate::station_support::STEP,
        )
        .await
        .unwrap();
    rig.shutdown().await;
}
