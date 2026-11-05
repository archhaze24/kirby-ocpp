//! Behaviour of the mock CSMS over plain `ws://`.

use std::time::Duration;

use crate::common::{
    STEP, connect_plain, cp_call, heartbeat, recv_frame, recv_text, result_payload, send_frame,
    wait_closed,
};
use chrono::{DateTime, Utc};
use kirby_ocpp_testkit::{
    CsmsOptions, ErrorCode, Fault, Frame, MockCsms, Reply, SubprotocolPolicy, TestkitError,
    TransactionIds,
};
use serde_json::{Value, json};

const SHORT: Duration = Duration::from_millis(100);

async fn start() -> MockCsms {
    MockCsms::start(CsmsOptions::default())
        .await
        .expect("start mock CSMS")
}

async fn connect_cp(csms: &MockCsms, id: &str) -> crate::common::PlainSocket {
    let (socket, _) = connect_plain(&csms.charge_point_url(id), &["ocpp1.6"])
        .await
        .expect("connect charge point");
    socket
}

#[tokio::test]
async fn boot_and_heartbeat_roundtrip() {
    let csms = MockCsms::start(CsmsOptions {
        heartbeat_interval: 7,
        ..CsmsOptions::default()
    })
    .await
    .expect("start");
    let before = Utc::now();
    let mut cp = connect_cp(&csms, "CP-001").await;

    let boot = result_payload(
        cp_call(
            &mut cp,
            "boot-1",
            "BootNotification",
            json!({"chargePointVendor": "Kirby", "chargePointModel": "TUI-16"}),
        )
        .await,
    );
    assert_eq!(boot["status"], "Accepted");
    assert_eq!(boot["interval"], 7);
    let current = DateTime::parse_from_rfc3339(boot["currentTime"].as_str().expect("time"))
        .expect("RFC 3339 currentTime");
    // currentTime has millisecond precision, so compare at that precision.
    assert!(current.timestamp_millis() >= before.timestamp_millis());

    let beat = result_payload(cp_call(&mut cp, "hb-1", "Heartbeat", heartbeat()).await);
    assert!(beat["currentTime"].as_str().is_some());

    let payload = csms
        .wait_for_call("BootNotification", STEP)
        .await
        .expect("boot recorded");
    assert_eq!(payload["chargePointVendor"], "Kirby");

    let received = csms.received();
    assert_eq!(received.len(), 2);
    assert_eq!(received[0].action, "BootNotification");
    assert_eq!(received[0].message_id, "boot-1");
    assert_eq!(received[1].action, "Heartbeat");
    assert!(received[0].ts <= received[1].ts);
    assert!(received.iter().all(|call| call.connection == 1));
}

#[tokio::test]
async fn magic_id_tags_for_authorize() {
    let csms = start().await;
    let mut cp = connect_cp(&csms, "CP-001").await;

    for (id_tag, status) in [
        ("BADTAG", "Blocked"),
        ("CONCURRENTAUTH", "ConcurrentTx"),
        ("TAG1", "Accepted"),
    ] {
        let reply =
            result_payload(cp_call(&mut cp, id_tag, "Authorize", json!({"idTag": id_tag})).await);
        assert_eq!(reply["idTagInfo"]["status"], status, "{id_tag}");
    }

    let expired =
        result_payload(cp_call(&mut cp, "e", "Authorize", json!({"idTag": "EXPIREDAUTH"})).await);
    assert_eq!(expired["idTagInfo"]["status"], "Accepted");
    let expiry = expired["idTagInfo"]["expiryDate"].as_str().expect("expiry");
    let expiry = DateTime::parse_from_rfc3339(expiry).expect("RFC 3339 expiry");
    assert!(
        expiry < Utc::now(),
        "EXPIREDAUTH expiry must be in the past"
    );

    let parent =
        result_payload(cp_call(&mut cp, "p", "Authorize", json!({"idTag": "PARENTAUTH"})).await);
    assert_eq!(parent["idTagInfo"]["parentIdTag"], "PARENTTAG");
}

