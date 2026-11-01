//! Schema validation in both directions, the serialized CALL queue, timeouts, close
//! and reply handling of `ws::Connection` (ports the seven tests of
//! `tests/ocpp-validation.test.mjs` plus the queue and timeout cases).

mod ws_support;

use std::time::Duration;

use futures_util::SinkExt;
use kirby_ocpp_core::error::CallError;
use kirby_ocpp_core::ocpp::Direction;
use kirby_ocpp_core::ocpp::ErrorCode;
use kirby_ocpp_core::ws::{ConnectOptions, ConnectionEvent, IncomingCall};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use ws_support::{
    Negotiate, RawServer, connect, next_call, next_closed, next_event, next_warning, options,
};

fn boot_payload() -> Value {
    json!({"chargePointVendor": "Kirby", "chargePointModel": "TUI-16"})
}

fn heartbeat_result(time: &str) -> Value {
    json!({ "currentTime": time })
}

#[tokio::test]
async fn responds_with_occurence_violation_for_missing_required_fields() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (_connection, mut events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    peer.send_json(&json!([2, "csms-1", "RemoteStartTransaction", {}]))
        .await;
    let reply = peer.recv_frame().await;
    assert_eq!(reply[0], 4);
    assert_eq!(reply[1], "csms-1");
    assert_eq!(reply[2], "OccurenceConstraintViolation");
    assert!(
        next_warning(&mut events)
            .await
            .contains("RemoteStartTransaction")
    );
}

#[tokio::test]
async fn responds_with_property_violation_for_enum_violations() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (_connection, _events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    peer.send_json(&json!([2, "csms-2", "Reset", {"type": "Warm"}]))
        .await;
    let reply = peer.recv_frame().await;
    assert_eq!(reply[0], 4);
    assert_eq!(reply[1], "csms-2");
    assert_eq!(reply[2], "PropertyConstraintViolation");
}

#[tokio::test]
async fn responds_with_formation_violation_for_malformed_calls_with_an_id() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (_connection, _events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    peer.send_json(&json!([2, "csms-3", "Reset"])).await;
    let reply = peer.recv_frame().await;
    assert_eq!(reply[0], 4);
    assert_eq!(reply[1], "csms-3");
    assert_eq!(reply[2], "FormationViolation");
}

#[tokio::test]
async fn unknown_action_is_answered_with_not_implemented() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (_connection, _events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    peer.send_json(&json!([2, "csms-4", "Frobnicate", {}]))
        .await;
    let reply = peer.recv_frame().await;
    assert_eq!(reply[0], 4);
    assert_eq!(reply[2], "NotImplemented");
}

#[tokio::test]
async fn rejects_pending_calls_when_the_callresult_violates_the_schema() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (connection, _events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    let call = {
        let connection = connection.clone();
        tokio::spawn(async move {
            connection
                .call("BootNotification", boot_payload(), None)
                .await
        })
    };
    let frame = peer.recv_frame().await;
    assert_eq!(frame[0], 2);
    assert_eq!(frame[2], "BootNotification");

    peer.send_json(&json!([3, frame[1], {"status": "Accepted"}]))
        .await;
    match call.await.unwrap() {
        Err(CallError::Schema(violation)) => {
            assert_eq!(
                violation.error_code(),
                ErrorCode::OccurenceConstraintViolation
            );
        }
        other => panic!("expected a schema error, got {other:?}"),
    }
}

