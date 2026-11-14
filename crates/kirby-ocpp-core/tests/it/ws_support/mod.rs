//! Shared helpers for the `ws_*` integration tests.
//!
//! * [`RawServer`]: a plain WebSocket server (no OCPP logic) that records the upgrade
//!   request and hands out each accepted socket as a [`RawPeer`]. Tests script the
//!   CSMS side frame by frame.
//! * [`collect_events`]: drains a connection's event stream in the background, so the
//!   connection is never stalled by an owner that awaits calls.
//! * Every wait has a timeout ([`STEP`]); nothing sleeps for a fixed time.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of the helpers"
)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use kirby_ocpp_core::config::TlsConfig;
use kirby_ocpp_core::ws::{ConnectOptions, Connection, ConnectionEvent};
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::{self, HeaderValue};
use tokio_tungstenite::{WebSocketStream, accept_hdr_async};

/// Upper bound for any single wait in a test.
pub const STEP: Duration = Duration::from_secs(5);

/// How the raw server answers the subprotocol offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Negotiate {
    /// Select this protocol whether or not it was offered.
    Select(String),
    /// Accept the upgrade without a `Sec-WebSocket-Protocol` answer.
    Omit,
    /// Refuse the upgrade with HTTP 400.
    Reject,
}

/// One accepted WebSocket, with what the upgrade request carried.
pub struct RawPeer {
    /// Request target: path and query exactly as the client sent them.
    pub uri: String,
    /// The `Authorization` header, if the client sent one.
    pub authorization: Option<String>,
    /// The `Host` header.
    pub host: Option<String>,
    pub ws: WebSocketStream<TcpStream>,
}

impl RawPeer {
    pub async fn send_text(&mut self, text: &str) {
        self.ws
            .send(Message::Text(text.into()))
            .await
            .expect("send text to the connection");
    }

    /// Sends a CALL-like frame built from a JSON value.
    pub async fn send_json(&mut self, value: &Value) {
        self.send_text(&value.to_string()).await;
    }

    /// Next message, or `None` once the socket has ended. Fails the test on timeout.
    pub async fn recv(&mut self) -> Option<Message> {
        let next = timeout(STEP, self.ws.next())
            .await
            .expect("no message from the connection");
        match next {
            Some(Ok(message)) => Some(message),
            Some(Err(_)) | None => None,
        }
    }

    /// Next text frame as JSON (`[2, ...]`, `[3, ...]`, ...). Other message kinds are skipped.
    pub async fn recv_frame(&mut self) -> Value {
        loop {
            match self.recv().await.expect("socket ended before a frame") {
                Message::Text(text) => {
                    return serde_json::from_str(text.as_str()).expect("valid JSON frame");
                }
                Message::Close(_) => panic!("socket closed before a frame"),
                _ => {}
            }
        }
    }

    /// Answers a CALL with a CALLRESULT carrying `payload`.
    pub async fn reply(&mut self, call: &Value, payload: Value) {
        let id = call[1].clone();
        self.send_json(&serde_json::json!([3, id, payload])).await;
    }

    /// Closes the socket from the server side with the given close code.
    pub async fn close_with(&mut self, code: u16) {
        use tokio_tungstenite::tungstenite::protocol::CloseFrame;
        use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
        let frame = CloseFrame {
            code: CloseCode::from(code),
            reason: "test".into(),
        };
        self.ws
            .send(Message::Close(Some(frame)))
            .await
            .expect("send close");
    }
}

/// What the server saw in the upgrade request.
#[derive(Debug, Default, Clone)]
struct Captured {
    uri: String,
    authorization: Option<String>,
    host: Option<String>,
}

/// A plain (`ws://`) WebSocket server on 127.0.0.1 with an ephemeral port.
pub struct RawServer {
    pub addr: SocketAddr,
    peers: mpsc::UnboundedReceiver<RawPeer>,
    accept_loop: JoinHandle<()>,
}

impl Drop for RawServer {
    fn drop(&mut self) {
        self.accept_loop.abort();
    }
}

