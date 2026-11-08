//! The `reply` scenario step: scripted answers to CSMS commands.

use std::time::{Duration, Instant};

use kirby_ocpp_core::ocpp::frame::Frame;
use kirby_ocpp_core::scenario::{CancelToken, Scenario, ScenarioReport, StepStatus, run_scenario};
use kirby_ocpp_testkit::MockCsms;
use serde_json::{Value, json};

use crate::station_support::{Rig, incrementing};

fn scenario(steps: &str) -> Scenario {
    Scenario::from_toml(&format!("name = \"t\"\n{steps}")).expect("valid scenario")
}

async fn booted_rig() -> Rig {
    let mut rig = Rig::with(incrementing(), |c| c).await;
    rig.connect_booted().await;
    rig
}

/// Runs `s` while `drive` talks to the station as the CSMS once the first reply is armed.
async fn run_with(
    rig: &mut Rig,
    s: &Scenario,
    armed: &str,
    drive: impl AsyncFnOnce(&MockCsms),
) -> ScenarioReport {
    let cancel = CancelToken::new();
    let station = rig.station.clone();
    let csms = &rig.csms;
    let observed = &mut rig.observed;
    let (report, ()) = tokio::join!(run_scenario(&station, s, &cancel, None), async {
        observed.wait_log(armed).await;
        drive(csms).await;
    });
    report
}

fn result_of(frame: &Frame) -> Value {
    match frame {
        Frame::CallResult { payload, .. } => payload.clone(),
        other => panic!("expected a CALLRESULT, got {other:?}"),
    }
}

const UNLOCK: &str = r#"{"connectorId":1}"#;

fn unlock() -> Value {
    serde_json::from_str(UNLOCK).unwrap()
}

#[tokio::test]
async fn a_result_reply_answers_the_next_call_only_and_expect_call_sees_it() {
    let mut rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "reply"
call = "UnlockConnector"
result = { status = "UnlockFailed" }
[[steps]]
action = "expect_call"
call = "UnlockConnector"
direction = "in"
match = { connectorId = 1 }
timeout_ms = 5000
"#,
    );
    let report = run_with(&mut rig, &s, "scripted reply armed", async |csms| {
        let first = csms.send_call("UnlockConnector", unlock()).await.unwrap();
        assert_eq!(result_of(&first), json!({"status": "UnlockFailed"}));
        // The reply is spent: the station's own handler answers the next CALL.
        let second = csms.send_call("UnlockConnector", unlock()).await.unwrap();
        assert_eq!(result_of(&second), json!({"status": "Unlocked"}));
    })
    .await;
    assert!(report.passed, "{report:#?}");
    rig.shutdown().await;
}

#[tokio::test]
async fn an_error_reply_sends_the_callerror_with_its_details() {
    let mut rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "reply"
call = "unlockconnector"
error = { code = "notsupported", description = "no lock", details = { why = "test" } }
[[steps]]
action = "expect_call"
call = "UnlockConnector"
direction = "in"
timeout_ms = 5000
"#,
    );
    let report = run_with(&mut rig, &s, "scripted reply armed", async |csms| {
        let frame = csms.send_call("UnlockConnector", unlock()).await.unwrap();
        match frame {
            Frame::CallError {
                code,
                description,
                details,
                ..
            } => {
                assert_eq!(code.as_str(), "NotSupported");
                assert_eq!(description, "no lock");
                assert_eq!(details, json!({"why": "test"}));
            }
            other => panic!("expected a CALLERROR, got {other:?}"),
        }
    })
    .await;
    assert!(report.passed, "{report:#?}");
    rig.shutdown().await;
}

#[tokio::test]
async fn times_and_delay_are_honored() {
    let mut rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "reply"
call = "UnlockConnector"
result = { status = "UnlockFailed" }
times = 2
delay_ms = 250
[[steps]]
action = "repeat"
count = 3
[[steps.steps]]
action = "expect_call"
call = "UnlockConnector"
direction = "in"
timeout_ms = 5000
"#,
    );
    let report = run_with(&mut rig, &s, "scripted reply armed", async |csms| {
        for _ in 0..2 {
            let started = Instant::now();
            let frame = csms.send_call("UnlockConnector", unlock()).await.unwrap();
            assert!(
                started.elapsed() >= Duration::from_millis(250),
                "{:?} {frame:?}",
                started.elapsed()
            );
            assert_eq!(result_of(&frame)["status"], "UnlockFailed");
        }
        let third = csms.send_call("UnlockConnector", unlock()).await.unwrap();
        assert_eq!(result_of(&third)["status"], "Unlocked");
    })
    .await;
    assert!(report.passed, "{report:#?}");
    rig.shutdown().await;
}

#[tokio::test]
async fn an_unused_reply_is_cleared_when_the_scenario_ends() {
    let rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "reply"
call = "UnlockConnector"
result = { status = "UnlockFailed" }
"#,
    );
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    assert!(
        report.passed,
        "an unused reply is no failure by default: {report:#?}"
    );
    let frame = rig
        .csms
        .send_call("UnlockConnector", unlock())
        .await
        .unwrap();
    assert_eq!(result_of(&frame)["status"], "Unlocked");
    rig.shutdown().await;
}

