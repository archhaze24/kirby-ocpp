//! Engine tests with real `kirby-ocpp-core` stations as clients, plus a raw WebSocket client
//! for protocol edge cases. Every test owns its engine and temp dirs; waits are event driven.

#![allow(
    clippy::match_wild_err_arm,
    reason = "a timeout is the only error these waits can produce"
)]

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use kirby_csms_core::{
    CsmsConfig, CsmsError, CsmsEvent, CsmsHandle, CsmsServer, Direction, DisconnectReason,
    ErrorCode, Fault, Frame, Reply,
};
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::types::RegistrationStatus;
use kirby_ocpp_core::{Station, StationConfig, StationHandle, SystemClock};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const STEP: Duration = Duration::from_secs(8);

async fn start(tweak: impl FnOnce(&mut CsmsConfig)) -> CsmsHandle {
    let mut config = CsmsConfig::new("127.0.0.1:0".parse().unwrap());
    tweak(&mut config);
    CsmsServer::start(config).await.expect("engine starts")
}

// ---- real station client -------------------------------------------------------------

struct Cp {
    handle: StationHandle,
    join: JoinHandle<()>,
    _dir: TempDir,
}

impl Cp {
    /// Spawns a station (no reconnect) for `<csms url><prefix>/<id>` and connects it.
    async fn connect(csms: &CsmsHandle, prefix: &str, id: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut config = StationConfig::new(format!("{}{prefix}", csms.url()))
            .with_charge_point_id(id)
            .with_connectors(2)
            .with_call_timeout_ms(2_000)
            .with_reconnect(ReconnectConfig {
                enabled: false,
                initial_delay_ms: 30,
                max_delay_ms: 100,
                max_attempts: 0,
            })
            .with_state_dir(dir.path());
        config.ws_ping_interval_secs = 0;
        let (handle, join) = Station::spawn(config, Arc::new(SystemClock));
        handle.connect().await.expect("connect");
        Self {
            handle,
            join,
            _dir: dir,
        }
    }

    async fn wait_accepted(&self) {
        let mut rx = self.handle.watch_snapshot();
        timeout(
            STEP,
            rx.wait_for(|s| s.registration == Some(RegistrationStatus::Accepted)),
        )
        .await
        .expect("boot accepted in time")
        .expect("station alive");
    }

    async fn stop(self) {
        self.handle.shutdown().await.expect("shutdown");
        self.join.await.expect("actor ends");
    }
}

async fn wait_event(
    rx: &mut broadcast::Receiver<CsmsEvent>,
    what: &str,
    mut pred: impl FnMut(&CsmsEvent) -> bool,
) -> CsmsEvent {
    loop {
        match timeout(STEP, rx.recv()).await {
            Ok(Ok(event)) if pred(&event) => return event,
            Ok(Ok(_) | Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(broadcast::error::RecvError::Closed)) => panic!("events closed for {what}"),
            Err(_) => panic!("timed out waiting for {what}"),
        }
    }
}

fn connected(id: &str) -> impl FnMut(&CsmsEvent) -> bool + '_ {
    move |e| matches!(e, CsmsEvent::Connected { station } if station == id)
}

fn result_payload(frame: Frame) -> Value {
    match frame {
        Frame::CallResult { payload, .. } => payload,
        other => panic!("expected CALLRESULT, got {}", other.to_json_string()),
    }
}

// ---- raw client ----------------------------------------------------------------------

type Raw =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn raw_connect(csms: &CsmsHandle, path: &str) -> Raw {
    let mut request = format!("{}{path}", csms.url())
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "ocpp1.6".parse().unwrap());
    let (socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    socket
}

async fn raw_send(socket: &mut Raw, text: &str) {
    socket.send(Message::text(text)).await.unwrap();
}

async fn raw_recv(socket: &mut Raw) -> Frame {
    loop {
        match timeout(STEP, socket.next()).await.expect("reply in time") {
            Some(Ok(Message::Text(text))) => return Frame::parse(text.as_str()).unwrap(),
            Some(Ok(_)) => {}
            other => panic!("socket ended: {other:?}"),
        }
    }
}

/// Reads until the server closes the socket.
async fn raw_expect_closed(socket: &mut Raw) {
    loop {
        match timeout(STEP, socket.next()).await.expect("close in time") {
            Some(Ok(Message::Close(_)) | Err(_)) | None => return,
            Some(Ok(_)) => {}
        }
    }
}

// ---- tests ---------------------------------------------------------------------------

#[tokio::test]
async fn three_stations_connect_at_once_and_are_listed() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let cps = tokio::join!(
        Cp::connect(&csms, "/ocpp", "CP-C"),
        Cp::connect(&csms, "/ocpp", "CP-A"),
        Cp::connect(&csms, "/ocpp", "CP-B"),
    );
    // Connection order is arbitrary: collect until all three were announced.
    let mut announced = std::collections::BTreeSet::new();
    while announced.len() < 3 {
        if let CsmsEvent::Connected { station } = wait_event(&mut events, "connects", |e| {
            matches!(e, CsmsEvent::Connected { .. })
        })
        .await
        {
            announced.insert(station);
        }
    }
    assert_eq!(announced.len(), 3);
    for cp in [&cps.0, &cps.1, &cps.2] {
        cp.wait_accepted().await;
    }

    let stations = csms.stations();
    let ids: Vec<&str> = stations.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["CP-A", "CP-B", "CP-C"]);
    for info in &stations {
        assert!(info.connected);
        assert!(info.connected_at.is_some());
        assert!(info.remote_addr.is_some());
        assert_eq!(info.subprotocol.as_deref(), Some("ocpp1.6"));
        assert!(info.received_calls >= 1);
        assert_eq!(info.received_by_action["BootNotification"], 1);
    }
    assert_eq!(csms.station("CP-B").unwrap().id, "CP-B");
    assert!(csms.station("CP-Z").is_none());

    tokio::join!(cps.0.stop(), cps.1.stop(), cps.2.stop());
    csms.shutdown().await;
}

#[tokio::test]
async fn id_is_the_last_path_segment_behind_any_prefix() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let cp = Cp::connect(&csms, "/some/deep/prefix", "CP-PREFIXED").await;
    wait_event(&mut events, "connect", connected("CP-PREFIXED")).await;
    assert_eq!(csms.stations().len(), 1);

    let mut raw = raw_connect(&csms, "/x/CP%2DENCODED").await;
    wait_event(&mut events, "raw connect", connected("CP-ENCODED")).await;
    raw.close(None).await.unwrap();

    cp.stop().await;
    csms.shutdown().await;
}

