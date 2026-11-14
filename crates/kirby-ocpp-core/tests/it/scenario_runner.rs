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
async fn each_new_builtin_passes_alone_on_a_fresh_station() {
    for name in [
        "heartbeat-and-status",
        "metering-burst",
        "data-transfer",
        "security-event",
        "reconnect",
    ] {
        let rig = booted_rig().await;
        let scenario = builtin::get(name).expect("built-in");
        let report = run_scenario(&rig.station, &scenario, &CancelToken::new(), None).await;
        assert!(report.passed, "{name}: {report:#?}");
        assert_eq!(report.steps.len(), scenario.steps.len(), "{name}");
        rig.shutdown().await;
    }
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
async fn expect_call_in_sees_a_call_that_arrived_before_the_runner_started() {
    // The subscription is taken before connecting (as `connect_and_run` does); the CSMS
    // calls right after the boot, while nothing runs the scenario yet.
    let mut rig = Rig::with(incrementing(), |c| c).await;
    let events = rig.station.subscribe();
    rig.connect_booted().await;
    let reply = rig
        .csms
        .send_call("Reset", serde_json::json!({"type": "Soft"}))
        .await
        .unwrap();
    drop(reply);
    let s = scenario(
        r#"
[[steps]]
action = "expect_call"
call = "Reset"
direction = "in"
match = { type = "Soft" }
timeout_ms = 3000
"#,
    );
    let report = kirby_ocpp_core::scenario::run_scenario_subscribed(
        &rig.station,
        &s,
        &CancelToken::new(),
        None,
        &kirby_ocpp_core::scenario::Vars::default(),
        1,
        events,
    )
    .await;
    assert!(report.passed, "{report:#?}");
    rig.shutdown().await;
}

#[tokio::test]
async fn a_raw_boot_notification_still_logs_its_result() {
    use crate::station_support::Observed;
    let rig = booted_rig().await;
    let mut seen = Observed::new(rig.station.subscribe());
    let boot = scenario(
        r#"
[[steps]]
action = "send_raw"
call = "BootNotification"
payload = { chargePointVendor = "v", chargePointModel = "m" }
"#,
    );
    for status in ["Accepted", "Rejected"] {
        rig.csms.respond("BootNotification", move |_| {
            kirby_ocpp_testkit::Reply::Result(serde_json::json!({
                "status": status,
                "currentTime": "2026-01-01T00:00:00Z",
                "interval": 300
            }))
        });
        let report = run_scenario(&rig.station, &boot, &CancelToken::new(), None).await;
        assert!(report.passed, "{status}: {report:#?}");
        seen.wait_log("BootNotification accepted").await;
    }
    rig.shutdown().await;
}

#[tokio::test]
async fn a_cancel_after_the_last_step_does_not_mark_the_report_cancelled() {
    let rig = booted_rig().await;
    // Nothing is left to cut short, so a fired token changes nothing: passed, not cancelled.
    let done = Scenario {
        name: "t".to_owned(),
        description: String::new(),
        vars: kirby_ocpp_core::scenario::Vars::default(),
        steps: Vec::new(),
        tags: Vec::new(),
        source: None,
    };
    let cancel = CancelToken::new();
    cancel.cancel();
    let report = run_scenario(&rig.station, &done, &cancel, None).await;
    assert!(report.passed && !report.cancelled, "{report:?}");

    // A step that never ran because of the token: cancelled, not passed.
    let s = scenario("[[steps]]\naction = \"heartbeat\"\n");
    let report = run_scenario(&rig.station, &s, &cancel, None).await;
    assert!(report.cancelled && !report.passed);
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
async fn events_are_drained_while_a_step_waits() {
    use kirby_ocpp_core::scenario::{ScenarioProgress, run_scenario_with_events};
    use kirby_ocpp_core::station::{Direction, Event, FrameRecord};
    use tokio::sync::{broadcast, mpsc};

    let rig = booted_rig().await;
    // Room for 2 events: a frame followed by a burst of noise overflows it unless the runner
    // reads the frame while the condition step is still waiting.
    let (tx, rx) = broadcast::channel(2);
    let s = scenario(
        r#"
[[steps]]
action = "wait_for"
condition = { kind = "connector_status", connector = 1, status = "Preparing" }
timeout_ms = 10000
[[steps]]
action = "expect_call"
call = "Heartbeat"
timeout_ms = 3000
"#,
    );
    let (progress, mut started) = mpsc::unbounded_channel();
    let cancel = CancelToken::new();
    let (report, ()) = tokio::join!(
        run_scenario_with_events(&rig.station, &s, &cancel, Some(&progress), rx),
        async {
            tokio::time::timeout(crate::station_support::STEP, async {
                loop {
                    match started.recv().await {
                        Some(ScenarioProgress::StepStarted { index: 0, .. }) => break,
                        Some(_) => {}
                        None => panic!("scenario progress channel closed"),
                    }
                }
            })
            .await
            .expect("the condition step started");
            tx.send(Event::Frame(FrameRecord {
                ts: chrono::Utc::now(),
                direction: Direction::Out,
                msg_id: Some("hb".to_owned()),
                action: Some("Heartbeat".to_owned()),
                frame: serde_json::json!([2, "hb", "Heartbeat", {}]),
            }))
            .unwrap();
            // The runner empties the queue before the condition can become true.
            let drained = tokio::time::timeout(crate::station_support::STEP, async {
                while !tx.is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await;
            for _ in 0..5 {
                tx.send(Event::StateChanged).unwrap();
            }
            assert!(
                drained.is_ok(),
                "the queue was not drained while waiting for the condition"
            );
            tokio::time::timeout(
                crate::station_support::STEP,
                rig.station
                    .plug_in(kirby_ocpp_core::ids::ConnectorId::new(1)),
            )
            .await
            .expect("plug command completed")
            .unwrap();
        }
    );
    assert!(report.passed, "{report:#?}");
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

#[tokio::test]
async fn variables_substitute_and_capture_across_steps() {
    use kirby_ocpp_core::scenario::{Vars, run_scenario_with_vars};

    let rig = booted_rig().await;
    let id = rig.station.snapshot().charge_point_id;
    let s = scenario(
        r#"
[vars]
n = 3
who = "me"
[[steps]]
action = "send_raw"
call = "Heartbeat"
capture = { ct = "/currentTime" }
[[steps]]
action = "data_transfer"
vendor_id = "v-${station_id}-${index}-${who}"
data = "${ct}"
[[steps]]
action = "expect_call"
call = "DataTransfer"
direction = "out"
match = { vendorId = "v-${station_id}-7-you", data = "${ct}" }
capture = { seen = "/data" }
[[steps]]
action = "send_raw"
call = "StatusNotification"
payload = { connectorId = "${n}", status = "Available", errorCode = "NoError", info = "${seen}" }
"#,
    );
    let mut overrides = Vars::new();
    overrides.insert("who", "you".into()).unwrap();
    let report =
        run_scenario_with_vars(&rig.station, &s, &CancelToken::new(), None, &overrides, 7).await;
    assert!(report.passed, "{report:#?}");
    let sent = rig.calls("StatusNotification", 0);
    let last = sent.last().expect("StatusNotification sent");
    // The exact `${n}` kept its number type, the captured text went into `info`.
    assert_eq!(last["connectorId"], 3, "{last}");
    assert!(
        last["info"].as_str().is_some_and(|t| t.contains('T')),
        "{last}"
    );
    let transfers = rig.calls("DataTransfer", 0);
    assert_eq!(transfers[0]["vendorId"], format!("v-{id}-7-you"));
    rig.shutdown().await;
}

#[tokio::test]
async fn substitution_and_capture_failures_fail_the_step() {
    let rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "data_transfer"
vendor_id = "x"
data = "${nope}"
"#,
    );
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    match &report.steps[0].status {
        StepStatus::Failed(reason) => assert!(reason.contains("nope"), "{reason}"),
        other => panic!("{other:?}"),
    }
    rig.assert_no_calls("DataTransfer", 0);

    let s = scenario(
        r#"
[[steps]]
action = "send_raw"
call = "Heartbeat"
capture = { x = "/missing" }
[[steps]]
action = "heartbeat"
"#,
    );
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    match &report.steps[0].status {
        StepStatus::Failed(reason) => {
            assert!(
                reason.contains("capture `x`") && reason.contains("/missing"),
                "{reason}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(report.steps[1].status, StepStatus::Skipped);
    rig.shutdown().await;
}

#[tokio::test]
async fn repeat_runs_each_iteration_with_counters_and_flat_names() {
    let rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "heartbeat"
[[steps]]
action = "repeat"
count = 2
[[steps.steps]]
action = "data_transfer"
vendor_id = "v${i}"
data = "outer-${i0}"
[[steps.steps]]
action = "repeat"
count = 2
var = "j"
[[steps.steps.steps]]
action = "data_transfer"
name = "inner"
vendor_id = "v"
data = "${i}.${j}.${j0}"
"#,
    );
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    assert!(report.passed, "{report:#?}");
    let names: Vec<_> = report.steps.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "heartbeat",
            "data_transfer [1]",
            "inner [1.1]",
            "inner [1.2]",
            "data_transfer [2]",
            "inner [2.1]",
            "inner [2.2]",
        ]
    );
    assert_eq!(report.steps.len() as u64, s.expanded_len());
    let data: Vec<_> = rig
        .calls("DataTransfer", 0)
        .iter()
        .map(|c| c["data"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        data,
        ["outer-0", "1.1.0", "1.2.1", "outer-1", "2.1.0", "2.2.1"]
    );
    // JUnit stays a flat list with unique names.
    let xml = kirby_ocpp_core::scenario::junit_xml(&[kirby_ocpp_core::scenario::JunitSuite {
        station: None,
        scenario: &s.name,
        outcome: Ok(&report),
    }]);
    assert_eq!(xml.matches("<testcase ").count(), 7, "{xml}");
    assert!(xml.contains("name=\"inner [2.2]\""), "{xml}");
    rig.shutdown().await;
}

#[tokio::test]
async fn a_failing_iteration_stops_the_loop_and_skips_the_rest() {
    let rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "repeat"
count = 4
[[steps.steps]]
action = "data_transfer"
vendor_id = "v"
data = "n${i0}"
[[steps.steps]]
action = "expect_call"
call = "DataTransfer"
direction = "out"
match = { data = "n0" }
timeout_ms = 200
[[steps]]
action = "heartbeat"
"#,
    );
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    assert!(!report.passed && !report.cancelled, "{report:#?}");
    assert_eq!(report.steps.len(), 9);
    let st = statuses(&report);
    assert_eq!(st[0], &StepStatus::Passed);
    assert_eq!(st[1], &StepStatus::Passed);
    assert_eq!(st[2], &StepStatus::Passed);
    assert!(matches!(st[3], StepStatus::Failed(_)), "{st:?}");
    assert!(st[4..].iter().all(|s| **s == StepStatus::Skipped), "{st:?}");
    assert_eq!(report.steps[3].name, "expect_call [2]");
    assert_eq!(report.steps[8].name, "heartbeat");
    rig.shutdown().await;
}

#[tokio::test]
async fn cancel_stops_a_loop() {
    let rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "repeat"
count = 1000
[[steps.steps]]
action = "sleep"
ms = 20
"#,
    );
    let cancel = CancelToken::new();
    let canceller = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        canceller.cancel();
    });
    let report = run_scenario(&rig.station, &s, &cancel, None).await;
    assert!(report.cancelled && !report.passed);
    assert_eq!(report.steps.len(), 1000);
    assert!(
        report
            .steps
            .iter()
            .any(|s| matches!(&s.status, StepStatus::Failed(r) if r == "cancelled"))
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn reports_carry_metadata_and_failure_evidence() {
    let rig = booted_rig().await;
    let mut s = scenario(
        r#"
[[steps]]
action = "heartbeat"
[[steps]]
action = "heartbeat"
expect = { payload = { currentTime = "x" } }
[[steps]]
action = "send_raw"
call = "ChangeConfiguration"
payload = { key = "AuthorizationKey", value = "hunter2hunter2" }
expect = { status = "Never" }
"#,
    );
    s.source = Some("builtin:t".to_owned());
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    assert_eq!(report.schema, 1);
    assert_eq!(report.ocpp_version, "1.6");
    assert_eq!(report.status.as_str(), "failed");
    assert_eq!(report.source.as_deref(), Some("builtin:t"));
    assert_eq!(
        report.station.as_deref(),
        Some(rig.station.snapshot().charge_point_id.as_str())
    );
    assert!(report.started_at.ends_with('Z') && report.started_at.contains('T'));

    let ok = &report.steps[0];
    assert_eq!(ok.detail.action, "heartbeat");
    assert_eq!(ok.detail.label, "heartbeat");
    assert!(ok.detail.params.is_none() && ok.detail.expected.is_none());

    let bad = &report.steps[1];
    assert!(matches!(bad.status, StepStatus::Failed(_)), "{report:#?}");
    assert_eq!(
        bad.detail.expected,
        Some(serde_json::json!({"currentTime": "x"}))
    );
    assert!(
        bad.detail
            .actual
            .as_deref()
            .is_some_and(|a| a.contains("currentTime"))
    );
    let params = bad.detail.params.as_ref().expect("params");
    assert_eq!(params["expect"]["payload"]["currentTime"], "x");
    assert!(params.get("action").is_none());

    // Step 3 is skipped (the run stops at the failure); run it alone for the redaction check.
    let s = scenario(
        r#"
[[steps]]
action = "send_raw"
call = "ChangeConfiguration"
payload = { key = "AuthorizationKey", value = "hunter2hunter2" }
expect = { status = "Never" }
"#,
    );
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    let step = &report.steps[0];
    assert!(matches!(step.status, StepStatus::Failed(_)), "{report:#?}");
    let text = serde_json::to_string(&report).unwrap();
    assert!(!text.contains("hunter2hunter2"), "{text}");
    assert_eq!(
        step.detail.params.as_ref().unwrap()["payload"]["value"],
        "***"
    );
    assert_eq!(step.detail.label, "send_raw ChangeConfiguration");
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["steps"][0]["action"], "send_raw");
    assert_eq!(json["steps"][0]["status"], "failed");
    rig.shutdown().await;
}

#[tokio::test]
async fn oversized_step_params_are_bounded_in_evidence() {
    let rig = booted_rig().await;
    let run = |payload: String| {
        let rig = &rig;
        async move {
            let s = scenario(&format!(
                r#"
[[steps]]
action = "send_raw"
call = "ChangeConfiguration"
payload = {payload}
expect = {{ status = "Never" }}
"#
            ));
            let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
            serde_json::to_value(&report).unwrap()["steps"][0].clone()
        }
    };
    let keys = |step: &serde_json::Value| -> Vec<String> {
        step.as_object().unwrap().keys().cloned().collect()
    };
    let small = run(r#"{ key = "k", value = "v" }"#.to_owned()).await;
    let long = "x".repeat(200_000);
    let clipped = run(format!(r#"{{ key = "k", value = "{long}" }}"#)).await;
    assert_eq!(keys(&small), keys(&clipped));
    let text = clipped["params"].to_string();
    assert!(text.len() < 4096, "{}", text.len());
    assert!(clipped["params"]["payload"]["value"].is_string(), "{text}");

    let items = vec!["\"y\""; 20_000].join(", ");
    let marker = run(format!(r#"{{ key = "k", value = [{items}] }}"#)).await;
    assert_eq!(keys(&small), keys(&marker));
    assert_eq!(marker["params"]["truncated"], true, "{marker}");
    assert!(marker["params"].to_string().len() < 256);
    rig.shutdown().await;
}

#[tokio::test]
async fn expected_payload_evidence_is_redacted() {
    let rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "send_raw"
call = "ChangeConfiguration"
payload = { key = "AuthorizationKey", value = "hunter2hunter2" }
expect = { payload = { key = "AuthorizationKey", value = "hunter2hunter2" } }
"#,
    );
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    let step = &report.steps[0];
    assert!(matches!(step.status, StepStatus::Failed(_)), "{report:#?}");
    let text = serde_json::to_string(&report).unwrap();
    assert!(!text.contains("hunter2hunter2"), "{text}");
    let junit = kirby_ocpp_core::scenario::junit_xml(&[kirby_ocpp_core::scenario::JunitSuite {
        station: None,
        scenario: "t",
        outcome: Ok(&report),
    }]);
    assert!(!junit.contains("hunter2hunter2"), "{junit}");
    rig.shutdown().await;
}