#[tokio::test]
async fn must_fire_fails_the_reply_step_when_the_reply_was_not_used_up() {
    let rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "reply"
call = "UnlockConnector"
result = { status = "UnlockFailed" }
times = 2
must_fire = true
[[steps]]
action = "heartbeat"
"#,
    );
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    assert!(!report.passed, "{report:#?}");
    match &report.steps[0].status {
        StepStatus::Failed(reason) => {
            assert!(reason.contains("0 of 2"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(report.steps[1].status, StepStatus::Passed);
    rig.shutdown().await;
}

#[tokio::test]
async fn must_fire_passes_when_the_reply_was_used() {
    let mut rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "reply"
call = "UnlockConnector"
result = { status = "UnlockFailed" }
must_fire = true
[[steps]]
action = "expect_call"
call = "UnlockConnector"
direction = "in"
timeout_ms = 5000
"#,
    );
    let report = run_with(&mut rig, &s, "scripted reply armed", async |csms| {
        csms.send_call("UnlockConnector", unlock()).await.unwrap();
    })
    .await;
    assert!(report.passed, "{report:#?}");
    rig.shutdown().await;
}

#[tokio::test]
async fn a_scripted_reply_wins_over_a_response_rule_which_stays_for_later_calls() {
    let mut rig = booted_rig().await;
    let s = scenario(
        r#"
[[steps]]
action = "set_rule"
rule = "UnlockConnector=reject"
[[steps]]
action = "reply"
call = "UnlockConnector"
result = { status = "Unlocked" }
[[steps]]
action = "expect_call"
call = "UnlockConnector"
direction = "in"
timeout_ms = 5000
"#,
    );
    let report = run_with(&mut rig, &s, "scripted reply armed", async |csms| {
        let first = csms.send_call("UnlockConnector", unlock()).await.unwrap();
        assert_eq!(result_of(&first)["status"], "Unlocked");
        // The rule did not fire for the first CALL; it answers the second.
        let second = csms.send_call("UnlockConnector", unlock()).await.unwrap();
        assert_eq!(result_of(&second)["status"], "NotSupported");
    })
    .await;
    assert!(report.passed, "{report:#?}");
    rig.shutdown().await;
}

#[tokio::test]
async fn variables_are_substituted_in_the_result() {
    let mut rig = booted_rig().await;
    let s = scenario(
        r#"
[vars]
answer = "UnlockFailed"
[[steps]]
action = "reply"
call = "UnlockConnector"
result = { status = "${answer}" }
[[steps]]
action = "expect_call"
call = "UnlockConnector"
direction = "in"
timeout_ms = 5000
"#,
    );
    let report = run_with(&mut rig, &s, "scripted reply armed", async |csms| {
        let frame = csms.send_call("UnlockConnector", unlock()).await.unwrap();
        assert_eq!(result_of(&frame)["status"], "UnlockFailed");
    })
    .await;
    assert!(report.passed, "{report:#?}");
    rig.shutdown().await;
}

#[tokio::test]
async fn a_bad_substituted_result_fails_the_step_and_arms_nothing() {
    let rig = booted_rig().await;
    let s = scenario(
        r#"
[vars]
answer = "Bogus"
[[steps]]
action = "reply"
call = "UnlockConnector"
result = { status = "${answer}" }
"#,
    );
    let report = run_scenario(&rig.station, &s, &CancelToken::new(), None).await;
    assert!(!report.passed);
    match &report.steps[0].status {
        StepStatus::Failed(reason) => assert!(reason.contains("not a valid"), "{reason}"),
        other => panic!("{other:?}"),
    }
    let frame = rig
        .csms
        .send_call("UnlockConnector", unlock())
        .await
        .unwrap();
    assert_eq!(result_of(&frame)["status"], "Unlocked");
    rig.shutdown().await;
}

#[tokio::test]
async fn the_station_never_sends_a_result_that_breaks_the_response_schema() {
    use kirby_ocpp_core::station::Command;
    use kirby_ocpp_core::station::scripted_reply::{ScriptedAnswer, ScriptedReply};

    let rig = booted_rig().await;
    rig.station
        .send(Command::ArmScriptedReply {
            reply: ScriptedReply {
                tag: 0,
                action: "UnlockConnector".to_owned(),
                answer: ScriptedAnswer::Result {
                    payload: json!({"status": "Bogus"}),
                },
                remaining: 1,
                delay_ms: 0,
            },
        })
        .await
        .unwrap();
    // The one reply of the CALL is the station's CALLERROR; nothing invalid reaches the wire.
    let frame = rig
        .csms
        .send_call("UnlockConnector", unlock())
        .await
        .unwrap();
    assert!(matches!(frame, Frame::CallError { .. }), "{frame:?}");
    rig.shutdown().await;
}
