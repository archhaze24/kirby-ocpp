//! OCPP 2.1 message types 5 (CALLRESULTERROR) and 6 (SEND) on `ws::Connection`, and the
//! unchanged handling of both on 1.6 and 2.0.1 connections.

use kirby_ocpp_core::config::TlsConfig;
use kirby_ocpp_core::error::CallError;
use kirby_ocpp_core::ocpp::schema::ViolationCause;
use kirby_ocpp_core::ocpp::{Direction, Frame, Protocol};
use kirby_ocpp_core::ws::{ConnectOptions, Connection, ConnectionEvent, SchemaSet, SendError};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms, Reply, SubprotocolPolicy};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::ws_support::{
    Negotiate, RawPeer, RawServer, STEP, collect_events, next_event, next_warning, options,
};

type Events = UnboundedReceiver<ConnectionEvent>;

const STREAM: &str = "NotifyPeriodicEventStream";

fn stream_payload() -> Value {
    json!({
        "id": 1,
        "pending": 0,
        "basetime": "2026-05-06T07:08:09Z",
        "data": [{"t": 0, "v": "1.5"}]
    })
}

async fn connect_as(
    url: &str,
    protocol: Protocol,
    options: ConnectOptions,
) -> (Connection, Events) {
    let options = options.with_schemas(SchemaSet::for_protocol(protocol).expect("schemas"));
    let (connection, events) = Connection::connect(
        url,
        "CP-001",
        &TlsConfig::default(),
        protocol.token(),
        options,
    )
    .await
    .expect("connect");
    (connection, collect_events(events))
}

/// A raw server and a connection of `protocol`, with the server's end of it.
async fn rig(protocol: Protocol) -> (Connection, Events, RawPeer, RawServer) {
    let mut server = RawServer::start(Negotiate::Select(protocol.token().into())).await;
    let (connection, events) = connect_as(&server.url("/ocpp"), protocol, options()).await;
    let peer = server.next_peer().await;
    (connection, events, peer, server)
}

/// Proves that nothing was written for the frames sent so far: a valid CSMS CALL is delivered
/// and answered, and its CALLRESULT must be the very next frame on the wire. The task handles
/// frames in order, so an earlier reply would show up first. Returns the events seen before
/// the CALL, frame records excluded.
async fn settle(peer: &mut RawPeer, events: &mut Events) -> Vec<ConnectionEvent> {
    peer.send_json(&json!([2, "sentinel", "ClearCache", {}]))
        .await;
    let mut seen = Vec::new();
    let call = tokio::time::timeout(STEP, async {
        loop {
            match events.recv().await.expect("event stream ended") {
                ConnectionEvent::Call(call) => break call,
                ConnectionEvent::Frame(_) => {}
                other => seen.push(other),
            }
        }
    })
    .await
    .expect("the sentinel CALL within the deadline");
    call.reply
        .accept_value(json!({"status": "Accepted"}))
        .expect("reply");
    let reply = peer.recv_frame().await;
    assert_eq!(
        reply,
        json!([3, "sentinel", {"status": "Accepted"}]),
        "the next frame on the wire is the sentinel's answer"
    );
    seen
}

#[tokio::test]
async fn a_valid_send_is_delivered_recorded_and_never_answered() {
    let (_connection, mut events, mut peer, _server) = rig(Protocol::V21).await;

    peer.send_json(&json!([6, "s-1", STREAM, stream_payload()]))
        .await;
    let record = next_event(&mut events, |event| match event {
        ConnectionEvent::Frame(record) if record.direction == Direction::In => Some(record),
        _ => None,
    })
    .await;
    assert_eq!(record.msg_id.as_deref(), Some("s-1"));
    assert_eq!(record.action.as_deref(), Some(STREAM));
    assert_eq!(record.frame[0], 6);

    let (id, action, payload) = next_event(&mut events, |event| match event {
        ConnectionEvent::Send {
            id,
            action,
            payload,
        } => Some((id, action, payload)),
        _ => None,
    })
    .await;
    assert_eq!((id.as_str(), action.as_str()), ("s-1", STREAM));
    assert_eq!(payload, stream_payload());

    assert!(settle(&mut peer, &mut events).await.is_empty());
}

