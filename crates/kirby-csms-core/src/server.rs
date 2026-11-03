use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use kirby_ocpp_core::ocpp::{ErrorCode, Frame};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Instant, timeout};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::{WebSocketStream, accept_hdr_async_with_config};

use crate::auth::{self, PeerIdentity};
use crate::config::{CsmsConfig, SecurityProfile};
use crate::defaults::{default_reply, is_charge_point_action};
use crate::error::CsmsError;
use crate::event::{CsmsEvent, Direction, DisconnectReason, FrameEntry, StationInfo};
use crate::fault::{Fault, Reply, Responder};
use crate::policy::TransactionIdAllocator;
use crate::state::StationState;

const SEC_WEBSOCKET_PROTOCOL: HeaderName = HeaderName::from_static("sec-websocket-protocol");
/// Time a new socket gets for the TLS and WebSocket handshakes.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Time sessions get to close their sockets on shutdown before they are aborted.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
/// Time one close frame may take to be written.
const CLOSE_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
/// Longest timeout of a CALL; larger ones are clamped (keeps the deadline arithmetic safe).
const MAX_CALL_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
/// Most distinct actions counted per station; further ones are counted as `other`.
const MAX_ACTION_KEYS: usize = 64;
/// Bucket of `received_by_action` for actions beyond [`MAX_ACTION_KEYS`].
const OTHER_ACTION: &str = "other";
/// Longest text kept in a [`CsmsEvent::Malformed`] event.
const MALFORMED_TEXT_CHARS: usize = 512;
/// Close code for injected closes (internal error), as in the test mock.
const CLOSE_CODE_ERROR: u16 = 1011;

/// Entry point: starts a multi-station OCPP 1.6J central system.
///
/// # Station identity
///
/// A station's id is the last non-empty segment of the WebSocket URL path, percent-decoded;
/// any prefix is accepted (`/ocpp/CP-1`, `/a/b/CP-1` and `/CP-1` are all station `CP-1`).
/// A second connection with an id that is already connected replaces the old one: the old
/// socket gets a close frame (1000) and [`CsmsEvent::Disconnected`] with
/// [`DisconnectReason::Replaced`] is emitted before [`CsmsEvent::Connected`] for the new one.
///
/// # Outbound CALLs
///
/// OCPP-J allows one outstanding CALL per direction per connection. Calls made with
/// [`CsmsHandle::call`] are queued per station and written one at a time: the next CALL is
/// sent when the previous one was answered or timed out. The timeout counts from the moment
/// a CALL is written, not from when it was queued; wrap the future in your own
/// `tokio::time::timeout` to bound the queueing time too. Dropping the future of a call that
/// was not sent yet cancels it.
///
/// # Example
///
/// ```no_run
/// use kirby_csms_core::{CsmsConfig, CsmsError, CsmsServer, Fault};
/// use serde_json::json;
///
/// # async fn run() -> Result<(), CsmsError> {
/// let csms = CsmsServer::start(CsmsConfig::new("127.0.0.1:9000".parse().unwrap())).await?;
/// csms.inject(Fault::call_error("StartTransaction").times(2));
///
/// let mut events = csms.subscribe();
/// let _ = events.recv().await; // CsmsEvent::Connected { station: "CP-1" } ...
///
/// let reply = csms
///     .call("CP-1", "GetConfiguration", json!({ "key": ["HeartbeatInterval"] }))
///     .await?;
/// println!("{}", reply.to_json_string());
/// csms.shutdown().await;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy)]
pub struct CsmsServer;