#[tokio::test]
async fn duplicate_id_replaces_the_old_connection() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let mut first = raw_connect(&csms, "/ocpp/CP-DUP").await;
    wait_event(&mut events, "first connect", connected("CP-DUP")).await;

    let second = Cp::connect(&csms, "/ocpp", "CP-DUP").await;
    let gone = wait_event(&mut events, "replacement", |e| {
        matches!(e, CsmsEvent::Disconnected { .. })
    })
    .await;
    assert_eq!(
        gone,
        CsmsEvent::Disconnected {
            station: "CP-DUP".into(),
            reason: DisconnectReason::Replaced
        }
    );
    // The new connection is announced after the old one ended.
    wait_event(&mut events, "second connect", connected("CP-DUP")).await;
    raw_expect_closed(&mut first).await;

    second.wait_accepted().await;
    assert_eq!(csms.stations().len(), 1);
    assert!(csms.station("CP-DUP").unwrap().connected);
    // Calls reach the new connection.
    let reply = csms
        .call(
            "CP-DUP",
            "GetConfiguration",
            json!({"key": ["HeartbeatInterval"]}),
        )
        .await
        .unwrap();
    assert!(matches!(reply, Frame::CallResult { .. }));

    second.stop().await;
    csms.shutdown().await;
}

#[tokio::test]
async fn frame_history_respects_the_cap() {
    let csms = start(|config| config.history_cap = 5).await;
    let mut events = csms.subscribe();
    let cp = Cp::connect(&csms, "/ocpp", "CP-RING").await;
    // Boot + 3 StatusNotifications (connector 0 and two connectors), each answered: 8 frames.
    let mut seen = Vec::new();
    while seen.len() < 8 {
        let event = wait_event(&mut events, "frames", |e| {
            matches!(e, CsmsEvent::Frame { .. })
        })
        .await;
        if let CsmsEvent::Frame { frame, .. } = event {
            seen.push(frame);
        }
    }
    let history = csms.history("CP-RING").unwrap();
    assert_eq!(history.len(), 5);
    let kept: Vec<&Frame> = history.iter().map(|e| &e.frame).collect();
    let expected: Vec<&Frame> = seen[seen.len() - 5..].iter().collect();
    assert_eq!(kept, expected);
    // Counters are not capped.
    assert!(csms.station("CP-RING").unwrap().received_calls >= 4);
    assert!(csms.history("nobody").is_none());

    cp.stop().await;
    csms.shutdown().await;
}

#[tokio::test]
async fn call_reaches_the_station_and_returns_its_real_reply() {
    let csms = start(|_| {}).await;
    let cp = Cp::connect(&csms, "/ocpp", "CP-CFG").await;
    cp.wait_accepted().await;

    let changed = csms
        .call(
            "CP-CFG",
            "ChangeConfiguration",
            json!({"key": "HeartbeatInterval", "value": "42"}),
        )
        .await
        .unwrap();
    assert_eq!(result_payload(changed)["status"], "Accepted");

    let got = csms
        .call(
            "CP-CFG",
            "GetConfiguration",
            json!({"key": ["HeartbeatInterval"]}),
        )
        .await
        .unwrap();
    let payload = result_payload(got);
    assert_eq!(payload["configurationKey"][0]["key"], "HeartbeatInterval");
    assert_eq!(payload["configurationKey"][0]["value"], "42");

    // The history shows both exchanges as Out CALL + In reply.
    let history = csms.history("CP-CFG").unwrap();
    let out_calls = history
        .iter()
        .filter(|e| e.direction == Direction::Out && e.frame.action().is_some())
        .count();
    assert_eq!(out_calls, 2);

    // A bad payload comes back as the station's CALLERROR, not as an engine error.
    let error = csms
        .call("CP-CFG", "ChangeConfiguration", json!({"key": 5}))
        .await
        .unwrap();
    assert!(matches!(error, Frame::CallError { .. }), "{error:?}");

    cp.stop().await;
    csms.shutdown().await;
}

#[tokio::test]
async fn concurrent_calls_are_serialised_per_station() {
    let csms = start(|_| {}).await;
    let cp = Cp::connect(&csms, "/ocpp", "CP-Q").await;
    cp.wait_accepted().await;
    let calls = (0..6).map(|n| {
        let csms = csms.clone();
        tokio::spawn(async move {
            csms.call(
                "CP-Q",
                "ChangeConfiguration",
                json!({"key": "HeartbeatInterval", "value": (10 + n).to_string()}),
            )
            .await
        })
    });
    for call in calls {
        assert!(matches!(
            call.await.unwrap().unwrap(),
            Frame::CallResult { .. }
        ));
    }
    // At most one CSMS CALL was outstanding: every Out CALL is answered before the next.
    let mut outstanding = 0_i32;
    for entry in csms.history("CP-Q").unwrap() {
        match (entry.direction, &entry.frame) {
            (Direction::Out, Frame::Call { .. }) => {
                outstanding += 1;
                assert!(outstanding <= 1, "two CALLs in flight");
            }
            (Direction::In, Frame::CallResult { .. } | Frame::CallError { .. }) => outstanding -= 1,
            _ => {}
        }
    }
    cp.stop().await;
    csms.shutdown().await;
}

#[tokio::test]
async fn call_errors_for_unknown_and_disconnected_stations() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    assert!(matches!(
        csms.call("ghost", "Reset", json!({"type": "Soft"})).await,
        Err(CsmsError::UnknownStation { .. })
    ));

    let cp = Cp::connect(&csms, "/ocpp", "CP-GONE").await;
    wait_event(&mut events, "connect", connected("CP-GONE")).await;
    cp.stop().await;
    wait_event(&mut events, "disconnect", |e| {
        matches!(e, CsmsEvent::Disconnected { .. })
    })
    .await;

    // Stays in the registry, disconnected.
    let info = csms.station("CP-GONE").unwrap();
    assert!(!info.connected);
    assert!(info.disconnected_at.is_some());
    assert!(matches!(
        csms.call("CP-GONE", "Reset", json!({"type": "Soft"})).await,
        Err(CsmsError::NotConnected { .. })
    ));

    assert!(csms.forget("CP-GONE"));
    assert!(!csms.forget("CP-GONE"));
    assert_eq!(csms.stations().len(), 0);
    assert!(matches!(
        csms.call("CP-GONE", "Reset", json!({})).await,
        Err(CsmsError::UnknownStation { .. })
    ));
    csms.shutdown().await;
}