#[tokio::test]
async fn invalid_and_unknown_sends_are_dropped_with_a_warning_and_no_reply() {
    let (_connection, mut events, mut peer, _server) = rig(Protocol::V21).await;

    // A schema violation, an action that is a CALL and not a SEND, and a made-up action.
    peer.send_json(&json!([6, "s-1", STREAM, {"id": 1}])).await;
    let warning = next_warning(&mut events).await;
    assert!(
        warning.starts_with("dropped SEND s-1 (NotifyPeriodicEventStream): "),
        "{warning}"
    );
    peer.send_json(&json!([6, "s-2", "Heartbeat", {}])).await;
    let warning = next_warning(&mut events).await;
    assert_eq!(
        warning,
        "dropped SEND s-2 (Heartbeat): Heartbeat is not an OCPP 2.1 SEND message"
    );
    peer.send_json(&json!([6, "s-3", "Nope", {}])).await;
    assert!(
        next_warning(&mut events)
            .await
            .starts_with("dropped SEND s-3 (Nope)")
    );
    // A SEND with a broken envelope: dropped as well, and a missing payload gets no CALLERROR.
    peer.send_json(&json!([6, "s-4", STREAM])).await;
    assert!(
        next_warning(&mut events)
            .await
            .contains("wrong number of elements")
    );

    let rest = settle(&mut peer, &mut events).await;
    assert!(
        rest.iter()
            .all(|event| !matches!(event, ConnectionEvent::Send { .. })),
        "no SEND event: {rest:?}"
    );
}

#[tokio::test]
async fn an_incoming_call_result_error_is_surfaced_and_never_answered() {
    let (_connection, mut events, mut peer, _server) = rig(Protocol::V21).await;

    peer.send_json(&json!([5, "c-9", "FormatViolation", "bad response", {"k": 1}]))
        .await;
    let (id, code, description, details) = next_event(&mut events, |event| match event {
        ConnectionEvent::CallResultError {
            id,
            code,
            description,
            details,
        } => Some((id, code, description, details)),
        _ => None,
    })
    .await;
    assert_eq!(id, "c-9");
    assert_eq!(code.as_str(), "FormatViolation");
    assert_eq!(description, "bad response");
    assert_eq!(details, json!({"k": 1}));

    assert!(settle(&mut peer, &mut events).await.is_empty());
}

#[tokio::test]
async fn a_schema_invalid_call_result_fails_the_call_and_is_answered_with_a_call_result_error() {
    let (connection, mut events, mut peer, _server) = rig(Protocol::V21).await;
    let call = {
        let connection = connection.clone();
        tokio::spawn(async move { connection.call("Heartbeat", json!({}), None).await })
    };
    let sent = peer.recv_frame().await;
    assert_eq!(
        (sent[0].clone(), sent[2].clone()),
        (json!(2), json!("Heartbeat"))
    );
    let id = sent[1].as_str().expect("id").to_owned();

    // `currentTime` is missing, and one property too many.
    peer.send_json(&json!([3, id, {"surprise": true}])).await;
    match call.await.unwrap() {
        Err(CallError::Schema(violation)) => {
            assert_eq!(violation.protocol, Protocol::V21);
        }
        other => panic!("expected a schema error, got {other:?}"),
    }
    let error = peer.recv_frame().await;
    assert_eq!(error[0], 5);
    assert_eq!(error[1], id.as_str(), "the id of the CALLRESULT");
    assert_eq!(
        error[2], "OccurrenceConstraintViolation",
        "the 2.x spelling"
    );
    let description = error[3].as_str().expect("description");
    assert!(
        !description.is_empty() && description.chars().count() <= 255,
        "{description}"
    );
    assert_eq!(error[4], json!({}));
    assert_eq!(error.as_array().map(Vec::len), Some(5));

    // The recording of our frame carries the action of the CALL.
    let record = next_event(&mut events, |event| match event {
        ConnectionEvent::Frame(record)
            if record.direction == Direction::Out && record.frame[0] == 5 =>
        {
            Some(record)
        }
        _ => None,
    })
    .await;
    assert_eq!(record.msg_id.as_deref(), Some(id.as_str()));
    assert_eq!(record.action.as_deref(), Some("Heartbeat"));

    // The next CALL still works: the connection is fine.
    let again = {
        let connection = connection.clone();
        tokio::spawn(async move { connection.call("Heartbeat", json!({}), None).await })
    };
    let sent = peer.recv_frame().await;
    peer.send_json(&json!([3, sent[1], {"currentTime": "2026-01-01T00:00:00Z"}]))
        .await;
    again.await.unwrap().expect("a valid answer");
}