impl CsmsServer {
    /// Binds `config.listen` and starts serving.
    ///
    /// # Errors
    ///
    /// [`CsmsError::Config`] if the security profile needs TLS (2 and 3) and
    /// [`CsmsConfig::tls`] is not set; [`CsmsError::Bind`] if the address is unavailable.
    pub async fn start(config: CsmsConfig) -> Result<CsmsHandle, CsmsError> {
        if config.tls.is_none()
            && matches!(
                config.security.profile,
                SecurityProfile::BasicAuthTls | SecurityProfile::MutualTls
            )
        {
            return Err(CsmsError::Config(format!(
                "security profile {} needs TLS (`tls` is not set)",
                config.security.profile.number()
            )));
        }
        let listener = TcpListener::bind(config.listen)
            .await
            .map_err(CsmsError::Bind)?;
        let local_addr = listener.local_addr().map_err(CsmsError::Bind)?;
        let acceptor = config.tls.clone().map(TlsAcceptor::from);
        let (events, _) = broadcast::channel(config.event_buffer.max(1));
        let (shutdown, _) = watch::channel(false);
        let inner = Arc::new(Inner {
            local_addr,
            secure: acceptor.is_some(),
            state: Mutex::new(State {
                stations: BTreeMap::new(),
                faults: Vec::new(),
                responders: std::collections::HashMap::new(),
                tx_ids: config.transaction_ids.allocator(),
                next_session: 0,
            }),
            config,
            events,
            shutdown,
            next_message: AtomicU64::new(0),
            accept: Mutex::new(None),
        });
        let task = tokio::spawn(accept_loop(listener, acceptor, Arc::clone(&inner)));
        *inner.accept.lock().unwrap_or_else(PoisonError::into_inner) = Some(task);
        Ok(CsmsHandle { inner })
    }
}

/// Cheap-to-clone handle to a running server. See [`CsmsServer`].
#[derive(Clone)]
pub struct CsmsHandle {
    inner: Arc<Inner>,
}

impl fmt::Debug for CsmsHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CsmsHandle")
            .field("local_addr", &self.inner.local_addr)
            .field("tls", &self.inner.secure)
            .finish_non_exhaustive()
    }
}

