//! OCPP 2.1 message types 5 (CALLRESULTERROR) and 6 (SEND) on the engine side.
//!
//! A station frame of either type is recorded and never answered. A raw station follows each
//! frame with a Heartbeat: its answer is the next frame the station gets, so nothing was
//! sent in between.

use kirby_csms_core::{CsmsError, CsmsEvent, CsmsHandle, Direction, ErrorCode, Frame, Protocol};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::v201_support::{Client, result_of, start};

const V21: &str = "ocpp2.1";
const V201: &str = "ocpp2.0.1";
const STREAM: &str = "NotifyPeriodicEventStream";

fn stream_payload() -> Value {
    json!({
        "id": 1,
        "pending": 0,
        "basetime": "2026-01-01T00:00:00Z",
        "data": [{ "t": 0.5, "v": "12.5" }]
    })
}

fn send_text(id: &str, action: &str, payload: &Value) -> String {
    json!([6, id, action, payload]).to_string()
}

/// Everything the engine emitted so far, without waiting.
fn drain(events: &mut broadcast::Receiver<CsmsEvent>) -> Vec<CsmsEvent> {
    let mut seen = Vec::new();
    loop {
        match events.try_recv() {
            Ok(event) => seen.push(event),
            Err(broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => return seen,
        }
    }
}

fn malformed(events: &[CsmsEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            CsmsEvent::Malformed { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn inbound<'a>(events: &'a [CsmsEvent], wanted: &'a Frame) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event, CsmsEvent::Frame { direction: Direction::In, frame, .. } if frame == wanted)
        })
        .count()
}

/// Sends `text` and then a Heartbeat; the Heartbeat answer proves `text` got no reply.
async fn send_then_heartbeat(station: &mut Client, text: String) {
    station.send_text(text).await;
    result_of(station.call("Heartbeat", json!({})).await);
}

fn history_has(csms: &CsmsHandle, station: &str, wanted: &Frame) -> bool {
    csms.history(station).is_some_and(|entries| {
        entries
            .iter()
            .any(|entry| entry.direction == Direction::In && &entry.frame == wanted)
    })
}

#[tokio::test]
async fn a_valid_send_is_recorded_and_never_answered() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-1", V21).await;
    let mut events = csms.subscribe();

    let payload = stream_payload();
    send_then_heartbeat(&mut station, send_text("s-1", STREAM, &payload)).await;

    let seen = drain(&mut events);
    let sent = Frame::Send {
        id: "s-1".into(),
        action: STREAM.into(),
        payload,
    };
    assert_eq!(inbound(&seen, &sent), 1);
    assert!(history_has(&csms, "CP-1", &sent));
    assert!(malformed(&seen).is_empty(), "{seen:?}");
    csms.shutdown().await;
}

#[tokio::test]
async fn an_invalid_send_is_recorded_reported_as_malformed_and_never_answered() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-1", V21).await;
    let mut events = csms.subscribe();

    // A schema violation, and an action that is not a 2.1 SEND message.
    let broken = json!({ "id": 1 });
    send_then_heartbeat(&mut station, send_text("s-1", STREAM, &broken)).await;
    send_then_heartbeat(&mut station, send_text("s-2", "Heartbeat", &json!({}))).await;
    send_then_heartbeat(&mut station, send_text("s-3", "Bad\nAction", &json!({}))).await;

    let seen = drain(&mut events);
    let reasons = malformed(&seen);
    assert_eq!(reasons.len(), 3, "{reasons:?}");
    assert!(
        reasons[0].starts_with("invalid SEND NotifyPeriodicEventStream: "),
        "{reasons:?}"
    );
    assert!(reasons[1].contains("Heartbeat") && reasons[1].contains("not an OCPP 2.1 SEND"));
    assert!(reasons[2].contains("not an OCPP 2.1 SEND"), "{reasons:?}");
    assert!(reasons.iter().all(|reason| !reason.contains('\n')));
    let recorded = Frame::Send {
        id: "s-1".into(),
        action: STREAM.into(),
        payload: broken,
    };
    assert_eq!(inbound(&seen, &recorded), 1);
    assert!(history_has(&csms, "CP-1", &recorded));
    csms.shutdown().await;
}