#[tokio::test]
async fn rejects_pending_calls_when_the_response_times_out() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (connection, _events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    let call = {
        let connection = connection.clone();
        tokio::spawn(async move {
            connection
                .call(
                    "BootNotification",
                    boot_payload(),
                    Some(Duration::from_millis(20)),
                )
                .await
        })
    };
    let frame = peer.recv_frame().await;
    assert_eq!(frame[2], "BootNotification");

    match call.await.unwrap() {
        Err(CallError::Timeout { action, after }) => {
            assert_eq!(action, "BootNotification");
            assert_eq!(after, Duration::from_millis(20));
        }
        other => panic!("expected a timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn rejects_pending_calls_when_the_socket_closes() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (connection, mut events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    let call = {
        let connection = connection.clone();
        tokio::spawn(async move { connection.call("Heartbeat", json!({}), None).await })
    };
    let frame = peer.recv_frame().await;
    assert_eq!(frame[2], "Heartbeat");
    drop(peer);

    assert!(matches!(
        call.await.unwrap(),
        Err(CallError::Closed { code: Some(1006) })
    ));
    let (code, _, by_us) = next_closed(&mut events).await;
    assert_eq!(code, Some(1006));
    assert!(!by_us);
}

#[tokio::test]
async fn preserves_schema_properties_named_id_when_validating() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (_connection, mut events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    peer.send_json(&json!([
        2,
        "csms-clear-profile",
        "ClearChargingProfile",
        {"id": 424_242}
    ]))
    .await;
    let IncomingCall {
        id,
        action,
        payload,
        reply,
    } = next_call(&mut events).await;
    assert_eq!(id, "csms-clear-profile");
    assert_eq!(action, "ClearChargingProfile");
    assert_eq!(payload, json!({"id": 424_242}));
    reply.accept_value(json!({"status": "Unknown"})).unwrap();

    let frame = peer.recv_frame().await;
    assert_eq!(
        frame,
        json!([3, "csms-clear-profile", {"status": "Unknown"}])
    );
}

#[tokio::test]
async fn serialized_calls_wait_for_each_answer_in_order() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (connection, mut events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    let first = {
        let connection = connection.clone();
        tokio::spawn(async move { connection.call("Heartbeat", json!({}), None).await })
    };
    let first_frame = peer.recv_frame().await;
    let second = {
        let connection = connection.clone();
        tokio::spawn(async move { connection.call("Heartbeat", json!({}), None).await })
    };

    // The second CALL must not reach the wire before the first is answered.
    peer.reply(&first_frame, heartbeat_result("2026-01-01T00:00:00Z"))
        .await;
    let second_frame = peer.recv_frame().await;
    peer.reply(&second_frame, heartbeat_result("2026-01-02T00:00:00Z"))
        .await;

    assert_eq!(
        first.await.unwrap().unwrap()["currentTime"],
        "2026-01-01T00:00:00Z"
    );
    assert_eq!(
        second.await.unwrap().unwrap()["currentTime"],
        "2026-01-02T00:00:00Z"
    );

    // Frame events, in wire order: out A, in A, out B, in B.
    let expected = [
        (first_frame[1].as_str().unwrap().to_owned(), "out"),
        (first_frame[1].as_str().unwrap().to_owned(), "in"),
        (second_frame[1].as_str().unwrap().to_owned(), "out"),
        (second_frame[1].as_str().unwrap().to_owned(), "in"),
    ];
    for (id, direction) in expected {
        let record = next_event(&mut events, |event| match event {
            ConnectionEvent::Frame(record) => Some(record),
            _ => None,
        })
        .await;
        assert_eq!(record.msg_id.as_deref(), Some(id.as_str()));
        let wanted = if direction == "out" {
            Direction::Out
        } else {
            Direction::In
        };
        assert_eq!(record.direction, wanted);
    }
}

#[tokio::test]
async fn serialization_can_be_disabled_for_negative_tests() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let options = ConnectOptions {
        serialize_calls: false,
        ..options()
    };
    let (connection, _events) = connect(&server.url("/ocpp"), options).await;
    let mut peer = server.next_peer().await;

    let first = {
        let connection = connection.clone();
        tokio::spawn(async move { connection.call("Heartbeat", json!({}), None).await })
    };
    let first_frame = peer.recv_frame().await;
    let second = {
        let connection = connection.clone();
        tokio::spawn(async move { connection.call("Heartbeat", json!({}), None).await })
    };
    // Both are on the wire before either is answered.
    let second_frame = peer.recv_frame().await;
    assert_ne!(first_frame[1], second_frame[1]);

    peer.reply(&second_frame, heartbeat_result("2026-01-02T00:00:00Z"))
        .await;
    peer.reply(&first_frame, heartbeat_result("2026-01-01T00:00:00Z"))
        .await;
    assert!(first.await.unwrap().is_ok());
    assert!(second.await.unwrap().is_ok());
}

#[tokio::test]
async fn late_response_after_a_timeout_is_ignored() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (connection, _events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    let slow = {
        let connection = connection.clone();
        tokio::spawn(async move {
            connection
                .call("Heartbeat", json!({}), Some(Duration::from_millis(50)))
                .await
        })
    };
    let slow_frame = peer.recv_frame().await;
    assert!(matches!(
        slow.await.unwrap(),
        Err(CallError::Timeout { .. })
    ));

    // The answer arrives too late: it must not satisfy anything.
    peer.reply(&slow_frame, heartbeat_result("2000-01-01T00:00:00Z"))
        .await;

    let next = {
        let connection = connection.clone();
        tokio::spawn(async move { connection.call("Heartbeat", json!({}), None).await })
    };
    let next_frame = peer.recv_frame().await;
    peer.reply(&next_frame, heartbeat_result("2030-01-01T00:00:00Z"))
        .await;
    assert_eq!(
        next.await.unwrap().unwrap()["currentTime"],
        "2030-01-01T00:00:00Z"
    );
}