impl CsmsHandle {
    /// The bound socket address (useful with port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.inner.local_addr
    }

    /// Base URL stations connect to (`ws://addr` or `wss://addr`, no trailing slash).
    /// Append `/<station id>`, optionally behind any path prefix.
    pub fn url(&self) -> String {
        let scheme = if self.inner.secure { "wss" } else { "ws" };
        format!("{scheme}://{}", self.inner.local_addr)
    }

    /// Subscribes to server events. See [`CsmsEvent`] for lag behaviour.
    pub fn subscribe(&self) -> broadcast::Receiver<CsmsEvent> {
        self.inner.events.subscribe()
    }

    /// Every station in the registry (connected or not), ordered by id.
    pub fn stations(&self) -> Vec<StationInfo> {
        self.inner
            .lock()
            .stations
            .values()
            .map(|rec| rec.info.clone())
            .collect()
    }

    /// One station of the registry.
    pub fn station(&self, id: &str) -> Option<StationInfo> {
        self.inner
            .lock()
            .stations
            .get(id)
            .map(|rec| rec.info.clone())
    }

    /// The most recent frames of a station (at most `history_cap`), oldest first.
    pub fn history(&self, id: &str) -> Option<Vec<FrameEntry>> {
        self.inner
            .lock()
            .stations
            .get(id)
            .map(|rec| rec.history.iter().cloned().collect())
    }

    /// The protocol state of a station (see [`StationState`]); it survives disconnects and
    /// reconnects and is dropped by [`CsmsHandle::forget`]. A change is announced by
    /// [`CsmsEvent::StateChanged`]; there is no per-station watch channel, which keeps a
    /// thousand idle stations cheap.
    pub fn state(&self, id: &str) -> Option<StationState> {
        self.inner
            .lock()
            .stations
            .get(id)
            .map(|rec| rec.state.clone())
    }

    /// Removes a station from the registry, closing its connection if it has one.
    /// Returns whether the station existed.
    pub fn forget(&self, id: &str) -> bool {
        let mut state = self.inner.lock();
        let Some(rec) = state.stations.remove(id) else {
            return false;
        };
        if let Some(session) = rec.session {
            let _ = session.tx.send(SessionCmd::Close {
                code: 1000,
                reason: "station forgotten",
            });
            self.inner.emit(CsmsEvent::Disconnected {
                station: id.to_owned(),
                reason: DisconnectReason::Forgotten,
            });
        }
        self.inner.emit(CsmsEvent::Forgotten {
            station: id.to_owned(),
        });
        true
    }

    /// Sends a CALL to a connected station and waits for its CALLRESULT or CALLERROR, using
    /// [`CsmsConfig::call_timeout`]. See [`CsmsServer`] for queueing rules.
    pub async fn call(
        &self,
        station: &str,
        action: &str,
        payload: Value,
    ) -> Result<Frame, CsmsError> {
        self.call_timeout(station, action, payload, self.inner.config.call_timeout)
            .await
    }

    /// Like [`CsmsHandle::call`] with an explicit timeout.
    pub async fn call_timeout(
        &self,
        station: &str,
        action: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Frame, CsmsError> {
        let timeout = timeout.min(MAX_CALL_TIMEOUT);
        let (reply, receiver) = oneshot::channel();
        {
            let state = self.inner.lock();
            let rec = state
                .stations
                .get(station)
                .ok_or_else(|| CsmsError::UnknownStation {
                    station: station.to_owned(),
                })?;
            let not_connected = || CsmsError::NotConnected {
                station: station.to_owned(),
            };
            let session = rec.session.as_ref().ok_or_else(not_connected)?;
            session
                .tx
                .send(SessionCmd::Call(QueuedCall {
                    action: action.to_owned(),
                    payload,
                    timeout,
                    reply,
                }))
                .map_err(|_| not_connected())?;
        }
        let result = match receiver.await {
            Ok(result) => result,
            Err(_) => Err(CsmsError::ConnectionClosed {
                station: station.to_owned(),
                action: action.to_owned(),
            }),
        };
        if matches!(
            result,
            Ok(Frame::CallError { .. }) | Err(CsmsError::Timeout { .. })
        ) {
            self.inner.note_failed_call(station);
        }
        result
    }

    /// Overrides the default answer for `action` for all stations. Faults still take
    /// precedence. The closure runs outside the internal lock but must not block.
    pub fn respond<F>(&self, action: &str, responder: F)
    where
        F: Fn(&Value) -> Reply + Send + Sync + 'static,
    {
        let responder: Responder = Arc::new(responder);
        self.inner
            .lock()
            .responders
            .insert(action.to_owned(), responder);
    }

    /// Removes the responder for `action`, if any.
    pub fn clear_responder(&self, action: &str) {
        self.inner.lock().responders.remove(action);
    }

    /// Scripts a fault for the next occurrences of an action, across all stations. Faults
    /// are used in the order injected.
    pub fn inject(&self, fault: Fault) {
        if fault.remaining > 0 {
            self.inner.lock().faults.push(fault);
        }
    }

    /// Number of fault occurrences still waiting to be triggered.
    pub fn faults_remaining(&self) -> usize {
        self.inner
            .lock()
            .faults
            .iter()
            .map(|fault| fault.remaining)
            .sum()
    }

    /// Actions that currently have a responder, sorted.
    pub fn responder_actions(&self) -> Vec<String> {
        let mut actions: Vec<String> = self.inner.lock().responders.keys().cloned().collect();
        actions.sort();
        actions
    }

    /// Drops every scripted fault that has not triggered yet.
    pub fn clear_faults(&self) {
        self.inner.lock().faults.clear();
    }

    /// Stops accepting, closes every connection (close frame 1001) and waits for the
    /// sessions to end (at most a couple of seconds). Calling it again is a no-op.
    pub async fn shutdown(&self) {
        self.inner.shutdown.send_replace(true);
        let task = self
            .inner
            .accept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            let _ = task.await;
        }
        // Sessions that were aborted could not report their end themselves.
        let mut state = self.inner.lock();
        for (id, rec) in &mut state.stations {
            if rec.session.take().is_some() {
                rec.info.connected = false;
                rec.info.disconnected_at = Some(Utc::now());
                self.inner.emit(CsmsEvent::Disconnected {
                    station: id.clone(),
                    reason: DisconnectReason::Shutdown,
                });
            }
        }
    }
}