#[tokio::test]
async fn a_send_is_not_validated_when_inbound_validation_is_off() {
    let csms = start(|config| config.validate_inbound = false).await;
    let mut station = Client::connect(&csms, "CP-1", V21).await;
    let mut events = csms.subscribe();

    let broken = json!({ "id": 1 });
    send_then_heartbeat(&mut station, send_text("s-1", STREAM, &broken)).await;

    let seen = drain(&mut events);
    let recorded = Frame::Send {
        id: "s-1".into(),
        action: STREAM.into(),
        payload: broken,
    };
    assert_eq!(inbound(&seen, &recorded), 1);
    assert!(malformed(&seen).is_empty(), "{seen:?}");
    csms.shutdown().await;
}

#[tokio::test]
async fn a_send_does_not_touch_a_pending_csms_call() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-1", V21).await;

    let call = {
        let csms = csms.clone();
        tokio::spawn(async move {
            csms.call("CP-1", "Reset", json!({"type": "Immediate"}))
                .await
        })
    };
    let Frame::Call { id, .. } = station.recv().await else {
        panic!("expected the CSMS CALL");
    };
    // A SEND that reuses the id of the pending CALL is not its answer.
    send_then_heartbeat(&mut station, send_text(&id, STREAM, &stream_payload())).await;
    assert!(!call.is_finished());
    station
        .send_text(Frame::call_result(id, json!({"status": "Accepted"})).to_json_string())
        .await;
    let answer = call
        .await
        .unwrap()
        .expect("the CALL completes with its CALLRESULT");
    assert_eq!(result_of(answer)["status"], "Accepted");
    csms.shutdown().await;
}

#[tokio::test]
async fn a_callresulterror_is_recorded_and_never_answered() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-1", V21).await;
    let mut events = csms.subscribe();

    let text = r#"[5,"r-1","FormatViolation","bad reply",{"k":1}]"#;
    send_then_heartbeat(&mut station, text.to_owned()).await;

    let seen = drain(&mut events);
    let recorded = Frame::parse_for(kirby_csms_core::Protocol::V21, text).unwrap();
    assert_eq!(inbound(&seen, &recorded), 1);
    assert!(history_has(&csms, "CP-1", &recorded));
    assert!(malformed(&seen).is_empty(), "{seen:?}");
    csms.shutdown().await;
}

#[tokio::test]
async fn a_callresulterror_does_not_complete_or_fail_a_pending_csms_call() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-1", V21).await;

    let call = {
        let csms = csms.clone();
        tokio::spawn(async move {
            csms.call("CP-1", "Reset", json!({"type": "Immediate"}))
                .await
        })
    };
    let Frame::Call { id, .. } = station.recv().await else {
        panic!("expected the CSMS CALL");
    };
    let text = json!([5, id, "FormatViolation", "bad reply", {}]).to_string();
    send_then_heartbeat(&mut station, text).await;
    assert!(!call.is_finished());
    station
        .send_text(Frame::call_result(id, json!({"status": "Accepted"})).to_json_string())
        .await;
    let answer = call
        .await
        .unwrap()
        .expect("the CALL completes with its CALLRESULT");
    assert_eq!(result_of(answer)["status"], "Accepted");
    csms.shutdown().await;
}

#[tokio::test]
async fn types_5_and_6_stay_malformed_on_older_sessions() {
    let csms = start(|_| {}).await;
    for (id, subprotocol) in [("CP-201", V201), ("CP-16", "ocpp1.6")] {
        let mut station = Client::connect(&csms, id, subprotocol).await;
        let mut events = csms.subscribe();
        let send = send_text("s-1", STREAM, &stream_payload());
        let error = r#"[5,"r-1","FormatViolation","bad",{}]"#;
        station.send_text(send).await;
        station.send_text(error.to_owned()).await;
        // On 2.0.1 the Heartbeat is answered; the unknown frames are not.
        let answer = station.call("Heartbeat", json!({})).await;
        assert!(matches!(answer, Frame::CallResult { .. }), "{id}");

        let seen = drain(&mut events);
        assert_eq!(malformed(&seen).len(), 2, "{id}: {seen:?}");
    }
    csms.shutdown().await;
}

