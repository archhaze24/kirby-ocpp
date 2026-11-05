//! Response rules: the station answers CSMS commands wrongly on purpose.

use std::time::{Duration, Instant};

use crate::station_support::{Rig, STEP, wait_state};
use kirby_ocpp_core::ocpp::types::RegistrationStatus;
use kirby_ocpp_core::ocpp::{ErrorCode, Frame};
use kirby_ocpp_core::station::{Command, Direction, ResponseRule};
use kirby_ocpp_core::{StationConfig, StationError};
use kirby_ocpp_testkit::TestkitError;
use serde_json::{Value, json};

fn rule(text: &str) -> ResponseRule {
    text.parse().expect("valid rule")
}

async fn booted() -> Rig {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    rig
}

async fn set(rig: &Rig, text: &str) {
    rig.send(Command::SetResponseRule { rule: rule(text) })
        .await;
}

async fn reply(rig: &Rig, action: &str, request: Value) -> Frame {
    rig.csms.send_call(action, request).await.expect("a reply")
}

async fn config_value(rig: &Rig, key: &str) -> Option<String> {
    let Frame::CallResult { payload, .. } =
        reply(rig, "GetConfiguration", json!({"key": [key]})).await
    else {
        panic!("GetConfiguration failed");
    };
    payload["configurationKey"][0]["value"]
        .as_str()
        .map(str::to_owned)
}

fn change(value: &str) -> Value {
    json!({"key": "MeterValueSampleInterval", "value": value})
}

#[tokio::test]
async fn reject_answers_with_the_actions_own_status_and_skips_the_handler() {
    let mut rig = booted().await;
    let before = config_value(&rig, "MeterValueSampleInterval").await;
    set(&rig, "ChangeConfiguration=reject").await;
    rig.observed.wait_log("response rule set").await;
    rig.csms
        .expect_status("ChangeConfiguration", change("77"), "Rejected")
        .await
        .unwrap();
    assert_eq!(
        config_value(&rig, "MeterValueSampleInterval").await,
        before,
        "the change must not have been applied"
    );
    rig.observed
        .wait_log("response rule: ChangeConfiguration -> reject")
        .await;
    // Unlimited: still active, and visible in the snapshot.
    assert_eq!(rig.station.snapshot().response_rules.len(), 1);
    rig.shutdown().await;
}

#[tokio::test]
async fn reject_does_not_execute_remote_start() {
    let rig = booted().await;
    set(&rig, "RemoteStartTransaction=reject").await;
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "RemoteStartTransaction",
            json!({"idTag": "TAG1", "connectorId": 1}),
            "Rejected",
        )
        .await
        .unwrap();
    // A later reply proves the station processed everything queued before it.
    rig.csms
        .expect_status("ClearCache", json!({}), "Accepted")
        .await
        .unwrap();
    rig.assert_no_calls("Authorize", mark);
    rig.assert_no_calls("StartTransaction", mark);
    rig.shutdown().await;
}