// ---- shared state -----------------------------------------------------------------

struct Inner {
    config: CsmsConfig,
    local_addr: SocketAddr,
    secure: bool,
    state: Mutex<State>,
    events: broadcast::Sender<CsmsEvent>,
    shutdown: watch::Sender<bool>,
    next_message: AtomicU64,
    accept: Mutex<Option<JoinHandle<()>>>,
}

struct State {
    stations: BTreeMap<String, StationRec>,
    faults: Vec<Fault>,
    responders: std::collections::HashMap<String, Responder>,
    tx_ids: TransactionIdAllocator,
    next_session: u64,
}

struct StationRec {
    info: StationInfo,
    history: VecDeque<FrameEntry>,
    state: StationState,
    session: Option<SessionLink>,
}

struct SessionLink {
    id: u64,
    tx: mpsc::UnboundedSender<SessionCmd>,
}

enum SessionCmd {
    Call(QueuedCall),
    Close { code: u16, reason: &'static str },
}

struct QueuedCall {
    action: String,
    payload: Value,
    timeout: Duration,
    reply: oneshot::Sender<Result<Frame, CsmsError>>,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sends an event. Callers hold the state lock where order matters.
    fn emit(&self, event: CsmsEvent) {
        // No receivers is fine.
        let _ = self.events.send(event);
    }

    /// Whether a connection for `id` would exceed [`CsmsConfig::max_stations`].
    fn station_limit_reached(&self, id: &str) -> bool {
        let state = self.lock();
        state.stations.len() >= self.config.max_stations && !state.stations.contains_key(id)
    }

    fn next_message_id(&self) -> String {
        format!(
            "csms-{}",
            self.next_message.fetch_add(1, Ordering::Relaxed) + 1
        )
    }

    /// Registers a new connection, replacing an older one with the same id. `None` if the
    /// server is shutting down.
    fn register(
        &self,
        id: &str,
        peer: SocketAddr,
        subprotocol: Option<String>,
        tx: mpsc::UnboundedSender<SessionCmd>,
    ) -> Option<u64> {
        let mut state = self.lock();
        if *self.shutdown.borrow() {
            return None;
        }
        state.next_session += 1;
        let session_id = state.next_session;
        let rec = state
            .stations
            .entry(id.to_owned())
            .or_insert_with(|| StationRec {
                info: StationInfo {
                    id: id.to_owned(),
                    connected: false,
                    connected_at: None,
                    disconnected_at: None,
                    remote_addr: None,
                    subprotocol: None,
                    received_calls: 0,
                    received_by_action: BTreeMap::new(),
                },
                history: VecDeque::new(),
                state: StationState::default(),
                session: None,
            });
        if let Some(old) = rec.session.take() {
            let _ = old.tx.send(SessionCmd::Close {
                code: 1000,
                reason: "replaced by a new connection",
            });
            self.emit(CsmsEvent::Disconnected {
                station: id.to_owned(),
                reason: DisconnectReason::Replaced,
            });
        }
        rec.session = Some(SessionLink { id: session_id, tx });
        rec.info.connected = true;
        rec.info.connected_at = Some(Utc::now());
        rec.info.disconnected_at = None;
        rec.info.remote_addr = Some(peer);
        rec.info.subprotocol = subprotocol;
        self.emit(CsmsEvent::Connected {
            station: id.to_owned(),
        });
        Some(session_id)
    }