#[tokio::test]
async fn the_call_result_error_description_is_cut_to_255_characters() {
    let (connection, _events, mut peer, _server) = rig(Protocol::V21).await;
    let call = {
        let connection = connection.clone();
        tokio::spawn(async move { connection.call("Heartbeat", json!({}), None).await })
    };
    let sent = peer.recv_frame().await;
    let mut payload = serde_json::Map::new();
    for index in 0..60 {
        payload.insert(format!("surplus_property_number_{index}"), json!(index));
    }
    peer.send_json(&json!([3, sent[1], payload])).await;
    let Err(CallError::Schema(violation)) = call.await.unwrap() else {
        panic!("expected a schema error");
    };
    assert!(
        violation.errors.join("; ").chars().count() > 255,
        "the violation is long enough to be cut"
    );
    let error = peer.recv_frame().await;
    assert_eq!(error[3].as_str().expect("description").chars().count(), 255);
}

#[tokio::test]
async fn older_versions_never_send_a_call_result_error() {
    for protocol in [Protocol::V16, Protocol::V201] {
        let (connection, mut events, mut peer, _server) = rig(protocol).await;
        let call = {
            let connection = connection.clone();
            tokio::spawn(async move { connection.call("Heartbeat", json!({}), None).await })
        };
        let sent = peer.recv_frame().await;
        peer.send_json(&json!([3, sent[1], {"surprise": true}]))
            .await;
        assert!(matches!(call.await.unwrap(), Err(CallError::Schema(_))));
        // The next frame on the wire is the answer to the sentinel, not a type 5 frame.
        let before = settle(&mut peer, &mut events).await;
        assert!(
            before
                .iter()
                .all(|event| !matches!(event, ConnectionEvent::CallResultError { .. })),
            "{protocol:?}"
        );
    }
}

#[tokio::test]
async fn send_unconfirmed_goes_out_while_a_call_is_outstanding() {
    let (connection, _events, mut peer, _server) = rig(Protocol::V21).await;
    let first = connection
        .enqueue("Heartbeat", json!({}), None, true)
        .expect("queue the first CALL");
    let second = connection
        .enqueue("Heartbeat", json!({}), None, true)
        .expect("queue the second CALL");
    let call = peer.recv_frame().await;
    assert_eq!(call[0], 2);

    // The first CALL is unanswered and the second waits behind it; the SEND does not.
    let id = connection
        .send_unconfirmed(STREAM, stream_payload())
        .expect("send");
    let sent = peer.recv_frame().await;
    assert_eq!(sent, json!([6, id, STREAM, stream_payload()]));
    let other = connection
        .send_unconfirmed(STREAM, stream_payload())
        .expect("send again");
    assert_ne!(id, other);
    let sent = peer.recv_frame().await;
    assert_eq!(sent[1], other.as_str());
    assert_ne!(sent[1], call[1], "CALL and SEND ids never repeat");

    // Answering the first CALL releases the second one.
    peer.send_json(&json!([3, call[1], {"currentTime": "2026-01-01T00:00:00Z"}]))
        .await;
    first.wait().await.expect("first answer");
    let call2 = peer.recv_frame().await;
    assert_eq!(call2[0], 2);
    assert_ne!(call2[1], call[1]);
    peer.send_json(&json!([3, call2[1], {"currentTime": "2026-01-01T00:00:00Z"}]))
        .await;
    second.wait().await.expect("second answer");
}

#[tokio::test]
async fn send_unconfirmed_validates_the_payload_and_the_action() {
    let (connection, mut events, mut peer, _server) = rig(Protocol::V21).await;
    match connection.send_unconfirmed(STREAM, json!({"id": 1})) {
        Err(SendError::Schema(violation)) => {
            assert_ne!(violation.cause, ViolationCause::UnknownAction);
        }
        other => panic!("expected a schema error, got {other:?}"),
    }
    // A request/response action is no SEND message.
    match connection.send_unconfirmed("Heartbeat", json!({})) {
        Err(SendError::Schema(violation)) => {
            assert_eq!(violation.cause, ViolationCause::UnknownAction);
        }
        other => panic!("expected an unknown action error, got {other:?}"),
    }
    // Nothing went out: the first frame is the SEND that follows.
    let id = connection
        .send_unconfirmed(STREAM, stream_payload())
        .expect("send");
    assert_eq!(peer.recv_frame().await[1], id.as_str());
    assert!(settle(&mut peer, &mut events).await.is_empty());
}

#[tokio::test]
async fn send_unconfirmed_is_refused_on_older_versions_and_closed_connections() {
    for protocol in [Protocol::V16, Protocol::V201] {
        let (connection, _events, mut peer, _server) = rig(protocol).await;
        match connection.send_unconfirmed(STREAM, stream_payload()) {
            Err(SendError::Unsupported { protocol: got }) => assert_eq!(got, protocol),
            other => panic!("expected Unsupported, got {other:?}"),
        }
        // Nothing was written: the next frame is the CALL that follows.
        let call = {
            let connection = connection.clone();
            tokio::spawn(async move { connection.call("Heartbeat", json!({}), None).await })
        };
        assert_eq!(peer.recv_frame().await[0], 2);
        call.abort();
    }

    let (connection, _events, _peer, _server) = rig(Protocol::V21).await;
    connection.close();
    assert!(matches!(
        connection.send_unconfirmed(STREAM, stream_payload()),
        Err(SendError::NotConnected)
    ));
}