#[tokio::test]
async fn magic_id_tags_for_start_transaction_and_fixed_ids() {
    let csms = start().await;
    let mut cp = connect_cp(&csms, "CP-001").await;

    let expected = [
        ("BLOCKSTART", "Blocked"),
        ("CONCURRENTSTART", "ConcurrentTx"),
        ("TAG1", "Accepted"),
    ];
    for (index, (id_tag, status)) in expected.into_iter().enumerate() {
        let reply = result_payload(
            cp_call(
                &mut cp,
                &format!("tx-{index}"),
                "StartTransaction",
                json!({"connectorId": 1, "idTag": id_tag, "meterStart": 0, "timestamp": "2026-01-01T00:00:00Z"}),
            )
            .await,
        );
        assert_eq!(reply["transactionId"], 123, "fixed transaction id");
        assert_eq!(reply["idTagInfo"]["status"], status, "{id_tag}");
    }

    let expired = result_payload(
        cp_call(
            &mut cp,
            "tx-expired",
            "StartTransaction",
            json!({"connectorId": 1, "idTag": "EXPIREDSTART", "meterStart": 0, "timestamp": "2026-01-01T00:00:00Z"}),
        )
        .await,
    );
    assert_eq!(expired["idTagInfo"]["status"], "Accepted");
    assert!(expired["idTagInfo"]["expiryDate"].is_string());
}

#[tokio::test]
async fn incrementing_transaction_ids() {
    let csms = MockCsms::start(CsmsOptions {
        transaction_ids: TransactionIds::Incrementing(1000),
        ..CsmsOptions::default()
    })
    .await
    .expect("start");
    let mut cp = connect_cp(&csms, "CP-001").await;

    let mut ids = Vec::new();
    for index in 0..3 {
        let reply = result_payload(
            cp_call(
                &mut cp,
                &format!("tx-{index}"),
                "StartTransaction",
                json!({"connectorId": 1, "idTag": "TAG1", "meterStart": 0, "timestamp": "2026-01-01T00:00:00Z"}),
            )
            .await,
        );
        ids.push(reply["transactionId"].as_i64().expect("id"));
    }
    assert_eq!(ids, [1000, 1001, 1002]);
}

#[tokio::test]
async fn default_answers_for_remaining_actions() {
    let csms = start().await;
    let mut cp = connect_cp(&csms, "CP-001").await;

    let cases = [
        (
            "StopTransaction",
            json!({"transactionId": 123, "meterStop": 5, "timestamp": "2026-01-01T00:00:00Z"}),
        ),
        ("MeterValues", json!({"connectorId": 1, "meterValue": []})),
        (
            "StatusNotification",
            json!({"connectorId": 1, "errorCode": "NoError", "status": "Available"}),
        ),
        ("DiagnosticsStatusNotification", json!({"status": "Idle"})),
        ("FirmwareStatusNotification", json!({"status": "Idle"})),
        ("SomeFutureAction", json!({})),
    ];
    for (action, payload) in cases {
        let reply = result_payload(cp_call(&mut cp, action, action, payload).await);
        assert_eq!(reply, json!({}), "{action}");
    }

    let data = result_payload(
        cp_call(
            &mut cp,
            "dt",
            "DataTransfer",
            json!({"vendorId": "Kirby", "data": "ping"}),
        )
        .await,
    );
    assert_eq!(data, json!({"status": "Accepted", "data": "pong"}));
}