#[tokio::test]
async fn reject_without_a_status_falls_back_to_call_error_not_supported() {
    let rig = booted().await;
    set(&rig, "GetConfiguration=reject").await;
    rig.csms
        .expect_call_error("GetConfiguration", json!({}), &ErrorCode::NotSupported)
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn error_replies_with_the_code_and_text_without_running_the_handler() {
    let rig = booted().await;
    let before = config_value(&rig, "MeterValueSampleInterval").await;
    set(&rig, "ChangeConfiguration=error:SecurityError:go away").await;
    let Frame::CallError {
        code, description, ..
    } = reply(&rig, "ChangeConfiguration", change("77")).await
    else {
        panic!("expected a CALLERROR");
    };
    assert_eq!(code, ErrorCode::SecurityError);
    assert_eq!(description, "go away");
    assert_eq!(config_value(&rig, "MeterValueSampleInterval").await, before);
    rig.shutdown().await;
}

#[tokio::test]
async fn delay_holds_the_reply_back_but_runs_the_handler_and_other_traffic_flows() {
    let rig = booted().await;
    set(&rig, "ChangeConfiguration=delay:700*1").await;
    let started = Instant::now();
    let call = async {
        let frame = rig
            .csms
            .send_call("ChangeConfiguration", change("77"))
            .await;
        (frame, Instant::now())
    };
    // While the reply is held back the station keeps working.
    let others = async {
        rig.send(Command::Heartbeat).await;
        Instant::now()
    };
    let frame = {
        let ((frame, call_done), others_done) = tokio::join!(call, others);
        assert!(
            others_done < call_done,
            "other traffic waited for the delay"
        );
        assert!(call_done - started >= Duration::from_millis(700));
        frame.unwrap()
    };
    assert!(
        matches!(&frame, Frame::CallResult { payload, .. } if payload["status"] == "Accepted"),
        "{frame:?}"
    );
    // The handler ran (side effect present) and the one-shot rule is gone.
    assert_eq!(
        config_value(&rig, "MeterValueSampleInterval")
            .await
            .as_deref(),
        Some("77")
    );
    assert_eq!(rules_of(&rig), Vec::<String>::new());
    rig.shutdown().await;
}

#[tokio::test]
async fn drop_closes_the_connection_without_a_reply_and_the_station_reconnects() {
    let mut rig = booted().await;
    set(&rig, "ClearCache=drop*1").await;
    let result = rig.csms.send_call("ClearCache", json!({})).await;
    assert!(
        matches!(result, Err(TestkitError::ConnectionClosed { .. })),
        "{result:?}"
    );
    rig.csms.wait_for_connection_count(2, STEP).await.unwrap();
    rig.wait_registration(RegistrationStatus::Accepted).await;
    wait_state(&rig.station, "reconnected", |s| s.registration.is_some()).await;
    // No frame answered the dropped CALL, not even the InternalError fallback.
    rig.observed.drain();
    let answered = rig
        .observed
        .frames
        .iter()
        .any(|f| f.direction == Direction::Out && f.action.as_deref() == Some("ClearCache"));
    assert!(!answered);
    assert_eq!(rules_of(&rig), Vec::<String>::new());
    rig.shutdown().await;
}

#[tokio::test]
async fn ignore_never_answers_not_even_with_the_drop_fallback() {
    let mut rig = booted().await;
    set(&rig, "ChangeAvailability=ignore*1").await;
    let request = json!({"connectorId": 1, "type": "Operative"});
    let result = rig
        .csms
        .send_call_timeout(
            "ChangeAvailability",
            request.clone(),
            Duration::from_millis(400),
        )
        .await;
    assert!(
        matches!(result, Err(TestkitError::Timeout { .. })),
        "{result:?}"
    );
    // The rule was used up: the next identical CALL is answered normally, and by then any
    // stray reply to the first one would have been written (replies are ordered).
    rig.csms
        .expect_status("ChangeAvailability", request, "Accepted")
        .await
        .unwrap();
    rig.observed.drain();
    let first_id = rig
        .observed
        .frames
        .iter()
        .find(|f| f.direction == Direction::In && f.action.as_deref() == Some("ChangeAvailability"))
        .map(|f| f.msg_id.clone())
        .expect("the first CALL was seen");
    assert!(first_id.is_some());
    let replies_to_first = rig
        .observed
        .frames
        .iter()
        .filter(|f| f.direction == Direction::Out && f.msg_id == first_id)
        .count();
    assert_eq!(replies_to_first, 0);
    rig.shutdown().await;
}

#[tokio::test]
async fn count_applies_the_rule_n_times_then_it_disappears() {
    let mut rig = booted().await;
    set(&rig, "ClearCache=reject*2").await;
    assert_eq!(rig.station.snapshot().response_rules[0].remaining, Some(2));
    for _ in 0..2 {
        rig.csms
            .expect_status("ClearCache", json!({}), "Rejected")
            .await
            .unwrap();
    }
    rig.csms
        .expect_status("ClearCache", json!({}), "Accepted")
        .await
        .unwrap();
    assert_eq!(rules_of(&rig), Vec::<String>::new());
    rig.observed
        .wait_log("response rule: ClearCache -> reject")
        .await;
    rig.shutdown().await;
}

#[tokio::test]
async fn set_replaces_and_clear_removes_rules() {
    let rig = booted().await;
    set(&rig, "ClearCache=reject").await;
    set(&rig, "Reset=drop").await;
    set(&rig, "ClearCache=ignore*4").await;
    let texts = |rig: &Rig| -> Vec<String> {
        rig.station
            .snapshot()
            .response_rules
            .iter()
            .map(ToString::to_string)
            .collect()
    };
    assert_eq!(texts(&rig), ["ClearCache=ignore*4", "Reset=drop"]);
    rig.send(Command::ClearResponseRules {
        action: Some("clearcache".into()),
    })
    .await;
    assert_eq!(texts(&rig), ["Reset=drop"]);
    rig.send(Command::ClearResponseRules { action: None }).await;
    assert_eq!(texts(&rig), Vec::<String>::new());
    // Normal handling is back.
    rig.csms
        .expect_status("ClearCache", json!({}), "Accepted")
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn invalid_rules_are_refused() {
    let rig = booted().await;
    let bad_action = ResponseRule {
        action: "Heartbeat".into(),
        ..rule("Reset=reject")
    };
    let error = rig
        .station
        .send(Command::SetResponseRule { rule: bad_action })
        .await
        .unwrap_err();
    assert!(matches!(error, StationError::InvalidArgument(_)), "{error}");
    let zero = ResponseRule {
        remaining: Some(0),
        ..rule("Reset=reject")
    };
    assert!(
        rig.station
            .send(Command::SetResponseRule { rule: zero })
            .await
            .is_err()
    );
    assert!(
        rig.station
            .send(Command::ClearResponseRules {
                action: Some("Nope".into())
            })
            .await
            .is_err()
    );
    assert_eq!(rules_of(&rig), Vec::<String>::new());
    rig.shutdown().await;
}

#[tokio::test]
async fn initial_rules_from_the_config_are_active_from_the_start() {
    let mut rig = Rig::with(
        kirby_ocpp_testkit::CsmsOptions::default(),
        |mut config: StationConfig| {
            config.response_rules = vec![rule("ClearCache=reject*1")];
            config
        },
    )
    .await;
    assert_eq!(rig.station.snapshot().response_rules.len(), 1);
    rig.connect_booted().await;
    rig.csms
        .expect_status("ClearCache", json!({}), "Rejected")
        .await
        .unwrap();
    rig.shutdown().await;
}

fn rules_of(rig: &Rig) -> Vec<String> {
    rig.station
        .snapshot()
        .response_rules
        .iter()
        .map(ToString::to_string)
        .collect()
}