#[tokio::test]
async fn close_fails_pending_and_queued_calls() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (connection, _events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    let in_flight = {
        let connection = connection.clone();
        tokio::spawn(async move { connection.call("Heartbeat", json!({}), None).await })
    };
    let frame = peer.recv_frame().await;
    assert_eq!(frame[2], "Heartbeat");

    let queued = {
        let connection = connection.clone();
        tokio::spawn(async move { connection.call("Heartbeat", json!({}), None).await })
    };
    // Current-thread runtime: one yield lets the queued call reach the task first.
    tokio::task::yield_now().await;
    connection.close();

    assert!(matches!(
        in_flight.await.unwrap(),
        Err(CallError::Closed { code: Some(1000) })
    ));
    assert!(matches!(
        queued.await.unwrap(),
        Err(CallError::Closed { .. })
    ));
    assert!(matches!(
        connection.call("Heartbeat", json!({}), None).await,
        Err(CallError::NotConnected)
    ));

    // The peer sees our Close; answering it (flush writes tungstenite's queued reply)
    // completes the close handshake.
    assert!(matches!(peer.recv().await, Some(Message::Close(_))));
    peer.ws.flush().await.expect("flush the close reply");
}

#[tokio::test]
async fn outgoing_schema_violation_sends_nothing() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (connection, _events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    match connection
        .call("Heartbeat", json!({"unexpected": 1}), None)
        .await
    {
        Err(CallError::Schema(violation)) => {
            assert_eq!(
                violation.error_code(),
                ErrorCode::PropertyConstraintViolation
            );
        }
        other => panic!("expected a schema error, got {other:?}"),
    }

    // The first frame the peer sees is the valid CALL that follows.
    let valid = {
        let connection = connection.clone();
        tokio::spawn(async move { connection.call("Heartbeat", json!({}), None).await })
    };
    let frame = peer.recv_frame().await;
    assert_eq!(frame[2], "Heartbeat");
    assert_eq!(frame[3], json!({}));
    peer.reply(&frame, heartbeat_result("2026-01-01T00:00:00Z"))
        .await;
    assert!(valid.await.unwrap().is_ok());
}

#[tokio::test]
async fn invalid_reply_from_the_station_is_replaced_by_internal_error() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (_connection, mut events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    peer.send_json(&json!([2, "hb-1", "Heartbeat", {}])).await;
    let IncomingCall { reply, .. } = next_call(&mut events).await;
    reply.accept_value(json!({"unexpected": true})).unwrap();

    let frame = peer.recv_frame().await;
    assert_eq!(frame[0], 4);
    assert_eq!(frame[1], "hb-1");
    assert_eq!(frame[2], "InternalError");
    assert!(next_warning(&mut events).await.contains("InternalError"));
}

#[tokio::test]
async fn dropped_reply_handle_answers_internal_error() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (_connection, mut events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    peer.send_json(&json!([2, "hb-2", "Heartbeat", {}])).await;
    let call = next_call(&mut events).await;
    drop(call);

    let frame = peer.recv_frame().await;
    assert_eq!(frame[0], 4);
    assert_eq!(frame[1], "hb-2");
    assert_eq!(frame[2], "InternalError");
}

#[tokio::test]
async fn valid_reply_is_sent_once_with_its_payload() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (_connection, mut events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    peer.send_json(&json!([2, "hb-3", "Heartbeat", {}])).await;
    let IncomingCall { reply, .. } = next_call(&mut events).await;
    reply
        .accept_value(heartbeat_result("2026-05-06T07:08:09Z"))
        .unwrap();

    let frame = peer.recv_frame().await;
    assert_eq!(
        frame,
        json!([3, "hb-3", {"currentTime": "2026-05-06T07:08:09Z"}])
    );
}

#[tokio::test]
async fn frames_in_and_out_are_reported_as_events() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (connection, mut events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    let call = {
        let connection = connection.clone();
        tokio::spawn(async move { connection.call("Heartbeat", json!({}), None).await })
    };
    let frame = peer.recv_frame().await;
    let record = next_event(&mut events, |event| match event {
        ConnectionEvent::Frame(record) => Some(record),
        _ => None,
    })
    .await;
    assert_eq!(record.direction, Direction::Out);
    assert_eq!(record.action.as_deref(), Some("Heartbeat"));
    assert_eq!(record.frame, frame);

    peer.reply(&frame, heartbeat_result("2026-01-01T00:00:00Z"))
        .await;
    call.await.unwrap().unwrap();
    let answer = next_event(&mut events, |event| match event {
        ConnectionEvent::Frame(record) => Some(record),
        _ => None,
    })
    .await;
    assert_eq!(answer.direction, Direction::In);
    assert_eq!(answer.action.as_deref(), Some("Heartbeat"), "correlated");
}

#[tokio::test]
async fn text_that_is_not_a_frame_is_reported_raw_and_ignored() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (_connection, mut events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    peer.send_text("not json at all").await;
    let record = next_event(&mut events, |event| match event {
        ConnectionEvent::Frame(record) => Some(record),
        _ => None,
    })
    .await;
    assert_eq!(record.frame, Value::String("not json at all".to_owned()));
    assert!(
        next_warning(&mut events)
            .await
            .contains("invalid OCPP-J frame")
    );
}