#[tokio::test]
async fn injected_call_error_is_counted_down() {
    let csms = start().await;
    let mut cp = connect_cp(&csms, "CP-001").await;
    csms.inject(Fault::call_error("StartTransaction").times(2));
    assert_eq!(csms.faults_remaining(), 2);

    for index in 0..2 {
        let reply = cp_call(
            &mut cp,
            &format!("tx-{index}"),
            "StartTransaction",
            json!({"connectorId": 1, "idTag": "TAG1", "meterStart": 0, "timestamp": "2026-01-01T00:00:00Z"}),
        )
        .await;
        match reply {
            Frame::CallError { code, .. } => assert_eq!(code, ErrorCode::InternalError),
            other => panic!("expected CALLERROR, got {other:?}"),
        }
    }
    assert_eq!(csms.faults_remaining(), 0);

    let reply = result_payload(
        cp_call(
            &mut cp,
            "tx-2",
            "StartTransaction",
            json!({"connectorId": 1, "idTag": "TAG1", "meterStart": 0, "timestamp": "2026-01-01T00:00:00Z"}),
        )
        .await,
    );
    assert_eq!(reply["idTagInfo"]["status"], "Accepted");
    assert_eq!(csms.received().len(), 3, "faulted calls are still recorded");
}

#[tokio::test]
async fn injected_silence_drops_only_that_reply() {
    let csms = start().await;
    let mut cp = connect_cp(&csms, "CP-001").await;
    csms.inject(Fault::silence("Heartbeat"));

    // The first heartbeat gets no reply. The next reply on the socket must belong to the second.
    send_frame(&mut cp, &Frame::call("hb-1", "Heartbeat", heartbeat())).await;
    csms.wait_for_call("Heartbeat", STEP)
        .await
        .expect("first heartbeat recorded");
    let reply = result_payload(cp_call(&mut cp, "hb-2", "Heartbeat", heartbeat()).await);
    assert!(reply["currentTime"].is_string());
    assert_eq!(csms.faults_remaining(), 0);
}

#[tokio::test]
async fn injected_close_drops_the_socket() {
    let csms = start().await;
    let mut cp = connect_cp(&csms, "CP-001").await;
    csms.inject(Fault::close("Heartbeat"));

    send_frame(&mut cp, &Frame::call("hb-1", "Heartbeat", heartbeat())).await;
    assert!(wait_closed(&mut cp).await, "socket should close");
    csms.wait_for_disconnect(STEP)
        .await
        .expect("server side closed");

    let mut again = connect_cp(&csms, "CP-001").await;
    result_payload(cp_call(&mut again, "hb-2", "Heartbeat", heartbeat()).await);
    assert_eq!(csms.connections(), 2);
    assert_eq!(csms.open_connections(), 1);
}

#[tokio::test]
async fn injected_invalid_payload_is_sent_verbatim() {
    let csms = start().await;
    let mut cp = connect_cp(&csms, "CP-001").await;
    csms.inject(Fault::invalid_payload("Authorize", json!("not an object")));

    send_frame(
        &mut cp,
        &Frame::call("auth-1", "Authorize", json!({"idTag": "TAG1"})),
    )
    .await;
    let raw: Value = serde_json::from_str(&recv_text(&mut cp).await).expect("JSON text");
    assert_eq!(raw, json!([3, "auth-1", "not an object"]));
}

#[tokio::test]
async fn responder_overrides_default_but_faults_take_precedence() {
    let csms = start().await;
    let mut cp = connect_cp(&csms, "CP-001").await;
    csms.respond("DataTransfer", |payload| {
        if payload["vendorId"] == "Kirby" {
            Reply::Result(json!({"status": "Rejected"}))
        } else {
            Reply::Error(ErrorCode::NotSupported, "unknown vendor".to_owned())
        }
    });
    csms.inject(Fault::call_error("DataTransfer"));

    // The fault wins for one call, then the responder answers.
    let first = cp_call(
        &mut cp,
        "dt-1",
        "DataTransfer",
        json!({"vendorId": "Kirby"}),
    )
    .await;
    assert!(matches!(first, Frame::CallError { .. }), "{first:?}");

    let second = result_payload(
        cp_call(
            &mut cp,
            "dt-2",
            "DataTransfer",
            json!({"vendorId": "Kirby"}),
        )
        .await,
    );
    assert_eq!(second["status"], "Rejected");

    let third = cp_call(
        &mut cp,
        "dt-3",
        "DataTransfer",
        json!({"vendorId": "Other"}),
    )
    .await;
    match third {
        Frame::CallError {
            code, description, ..
        } => {
            assert_eq!(code, ErrorCode::NotSupported);
            assert_eq!(description, "unknown vendor");
        }
        other => panic!("expected CALLERROR, got {other:?}"),
    }
}