impl RawServer {
    pub async fn start(negotiate: Negotiate) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind raw server");
        let addr = listener.local_addr().expect("local address");
        let (peers_tx, peers) = mpsc::unbounded_channel();
        let accept_loop = tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                let negotiate = negotiate.clone();
                let peers_tx = peers_tx.clone();
                tokio::spawn(async move {
                    let captured = Arc::new(Mutex::new(Captured::default()));
                    let captured_in_callback = Arc::clone(&captured);
                    #[allow(
                        clippy::result_large_err,
                        reason = "fixed by the tungstenite Callback type"
                    )]
                    let callback = move |request: &Request, mut response: Response| {
                        let header = |name: &str| {
                            request
                                .headers()
                                .get(name)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_owned)
                        };
                        *captured_in_callback.lock().expect("lock") = Captured {
                            uri: request
                                .uri()
                                .path_and_query()
                                .map_or_else(String::new, ToString::to_string),
                            authorization: header("authorization"),
                            host: header("host"),
                        };
                        match &negotiate {
                            Negotiate::Select(name) => {
                                response.headers_mut().insert(
                                    "sec-websocket-protocol",
                                    HeaderValue::from_str(name).expect("valid header"),
                                );
                            }
                            Negotiate::Omit => {}
                            Negotiate::Reject => {
                                let mut refusal = ErrorResponse::new(Some("refused".to_owned()));
                                *refusal.status_mut() = http::StatusCode::BAD_REQUEST;
                                return Err(refusal);
                            }
                        }
                        Ok(response)
                    };
                    if let Ok(ws) = accept_hdr_async(tcp, callback).await {
                        let captured = captured.lock().expect("lock").clone();
                        let _ = peers_tx.send(RawPeer {
                            uri: captured.uri,
                            authorization: captured.authorization,
                            host: captured.host,
                            ws,
                        });
                    }
                });
            }
        });
        Self {
            addr,
            peers,
            accept_loop,
        }
    }

    /// `ws://127.0.0.1:<port><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("ws://{}{path}", self.addr)
    }

    /// The next accepted socket. Fails the test if none arrives in time.
    pub async fn next_peer(&mut self) -> RawPeer {
        timeout(STEP, self.peers.recv())
            .await
            .expect("no connection reached the raw server")
            .expect("raw server stopped")
    }
}

/// Default options for tests: no pings (they would race the scripted frames) and a
/// short call timeout.
pub fn options() -> ConnectOptions {
    ConnectOptions {
        ping_interval: None,
        call_timeout: Duration::from_secs(5),
        serialize_calls: true,
        show_secrets: false,
        write_timeout: Duration::from_secs(10),
        ..ConnectOptions::default()
    }
}

/// Connects a charge point `CP-001` to `url` with the OCPP subprotocol.
pub async fn connect(
    url: &str,
    options: ConnectOptions,
) -> (Connection, mpsc::UnboundedReceiver<ConnectionEvent>) {
    connect_with_tls(url, &TlsConfig::default(), options).await
}

pub async fn connect_with_tls(
    url: &str,
    tls: &TlsConfig,
    options: ConnectOptions,
) -> (Connection, mpsc::UnboundedReceiver<ConnectionEvent>) {
    let (connection, events) = Connection::connect(url, "CP-001", tls, "ocpp1.6", options)
        .await
        .expect("connect");
    (connection, collect_events(events))
}

/// Drains the connection's bounded event receiver into an unbounded one, so the
/// connection is never stalled while the test awaits calls.
pub fn collect_events(
    mut events: mpsc::Receiver<ConnectionEvent>,
) -> mpsc::UnboundedReceiver<ConnectionEvent> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if tx.send(event).is_err() {
                break;
            }
        }
    });
    rx
}

/// Waits for the next event for which `matches` returns `Some`; other events are discarded.
pub async fn next_event<T>(
    events: &mut mpsc::UnboundedReceiver<ConnectionEvent>,
    mut matches: impl FnMut(ConnectionEvent) -> Option<T>,
) -> T {
    timeout(STEP, async {
        loop {
            let event = events.recv().await.expect("event stream ended");
            if let Some(found) = matches(event) {
                return found;
            }
        }
    })
    .await
    .expect("expected connection event did not arrive")
}

/// Waits for the `Closed` event and returns `(code, reason, by_us)`.
pub async fn next_closed(
    events: &mut mpsc::UnboundedReceiver<ConnectionEvent>,
) -> (Option<u16>, String, bool) {
    next_event(events, |event| match event {
        ConnectionEvent::Closed {
            code,
            reason,
            by_us,
        } => Some((code, reason, by_us)),
        _ => None,
    })
    .await
}

/// Waits for the next `ProtocolWarning` and returns its text.
pub async fn next_warning(events: &mut mpsc::UnboundedReceiver<ConnectionEvent>) -> String {
    next_event(events, |event| match event {
        ConnectionEvent::ProtocolWarning(text) => Some(text),
        _ => None,
    })
    .await
}

/// Waits for the next incoming CALL.
pub async fn next_call(
    events: &mut mpsc::UnboundedReceiver<ConnectionEvent>,
) -> kirby_ocpp_core::ws::IncomingCall {
    next_event(events, |event| match event {
        ConnectionEvent::Call(call) => Some(call),
        _ => None,
    })
    .await
}