fn spawn_reset(
    csms: &CsmsHandle,
    station: &str,
) -> tokio::task::JoinHandle<Result<Frame, CsmsError>> {
    let csms = csms.clone();
    let station = station.to_owned();
    tokio::spawn(async move {
        csms.call(&station, "Reset", json!({"type": "Immediate"}))
            .await
    })
}

async fn next_call_id(station: &mut Client) -> String {
    let Frame::Call { id, .. } = station.recv().await else {
        panic!("expected the CSMS CALL");
    };
    id
}

#[tokio::test]
async fn an_invalid_callresult_on_21_is_answered_with_a_callresulterror_and_fails_the_call() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-1", V21).await;
    let call = spawn_reset(&csms, "CP-1");
    let id = next_call_id(&mut station).await;

    station
        .send_text(Frame::call_result(id.as_str(), json!({"status": "Bogus"})).to_json_string())
        .await;
    let Frame::CallResultError {
        id: error_id,
        code,
        description,
        ..
    } = station.recv_for(Protocol::V21).await
    else {
        panic!("expected a CALLRESULTERROR");
    };
    assert_eq!(error_id, id);
    assert_eq!(code, ErrorCode::PropertyConstraintViolation);
    assert!(!description.is_empty() && description.chars().count() <= 255);
    let error = call.await.unwrap().expect_err("the call fails");
    assert!(
        matches!(&error, CsmsError::InvalidResponse { action, .. } if action == "Reset"),
        "{error:?}"
    );
    // The frame is recorded like any outbound frame.
    let recorded = csms.history("CP-1").unwrap().into_iter().any(|entry| {
        entry.direction == Direction::Out
            && matches!(&entry.frame, Frame::CallResultError { id: i, .. } if *i == id)
    });
    assert!(recorded);
    csms.shutdown().await;
}

#[tokio::test]
async fn a_valid_callresult_on_21_gets_no_callresulterror() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-1", V21).await;
    let call = spawn_reset(&csms, "CP-1");
    let id = next_call_id(&mut station).await;
    station
        .send_text(Frame::call_result(id, json!({"status": "Accepted"})).to_json_string())
        .await;
    let answer = call.await.unwrap().expect("the call completes");
    assert_eq!(result_of(answer)["status"], "Accepted");
    // The Heartbeat answer is the next frame: nothing was sent in between.
    result_of(station.call("Heartbeat", json!({})).await);
    csms.shutdown().await;
}

#[tokio::test]
async fn an_invalid_callresult_on_201_gets_no_callresulterror() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-201", V201).await;
    let call = spawn_reset(&csms, "CP-201");
    let id = next_call_id(&mut station).await;
    station
        .send_text(Frame::call_result(id, json!({"status": "Bogus"})).to_json_string())
        .await;
    let answer = call.await.unwrap().expect("the call still completes");
    assert_eq!(result_of(answer)["status"], "Bogus");
    result_of(station.call("Heartbeat", json!({})).await);
    csms.shutdown().await;
}

#[tokio::test]
async fn an_invalid_callresult_on_21_is_not_checked_when_validation_is_off() {
    let csms = start(|config| config.validate_inbound = false).await;
    let mut station = Client::connect(&csms, "CP-1", V21).await;
    let call = spawn_reset(&csms, "CP-1");
    let id = next_call_id(&mut station).await;
    station
        .send_text(Frame::call_result(id, json!({"status": "Bogus"})).to_json_string())
        .await;
    let answer = call.await.unwrap().expect("the call completes");
    assert_eq!(result_of(answer)["status"], "Bogus");
    result_of(station.call("Heartbeat", json!({})).await);
    csms.shutdown().await;
}