#[tokio::test]
async fn wait_helpers_respect_marks_and_timeouts() {
    let csms = start().await;
    let mut cp = connect_cp(&csms, "CP-001").await;

    cp_call(&mut cp, "hb-0", "Heartbeat", heartbeat()).await;
    let mark = csms.mark();
    for index in 1..=3 {
        cp_call(
            &mut cp,
            &format!("hb-{index}"),
            "Heartbeat",
            json!({"n": index}),
        )
        .await;
    }

    let after = csms
        .wait_for_call_count_after(mark, "Heartbeat", 3, |_| true, STEP)
        .await
        .expect("three heartbeats after mark");
    assert_eq!(
        after,
        vec![json!({"n": 1}), json!({"n": 2}), json!({"n": 3})]
    );
    assert_eq!(csms.calls_since(mark).len(), 3);

    let matched = csms
        .wait_for_call_after(mark, "Heartbeat", |payload| payload["n"] == 2, STEP)
        .await
        .expect("predicate match after mark");
    assert_eq!(matched, json!({"n": 2}));

    let missing = csms
        .wait_for_call_after(mark, "Heartbeat", |payload| payload["n"] == 99, SHORT)
        .await;
    assert!(
        matches!(missing, Err(TestkitError::Timeout { .. })),
        "{missing:?}"
    );
}

#[tokio::test]
async fn csms_originated_call_roundtrip() {
    let csms = start().await;
    let mut cp = connect_cp(&csms, "CP-001").await;
    csms.wait_for_connection(STEP)
        .await
        .expect("connection open");

    let charge_point = async {
        let Frame::Call {
            id,
            action,
            payload,
        } = recv_frame(&mut cp).await
        else {
            panic!("expected a CALL from the CSMS");
        };
        assert_eq!(action, "ChangeConfiguration");
        assert_eq!(payload["key"], "HeartbeatInterval");
        send_frame(
            &mut cp,
            &Frame::call_result(id, json!({"status": "Accepted"})),
        )
        .await;
        cp
    };
    let (reply, _cp) = tokio::join!(
        csms.send_call(
            "ChangeConfiguration",
            json!({"key": "HeartbeatInterval", "value": "6"}),
        ),
        charge_point,
    );
    let reply = reply.expect("CALLRESULT");
    assert_eq!(reply.message_type(), 3);
    assert_eq!(reply.id(), "csms-1");
}

#[tokio::test]
async fn csms_call_callerror_and_expect_helpers() {
    let csms = start().await;
    let mut cp = connect_cp(&csms, "CP-001").await;
    csms.wait_for_connection(STEP)
        .await
        .expect("connection open");

    let charge_point = async {
        for reply_with in ["error", "rejected"] {
            let Frame::Call { id, .. } = recv_frame(&mut cp).await else {
                panic!("expected a CALL");
            };
            let frame = if reply_with == "error" {
                Frame::call_error(id, ErrorCode::NotSupported, "nope")
            } else {
                Frame::call_result(id, json!({"status": "Rejected"}))
            };
            send_frame(&mut cp, &frame).await;
        }
        cp
    };
    let csms_side = async {
        let error = csms
            .expect_call_error("ClearCache", json!({}), &ErrorCode::NotSupported)
            .await;
        let mismatch = csms
            .expect_status(
                "ChangeConfiguration",
                json!({"key": "X", "value": "1"}),
                "Accepted",
            )
            .await;
        (error, mismatch)
    };
    let (_cp, (error, mismatch)) = tokio::join!(charge_point, csms_side);
    error.expect("CALLERROR NotSupported matches");
    assert!(
        matches!(mismatch, Err(TestkitError::UnexpectedReply { .. })),
        "{mismatch:?}"
    );
}