    /// Marks a session as ended, unless it was replaced or forgotten already.
    fn session_ended(&self, station: &str, session_id: u64, reason: DisconnectReason) {
        let mut state = self.lock();
        let Some(rec) = state.stations.get_mut(station) else {
            return;
        };
        if rec.session.as_ref().map(|s| s.id) != Some(session_id) {
            return;
        }
        rec.session = None;
        rec.info.connected = false;
        rec.info.disconnected_at = Some(Utc::now());
        self.emit(CsmsEvent::Disconnected {
            station: station.to_owned(),
            reason,
        });
    }

    fn record_frame(&self, station: &str, session_id: u64, direction: Direction, frame: &Frame) {
        let mut state = self.lock();
        let Some(rec) = state.stations.get_mut(station) else {
            return;
        };
        if rec.session.as_ref().map(|s| s.id) != Some(session_id) {
            return; // A session that was replaced or forgotten must not write into the record.
        }
        if let (Direction::In, Frame::Call { action, .. }) = (direction, frame) {
            rec.info.received_calls += 1;
            let by_action = &mut rec.info.received_by_action;
            let key = if by_action.contains_key(action) || by_action.len() < MAX_ACTION_KEYS {
                action.as_str()
            } else {
                OTHER_ACTION
            };
            *by_action.entry(key.to_owned()).or_insert(0) += 1;
        }
        let cap = self.config.history_cap;
        if cap > 0 {
            while rec.history.len() >= cap {
                rec.history.pop_front();
            }
            rec.history.push_back(FrameEntry {
                ts: Utc::now(),
                direction,
                frame: frame.clone(),
            });
        }
        if self.events.receiver_count() > 0 {
            self.emit(CsmsEvent::Frame {
                station: station.to_owned(),
                direction,
                frame: frame.clone(),
            });
        }
    }

    /// Folds an answered inbound CALL into the station's state.
    fn track(&self, station: &str, session_id: u64, action: &str, request: &Value, reply: &Reply) {
        let Reply::Result(response) = reply else {
            return;
        };
        let mut state = self.lock();
        let Some(rec) = state.stations.get_mut(station) else {
            return;
        };
        if rec.session.as_ref().map(|s| s.id) != Some(session_id) {
            return;
        }
        let cap = self.config.transaction_history_cap;
        if rec.state.apply(action, request, response, Utc::now(), cap) {
            self.emit(CsmsEvent::StateChanged {
                station: station.to_owned(),
            });
        }
    }

    fn note_failed_call(&self, station: &str) {
        let mut state = self.lock();
        if let Some(rec) = state.stations.get_mut(station) {
            rec.state.failed_calls += 1;
            self.emit(CsmsEvent::StateChanged {
                station: station.to_owned(),
            });
        }
    }

