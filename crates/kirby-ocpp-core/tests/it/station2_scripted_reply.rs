//! Scripted replies on the OCPP 2.0.1 station: an armed answer takes the place of the handler
//! for the next CALLs of one action.

use std::time::{Duration, Instant};

use kirby_ocpp_core::ocpp::{ErrorCode, Frame};
use kirby_ocpp_core::station::scripted_reply::{ScriptedAnswer, ScriptedReply};
use kirby_ocpp_core::{Command2, Outcome2, StationError};
use kirby_ocpp_testkit::MockCsms;
use serde_json::{Value, json};

use crate::station2_support::{STEP, v201_csms};
use crate::station2_transactions::{booted, send, stop};

fn armed(tag: u64, action: &str, answer: ScriptedAnswer, remaining: u32) -> Command2 {
    Command2::ArmScriptedReply {
        reply: ScriptedReply {
            tag,
            action: action.to_owned(),
            answer,
            remaining,
            delay_ms: 0,
        },
    }
}

fn result(payload: Value) -> ScriptedAnswer {
    ScriptedAnswer::Result { payload }
}

async fn status_frame(csms: &MockCsms) -> Frame {
    tokio::time::timeout(STEP, csms.send_call("GetTransactionStatus", json!({})))
        .await
        .expect("the CALL is answered in time")
        .expect("the station answers")
}

fn result_payload(frame: Frame) -> Value {
    match frame {
        Frame::CallResult { payload, .. } => payload,
        other => panic!("expected a CALLRESULT, got {other:?}"),
    }
}

#[tokio::test]
async fn an_armed_reply_answers_instead_of_the_handler_and_runs_out() {
    let csms = v201_csms().await;
    let (station, task) = booted(&csms).await;
    // The case of the action name does not matter.
    send(
        &station,
        armed(
            4,
            "getTransactionStatus",
            result(json!({"messagesInQueue": true, "ongoingIndicator": true})),
            2,
        ),
    )
    .await;

    for _ in 0..2 {
        let payload = result_payload(status_frame(&csms).await);
        assert_eq!(
            payload,
            json!({"messagesInQueue": true, "ongoingIndicator": true})
        );
    }
    // Used up: the handler answers again.
    assert_eq!(
        result_payload(status_frame(&csms).await),
        json!({"messagesInQueue": false})
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn an_armed_error_is_sent_as_a_callerror() {
    let csms = v201_csms().await;
    let (station, task) = booted(&csms).await;
    send(
        &station,
        armed(
            1,
            "GetTransactionStatus",
            ScriptedAnswer::Error {
                code: ErrorCode::NotSupported,
                description: "scripted".to_owned(),
                details: json!({}),
            },
            1,
        ),
    )
    .await;
    match status_frame(&csms).await {
        Frame::CallError {
            code, description, ..
        } => {
            assert_eq!(code, ErrorCode::NotSupported);
            assert_eq!(description, "scripted");
        }
        other => panic!("expected a CALLERROR, got {other:?}"),
    }
    assert_eq!(
        result_payload(status_frame(&csms).await),
        json!({"messagesInQueue": false})
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn an_armed_reply_only_answers_its_own_action() {
    let csms = v201_csms().await;
    let (station, task) = booted(&csms).await;
    send(
        &station,
        armed(1, "ClearCache", result(json!({"status": "Rejected"})), 1),
    )
    .await;
    assert_eq!(
        result_payload(status_frame(&csms).await),
        json!({"messagesInQueue": false})
    );
    let cache = tokio::time::timeout(STEP, csms.send_call("ClearCache", json!({})))
        .await
        .expect("answered in time")
        .expect("answered");
    assert_eq!(result_payload(cache), json!({"status": "Rejected"}));
    stop(&station, task).await;
}

#[tokio::test]
async fn a_delayed_reply_is_held_back() {
    let csms = v201_csms().await;
    let (station, task) = booted(&csms).await;
    let mut command = armed(
        1,
        "GetTransactionStatus",
        result(json!({"messagesInQueue": true})),
        1,
    );
    if let Command2::ArmScriptedReply { reply } = &mut command {
        reply.delay_ms = 300;
    }
    send(&station, command).await;
    let started = Instant::now();
    let payload = result_payload(status_frame(&csms).await);
    assert_eq!(payload, json!({"messagesInQueue": true}));
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "answered after {:?}",
        started.elapsed()
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn clearing_reports_the_replies_that_were_not_used_up() {
    let csms = v201_csms().await;
    let (station, task) = booted(&csms).await;
    send(
        &station,
        armed(
            7,
            "GetTransactionStatus",
            result(json!({"messagesInQueue": true})),
            3,
        ),
    )
    .await;
    send(
        &station,
        armed(8, "reset", result(json!({"status": "Rejected"})), 1),
    )
    .await;
    result_payload(status_frame(&csms).await);

    let outcome = station
        .send(Command2::ClearScriptedReplies)
        .await
        .expect("cleared");
    let Outcome2::Response { payload } = outcome else {
        panic!("expected a Response, got {outcome:?}");
    };
    assert_eq!(
        payload,
        json!({"unused": [
            {"tag": 7, "action": "GetTransactionStatus", "remaining": 2},
            {"tag": 8, "action": "Reset", "remaining": 1},
        ]})
    );
    // Nothing is left to answer with, and a second clear reports nothing.
    assert_eq!(
        result_payload(status_frame(&csms).await),
        json!({"messagesInQueue": false})
    );
    let again = station.send(Command2::ClearScriptedReplies).await.unwrap();
    assert_eq!(
        again,
        Outcome2::Response {
            payload: json!({"unused": []})
        }
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn actions_the_csms_cannot_send_are_refused() {
    let csms = v201_csms().await;
    let (station, task) = booted(&csms).await;
    for action in ["BootNotification", "Frobnicate", "RemoteStartTransaction"] {
        let error = station
            .send(armed(1, action, result(json!({})), 1))
            .await
            .expect_err("refused");
        assert!(
            matches!(error, StationError::InvalidArgument(_)),
            "{action}: {error:?}"
        );
    }
    // Out-of-range counts come back from the shared list.
    let error = station
        .send(armed(1, "Reset", result(json!({})), 0))
        .await
        .expect_err("refused");
    assert!(
        matches!(error, StationError::InvalidArgument(_)),
        "{error:?}"
    );
    stop(&station, task).await;
}