#[tokio::test]
async fn csms_call_timeout_and_no_connection() {
    let csms = start().await;
    let no_connection = csms.send_call("ClearCache", json!({})).await;
    assert!(
        matches!(no_connection, Err(TestkitError::NotConnected)),
        "{no_connection:?}"
    );

    let _cp = connect_cp(&csms, "CP-001").await;
    csms.wait_for_connection(STEP)
        .await
        .expect("connection open");
    let silent = csms
        .send_call_timeout("GetConfiguration", json!({}), SHORT)
        .await;
    assert!(
        matches!(silent, Err(TestkitError::Timeout { .. })),
        "{silent:?}"
    );
}

#[tokio::test]
async fn pending_csms_call_fails_when_the_socket_closes() {
    let csms = start().await;
    let mut cp = connect_cp(&csms, "CP-001").await;
    csms.wait_for_connection(STEP)
        .await
        .expect("connection open");

    let call = csms.send_call("GetConfiguration", json!({}));
    let drop_cp = async {
        let _ = recv_frame(&mut cp).await;
        drop(cp);
    };
    let (outcome, ()) = tokio::join!(call, drop_cp);
    assert!(
        matches!(outcome, Err(TestkitError::ConnectionClosed { .. })),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn connection_recording_and_charge_point_ids() {
    let csms = start().await;
    let mut first = connect_cp(&csms, "CP-001").await;
    let mut second = connect_cp(&csms, "CP-002").await;
    csms.wait_for_connection_count(2, STEP)
        .await
        .expect("two accepted");

    assert_eq!(csms.connections(), 2);
    assert_eq!(csms.open_connections(), 2);
    let log = csms.connection_log();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].charge_point_id.as_deref(), Some("CP-001"));
    assert_eq!(log[0].path, "/ocpp/CP-001");
    assert_eq!(log[1].charge_point_id.as_deref(), Some("CP-002"));
    assert_eq!(log[0].subprotocol.as_deref(), Some("ocpp1.6"));
    assert!(log.iter().all(|info| info.open));

    cp_call(&mut second, "hb", "Heartbeat", heartbeat()).await;
    let current = csms
        .wait_for_connection(STEP)
        .await
        .expect("open connection");
    assert_eq!(current.id, 2, "CSMS calls go to the most recent connection");
    assert_eq!(csms.received()[0].connection, 2);

    assert!(wait_closed_after_drop(&mut first).await);
    assert!(wait_closed_after_drop(&mut second).await);
    csms.wait_for_disconnect(STEP).await.expect("all closed");
    assert_eq!(csms.open_connections(), 0);
    assert!(csms.connection_log().iter().all(|info| !info.open));
}

/// Closes the client side of a socket and reports that the close went through.
async fn wait_closed_after_drop(socket: &mut crate::common::PlainSocket) -> bool {
    let _ = socket.close(None).await;
    wait_closed(socket).await
}

#[tokio::test]
async fn drop_connection_closes_the_current_socket() {
    let csms = start().await;
    assert!(matches!(
        csms.drop_connection(),
        Err(TestkitError::NotConnected)
    ));

    let mut cp = connect_cp(&csms, "CP-001").await;
    csms.wait_for_connection(STEP)
        .await
        .expect("connection open");
    csms.drop_connection().expect("connection to drop");
    assert!(wait_closed(&mut cp).await);
    csms.wait_for_disconnect(STEP)
        .await
        .expect("server closed it");
}

