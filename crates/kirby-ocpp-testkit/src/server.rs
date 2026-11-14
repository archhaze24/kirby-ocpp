use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use kirby_csms_core::{Fault, Reply, Responder};
use kirby_ocpp_core::ocpp::{ErrorCode, Frame};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use crate::error::{TestkitError, TestkitResult};
use crate::options::CsmsOptions;
use crate::record::{ConnectionInfo, Received};
use crate::state::{Outbound, Shared, StartedCall, wait_until};
use crate::tls::{self, TlsMaterial, TlsOpts};

const SEC_WEBSOCKET_PROTOCOL: HeaderName = HeaderName::from_static("sec-websocket-protocol");

/// A scripted OCPP 1.6J central system listening on `127.0.0.1`.
///
/// Start one with [`MockCsms::start`] (or [`MockCsms::start_tls`]). It answers the
/// charge point's CALLs with defaults ported from the TS smoke harness, lets a test
/// override or fault individual actions, records every CALL, and can send CALLs to
/// the charge point. Dropping it stops the server and closes its connections.
///
/// # Example
///
/// ```no_run
/// use std::time::Duration;
/// use kirby_ocpp_testkit::{CsmsOptions, Fault, MockCsms, TestkitError};
///
/// # async fn run() -> Result<(), TestkitError> {
/// let csms = MockCsms::start(CsmsOptions::default()).await?;
/// // Point the emulator at `csms.url()`; it connects to `<url>/<charge point id>`.
/// csms.inject(Fault::call_error("StartTransaction").times(2));
///
/// let boot = csms.wait_for_call("BootNotification", Duration::from_secs(5)).await?;
/// assert!(boot.get("chargePointModel").is_some());
///
/// let reply = csms
///     .send_call("ChangeConfiguration", serde_json::json!({ "key": "HeartbeatInterval", "value": "6" }))
///     .await?;
/// # let _ = reply;
/// csms.shutdown().await;
/// # Ok(())
/// # }
/// ```
pub struct MockCsms {
    addr: SocketAddr,
    url: String,
    shared: Arc<Shared>,
    call_timeout: Duration,
    tls: Option<TlsMaterial>,
    stop: Mutex<Option<oneshot::Sender<()>>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl fmt::Debug for MockCsms {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MockCsms")
            .field("url", &self.url)
            .field("tls", &self.tls.is_some())
            .finish_non_exhaustive()
    }
}

impl MockCsms {
    /// Binds `127.0.0.1` on an ephemeral port and starts serving.
    pub async fn start(options: CsmsOptions) -> TestkitResult<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(TestkitError::Start)?;
        let addr = listener.local_addr().map_err(TestkitError::Start)?;

        let (acceptor, tls_material, scheme) = match &options.tls {
            Some(tls_options) => {
                let (config, material) = tls::build(*tls_options)?;
                (Some(TlsAcceptor::from(config)), Some(material), "wss")
            }
            None => (None, None, "ws"),
        };
        let url = format!("{scheme}://{addr}{}", options.base_path);

        let shared = Arc::new(Shared::new(&options));
        let (stop_tx, stop_rx) = oneshot::channel();
        let task = tokio::spawn(accept_loop(
            listener,
            acceptor,
            Arc::clone(&shared),
            stop_rx,
        ));