    /// Decides the reply to a CALL: fault, then responder, then default; actions a charge
    /// point may not send get `NotSupported`.
    fn decide(&self, action: &str, payload: &Value) -> Reply {
        let mut state = self.lock();
        if let Some(fault) = state
            .faults
            .iter_mut()
            .find(|fault| fault.action == action && fault.remaining > 0)
        {
            fault.remaining -= 1;
            let reply = fault.reply.clone();
            state.faults.retain(|fault| fault.remaining > 0);
            return reply;
        }
        if let Some(responder) = state.responders.get(action).cloned() {
            // Run outside the lock: the closure may call back into the handle.
            drop(state);
            return (*responder)(payload);
        }
        if !is_charge_point_action(action) {
            return Reply::Error(
                ErrorCode::NotSupported,
                format!("action {action} is not supported"),
            );
        }
        let transaction_id = (action == "StartTransaction").then(|| state.tx_ids.allocate());
        default_reply(
            action,
            payload,
            self.config.heartbeat_interval,
            transaction_id,
        )
    }
}

// ---- listener and sessions ----------------------------------------------------------

async fn accept_loop(listener: TcpListener, acceptor: Option<TlsAcceptor>, inner: Arc<Inner>) {
    let mut stop = inner.shutdown.subscribe();
    let mut sessions = JoinSet::new();
    loop {
        tokio::select! {
            () = async { let _ = stop.wait_for(|stopped| *stopped).await; } => break,
            accepted = listener.accept() => match accepted {
                Ok((tcp, peer)) => {
                    sessions.spawn(serve_tcp(tcp, peer, acceptor.clone(), Arc::clone(&inner)));
                }
                // Out of file descriptors and the like: do not spin.
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            },
            Some(_) = sessions.join_next(), if !sessions.is_empty() => {}
        }
    }
    drop(listener);
    // Sessions watch the same flag and close their sockets themselves.
    let _ = timeout(SHUTDOWN_GRACE, async {
        while sessions.join_next().await.is_some() {}
    })
    .await;
    sessions.shutdown().await;
}

async fn serve_tcp(
    tcp: TcpStream,
    peer: SocketAddr,
    acceptor: Option<TlsAcceptor>,
    inner: Arc<Inner>,
) {
    let _ = tcp.set_nodelay(true);
    match acceptor {
        Some(acceptor) => {
            // A failed or stalled TLS handshake is never registered as a connection.
            match timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await {
                Ok(Ok(tls)) => {
                    let leaf = tls
                        .get_ref()
                        .1
                        .peer_certificates()
                        .and_then(|chain| chain.first())
                        .map(AsRef::as_ref);
                    let identity = PeerIdentity::from_leaf(leaf);
                    run_session(tls, peer, identity, inner).await;
                }
                // With profile 3 a failed handshake is most likely a missing or untrusted
                // client certificate; say so (other profiles stay quiet about scanners).
                _ if inner.config.security.profile == SecurityProfile::MutualTls => {
                    inner.emit(CsmsEvent::AuthFailed {
                        station: None,
                        remote_addr: peer,
                        reason: "TLS handshake failed".to_owned(),
                    });
                }
                _ => {}
            }
        }
        None => run_session(tcp, peer, PeerIdentity::Plain, inner).await,
    }
}

async fn run_session<S>(stream: S, peer: SocketAddr, identity: PeerIdentity, inner: Arc<Inner>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut target: Option<(String, Option<String>)> = None;
    let limit = inner.config.max_message_size;
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(limit))
        .max_frame_size(Some(limit));
    // `ErrorResponse` (the refusal) is large, but its type is fixed by tungstenite's `Callback`.
    #[allow(clippy::result_large_err)]
    let callback = |request: &Request, mut response: Response| -> Result<Response, ErrorResponse> {
        let id = station_id_from_path(request.uri().path())
            .ok_or_else(|| refusal("no charge point id in the URL path"))?;
        if let Err(denied) = auth::check(&inner.config.security, request, &id, &identity) {
            inner.emit(CsmsEvent::AuthFailed {
                station: Some(id),
                remote_addr: peer,
                reason: denied.reason.to_owned(),
            });
            return Err(denied.response());
        }
        let chosen = inner
            .config
            .subprotocol
            .select(&offered_protocols(request))
            .map_err(refusal)?;
        if inner.station_limit_reached(&id) {
            return Err(unavailable("too many stations"));
        }
        if let Some(name) = &chosen {
            let value = HeaderValue::from_str(name)
                .map_err(|_| refusal("invalid subprotocol name in configuration"))?;
            response.headers_mut().insert(SEC_WEBSOCKET_PROTOCOL, value);
        }
        target = Some((id, chosen));
        Ok(response)
    };
    let Ok(Ok(socket)) = timeout(
        HANDSHAKE_TIMEOUT,
        accept_hdr_async_with_config(stream, callback, Some(ws_config)),
    )
    .await
    else {
        return;
    };
    let Some((station, subprotocol)) = target else {
        return;
    };

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let Some(session_id) = inner.register(&station, peer, subprotocol, cmd_tx) else {
        return;
    };
    // Ends the session in the registry even if the user's responder panics below.
    let mut guard = EndGuard {
        inner: Arc::clone(&inner),
        station: station.clone(),
        link_id: session_id,
        reason: DisconnectReason::Closed,
    };
    let mut session = Session {
        inner: Arc::clone(&inner),
        link_id: session_id,
        station,
        socket,
        queue: VecDeque::new(),
        in_flight: None,
    };
    guard.reason = session.run(cmd_rx).await;
}