#[tokio::test]
async fn call_times_out_and_the_next_call_still_goes_out() {
    let csms = start(|_| {}).await;
    let mut raw = raw_connect(&csms, "/ocpp/CP-SLOW").await;
    let first = tokio::spawn({
        let csms = csms.clone();
        async move {
            csms.call_timeout(
                "CP-SLOW",
                "Reset",
                json!({"type": "Soft"}),
                Duration::from_millis(100),
            )
            .await
        }
    });
    // The raw client reads the CALL and never answers.
    let Frame::Call { .. } = raw_recv(&mut raw).await else {
        panic!("expected a CALL");
    };
    assert!(matches!(
        first.await.unwrap(),
        Err(CsmsError::Timeout { .. })
    ));

    // The queue moves on: the next CALL is written and can be answered.
    let next = tokio::spawn({
        let csms = csms.clone();
        async move { csms.call("CP-SLOW", "GetConfiguration", json!({})).await }
    });
    let Frame::Call { id, action, .. } = raw_recv(&mut raw).await else {
        panic!("expected a CALL");
    };
    assert_eq!(action, "GetConfiguration");
    raw_send(
        &mut raw,
        &Frame::call_result(id, json!({})).to_json_string(),
    )
    .await;
    assert!(matches!(
        next.await.unwrap().unwrap(),
        Frame::CallResult { .. }
    ));
    csms.shutdown().await;
}

#[tokio::test]
async fn rules_can_be_listed_and_cleared() {
    let csms = start(|_| {}).await;
    assert_eq!(csms.responder_actions(), Vec::<String>::new());
    csms.respond("Heartbeat", |_| Reply::Silence);
    csms.respond("Authorize", |_| Reply::Silence);
    csms.inject(Fault::close("Heartbeat").times(3));
    assert_eq!(csms.responder_actions(), ["Authorize", "Heartbeat"]);
    assert_eq!(csms.faults_remaining(), 3);
    csms.clear_faults();
    assert!(csms.clear_responder("Authorize"));
    assert!(!csms.clear_responder("Authorize"));
    assert_eq!(csms.faults_remaining(), 0);
    assert_eq!(csms.responder_actions(), ["Heartbeat"]);
    csms.shutdown().await;
}