        Ok(Self {
            addr,
            url,
            shared,
            call_timeout: options.call_timeout,
            tls: tls_material,
            stop: Mutex::new(Some(stop_tx)),
            task: Mutex::new(Some(task)),
        })
    }

    /// Starts a `wss://` server with generated certificates and default options otherwise.
    /// Use [`MockCsms::start`] with `CsmsOptions { tls: Some(..), .. }` to combine TLS with
    /// other options.
    pub async fn start_tls(tls: TlsOpts) -> TestkitResult<Self> {
        Self::start(CsmsOptions {
            tls: Some(tls),
            ..CsmsOptions::default()
        })
        .await
    }

    /// Central system URL, e.g. `ws://127.0.0.1:41234/ocpp` (`wss://` with TLS).
    /// Pass it to the emulator as the central system URL; it appends the charge point id.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Full URL for a charge point id, i.e. `url()` followed by `/<charge_point_id>`.
    pub fn charge_point_url(&self, charge_point_id: &str) -> String {
        format!("{}/{charge_point_id}", self.url)
    }

    /// The bound socket address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The generated PEM material, if this server uses TLS.
    pub fn tls_material(&self) -> Option<&TlsMaterial> {
        self.tls.as_ref()
    }

    /// Total number of WebSocket connections accepted so far (including closed ones).
    pub fn connections(&self) -> usize {
        self.shared.lock().connections.len()
    }

    /// Number of connections that are currently open.
    pub fn open_connections(&self) -> usize {
        self.shared.lock().open.len()
    }

    /// Every connection accepted so far, in order.
    pub fn connection_log(&self) -> Vec<ConnectionInfo> {
        self.shared.lock().connections.clone()
    }

    /// Waits until a connection is open and returns the most recent one.
    pub async fn wait_for_connection(&self, timeout: Duration) -> TestkitResult<ConnectionInfo> {
        wait_until(&self.shared, "an open connection", timeout, |state| {
            let (id, _) = state.current()?;
            state.connections.iter().find(|info| info.id == id).cloned()
        })
        .await
    }

    /// Waits until at least `total` connections have been accepted in all (counting closed ones).
    pub async fn wait_for_connection_count(
        &self,
        total: usize,
        timeout: Duration,
    ) -> TestkitResult<()> {
        let what = format!("{total} accepted connection(s)");
        wait_until(&self.shared, &what, timeout, |state| {
            (state.connections.len() >= total).then_some(())
        })
        .await
    }

    /// Waits until no connection is open.
    pub async fn wait_for_disconnect(&self, timeout: Duration) -> TestkitResult<()> {
        wait_until(&self.shared, "all connections to close", timeout, |state| {
            state.open.is_empty().then_some(())
        })
        .await
    }

    /// Closes the current connection (close code 1011), as the TS harness does on a
    /// dropped socket. Fails with [`TestkitError::NotConnected`] if none is open.
    pub fn drop_connection(&self) -> TestkitResult<()> {
        let (_, outbound) = self
            .shared
            .lock()
            .current()
            .ok_or(TestkitError::NotConnected)?;
        outbound
            .send(Outbound::Close {
                code: crate::state::CLOSE_CODE_ERROR,
                reason: "dropped by mock CSMS".to_owned(),
            })
            .map_err(|_| TestkitError::NotConnected)
    }

    /// A position in the recorded CALL log. Pass it to the `_after`/`calls_since` methods so
    /// that only calls made after this point count.
    pub fn mark(&self) -> usize {
        self.shared.lock().received.len()
    }

    /// Every CP-originated CALL recorded so far, in arrival order.
    pub fn received(&self) -> Vec<Received> {
        self.shared.lock().received.clone()
    }

    /// CP-originated CALLs recorded since `mark` (see [`MockCsms::mark`]).
    pub fn calls_since(&self, mark: usize) -> Vec<Received> {
        let state = self.shared.lock();
        state.received.get(mark..).unwrap_or_default().to_vec()
    }

    /// Waits for a CALL with `action`, counting calls that already arrived, and returns its payload.
    pub async fn wait_for_call(&self, action: &str, timeout: Duration) -> TestkitResult<Value> {
        self.wait_for_call_after(0, action, |_| true, timeout).await
    }

    /// Waits for a CALL with `action` made after `mark` whose payload satisfies `predicate`.
    /// Returns the first match's payload.
    pub async fn wait_for_call_after<F>(
        &self,
        mark: usize,
        action: &str,
        predicate: F,
        timeout: Duration,
    ) -> TestkitResult<Value>
    where
        F: Fn(&Value) -> bool + Send,
    {
        let what = format!("CP {action} CALL");
        wait_until(&self.shared, &what, timeout, |state| {
            state
                .received
                .get(mark..)
                .unwrap_or_default()
                .iter()
                .find(|received| received.action == action && predicate(&received.payload))
                .map(|received| received.payload.clone())
        })
        .await
    }

    /// Waits until `count` CALLs with `action` made after `mark` satisfy `predicate`.
    /// Returns the first `count` matching payloads.
    pub async fn wait_for_call_count_after<F>(
        &self,
        mark: usize,
        action: &str,
        count: usize,
        predicate: F,
        timeout: Duration,
    ) -> TestkitResult<Vec<Value>>
    where
        F: Fn(&Value) -> bool + Send,
    {
        let what = format!("{count} CP {action} CALL(s)");
        wait_until(&self.shared, &what, timeout, |state| {
            let found: Vec<Value> = state
                .received
                .get(mark..)
                .unwrap_or_default()
                .iter()
                .filter(|received| received.action == action && predicate(&received.payload))
                .take(count)
                .map(|received| received.payload.clone())
                .collect();
            (found.len() == count).then_some(found)
        })
        .await
    }

    /// Scripts a fault for the next occurrences of an action. Faults are used in the order injected.
    pub fn inject(&self, fault: Fault) {
        self.shared.inject(fault);
    }

    /// Number of fault occurrences still waiting to be triggered.
    pub fn faults_remaining(&self) -> usize {
        self.shared
            .lock()
            .faults
            .iter()
            .map(|fault| fault.remaining)
            .sum()
    }

    /// Overrides the default answer for `action`. Faults still take precedence.
    ///
    /// The closure receives the CALL payload and runs outside the internal lock.
    pub fn respond<F>(&self, action: &str, responder: F)
    where
        F: Fn(&Value) -> Reply + Send + Sync + 'static,
    {
        let responder: Responder = Arc::new(responder);
        self.shared.set_responder(action, responder);
    }

    /// Sends a CSMS-originated CALL to the current connection and waits for its reply, using
    /// [`CsmsOptions::call_timeout`]. Returns the CALLRESULT or CALLERROR frame.
    pub async fn send_call(&self, action: &str, payload: Value) -> TestkitResult<Frame> {
        self.send_call_timeout(action, payload, self.call_timeout)
            .await
    }

    /// Like [`MockCsms::send_call`] with an explicit timeout.
    pub async fn send_call_timeout(
        &self,
        action: &str,
        payload: Value,
        timeout: Duration,
    ) -> TestkitResult<Frame> {
        let StartedCall {
            id,
            outbound,
            reply,
        } = self.shared.start_call()?;
        let text = Frame::call(id.clone(), action, payload).to_json_string();
        if outbound.send(Outbound::Text(text)).is_err() {
            self.shared.cancel_call(&id);
            return Err(TestkitError::NotConnected);
        }

        match tokio::time::timeout(timeout, reply).await {
            Ok(Ok(frame)) => Ok(frame),
            Ok(Err(_)) => Err(TestkitError::ConnectionClosed {
                action: action.to_owned(),
            }),
            Err(_) => {
                self.shared.cancel_call(&id);
                Err(TestkitError::Timeout {
                    what: format!("the charge point's reply to {action}"),
                    timeout,
                })
            }
        }
    }

    /// Sends a CSMS-originated CALL and checks that the CALLRESULT carries `status`.
    /// Returns the CALLRESULT payload.
    pub async fn expect_status(
        &self,
        action: &str,
        payload: Value,
        status: &str,
    ) -> TestkitResult<Value> {
        let frame = self.send_call(action, payload).await?;
        match &frame {
            Frame::CallResult { payload, .. }
                if payload.get("status").and_then(Value::as_str) == Some(status) =>
            {
                Ok(payload.clone())
            }
            other => Err(TestkitError::UnexpectedReply {
                action: action.to_owned(),
                expected: format!("CALLRESULT with status {status}"),
                got: other.to_json_string(),
            }),
        }
    }

    /// Sends a CSMS-originated CALL and checks that the reply is a CALLERROR with `code`.
    pub async fn expect_call_error(
        &self,
        action: &str,
        payload: Value,
        code: &ErrorCode,
    ) -> TestkitResult<()> {
        let frame = self.send_call(action, payload).await?;
        match &frame {
            Frame::CallError { code: got, .. } if got == code => Ok(()),
            other => Err(TestkitError::UnexpectedReply {
                action: action.to_owned(),
                expected: format!("CALLERROR {code}"),
                got: other.to_json_string(),
            }),
        }
    }

    /// Stops the server and closes all connections. Waits until the accept loop has finished.
    /// Calling it again is a no-op.
    pub async fn shutdown(&self) {
        let stop = self
            .stop
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(stop) = stop {
            let _ = stop.send(());
        }
        let task = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            let _ = task.await;
        }
        self.shared.close_all();
    }
}