/// Reports the end of a session when dropped, also while unwinding from a panic.
struct EndGuard {
    inner: Arc<Inner>,
    station: String,
    link_id: u64,
    reason: DisconnectReason,
}

impl Drop for EndGuard {
    fn drop(&mut self) {
        self.inner
            .session_ended(&self.station, self.link_id, self.reason);
    }
}

struct InFlight {
    id: String,
    action: String,
    timeout: Duration,
    deadline: Instant,
    reply: oneshot::Sender<Result<Frame, CsmsError>>,
}

struct Session<S> {
    inner: Arc<Inner>,
    link_id: u64,
    station: String,
    socket: WebSocketStream<S>,
    queue: VecDeque<QueuedCall>,
    in_flight: Option<InFlight>,
}

impl<S> Session<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Pumps the socket and the command queue until the session ends. Dropping the session
    /// afterwards fails every queued or in-flight call with `ConnectionClosed`.
    async fn run(&mut self, mut commands: mpsc::UnboundedReceiver<SessionCmd>) -> DisconnectReason {
        let mut stop = self.inner.shutdown.subscribe();
        loop {
            if let Some(reason) = self.pump().await {
                return reason;
            }
            let deadline = self.in_flight.as_ref().map(|flight| flight.deadline);
            let ended = tokio::select! {
                () = async { let _ = stop.wait_for(|stopped| *stopped).await; } => {
                    self.close(1001, "server shutting down").await;
                    Some(DisconnectReason::Shutdown)
                }
                inbound = self.socket.next() => match inbound {
                    Some(Ok(Message::Text(text))) => self.on_text(text.as_str()).await,
                    Some(Ok(Message::Close(_)) | Err(_)) | None => Some(DisconnectReason::Closed),
                    Some(Ok(_)) => None,
                },
                command = commands.recv() => match command {
                    Some(SessionCmd::Call(call)) => {
                        self.queue.push_back(call);
                        None
                    }
                    Some(SessionCmd::Close { code, reason }) => {
                        self.close(code, reason).await;
                        Some(DisconnectReason::Closed)
                    }
                    None => Some(DisconnectReason::Closed),
                },
                // The caller dropped its `call()` future: free the slot, a late reply is ignored.
                () = async {
                    match self.in_flight.as_mut() {
                        Some(flight) => flight.reply.closed().await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.in_flight = None;
                    None
                }
                () = async {
                    match deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.expire_in_flight();
                    None
                }
            };
            if let Some(reason) = ended {
                return reason;
            }
        }
    }

    /// Writes the next queued CALL if none is outstanding.
    async fn pump(&mut self) -> Option<DisconnectReason> {
        while self.in_flight.is_none() {
            let call = self.queue.pop_front()?;
            if call.reply.is_closed() {
                continue; // The caller gave up before it was sent.
            }
            let id = self.inner.next_message_id();
            let frame = Frame::call(id.clone(), call.action.clone(), call.payload);
            if let Some(reason) = self.send(&frame).await {
                // `call.reply` is dropped here, which the caller sees as ConnectionClosed.
                return Some(reason);
            }
            self.in_flight = Some(InFlight {
                id,
                action: call.action,
                timeout: call.timeout,
                deadline: Instant::now()
                    .checked_add(call.timeout)
                    .unwrap_or_else(|| Instant::now() + MAX_CALL_TIMEOUT),
                reply: call.reply,
            });
        }
        None
    }

    fn expire_in_flight(&mut self) {
        if let Some(flight) = self.in_flight.take() {
            let _ = flight.reply.send(Err(CsmsError::Timeout {
                station: self.station.clone(),
                action: flight.action,
                timeout: flight.timeout,
            }));
        }
    }

    async fn on_text(&mut self, text: &str) -> Option<DisconnectReason> {
        let frame = match Frame::parse(text) {
            Ok(frame) => frame,
            Err(error) => {
                self.inner.emit(CsmsEvent::Malformed {
                    station: self.station.clone(),
                    text: text.chars().take(MALFORMED_TEXT_CHARS).collect(),
                });
                // Only a CALL with a readable id gets a FormationViolation; the rest is dropped.
                return match error.reply() {
                    Some(reply) => self.send(&reply).await,
                    None => None,
                };
            }
        };
        self.inner
            .record_frame(&self.station, self.link_id, Direction::In, &frame);
        match frame {
            Frame::Call {
                id,
                action,
                payload,
            } => {
                let reply = self.inner.decide(&action, &payload);
                self.inner
                    .track(&self.station, self.link_id, &action, &payload, &reply);
                match reply {
                    Reply::Result(payload) => self.send(&Frame::call_result(id, payload)).await,
                    Reply::Error(code, description) => {
                        self.send(&Frame::call_error(id, code, description)).await
                    }
                    Reply::Silence => None,
                    Reply::Close => {
                        self.close(CLOSE_CODE_ERROR, "injected close").await;
                        Some(DisconnectReason::Injected)
                    }
                }
            }
            Frame::CallResult { .. } | Frame::CallError { .. } => {
                if self
                    .in_flight
                    .as_ref()
                    .is_some_and(|flight| flight.id == frame.id())
                    && let Some(flight) = self.in_flight.take()
                {
                    let _ = flight.reply.send(Ok(frame));
                }
                // Anything else answers a CALL that already timed out, or was never sent.
                None
            }
        }
    }

    /// Records and writes a frame. `Some` means the socket is gone.
    async fn send(&mut self, frame: &Frame) -> Option<DisconnectReason> {
        self.inner
            .record_frame(&self.station, self.link_id, Direction::Out, frame);
        let write = self.socket.send(Message::text(frame.to_json_string()));
        match timeout(self.inner.config.write_timeout, write).await {
            Ok(Ok(())) => None,
            // Failed, or the peer stopped reading for too long.
            Ok(Err(_)) | Err(_) => Some(DisconnectReason::Closed),
        }
    }

    async fn close(&mut self, code: u16, reason: &str) {
        let close = Message::Close(Some(CloseFrame {
            code: CloseCode::from(code),
            reason: reason.to_owned().into(),
        }));
        let _ = timeout(CLOSE_WRITE_TIMEOUT, self.socket.send(close)).await;
    }
}