#[tokio::test]
async fn subprotocol_policies() {
    // Default policy: ocpp1.6 is required.
    let csms = start().await;
    let refused = connect_plain(&csms.charge_point_url("CP-001"), &["ocpp2.0.1"]).await;
    assert!(
        refused.is_err(),
        "client that did not offer ocpp1.6 must be refused"
    );
    let refused_none = connect_plain(&csms.charge_point_url("CP-001"), &[]).await;
    assert!(
        refused_none.is_err(),
        "client with no subprotocol must be refused"
    );
    assert_eq!(csms.connections(), 0, "refused upgrades are not recorded");
    let (_cp, response) = connect_plain(&csms.charge_point_url("CP-001"), &["ocpp1.6"])
        .await
        .expect("ocpp1.6 accepted");
    assert_eq!(
        response
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|v| v.to_str().ok()),
        Some("ocpp1.6")
    );
    csms.shutdown().await;

    // Require a different protocol: accepted when offered, with that protocol selected.
    let csms = MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Require("ocpp2.0.1".to_owned()),
        ..CsmsOptions::default()
    })
    .await
    .expect("start");
    let (_cp, response) =
        connect_plain(&csms.charge_point_url("CP-001"), &["ocpp1.6", "ocpp2.0.1"])
            .await
            .expect("ocpp2.0.1 offered and accepted");
    assert_eq!(
        response
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|v| v.to_str().ok()),
        Some("ocpp2.0.1")
    );
    assert_eq!(
        csms.connection_log()[0].subprotocol.as_deref(),
        Some("ocpp2.0.1")
    );
    assert!(
        connect_plain(&csms.charge_point_url("CP-002"), &["ocpp1.6"])
            .await
            .is_err()
    );

    // Reject everything.
    let csms = MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Reject,
        ..CsmsOptions::default()
    })
    .await
    .expect("start");
    assert!(
        connect_plain(&csms.charge_point_url("CP-001"), &["ocpp1.6"])
            .await
            .is_err()
    );
    assert_eq!(csms.connections(), 0);

    // Select a protocol the client did not offer: the client must fail the handshake.
    let csms = MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Select("ocpp2.0.1".to_owned()),
        ..CsmsOptions::default()
    })
    .await
    .expect("start");
    assert!(
        connect_plain(&csms.charge_point_url("CP-001"), &["ocpp1.6"])
            .await
            .is_err()
    );

    // Omit the subprotocol: the client, which asked for one, must fail the handshake.
    let csms = MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Omit,
        ..CsmsOptions::default()
    })
    .await
    .expect("start");
    assert!(
        connect_plain(&csms.charge_point_url("CP-001"), &["ocpp1.6"])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn shutdown_and_drop_close_connections() {
    let csms = start().await;
    let url = csms.charge_point_url("CP-001");
    let mut cp = connect_cp(&csms, "CP-001").await;
    csms.wait_for_connection(STEP)
        .await
        .expect("connection open");
    csms.shutdown().await;
    assert!(wait_closed(&mut cp).await, "shutdown closes open sockets");
    assert!(
        connect_plain(&url, &["ocpp1.6"]).await.is_err(),
        "listener is gone"
    );
    csms.shutdown().await; // idempotent

    let csms = start().await;
    let mut cp = connect_cp(&csms, "CP-001").await;
    csms.wait_for_connection(STEP)
        .await
        .expect("connection open");
    drop(csms);
    assert!(
        wait_closed(&mut cp).await,
        "dropping the handle closes sockets"
    );
}

#[test]
fn handle_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MockCsms>();
}

#[tokio::test]
async fn plain_server_has_no_tls_material_and_ws_url() {
    let csms = start().await;
    assert!(csms.tls_material().is_none());
    assert!(csms.url().starts_with("ws://127.0.0.1:"));
    assert!(csms.url().ends_with("/ocpp"));
    assert_eq!(
        csms.charge_point_url("CP-9"),
        format!("{}/CP-9", csms.url())
    );
    assert_eq!(csms.addr().ip().to_string(), "127.0.0.1");
}