impl Drop for MockCsms {
    fn drop(&mut self) {
        if let Some(task) = self
            .task
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
        self.shared.close_all();
    }
}

async fn accept_loop(
    listener: TcpListener,
    acceptor: Option<TlsAcceptor>,
    shared: Arc<Shared>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut sessions = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut stop => break,
            accepted = listener.accept() => {
                if let Ok((tcp, _peer)) = accepted {
                    sessions.spawn(serve_tcp(tcp, acceptor.clone(), Arc::clone(&shared)));
                }
            }
            Some(_) = sessions.join_next(), if !sessions.is_empty() => {}
        }
    }
    sessions.shutdown().await;
}

async fn serve_tcp(tcp: TcpStream, acceptor: Option<TlsAcceptor>, shared: Arc<Shared>) {
    match acceptor {
        Some(acceptor) => {
            // A failed TLS handshake is never registered as a connection.
            if let Ok(tls) = acceptor.accept(tcp).await {
                serve_socket(tls, shared).await;
            }
        }
        None => serve_socket(tcp, shared).await,
    }
}

/// Performs the WebSocket handshake (with subprotocol negotiation), registers the
/// connection and pumps frames until the socket or the server closes it.
async fn serve_socket<S>(stream: S, shared: Arc<Shared>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut target: Option<(String, Option<String>)> = None;
    // `ErrorResponse` (the refusal) is large, but its type is fixed by tungstenite's `Callback`.
    #[allow(clippy::result_large_err)]
    let callback = |request: &Request, mut response: Response| -> Result<Response, ErrorResponse> {
        let offered = offered_protocols(request);
        let chosen = shared.subprotocol.select(&offered).map_err(refusal)?;
        if let Some(name) = &chosen {
            let value = HeaderValue::from_str(name)
                .map_err(|_| refusal("invalid subprotocol name in configuration"))?;
            response.headers_mut().insert(SEC_WEBSOCKET_PROTOCOL, value);
        }
        target = Some((request.uri().path().to_owned(), chosen));
        Ok(response)
    };
    let Ok(mut socket) = accept_hdr_async(stream, callback).await else {
        return;
    };
    let Some((path, subprotocol)) = target else {
        return;
    };

    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel();
    let connection = shared.register(path, subprotocol, outbound_tx);

    loop {
        let outbound = tokio::select! {
            inbound = socket.next() => match inbound {
                Some(Ok(Message::Text(text))) => shared.handle_inbound(connection, text.as_str()),
                Some(Ok(Message::Close(_))) => {
                    // Send the queued Close reply before the socket is dropped.
                    let _ = socket.flush().await;
                    break;
                }
                Some(Err(_)) | None => break,
                Some(Ok(_)) => None,
            },
            queued = outbound_rx.recv() => match queued {
                Some(outbound) => Some(outbound),
                None => break,
            },
        };
        let Some(outbound) = outbound else {
            continue;
        };
        let closing = matches!(outbound, Outbound::Close { .. });
        if socket.send(into_message(outbound)).await.is_err() || closing {
            break;
        }
    }
    shared.close_connection(connection);
}

fn into_message(outbound: Outbound) -> Message {
    match outbound {
        Outbound::Text(text) => Message::text(text),
        Outbound::Close { code, reason } => Message::Close(Some(CloseFrame {
            code: CloseCode::from(code),
            reason: reason.into(),
        })),
    }
}

/// The subprotocols the client offered, from every `Sec-WebSocket-Protocol` header line.
fn offered_protocols(request: &Request) -> Vec<String> {
    request
        .headers()
        .get_all(SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect()
}

/// An HTTP 400 refusal. `ErrorResponse` is large, but its shape is fixed by tungstenite's
/// `Callback` trait, so it cannot be boxed here.
fn refusal(reason: &str) -> ErrorResponse {
    let mut response = ErrorResponse::new(Some(reason.to_owned()));
    *response.status_mut() = StatusCode::BAD_REQUEST;
    response
}