// ---- handshake helpers --------------------------------------------------------------

/// The station id of a URL path: the last non-empty segment, percent-decoded. `None` if
/// there is none or it is not valid UTF-8 after decoding.
pub fn station_id_from_path(path: &str) -> Option<String> {
    let segment = path.rsplit('/').find(|segment| !segment.is_empty())?;
    let decoded = percent_encoding::percent_decode_str(segment)
        .decode_utf8()
        .ok()?;
    (!decoded.is_empty()).then(|| decoded.into_owned())
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

/// An HTTP 503 refusal.
fn unavailable(reason: &str) -> ErrorResponse {
    let mut response = ErrorResponse::new(Some(reason.to_owned()));
    *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    response
}

/// An HTTP 400 refusal.
fn refusal(reason: &str) -> ErrorResponse {
    let mut response = ErrorResponse::new(Some(reason.to_owned()));
    *response.status_mut() = StatusCode::BAD_REQUEST;
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn station_id_is_last_decoded_segment() {
        assert_eq!(station_id_from_path("/ocpp/CP-1"), Some("CP-1".to_owned()));
        assert_eq!(
            station_id_from_path("/a/b/c/CP%2D2/"),
            Some("CP-2".to_owned())
        );
        assert_eq!(station_id_from_path("/CP%20X"), Some("CP X".to_owned()));
        assert_eq!(station_id_from_path("/"), None);
        assert_eq!(station_id_from_path("/x/%FF"), None);
    }
}