#[tokio::test]
async fn a_2_0_1_connection_ignores_message_types_5_and_6_as_before() {
    let (_connection, mut events, mut peer, _server) = rig(Protocol::V201).await;
    for text in [
        r#"[6,"s-1","NotifyPeriodicEventStream",{"id":1}]"#,
        r#"[5,"c-1","FormatViolation","d",{}]"#,
    ] {
        peer.send_text(text).await;
        let warning = next_warning(&mut events).await;
        assert!(warning.contains("unsupported message type"), "{warning}");
    }
    let rest = settle(&mut peer, &mut events).await;
    assert!(
        rest.iter().all(|event| !matches!(
            event,
            ConnectionEvent::Send { .. } | ConnectionEvent::CallResultError { .. }
        )),
        "{rest:?}"
    );
}

#[tokio::test]
async fn a_2_1_connection_logs_other_message_types_and_does_not_answer() {
    let (_connection, mut events, mut peer, _server) = rig(Protocol::V21).await;
    peer.send_text(r#"[7,"x-1","Whatever",{"secret":"payload"}]"#)
        .await;
    let warning = next_warning(&mut events).await;
    assert!(warning.contains("unsupported message type"), "{warning}");
    assert!(settle(&mut peer, &mut events).await.is_empty());
}

/// The mock CSMS on `ocpp2.1`: the testkit sends raw frames and shows the frames of type 5
/// and 6 the connection writes.
#[tokio::test]
async fn the_mock_csms_exchanges_types_5_and_6_with_a_2_1_connection() {
    let csms = MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Require("ocpp2.1".to_owned()),
        ..CsmsOptions::default()
    })
    .await
    .expect("start the mock");
    let (connection, mut events) = connect_as(csms.url(), Protocol::V21, options()).await;
    csms.wait_for_connection(STEP).await.expect("connection");
    let mark = csms.frame_mark();

    // Inbound SEND and CALLRESULTERROR from the mock.
    csms.send_raw(json!([6, "m-1", STREAM, stream_payload()]).to_string())
        .expect("send raw");
    csms.send_raw(r#"[5,"m-2","GenericError","nope",{}]"#)
        .expect("send raw");
    next_event(&mut events, |event| {
        matches!(&event, ConnectionEvent::Send { id, .. } if id == "m-1").then_some(())
    })
    .await;
    next_event(&mut events, |event| {
        matches!(&event, ConnectionEvent::CallResultError { id, .. } if id == "m-2").then_some(())
    })
    .await;

    // Outbound SEND.
    let id = connection
        .send_unconfirmed(STREAM, stream_payload())
        .expect("send");
    let seen = csms
        .wait_for_frame_after(mark, |text| text.starts_with("[6,"), STEP)
        .await
        .expect("the SEND reached the mock");
    let Ok(Frame::Send {
        id: seen_id,
        action,
        payload,
    }) = Frame::parse_for(Protocol::V21, &seen.text)
    else {
        panic!("not a SEND: {}", seen.text);
    };
    assert_eq!((seen_id, action.as_str()), (id, STREAM));
    assert_eq!(payload, stream_payload());

    // An empty Heartbeat answer breaks the 2.1 schema: the call fails and the mock gets a
    // CALLRESULTERROR for it.
    csms.respond("Heartbeat", |_| Reply::Result(json!({})));
    let result = connection.call("Heartbeat", json!({}), Some(STEP)).await;
    assert!(matches!(result, Err(CallError::Schema(_))), "{result:?}");
    let seen = csms
        .wait_for_frame_after(mark, |text| text.starts_with("[5,"), STEP)
        .await
        .expect("the CALLRESULTERROR reached the mock");
    let Ok(Frame::CallResultError { id, .. }) = Frame::parse_for(Protocol::V21, &seen.text) else {
        panic!("not a CALLRESULTERROR: {}", seen.text);
    };
    let heartbeat = csms
        .received()
        .into_iter()
        .find(|r| r.action == "Heartbeat");
    assert_eq!(heartbeat.map(|r| r.message_id), Some(id));
    assert_eq!(
        csms.frames_since(mark).len(),
        3,
        "SEND, CALL, CALLRESULTERROR"
    );
}