#[tokio::test]
async fn faults_responders_and_protocol_errors() {
    let csms = start(|_| {}).await;
    csms.inject(Fault::call_error("Heartbeat"));
    csms.respond("DataTransfer", |_| {
        Reply::Result(json!({"status": "Rejected"}))
    });
    assert_eq!(csms.faults_remaining(), 1);

    // Faults and responders apply to every station.
    for id in ["CP-1", "CP-2"] {
        let mut raw = raw_connect(&csms, &format!("/ocpp/{id}")).await;
        raw_send(&mut raw, r#"[2,"d1","DataTransfer",{"vendorId":"x"}]"#).await;
        assert_eq!(
            result_payload(raw_recv(&mut raw).await),
            json!({"status": "Rejected"})
        );
        raw.close(None).await.unwrap();
    }

    let mut raw = raw_connect(&csms, "/ocpp/CP-3").await;
    raw_send(&mut raw, r#"[2,"h1","Heartbeat",{}]"#).await;
    match raw_recv(&mut raw).await {
        Frame::CallError { id, code, .. } => {
            assert_eq!(id, "h1");
            assert_eq!(code, ErrorCode::InternalError);
        }
        other => panic!("expected CALLERROR, got {other:?}"),
    }
    assert_eq!(csms.faults_remaining(), 0);
    raw_send(&mut raw, r#"[2,"h2","Heartbeat",{}]"#).await;
    assert!(result_payload(raw_recv(&mut raw).await)["currentTime"].is_string());

    // Magic idTag and transaction id policy (fixed 123 by default).
    raw_send(
        &mut raw,
        r#"[2,"s1","StartTransaction",{"connectorId":1,"idTag":"BLOCKSTART","meterStart":0,"timestamp":"2026-01-01T00:00:00Z"}]"#,
    )
    .await;
    let started = result_payload(raw_recv(&mut raw).await);
    assert_eq!(started["transactionId"], 123);
    assert_eq!(started["idTagInfo"]["status"], "Blocked");

    // Unknown action: NotImplemented. A known action a charge point may not send: NotSupported.
    raw_send(&mut raw, r#"[2,"u1","Teleport",{}]"#).await;
    match raw_recv(&mut raw).await {
        Frame::CallError { code, .. } => assert_eq!(code, ErrorCode::NotImplemented),
        other => panic!("expected CALLERROR, got {other:?}"),
    }
    raw_send(&mut raw, r#"[2,"u2","Reset",{"type":"Soft"}]"#).await;
    match raw_recv(&mut raw).await {
        Frame::CallError { code, .. } => assert_eq!(code, ErrorCode::NotSupported),
        other => panic!("expected CALLERROR, got {other:?}"),
    }

    // Malformed frames never kill the connection.
    raw_send(&mut raw, "this is not json").await;
    raw_send(&mut raw, r#"[2,"m1"]"#).await; // readable id: FormationViolation
    match raw_recv(&mut raw).await {
        Frame::CallError { id, code, .. } => {
            assert_eq!(id, "m1");
            assert_eq!(code, ErrorCode::FormationViolation);
        }
        other => panic!("expected CALLERROR, got {other:?}"),
    }
    raw_send(&mut raw, r#"{"a":1}"#).await; // no id: dropped
    raw_send(&mut raw, r#"[2,"h3","Heartbeat",{}]"#).await;
    match raw_recv(&mut raw).await {
        Frame::CallResult { id, .. } => assert_eq!(id, "h3"),
        other => panic!("expected CALLRESULT, got {other:?}"),
    }

    // A close fault ends the connection with a close frame.
    csms.inject(Fault::close("MeterValues"));
    raw_send(
        &mut raw,
        r#"[2,"v1","MeterValues",{"connectorId":1,"meterValue":[]}]"#,
    )
    .await;
    raw_expect_closed(&mut raw).await;
    csms.shutdown().await;
}

#[tokio::test]
async fn handshake_refusals() {
    let csms = start(|_| {}).await;
    // No subprotocol offered: refused by the default Require policy.
    let request = format!("{}/ocpp/CP-NOPROTO", csms.url())
        .into_client_request()
        .unwrap();
    assert!(tokio_tungstenite::connect_async(request).await.is_err());
    // No station id in the path.
    let mut request = format!("{}/", csms.url()).into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "ocpp1.6".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(request).await.is_err());
    assert_eq!(csms.stations().len(), 0);
    csms.shutdown().await;
}

#[tokio::test]
async fn events_arrive_in_order_connected_frames_disconnected() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let cp = Cp::connect(&csms, "/ocpp", "CP-EV").await;
    cp.wait_accepted().await;
    cp.stop().await;

    let mut collected = Vec::new();
    loop {
        let event = wait_event(&mut events, "events", |_| true).await;
        let done = matches!(event, CsmsEvent::Disconnected { .. });
        collected.push(event);
        if done {
            break;
        }
    }
    assert_eq!(
        collected.first(),
        Some(&CsmsEvent::Connected {
            station: "CP-EV".into()
        })
    );
    assert_eq!(
        collected.last(),
        Some(&CsmsEvent::Disconnected {
            station: "CP-EV".into(),
            reason: DisconnectReason::Closed
        })
    );
    let frames: Vec<_> = collected[1..collected.len() - 1]
        .iter()
        .filter(|event| !matches!(event, CsmsEvent::StateChanged { .. }))
        .map(|event| match event {
            CsmsEvent::Frame {
                station,
                direction,
                frame,
            } => {
                assert_eq!(station, "CP-EV");
                (*direction, frame.clone())
            }
            other => panic!("unexpected event between connect and disconnect: {other:?}"),
        })
        .collect();
    assert!(matches!(
        frames.first(),
        Some((Direction::In, Frame::Call { action, .. })) if action == "BootNotification"
    ));
    assert!(matches!(
        frames.get(1),
        Some((Direction::Out, Frame::CallResult { .. }))
    ));
    csms.shutdown().await;
}

#[tokio::test]
async fn shutdown_closes_everything_promptly() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let mut a = raw_connect(&csms, "/ocpp/CP-S1").await;
    let mut b = raw_connect(&csms, "/ocpp/CP-S2").await;
    wait_event(&mut events, "s1", connected("CP-S1")).await;
    wait_event(&mut events, "s2", connected("CP-S2")).await;

    // A pending call must fail, not hang.
    let pending = tokio::spawn({
        let csms = csms.clone();
        async move { csms.call("CP-S1", "Reset", json!({"type": "Soft"})).await }
    });
    let Frame::Call { .. } = raw_recv(&mut a).await else {
        panic!("expected a CALL");
    };

    timeout(Duration::from_secs(5), csms.shutdown())
        .await
        .expect("shutdown returns promptly");
    raw_expect_closed(&mut a).await;
    raw_expect_closed(&mut b).await;
    assert!(matches!(
        pending.await.unwrap(),
        Err(CsmsError::ConnectionClosed { .. })
    ));
    assert!(csms.stations().iter().all(|s| !s.connected));
    wait_event(&mut events, "shutdown disconnect", |e| {
        matches!(
            e,
            CsmsEvent::Disconnected {
                reason: DisconnectReason::Shutdown,
                ..
            }
        )
    })
    .await;
    // Idempotent, and new connections are refused.
    csms.shutdown().await;
    assert!(
        tokio::net::TcpStream::connect(csms.local_addr())
            .await
            .is_err()
    );
}

// ---- hardening ---------------------------------------------------------------------

#[tokio::test]
async fn huge_timeout_does_not_panic() {
    let csms = start(|_| {}).await;
    let mut raw = raw_connect(&csms, "/CP-BIG").await;
    let call = tokio::spawn({
        let csms = csms.clone();
        async move {
            csms.call_timeout("CP-BIG", "Reset", json!({}), Duration::MAX)
                .await
        }
    });
    let Frame::Call { id, .. } = raw_recv(&mut raw).await else {
        panic!("expected a CALL");
    };
    raw_send(
        &mut raw,
        &Frame::call_result(id, json!({})).to_json_string(),
    )
    .await;
    assert!(matches!(
        call.await.unwrap().unwrap(),
        Frame::CallResult { .. }
    ));
    csms.shutdown().await;
}

#[tokio::test]
async fn panicking_responder_ends_the_session() {
    let csms = start(|_| {}).await;
    csms.respond("Authorize", |_| panic!("responder bug"));
    let mut raw = raw_connect(&csms, "/CP-PANIC").await;
    raw_send(&mut raw, r#"[2,"a1","Authorize",{"idTag":"X"}]"#).await;
    raw_expect_closed(&mut raw).await;
    let info = timeout(STEP, async {
        loop {
            match csms.station("CP-PANIC") {
                Some(info) if !info.connected => return info,
                _ => tokio::task::yield_now().await,
            }
        }
    })
    .await
    .expect("session ended in the registry");
    assert!(info.disconnected_at.is_some());
    csms.shutdown().await;
}

#[tokio::test]
async fn oversize_message_closes_the_connection() {
    let csms = start(|c| c.max_message_size = 1024).await;
    let mut raw = raw_connect(&csms, "/CP-FAT").await;
    let pad = "x".repeat(4096);
    raw_send(
        &mut raw,
        &json!([2, "f1", "Authorize", {"idTag": pad}]).to_string(),
    )
    .await;
    raw_expect_closed(&mut raw).await;
    csms.shutdown().await;
}

#[tokio::test]
async fn unknown_actions_share_one_bucket() {
    let csms = start(|_| {}).await;
    let mut raw = raw_connect(&csms, "/CP-MANY").await;
    for n in 0..80 {
        raw_send(
            &mut raw,
            &json!([2, format!("m{n}"), format!("Act{n}"), {}]).to_string(),
        )
        .await;
    }
    for _ in 0..80 {
        raw_recv(&mut raw).await;
    }
    let info = csms.station("CP-MANY").unwrap();
    assert_eq!(info.received_calls, 80);
    assert!(info.received_by_action.len() <= 65, "{info:?}");
    assert_eq!(info.received_by_action.get("other"), Some(&16));
    csms.shutdown().await;
}

#[tokio::test]
async fn station_limit_refuses_new_ids_only() {
    let csms = start(|c| c.max_stations = 1).await;
    let first = raw_connect(&csms, "/CP-A").await;
    let mut request = format!("{}/CP-B", csms.url())
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "ocpp1.6".parse().unwrap());
    match tokio_tungstenite::connect_async(request).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status().as_u16(), 503);
        }
        other => panic!("expected a 503 refusal, got {other:?}"),
    }
    // The known id can reconnect (and replaces the old socket).
    let _again = raw_connect(&csms, "/CP-A").await;
    drop(first);
    assert_eq!(csms.stations().len(), 1);
    csms.shutdown().await;
}

#[tokio::test]
async fn heartbeat_updates_state_without_an_event() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let mut raw = raw_connect(&csms, "/CP-HB").await;
    raw_send(&mut raw, r#"[2,"h1","Heartbeat",{}]"#).await;
    raw_recv(&mut raw).await;
    // A later status change arrives; no StateChanged may precede it from the heartbeat.
    raw_send(
        &mut raw,
        r#"[2,"s1","StatusNotification",{"connectorId":1,"errorCode":"NoError","status":"Available"}]"#,
    )
    .await;
    raw_recv(&mut raw).await;
    let mut changed = 0;
    wait_event(&mut events, "status state change", |e| {
        if matches!(e, CsmsEvent::StateChanged { .. }) {
            changed += 1;
            return true;
        }
        false
    })
    .await;
    assert_eq!(changed, 1);
    assert!(csms.state("CP-HB").unwrap().last_heartbeat_at.is_some());
    csms.shutdown().await;
}

#[tokio::test]
async fn dropped_call_frees_the_queue_and_its_late_answer_is_ignored() {
    // The first call has a one hour timeout; once its caller is gone the session waits at most
    // the default call timeout (400 ms here) for it.
    let csms = start(|c| c.call_timeout = Duration::from_millis(400)).await;
    let mut raw = raw_connect(&csms, "/CP-DROP").await;
    let first = tokio::spawn({
        let csms = csms.clone();
        async move {
            csms.call_timeout(
                "CP-DROP",
                "Reset",
                json!({"type": "Soft"}),
                Duration::from_secs(3600),
            )
            .await
        }
    });
    let Frame::Call { id: first_id, .. } = raw_recv(&mut raw).await else {
        panic!("expected a CALL");
    };
    first.abort();
    let _ = first.await;
    let next = tokio::spawn({
        let csms = csms.clone();
        async move {
            csms.call_timeout(
                "CP-DROP",
                "GetConfiguration",
                json!({}),
                Duration::from_secs(30),
            )
            .await
        }
    });
    // The abandoned CALL stops blocking the queue after the default timeout, though the
    // station never answered it.
    let Frame::Call { id, action, .. } = raw_recv(&mut raw).await else {
        panic!("expected a CALL");
    };
    assert_eq!(action, "GetConfiguration");
    // The late answer to the abandoned CALL does not settle the new one.
    raw_send(
        &mut raw,
        &Frame::call_result(first_id, json!({"status": "Accepted"})).to_json_string(),
    )
    .await;
    raw_send(
        &mut raw,
        &Frame::call_result(id, json!({"configurationKey": []})).to_json_string(),
    )
    .await;
    let answer = next.await.unwrap().unwrap();
    assert_eq!(
        result_payload(answer),
        json!({"configurationKey": []}),
        "the late answer settled the wrong call"
    );
    csms.shutdown().await;
}

#[tokio::test]
async fn call_timeout_covers_the_time_spent_queued() {
    let csms = start(|_| {}).await;
    let mut raw = raw_connect(&csms, "/CP-QTIME").await;
    // The first call occupies the station for a long time (it is never answered).
    let first = tokio::spawn({
        let csms = csms.clone();
        async move {
            csms.call_timeout(
                "CP-QTIME",
                "Reset",
                json!({"type": "Soft"}),
                Duration::from_secs(600),
            )
            .await
        }
    });
    let Frame::Call { .. } = raw_recv(&mut raw).await else {
        panic!("expected a CALL");
    };
    let started = tokio::time::Instant::now();
    let queued = csms
        .call_timeout(
            "CP-QTIME",
            "GetConfiguration",
            json!({}),
            Duration::from_millis(300),
        )
        .await;
    assert!(
        matches!(queued, Err(CsmsError::Timeout { .. })),
        "{queued:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the queued call waited for the outstanding one"
    );
    // The expired call was never written.
    assert!(
        timeout(Duration::from_millis(200), raw.next())
            .await
            .is_err(),
        "an expired queued call was written"
    );
    first.abort();
    csms.shutdown().await;
}

#[tokio::test]
async fn history_is_capped_in_bytes_and_huge_frames_are_truncated() {
    let csms = start(|c| c.history_bytes_cap = 2000).await;
    let mut raw = raw_connect(&csms, "/CP-BYTES").await;
    for n in 0..20 {
        let call = json!([2, format!("d{n}"), "DataTransfer",
            {"vendorId": "V", "data": "x".repeat(300)}]);
        raw_send(&mut raw, &call.to_string()).await;
        raw_recv(&mut raw).await;
    }
    // A frame far above the cap is kept as a marker, not in full.
    let huge = json!([2, "huge", "DataTransfer", {"vendorId": "V", "data": "y".repeat(100_000)}]);
    raw_send(&mut raw, &huge.to_string()).await;
    raw_recv(&mut raw).await;
    let history = csms.history("CP-BYTES").unwrap();
    let total: usize = history.iter().map(|e| e.frame.to_json_string().len()).sum();
    assert!(total <= 2000, "history holds {total} bytes");
    assert!(
        history.len() >= 2 && history.len() < 40,
        "{}",
        history.len()
    );
    let marked = history
        .iter()
        .find_map(|e| match &e.frame {
            Frame::Call { id, payload, .. } if id.as_str() == "huge" => Some(payload.clone()),
            _ => None,
        })
        .expect("the huge CALL is in the history");
    assert_eq!(marked["truncated"], true);
    assert!(marked["originalBytes"].as_u64().unwrap() > 100_000);
    csms.shutdown().await;
}

#[tokio::test]
async fn long_action_names_are_counted_as_other() {
    let csms = start(|_| {}).await;
    let mut raw = raw_connect(&csms, "/CP-LONG").await;
    let long = "A".repeat(10_000);
    raw_send(&mut raw, &json!([2, "l1", long, {}]).to_string()).await;
    raw_recv(&mut raw).await;
    raw_send(&mut raw, &json!([2, "l2", "Heartbeat", {}]).to_string()).await;
    raw_recv(&mut raw).await;
    let info = csms.station("CP-LONG").unwrap();
    assert_eq!(info.received_by_action.get("other"), Some(&1));
    assert_eq!(info.received_by_action.get("Heartbeat"), Some(&1));
    assert!(
        info.received_by_action
            .keys()
            .all(|k| k.chars().count() <= 64)
    );
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_handshakes_cannot_exceed_the_station_limit() {
    let csms = start(|c| c.max_stations = 2).await;
    let mut tasks = Vec::new();
    for n in 0..16 {
        let url = csms.url().clone();
        tasks.push(tokio::spawn(async move {
            let mut request = format!("{url}/CP-RACE-{n}").into_client_request().unwrap();
            request
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", "ocpp1.6".parse().unwrap());
            tokio_tungstenite::connect_async(request).await.ok()
        }));
    }
    let mut sockets = Vec::new();
    for task in tasks {
        sockets.push(task.await.unwrap());
    }
    // Wait until every attempt has been settled by the server (kept or closed).
    for socket in sockets.iter_mut().flatten() {
        let _ = timeout(Duration::from_millis(500), socket.0.next()).await;
    }
    assert!(
        csms.stations().len() <= 2,
        "{} stations",
        csms.stations().len()
    );
    csms.shutdown().await;
}

#[tokio::test]
async fn stalled_reader_is_dropped_after_the_write_timeout() {
    let csms = start(|c| c.write_timeout = Duration::from_millis(300)).await;
    // Big replies to CALLs that the client never reads. One write may vanish into the
    // kernel (Windows accepts a large non-blocking send whole), but a later one cannot.
    csms.respond("DataTransfer", |_| {
        Reply::Result(json!({"status": "Accepted", "data": "x".repeat(32 * 1024 * 1024)}))
    });
    let mut events = csms.subscribe();
    let mut raw = raw_connect(&csms, "/CP-STALL").await;
    wait_event(&mut events, "connect", connected("CP-STALL")).await;
    for n in 0..4 {
        let call = format!(r#"[2,"s{n}","DataTransfer",{{"vendorId":"V"}}]"#);
        if raw.send(Message::text(call)).await.is_err() {
            break;
        }
    }
    let gone = wait_event(&mut events, "write timeout", |e| {
        matches!(e, CsmsEvent::Disconnected { .. })
    })
    .await;
    assert_eq!(
        gone,
        CsmsEvent::Disconnected {
            station: "CP-STALL".into(),
            reason: DisconnectReason::Closed
        }
    );
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replaced_session_cannot_write_into_the_record() {
    let csms = start(|_| {}).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = std::sync::Mutex::new(release_rx);
    csms.respond("BootNotification", {
        let entered = Arc::clone(&entered);
        move |_| {
            entered.notify_one();
            // block_in_place hands the worker's queue (accept loop!) to another thread.
            tokio::task::block_in_place(|| {
                let _ = release_rx.lock().unwrap().recv();
            });
            Reply::Result(json!({
                "status": "Accepted",
                "currentTime": "2026-01-01T00:00:00Z",
                "interval": 60
            }))
        }
    });
    let mut old = raw_connect(&csms, "/CP-STALE").await;
    raw_send(
        &mut old,
        r#"[2,"b1","BootNotification",{"chargePointVendor":"V","chargePointModel":"M"}]"#,
    )
    .await;
    timeout(STEP, entered.notified())
        .await
        .expect("responder entered");
    let mut events = csms.subscribe();
    let _new = raw_connect(&csms, "/CP-STALE").await;
    wait_event(&mut events, "replacement", connected("CP-STALE")).await;
    release_tx.send(()).unwrap();
    raw_expect_closed(&mut old).await;
    let state = csms.state("CP-STALE").unwrap();
    assert!(state.boot.is_none(), "stale session wrote state: {state:?}");
    let history = csms.history("CP-STALE").unwrap();
    assert!(
        history
            .iter()
            .all(|e| e.direction == kirby_csms_core::Direction::In),
        "stale session wrote an outbound frame"
    );
    csms.shutdown().await;
}

// ---- inbound validation, ping watchdog, installed certificates -------------------------

#[tokio::test]
async fn invalid_inbound_calls_get_a_callerror_unless_validation_is_off() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let mut raw = raw_connect(&csms, "/ocpp/CP-V").await;
    wait_event(&mut events, "connect", connected("CP-V")).await;

    // Missing required property: CALLERROR, recorded, not tracked.
    raw_send(
        &mut raw,
        r#"[2,"b1","BootNotification",{"chargePointModel":"M"}]"#,
    )
    .await;
    match raw_recv(&mut raw).await {
        Frame::CallError { id, code, .. } => {
            assert_eq!(id, "b1");
            assert_eq!(code, ErrorCode::OccurenceConstraintViolation);
        }
        other => panic!("expected CALLERROR, got {other:?}"),
    }
    wait_event(&mut events, "CALLERROR frame", |e| {
        matches!(
            e,
            CsmsEvent::Frame {
                direction: Direction::Out,
                frame: Frame::CallError { .. },
                ..
            }
        )
    })
    .await;
    assert!(csms.state("CP-V").is_some_and(|s| s.boot.is_none()));
    assert_eq!(csms.history("CP-V").map_or(0, |h| h.len()), 2);

    // Wrong type.
    raw_send(&mut raw, r#"[2,"s1","StatusNotification",{"connectorId":"x","status":"Available","errorCode":"NoError"}]"#).await;
    match raw_recv(&mut raw).await {
        Frame::CallError { code, .. } => assert_eq!(code, ErrorCode::TypeConstraintViolation),
        other => panic!("expected CALLERROR, got {other:?}"),
    }

    // A valid CALL still works.
    raw_send(&mut raw, r#"[2,"h1","Heartbeat",{}]"#).await;
    assert!(result_payload(raw_recv(&mut raw).await)["currentTime"].is_string());

    // A scripted fault on the action wins over validation.
    csms.inject(Fault::call_error("BootNotification"));
    raw_send(&mut raw, r#"[2,"b2","BootNotification",{}]"#).await;
    match raw_recv(&mut raw).await {
        Frame::CallError { code, .. } => assert_eq!(code, ErrorCode::InternalError),
        other => panic!("expected CALLERROR, got {other:?}"),
    }
    assert_eq!(csms.faults_remaining(), 0);
    csms.shutdown().await;

    let lax = start(|c| c.validate_inbound = false).await;
    let mut raw = raw_connect(&lax, "/ocpp/CP-V").await;
    raw_send(
        &mut raw,
        r#"[2,"b1","BootNotification",{"chargePointModel":"M"}]"#,
    )
    .await;
    assert!(result_payload(raw_recv(&mut raw).await)["status"].is_string());
    lax.shutdown().await;
}

#[tokio::test]
async fn silent_station_is_dropped_by_the_ping_watchdog() {
    let csms = start(|c| {
        c.ping_interval = Duration::from_millis(60);
        c.pong_timeout = Duration::from_millis(120);
    })
    .await;
    let mut events = csms.subscribe();
    // Never polled after the handshake: tungstenite answers pings only while it is read.
    let _raw = raw_connect(&csms, "/ocpp/CP-W").await;
    wait_event(&mut events, "connect", connected("CP-W")).await;
    let gone = wait_event(&mut events, "ping timeout", |e| {
        matches!(e, CsmsEvent::Disconnected { .. })
    })
    .await;
    assert_eq!(
        gone,
        CsmsEvent::Disconnected {
            station: "CP-W".to_owned(),
            reason: DisconnectReason::PingTimeout
        }
    );
    csms.shutdown().await;
}

#[tokio::test]
async fn station_that_answers_pings_stays_connected() {
    let csms = start(|c| {
        c.ping_interval = Duration::from_millis(40);
        c.pong_timeout = Duration::from_millis(2_000);
    })
    .await;
    let mut events = csms.subscribe();
    let mut raw = raw_connect(&csms, "/ocpp/CP-P").await;
    wait_event(&mut events, "connect", connected("CP-P")).await;
    // Reading answers each ping with a pong; see several of them.
    let mut pings = 0;
    while pings < 4 {
        match timeout(STEP, raw.next()).await.expect("ping in time") {
            Some(Ok(Message::Ping(_))) => pings += 1,
            Some(Ok(_)) => {}
            other => panic!("socket ended: {other:?}"),
        }
    }
    assert!(
        csms.stations()
            .iter()
            .any(|s| s.id == "CP-P" && s.connected)
    );
    csms.shutdown().await;
}

#[tokio::test]
async fn installed_certificate_ids_are_tracked_and_capped() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let mut raw = raw_connect(&csms, "/ocpp/CP-C").await;
    wait_event(&mut events, "connect", connected("CP-C")).await;
    let hash = |n: usize| {
        json!({
            "hashAlgorithm": "SHA256",
            "issuerNameHash": format!("{n:x}"),
            "issuerKeyHash": "ab",
            "serialNumber": "01"
        })
    };
    for (kind, count, expected) in [
        ("CentralSystemRootCertificate", 2, 2),
        ("ManufacturerRootCertificate", 70, 64),
    ] {
        let caller = csms.clone();
        let task = tokio::spawn(async move {
            caller
                .call(
                    "CP-C",
                    "GetInstalledCertificateIds",
                    json!({"certificateType": kind}),
                )
                .await
        });
        let Frame::Call { id, action, .. } = raw_recv(&mut raw).await else {
            panic!("expected a CALL");
        };
        assert_eq!(action, "GetInstalledCertificateIds");
        let hashes: Vec<Value> = (0..count).map(hash).collect();
        raw_send(
            &mut raw,
            &json!([3, id, {"status": "Accepted", "certificateHashData": hashes}]).to_string(),
        )
        .await;
        task.await.unwrap().unwrap();
        wait_event(&mut events, "state change", |e| {
            matches!(e, CsmsEvent::StateChanged { .. })
        })
        .await;
        let state = csms.state("CP-C").unwrap();
        let tracked = &state.installed_certificates[kind];
        assert_eq!(tracked.certificates.len(), expected);
        assert_eq!(tracked.total, count);
    }
    csms.shutdown().await;
}

#[tokio::test]
async fn sockets_beyond_the_pending_handshake_limit_are_closed() {
    use tokio::io::AsyncReadExt as _;
    use tokio::net::TcpStream;

    let csms = start(|c| c.max_pending_handshakes = 2).await;
    // Two sockets that never start a handshake hold both permits.
    let idle = [
        TcpStream::connect(csms.local_addr()).await.unwrap(),
        TcpStream::connect(csms.local_addr()).await.unwrap(),
    ];
    let mut third = TcpStream::connect(csms.local_addr()).await.unwrap();
    let mut buf = [0_u8; 1];
    // The server closes it at once (EOF or reset) instead of waiting for a handshake.
    match timeout(STEP, third.read(&mut buf)).await {
        Ok(Ok(0) | Err(_)) => {}
        other => panic!("expected the third socket to be closed, got {other:?}"),
    }

    // Once the idle sockets are gone a real station gets through.
    drop(idle);
    let deadline = tokio::time::Instant::now() + STEP;
    loop {
        let mut request = format!("{}/CP-LATE", csms.url())
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "ocpp1.6".parse().unwrap());
        if tokio_tungstenite::connect_async(request).await.is_ok() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no permit was freed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    csms.shutdown().await;
}

#[tokio::test]
async fn calls_beyond_the_per_station_queue_limit_are_refused() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    // A station that never answers: the first CALL is written, the rest queue up behind it.
    let _silent = raw_connect(&csms, "/ocpp/CP-QUEUE").await;
    wait_event(&mut events, "queue station", connected("CP-QUEUE")).await;

    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut tasks = Vec::new();
    for _ in 0..1100 {
        let (csms, done_tx) = (csms.clone(), done_tx.clone());
        tasks.push(tokio::spawn(async move {
            let result = csms
                .call_timeout(
                    "CP-QUEUE",
                    "Reset",
                    json!({"type": "Soft"}),
                    Duration::from_secs(3600),
                )
                .await;
            let _ = done_tx.send(result);
        }));
    }
    // Only the refused ones finish: 1100 calls minus one in flight minus a full queue.
    for _ in 0..10 {
        match timeout(STEP, done_rx.recv()).await {
            Ok(Some(Err(CsmsError::QueueFull { station }))) => assert_eq!(station, "CP-QUEUE"),
            other => panic!("expected QueueFull, got {other:?}"),
        }
    }
    for task in tasks {
        task.abort();
    }
    csms.shutdown().await;
}

#[tokio::test]
async fn queue_limit_holds_while_the_session_is_stuck_in_a_write() {
    let csms = start(|config| {
        config.max_message_size = 64 * 1024 * 1024;
        config.write_timeout = Duration::from_secs(3600);
    })
    .await;
    let mut events = csms.subscribe();
    // The station never reads: a huge first CALL blocks the session loop in its write, so
    // later calls pile up in the command channel instead of the session queue.
    let _silent = raw_connect(&csms, "/ocpp/CP-STUCK").await;
    wait_event(&mut events, "stuck station", connected("CP-STUCK")).await;
    let big = {
        let csms = csms.clone();
        tokio::spawn(async move {
            csms.call_timeout(
                "CP-STUCK",
                "DataTransfer",
                json!({"vendorId": "v", "data": "x".repeat(48 * 1024 * 1024)}),
                Duration::from_secs(3600),
            )
            .await
        })
    };
    wait_event(&mut events, "big call written", |event| {
        matches!(
            event,
            CsmsEvent::Frame {
                direction: Direction::Out,
                ..
            }
        )
    })
    .await;

    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut tasks = Vec::new();
    for _ in 0..1100 {
        let (csms, done_tx) = (csms.clone(), done_tx.clone());
        tasks.push(tokio::spawn(async move {
            let result = csms
                .call_timeout(
                    "CP-STUCK",
                    "Reset",
                    json!({"type": "Soft"}),
                    Duration::from_secs(3600),
                )
                .await;
            let _ = done_tx.send(result);
        }));
    }
    // The refusals come at once, not after the write timeout of an hour.
    for _ in 0..70 {
        match timeout(STEP, done_rx.recv()).await {
            Ok(Some(Err(CsmsError::QueueFull { station }))) => assert_eq!(station, "CP-STUCK"),
            other => panic!("expected QueueFull, got {other:?}"),
        }
    }
    big.abort();
    for task in tasks {
        task.abort();
    }
    csms.shutdown().await;
}

#[tokio::test]
async fn consumed_faults_free_their_bytes() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let mut socket = raw_connect(&csms, "/ocpp/CP-FAULT").await;
    wait_event(&mut events, "station", connected("CP-FAULT")).await;
    let big = json!({"x": "a".repeat(3 * 1024 * 1024)});
    csms.try_inject(Fault::invalid_payload("Heartbeat", big.clone()))
        .unwrap();
    csms.try_inject(Fault::invalid_payload("Authorize", big.clone()))
        .unwrap();
    raw_send(&mut socket, r#"[2,"h1","Heartbeat",{}]"#).await;
    raw_recv(&mut socket).await;
    assert_eq!(csms.faults_remaining(), 1);
    // 3 MiB are stored, so another 3 MiB fit and a further 3 MiB do not.
    csms.try_inject(Fault::invalid_payload("StatusNotification", big.clone()))
        .unwrap();
    let error = csms
        .try_inject(Fault::invalid_payload("MeterValues", big.clone()))
        .unwrap_err();
    assert!(error.contains("8 MiB"), "{error}");
    csms.clear_faults();
    csms.try_inject(Fault::invalid_payload("A", big.clone()))
        .unwrap();
    csms.try_inject(Fault::invalid_payload("B", big)).unwrap();
}

#[tokio::test]
async fn fault_counters_saturate_and_storage_is_bounded() {
    let csms = start(|_| {}).await;
    csms.inject(Fault::silence("Heartbeat").times(usize::MAX));
    csms.inject(Fault::silence("Heartbeat").times(usize::MAX));
    assert_eq!(csms.faults_remaining(), usize::MAX);
    csms.clear_faults();

    for _ in 0..1000 {
        csms.try_inject(Fault::silence("Heartbeat")).unwrap();
    }
    let error = csms.try_inject(Fault::silence("Heartbeat")).unwrap_err();
    assert!(error.contains("too many faults"), "{error}");
    csms.clear_faults();

    let big = json!({"x": "a".repeat(3 * 1024 * 1024)});
    csms.try_inject(Fault::invalid_payload("A", big.clone()))
        .unwrap();
    csms.try_inject(Fault::invalid_payload("B", big.clone()))
        .unwrap();
    let error = csms
        .try_inject(Fault::invalid_payload("C", big))
        .unwrap_err();
    assert!(error.contains("8 MiB"), "{error}");
    assert_eq!(csms.faults_remaining(), 2);
}

#[tokio::test]
async fn forget_if_disconnected_is_atomic_and_history_tail_is_bounded() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    assert_eq!(csms.forget_if_disconnected("ghost"), None);
    let cp = Cp::connect(&csms, "/ocpp", "CP-TAIL").await;
    wait_event(&mut events, "connect", connected("CP-TAIL")).await;
    assert_eq!(csms.forget_if_disconnected("CP-TAIL"), Some(false));
    assert!(csms.station("CP-TAIL").is_some());
    let all = csms.history("CP-TAIL").unwrap();
    let tail = csms.history_tail("CP-TAIL", 1).unwrap();
    assert_eq!(tail.len(), all.len().min(1));
    assert_eq!(csms.history_tail("CP-TAIL", 0).unwrap().len(), 0);
    assert_eq!(
        csms.history_tail("CP-TAIL", usize::MAX).unwrap().len(),
        all.len()
    );
    assert!(csms.history_tail("ghost", 1).is_none());
    cp.stop().await;
    wait_event(&mut events, "disconnect", |e| {
        matches!(e, CsmsEvent::Disconnected { .. })
    })
    .await;
    assert_eq!(csms.forget_if_disconnected("CP-TAIL"), Some(true));
    assert!(csms.station("CP-TAIL").is_none());
}
