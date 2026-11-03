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

    // Unknown action: NotSupported.
    raw_send(&mut raw, r#"[2,"u1","Teleport",{}]"#).await;
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
