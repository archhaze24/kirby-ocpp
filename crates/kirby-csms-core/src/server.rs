use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use kirby_ocpp_core::ocpp::frame::{Direction as RecordDirection, FrameRecord};
use kirby_ocpp_core::ocpp::{ErrorCode, Frame, Protocol, SchemaRegistry, redact_secrets};
use kirby_ocpp_core::recording::RecordSink;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast, mpsc, oneshot, watch};
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
use crate::defaults::{default_reply_for, is_charge_point_action_for};
use crate::error::CsmsError;
use crate::event::{CsmsEvent, Direction, DisconnectReason, FrameEntry, StationInfo};
use crate::fault::{Fault, Reply, Responder};
use crate::observe::{self, MAX_OBSERVED_ID_TAG, Observation, ObserveSender, Observer, SessionId};
use crate::policy::TransactionIdAllocator;
use crate::rules::{FaultInfo, RuleInfo, Scope, Scripted};
use crate::schemas::{EngineSchemas, session_protocol};
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
/// Most CALLs that may wait for one station (it answers one at a time); more are refused.
const MAX_QUEUED_CALLS: usize = 1024;
/// Most distinct actions counted per station; further ones are counted as `other`.
const MAX_ACTION_KEYS: usize = 64;
/// Bucket of `received_by_action` for actions beyond [`MAX_ACTION_KEYS`].
const OTHER_ACTION: &str = "other";
/// Longest action name kept as a key of `received_by_action`, in characters; longer names are
/// counted under `other`.
const MAX_ACTION_KEY_CHARS: usize = 64;
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
/// sent when the previous one was answered, timed out or abandoned. The timeout counts from
/// the moment `call()` is made, so it covers the time spent queued too: a call that waited
/// longer than its timeout fails with [`CsmsError::Timeout`] without being written. Dropping
/// the future of a call cancels it. After the CALL was written the answer is discarded and the
/// session waits at most the default call timeout ([`CsmsConfig::call_timeout`]) more for
/// it; after that the next queued CALL may be written while the station is still working on the
/// abandoned one, and a late answer to it is recorded but ignored.
///
/// # Memory and the registry
///
/// Station records, their state and their frame history are never evicted by the server: a
/// station id stays in the registry (up to [`CsmsConfig::max_stations`]) until
/// [`CsmsHandle::forget`] removes it. Each record is bounded ([`CsmsConfig::history_cap`] frames
/// and [`CsmsConfig::history_bytes_cap`] bytes of history, a bounded number of action
/// counters), but a CSMS that accepts connections from untrusted networks should still call
/// `forget` for stations it no longer wants. The default [`SecurityPolicy`] is unauthenticated
/// (profile 0): anybody who can reach the listener may register stations; use a security
/// profile or a firewall.
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
        Self::start_inner(config, None, None)
            .await
            .map(|(handle, _)| handle)
    }

    /// Starts the server with a recorder attached before connections are accepted.
    ///
    /// # Errors
    /// Returns the same errors as [`Self::start`].
    pub async fn start_recorded(
        config: CsmsConfig,
        sink: RecordSink,
    ) -> Result<CsmsHandle, CsmsError> {
        Self::start_inner(config, Some(sink), None)
            .await
            .map(|(handle, _)| handle)
    }

    /// Starts the server with the optional extras of [`StartOptions`], all attached before
    /// connections are accepted. The [`Observer`] is returned when
    /// [`StartOptions::observe`] asked for one.
    ///
    /// # Errors
    /// Returns the same errors as [`Self::start`].
    pub async fn start_with(
        config: CsmsConfig,
        options: StartOptions,
    ) -> Result<(CsmsHandle, Option<Observer>), CsmsError> {
        Self::start_inner(config, options.recording, options.observe).await
    }

    async fn start_inner(
        config: CsmsConfig,
        recording: Option<RecordSink>,
        observe: Option<usize>,
    ) -> Result<(CsmsHandle, Option<Observer>), CsmsError> {
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
        if config.security.has_empty_key() {
            return Err(CsmsError::Config(
                "security keys must not be empty".to_owned(),
            ));
        }
        let listener = TcpListener::bind(config.listen)
            .await
            .map_err(CsmsError::Bind)?;
        let local_addr = listener.local_addr().map_err(CsmsError::Bind)?;
        let acceptor = config.tls.clone().map(TlsAcceptor::from);
        let (events, _) = broadcast::channel(config.event_buffer.max(1));
        let (shutdown, _) = watch::channel(false);
        let (observer_tx, observer) = match observe.map(observe::channel) {
            Some((tx, rx)) => (Some(tx), Some(rx)),
            None => (None, None),
        };
        let inner = Arc::new(Inner {
            local_addr,
            secure: acceptor.is_some(),
            state: Mutex::new(State {
                stations: BTreeMap::new(),
                scripted: Scripted::default(),
                responders: std::collections::HashMap::new(),
                tx_ids: config.transaction_ids.allocator(),
                next_session: 0,
                frame_seq: unix_micros(),
                history_dropped: 0,
            }),
            config,
            schemas: EngineSchemas::load(),
            recording,
            observer: observer_tx,
            events,
            shutdown,
            next_message: AtomicU64::new(0),
            accept: Mutex::new(None),
        });
        let task = tokio::spawn(accept_loop(listener, acceptor, Arc::clone(&inner)));
        *inner.accept.lock().unwrap_or_else(PoisonError::into_inner) = Some(task);
        Ok((CsmsHandle { inner }, observer))
    }
}

/// Extras that must be attached before the server accepts its first connection; see
/// [`CsmsServer::start_with`].
#[derive(Debug, Default)]
pub struct StartOptions {
    /// Record every frame of every station.
    pub recording: Option<RecordSink>,
    /// Feed an [`Observer`] with this many pending observations at most. `None` observes
    /// nothing and costs nothing.
    pub observe: Option<usize>,
}

/// Cheap-to-clone handle to a running server. See [`CsmsServer`].
#[derive(Clone)]
pub struct CsmsHandle {
    inner: Arc<Inner>,
}

/// An opaque reference to one connected session on one server.
///
/// A replacement connection gets a different token, even when its station id is unchanged.
#[derive(Clone)]
pub struct ConnectedSession {
    station: String,
    protocol: Option<String>,
    generation: u64,
    server: Weak<Inner>,
}

impl ConnectedSession {
    /// The station id of this session.
    pub fn station(&self) -> &str {
        &self.station
    }

    /// The negotiated WebSocket subprotocol.
    pub fn protocol(&self) -> Option<&str> {
        self.protocol.as_deref()
    }

    /// The OCPP version of the negotiated subprotocol; `None` when none was negotiated or the
    /// name is not one this build knows.
    pub fn protocol_kind(&self) -> Option<Protocol> {
        self.protocol.as_deref().and_then(Protocol::from_token)
    }

    /// The identity of this connection in [`Observation`]s.
    pub fn id(&self) -> SessionId {
        SessionId(self.generation)
    }
}

impl fmt::Debug for ConnectedSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectedSession")
            .field("station", &self.station)
            .field("protocol", &self.protocol)
            .finish_non_exhaustive()
    }
}

impl PartialEq for ConnectedSession {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation && Weak::ptr_eq(&self.server, &other.server)
    }
}

impl Eq for ConnectedSession {}

impl std::hash::Hash for ConnectedSession {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.generation, state);
        std::hash::Hash::hash(&self.server.as_ptr(), state);
    }
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

    /// The configured connection security profile.
    pub fn security_profile(&self) -> SecurityProfile {
        self.inner.config.security.profile
    }

    /// Whether secrets are shown as received ([`CsmsConfig::show_secrets`]); by default frames
    /// and replies handed to callers have them masked.
    pub fn show_secrets(&self) -> bool {
        self.inner.config.show_secrets
    }

    /// Subscribes to server events. See [`CsmsEvent`] for lag behaviour.
    pub fn subscribe(&self) -> broadcast::Receiver<CsmsEvent> {
        self.inner.events.subscribe()
    }

    /// A token for the current connection of a station, or `None` when disconnected.
    pub fn connected_session(&self, station: &str) -> Option<ConnectedSession> {
        let state = self.inner.lock();
        let rec = state.stations.get(station)?;
        self.session_token(rec)
    }

    /// The protocol `station` negotiated; 1.6 for an unknown station, a station without a
    /// subprotocol and a subprotocol name this build does not know.
    pub(crate) fn station_protocol(&self, station: &str) -> Protocol {
        let state = self.inner.lock();
        session_protocol(
            state
                .stations
                .get(station)
                .and_then(|rec| rec.info.subprotocol.as_deref()),
        )
    }

    /// The schemas of `protocol`; `None` for a protocol this build has none for, which
    /// leaves its payloads unchecked.
    pub(crate) fn schemas_for(&self, protocol: Protocol) -> Option<&SchemaRegistry> {
        self.inner.schemas.schemas_for(protocol)
    }

    /// Tokens for every currently connected session.
    pub fn connected_sessions(&self) -> Vec<ConnectedSession> {
        self.inner
            .lock()
            .stations
            .values()
            .filter_map(|rec| self.session_token(rec))
            .collect()
    }

    fn session_token(&self, rec: &StationRec) -> Option<ConnectedSession> {
        Some(ConnectedSession {
            station: rec.info.id.clone(),
            protocol: rec.info.subprotocol.clone(),
            generation: rec.session.as_ref()?.id,
            server: Arc::downgrade(&self.inner),
        })
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

    /// How many stations the registry holds and how many of them are connected.
    pub fn station_counts(&self) -> (usize, usize) {
        let state = self.inner.lock();
        let connected = state
            .stations
            .values()
            .filter(|rec| rec.info.connected)
            .count();
        (state.stations.len(), connected)
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
            .map(|rec| rec.history.iter().map(|item| item.entry.clone()).collect())
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

    /// The newest `limit` frames of a station, oldest first (like [`CsmsHandle::history`], but
    /// clones only the tail).
    pub fn history_tail(&self, id: &str, limit: usize) -> Option<Vec<FrameEntry>> {
        self.inner.lock().stations.get(id).map(|rec| {
            let skip = rec.history.len().saturating_sub(limit);
            rec.history
                .iter()
                .skip(skip)
                .map(|item| item.entry.clone())
                .collect()
        })
    }

    /// The frames of a station that are newer than the sequence `after` (all of them for
    /// `None`), oldest first, each with its sequence number. Sequence numbers increase over
    /// every station and never repeat during one run. See [`HistoryPage`].
    pub fn history_since(&self, id: &str, after: Option<u64>) -> Option<HistoryPage> {
        let state = self.inner.lock();
        let newest = state.frame_seq;
        state.stations.get(id).map(|rec| {
            let after = after.unwrap_or(0);
            let start = rec.history.partition_point(|item| item.seq <= after);
            HistoryPage {
                frames: rec
                    .history
                    .iter()
                    .skip(start)
                    .map(|item| (item.seq, item.entry.clone()))
                    .collect(),
                dropped_through: rec.dropped_seq,
                newest,
            }
        })
    }

    /// How many frames left a station history for good so far (evicted by a cap, or too big
    /// to be kept), over every station of this run.
    pub fn history_dropped(&self) -> u64 {
        self.inner.lock().history_dropped
    }

    /// Up to `limit` transactions of the registry, ordered by station id and then as the
    /// station state holds them, with the station id. `active` keeps only the running
    /// (`Some(true)`) or only the stopped ones, `since` those that started (by the station's
    /// clock) at or after it. The flag is `true` when more matched than `limit`.
    pub fn transactions(
        &self,
        station: Option<&str>,
        active: Option<bool>,
        since: Option<chrono::DateTime<Utc>>,
        limit: usize,
    ) -> (Vec<(String, crate::state::TransactionRecord)>, bool) {
        let state = self.inner.lock();
        let mut found = Vec::new();
        let mut more = false;
        for (id, rec) in &state.stations {
            if station.is_some_and(|wanted| wanted != id) {
                continue;
            }
            for tx in &rec.state.transactions {
                let running = tx.status == crate::state::TransactionStatus::Active;
                if active.is_some_and(|wanted| wanted != running)
                    || since.is_some_and(|since| tx.started_at < since)
                {
                    continue;
                }
                if found.len() >= limit {
                    more = true;
                    return (found, more);
                }
                found.push((id.clone(), tx.clone()));
            }
        }
        (found, more)
    }

    /// Removes a station from the registry, closing its connection if it has one.
    /// Returns whether the station existed.
    pub fn forget(&self, id: &str) -> bool {
        self.forget_inner(id, false) == Some(true)
    }

    /// Like [`CsmsHandle::forget`], but only when the station has no open connection, decided
    /// atomically with the removal. `Some(true)`: forgotten; `Some(false)`: the station is
    /// connected and was left alone; `None`: unknown station.
    pub fn forget_if_disconnected(&self, id: &str) -> Option<bool> {
        self.forget_inner(id, true)
    }

    fn forget_inner(&self, id: &str, only_disconnected: bool) -> Option<bool> {
        let mut state = self.inner.lock();
        if only_disconnected && state.stations.get(id)?.session.is_some() {
            return Some(false);
        }
        let rec = state.stations.remove(id)?;
        if let Some(session) = rec.session {
            let _ = session.tx.send(SessionCmd::Close {
                code: 1000,
                reason: "station forgotten",
            });
            self.inner.observe(|| Observation::Ended {
                station: id.to_owned(),
                session: SessionId(session.id),
            });
            self.inner.emit(CsmsEvent::Disconnected {
                station: id.to_owned(),
                reason: DisconnectReason::Forgotten,
            });
        }
        self.inner.emit(CsmsEvent::Forgotten {
            station: id.to_owned(),
        });
        Some(true)
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
        self.call_inner(station, action, payload, timeout, None)
            .await
    }

    /// Sends only when `session` is still the station's current connection on this server.
    /// The identity check and enqueue happen under the same registry lock.
    pub async fn call_session_timeout(
        &self,
        session: &ConnectedSession,
        action: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Frame, CsmsError> {
        self.call_inner(session.station(), action, payload, timeout, Some(session))
            .await
    }

    async fn call_inner(
        &self,
        station: &str,
        action: &str,
        payload: Value,
        timeout: Duration,
        expected: Option<&ConnectedSession>,
    ) -> Result<Frame, CsmsError> {
        let timeout = timeout.min(MAX_CALL_TIMEOUT);
        let tracked_request = (action == "GetInstalledCertificateIds").then(|| payload.clone());
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(|| Instant::now() + MAX_CALL_TIMEOUT);
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
            if expected.is_some_and(|token| {
                token.generation != session.id
                    || !Weak::ptr_eq(&token.server, &Arc::downgrade(&self.inner))
            }) {
                return Err(not_connected());
            }
            let Some(slot) = session.slots.reserve() else {
                // Calls whose callers gave up may hold the slots: have the session drop them.
                if !session.slots.prune.swap(true, Ordering::AcqRel) {
                    let _ = session.tx.send(SessionCmd::Prune);
                }
                return Err(CsmsError::QueueFull {
                    station: station.to_owned(),
                });
            };
            session
                .tx
                .send(SessionCmd::Call(QueuedCall {
                    action: action.to_owned(),
                    payload,
                    timeout,
                    deadline,
                    reply,
                    _slot: slot,
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
        if let (Some(request), Ok(Frame::CallResult { payload, .. })) = (&tracked_request, &result)
        {
            self.inner.track_result(station, action, request, payload);
        }
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

    /// Removes the responder for `action`; returns whether there was one.
    pub fn clear_responder(&self, action: &str) -> bool {
        self.inner.lock().responders.remove(action).is_some()
    }

    /// Scripts a fault for the next occurrences of an action, across all stations. Faults
    /// are used in the order injected.
    pub fn inject(&self, fault: Fault) {
        let _ = self.inject_scoped(fault, Scope::global());
    }

    /// Like [`CsmsHandle::inject`], but for one [`Scope`] (a station, a protocol or both).
    /// Returns the id of the stored fault (`None` for a fault with no occurrences). Stores
    /// any fault, without the caps of [`CsmsHandle::try_inject_scoped`].
    pub fn inject_scoped(&self, fault: Fault, scope: Scope) -> Option<String> {
        if fault.remaining == 0 {
            return None;
        }
        self.inner
            .lock()
            .scripted
            .add_fault(fault, scope, false)
            .ok()
    }

    /// Like [`CsmsHandle::inject`], but refuses a fault that would take the stored faults over
    /// 1000 entries or 8 MiB of action and payload text. Only the `try_` methods enforce the
    /// caps: [`CsmsHandle::inject`] (used by the terminal UI) stores any fault.
    pub fn try_inject(&self, fault: Fault) -> Result<(), String> {
        self.try_inject_scoped(fault, Scope::global()).map(|_| ())
    }

    /// Like [`CsmsHandle::try_inject`] for one [`Scope`]; also refuses more than 100 faults
    /// for one station. Returns the id of the fault (an empty string for a fault with no
    /// occurrences, which is not stored).
    pub fn try_inject_scoped(&self, fault: Fault, scope: Scope) -> Result<String, String> {
        if fault.remaining == 0 {
            return Ok(String::new());
        }
        self.inner.lock().scripted.add_fault(fault, scope, true)
    }

    /// Number of fault occurrences still waiting to be triggered, over all scopes.
    pub fn faults_remaining(&self) -> usize {
        self.inner.lock().scripted.faults_remaining()
    }

    /// The stored faults in injection order.
    pub fn faults(&self) -> Vec<FaultInfo> {
        self.inner.lock().scripted.faults()
    }

    /// Removes one fault by id; returns whether it was still stored.
    pub fn remove_fault(&self, id: &str) -> bool {
        self.inner.lock().scripted.remove_fault(id)
    }

    /// Sets the fixed CALLRESULT payload for `action` in `scope`, replacing the rule of the
    /// same station (or the same protocol-scoped global rule) and action. A scope with a
    /// station or a protocol is stored as data; refuses a rule beyond 1000 rules, 64 for one
    /// station or 8 MiB of text.
    pub fn respond_scoped(
        &self,
        scope: &Scope,
        action: &str,
        payload: Value,
    ) -> Result<(), String> {
        self.inner.lock().scripted.set_rule(scope, action, payload)
    }

    /// Removes the scoped rule of `station` (`None`: the global rule with a protocol) for
    /// `action`; returns whether there was one.
    pub fn clear_scoped_rule(&self, station: Option<&str>, action: &str) -> bool {
        self.inner.lock().scripted.clear_rule(station, action)
    }

    /// Clears the global rule for `action` only when it names `protocol`, under one lock.
    pub fn clear_global_rule_for_protocol(&self, action: &str, protocol: &str) -> bool {
        self.inner
            .lock()
            .scripted
            .clear_global_rule_for_protocol(action, protocol)
    }

    /// The scoped rule of one station for `action`.
    pub fn scoped_rule(&self, station: &str, action: &str) -> Option<RuleInfo> {
        self.inner.lock().scripted.rule(station, action)
    }

    /// The scoped rules of one station (`Some`) or of every station and the global rules with
    /// a protocol (`None`), sorted by station then action.
    pub fn scoped_rules(&self, station: Option<&str>) -> Vec<RuleInfo> {
        self.inner.lock().scripted.rules(station)
    }

    /// Drops every scoped rule (not the global responders).
    pub fn clear_scoped_rules(&self) {
        self.inner.lock().scripted.clear_rules();
    }

    /// Actions that currently have a responder, sorted.
    pub fn responder_actions(&self) -> Vec<String> {
        let mut actions: Vec<String> = self.inner.lock().responders.keys().cloned().collect();
        actions.sort();
        actions
    }

    /// Drops every scripted fault that has not triggered yet, in every scope.
    pub fn clear_faults(&self) {
        self.inner.lock().scripted.clear_faults();
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
            if let Some(session) = rec.session.take() {
                self.inner.observe(|| Observation::Ended {
                    station: id.clone(),
                    session: SessionId(session.id),
                });
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
    /// Schemas per protocol, loaded once for this server.
    schemas: EngineSchemas,
    recording: Option<RecordSink>,
    observer: Option<ObserveSender>,
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
    /// Faults and scoped rules (per station and protocol), with their ids and caps.
    scripted: Scripted,
    /// Global rules without a protocol: one closure per action.
    responders: std::collections::HashMap<String, Responder>,
    tx_ids: TransactionIdAllocator,
    next_session: u64,
    /// Sequence of the last frame recorded in any station history. It starts at the start time
    /// in microseconds, so a sequence from an earlier run is smaller than every one of this run.
    frame_seq: u64,
    /// Frames that left a history for good: evicted by a cap, or too big to be kept.
    history_dropped: u64,
}

/// Microseconds since the Unix epoch (0 before it).
fn unix_micros() -> u64 {
    u64::try_from(Utc::now().timestamp_micros()).unwrap_or(0)
}

/// A page of a station's frame history, see [`CsmsHandle::history_since`].
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryPage {
    /// Every frame still held that is newer than `after`, oldest first, with its sequence
    /// number.
    pub frames: Vec<(u64, FrameEntry)>,
    /// The largest sequence of a frame of this station that left the history without being
    /// kept (0 when none did): a cursor older than this has lost frames.
    pub dropped_through: u64,
    /// The sequence of the newest frame recorded in any history so far.
    pub newest: u64,
}

struct StationRec {
    info: StationInfo,
    history: VecDeque<HistoryItem>,
    /// Sequence of the newest frame of this station that was evicted or not kept.
    dropped_seq: u64,
    /// Sum of `HistoryItem::bytes`.
    history_bytes: usize,
    state: StationState,
    session: Option<SessionLink>,
}

struct HistoryItem {
    /// Position in the sequence of [`State::frame_seq`].
    seq: u64,
    entry: FrameEntry,
    /// Size of the frame as JSON text.
    bytes: usize,
}

struct SessionLink {
    id: u64,
    tx: mpsc::UnboundedSender<SessionCmd>,
    slots: Arc<CallSlots>,
}

/// Calls of one session that [`CsmsHandle::call`] accepted and that have not left its queue
/// yet (in the command channel or in the session's queue). Counted at call time, because the
/// channel is unbounded and the session loop may be stuck in a write.
#[derive(Default)]
struct CallSlots {
    used: AtomicUsize,
    /// A `Prune` command is on its way.
    prune: AtomicBool,
}

impl CallSlots {
    /// Takes a slot, or `None` when [`MAX_QUEUED_CALLS`] are in use.
    fn reserve(self: &Arc<Self>) -> Option<CallSlot> {
        if self.used.fetch_add(1, Ordering::AcqRel) >= MAX_QUEUED_CALLS {
            self.used.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(CallSlot(Arc::clone(self)))
    }
}

/// A taken slot of [`CallSlots`]; freed when dropped.
struct CallSlot(Arc<CallSlots>);

impl Drop for CallSlot {
    fn drop(&mut self) {
        self.0.used.fetch_sub(1, Ordering::AcqRel);
    }
}

enum SessionCmd {
    Call(QueuedCall),
    /// Drop queued calls whose callers gave up.
    Prune,
    Close {
        code: u16,
        reason: &'static str,
    },
}

struct QueuedCall {
    action: String,
    payload: Value,
    timeout: Duration,
    /// When the call times out, counted from `call()` (queueing included).
    deadline: Instant,
    reply: oneshot::Sender<Result<Frame, CsmsError>>,
    /// Freed when the call leaves the queue.
    _slot: CallSlot,
}

enum RegisterError {
    Shutdown,
    TooManyStations,
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

    /// Feeds the observer, if there is one; the closure is not run otherwise. Callers hold
    /// the state lock so that the order matches the registry.
    fn observe(&self, make: impl FnOnce() -> Observation) {
        if let Some(observer) = &self.observer {
            observer.send(make());
        }
    }

    /// Reports an answered `BootNotification`, `StartTransaction` or `StopTransaction` of the
    /// session, unless the session was replaced or forgotten meanwhile.
    fn observe_inbound(
        &self,
        station: &str,
        session_id: u64,
        action: &str,
        request: &Value,
        reply: &Reply,
    ) {
        if self.observer.is_none()
            || !matches!(
                action,
                "BootNotification" | "StartTransaction" | "StopTransaction"
            )
        {
            return;
        }
        let state = self.lock();
        if state
            .stations
            .get(station)
            .and_then(|rec| rec.session.as_ref())
            .map(|s| s.id)
            != Some(session_id)
        {
            return;
        }
        let station = station.to_owned();
        let session = SessionId(session_id);
        let int = |value: Option<&Value>| {
            value
                .and_then(Value::as_i64)
                .and_then(|n| i32::try_from(n).ok())
        };
        self.observe(|| match action {
            "BootNotification" => Observation::Boot {
                station,
                session,
                accepted: matches!(reply, Reply::Result(r) if r.get("status").and_then(Value::as_str) == Some("Accepted")),
            },
            "StartTransaction" => Observation::Start {
                station,
                session,
                connector: request
                    .get("connectorId")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok()),
                id_tag: request
                    .get("idTag")
                    .and_then(Value::as_str)
                    .filter(|tag| tag.len() <= MAX_OBSERVED_ID_TAG)
                    .map(str::to_owned),
                transaction_id: match reply {
                    Reply::Result(r) => int(r.get("transactionId")),
                    _ => None,
                },
            },
            _ => Observation::Stop {
                station,
                session,
                transaction_id: int(request.get("transactionId")),
            },
        });
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

    /// Registers a new connection, replacing an older one with the same id. Refused if the
    /// server is shutting down or the station limit was reached in the meantime.
    fn register(
        self: &Arc<Self>,
        id: &str,
        peer: SocketAddr,
        subprotocol: Option<String>,
        tx: mpsc::UnboundedSender<SessionCmd>,
        slots: Arc<CallSlots>,
    ) -> Result<u64, RegisterError> {
        let mut state = self.lock();
        if *self.shutdown.borrow() {
            return Err(RegisterError::Shutdown);
        }
        // The handshake callback checked this too, but between the two the registry may have
        // filled up (several handshakes can pass the check together).
        if state.stations.len() >= self.config.max_stations && !state.stations.contains_key(id) {
            return Err(RegisterError::TooManyStations);
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
                dropped_seq: 0,
                history_bytes: 0,
                state: StationState::default(),
                session: None,
            });
        if let Some(old) = rec.session.take() {
            let _ = old.tx.send(SessionCmd::Close {
                code: 1000,
                reason: "replaced by a new connection",
            });
            self.observe(|| Observation::Ended {
                station: id.to_owned(),
                session: SessionId(old.id),
            });
            self.emit(CsmsEvent::Disconnected {
                station: id.to_owned(),
                reason: DisconnectReason::Replaced,
            });
        }
        rec.session = Some(SessionLink {
            id: session_id,
            tx,
            slots,
        });
        rec.info.connected = true;
        rec.info.connected_at = Some(Utc::now());
        rec.info.disconnected_at = None;
        rec.info.remote_addr = Some(peer);
        rec.info.subprotocol = subprotocol;
        self.observe(|| {
            Observation::Connected(ConnectedSession {
                station: id.to_owned(),
                protocol: rec.info.subprotocol.clone(),
                generation: session_id,
                server: Arc::downgrade(self),
            })
        });
        self.emit(CsmsEvent::Connected {
            station: id.to_owned(),
        });
        Ok(session_id)
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
        self.observe(|| Observation::Ended {
            station: station.to_owned(),
            session: SessionId(session_id),
        });
        self.emit(CsmsEvent::Disconnected {
            station: station.to_owned(),
            reason,
        });
    }

    fn record_frame(&self, station: &str, session_id: u64, direction: Direction, frame: &Frame) {
        let mut guard = self.lock();
        let state = &mut *guard;
        if let Some(sink) = &self.recording {
            sink.frame_for_station(
                station,
                session_id,
                &FrameRecord {
                    ts: Utc::now(),
                    direction: match direction {
                        Direction::In => RecordDirection::In,
                        Direction::Out => RecordDirection::Out,
                    },
                    msg_id: None,
                    action: None,
                    frame: frame.to_value(),
                },
            );
        }
        let Some(rec) = state.stations.get_mut(station) else {
            return;
        };
        if rec.session.as_ref().map(|s| s.id) != Some(session_id) {
            return; // A session that was replaced or forgotten must not write into the record.
        }
        state.frame_seq += 1;
        let seq = state.frame_seq;
        if let (Direction::In, Frame::Call { action, .. }) = (direction, frame) {
            rec.info.received_calls += 1;
            let by_action = &mut rec.info.received_by_action;
            let short =
                action.chars().take(MAX_ACTION_KEY_CHARS + 1).count() <= MAX_ACTION_KEY_CHARS;
            let key =
                if short && (by_action.contains_key(action) || by_action.len() < MAX_ACTION_KEYS) {
                    action.as_str()
                } else {
                    OTHER_ACTION
                };
            *by_action.entry(key.to_owned()).or_insert(0) += 1;
        }
        let redacted;
        let frame = if self.config.show_secrets {
            frame
        } else {
            redacted = redact_secrets(frame);
            &redacted
        };
        let cap = self.config.history_cap;
        if cap > 0 {
            let bytes_cap = self.config.history_bytes_cap;
            let mut stored = frame.clone();
            let mut bytes = stored.to_json_string().len();
            if bytes > bytes_cap {
                stored = truncated_frame(frame, bytes);
                bytes = stored.to_json_string().len();
            }
            while rec.history.len() >= cap
                || (rec.history_bytes.saturating_add(bytes) > bytes_cap && !rec.history.is_empty())
            {
                if let Some(old) = rec.history.pop_front() {
                    rec.history_bytes = rec.history_bytes.saturating_sub(old.bytes);
                    rec.dropped_seq = rec.dropped_seq.max(old.seq);
                    state.history_dropped += 1;
                }
            }
            if rec.history_bytes.saturating_add(bytes) <= bytes_cap {
                rec.history_bytes += bytes;
                rec.history.push_back(HistoryItem {
                    seq,
                    entry: FrameEntry {
                        ts: Utc::now(),
                        direction,
                        frame: stored,
                    },
                    bytes,
                });
            } else {
                rec.dropped_seq = rec.dropped_seq.max(seq);
                state.history_dropped += 1;
            }
        }
        if self.events.receiver_count() > 0 {
            self.emit(CsmsEvent::Frame {
                station: station.to_owned(),
                direction,
                frame: event_frame(frame),
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

    /// Folds the answer of a CSMS-originated CALL into the station's state.
    fn track_result(&self, station: &str, action: &str, request: &Value, response: &Value) {
        let mut state = self.lock();
        let Some(rec) = state.stations.get_mut(station) else {
            return;
        };
        if rec
            .state
            .apply_result(action, request, response, Utc::now())
        {
            self.emit(CsmsEvent::StateChanged {
                station: station.to_owned(),
            });
        }
    }

    /// Decides the reply to a CALL from `station`: fault (the station's own first, then
    /// global ones), then schema validation (`validate_inbound`), then the station's rule, a
    /// global rule with a protocol, a global responder, then the default; known actions a
    /// charge point may not send get `NotSupported`, unknown ones `NotImplemented`.
    fn decide(&self, station: &str, action: &str, payload: &Value) -> Reply {
        let mut state = self.lock();
        let subprotocol = state
            .stations
            .get(station)
            .and_then(|rec| rec.info.subprotocol.clone());
        let protocol = session_protocol(subprotocol.as_deref());
        if let Some(reply) = state
            .scripted
            .take_fault(station, subprotocol.as_deref(), action)
        {
            return reply;
        }
        if self.config.validate_inbound
            && let Some((code, errors)) = self.schemas.inbound_violation(protocol, action, payload)
        {
            return Reply::Error(code, truncate_chars(&errors, MAX_ERROR_DESCRIPTION));
        }
        if let Some(payload) = state
            .scripted
            .rule_payload(station, subprotocol.as_deref(), action)
        {
            return Reply::Result(payload);
        }
        if let Some(responder) = state.responders.get(action).cloned() {
            // Run outside the lock: the closure may call back into the handle.
            drop(state);
            return (*responder)(payload);
        }
        if !is_charge_point_action_for(protocol, action) {
            // OCPP-J: NotImplemented for a name nobody knows, NotSupported for a known action
            // that a charge point may not send to a central system.
            return if protocol == Protocol::V16
                && kirby_ocpp_core::ocpp::messages::registry::find(action).is_some()
            {
                Reply::Error(
                    ErrorCode::NotSupported,
                    format!("action {} is not supported", sanitize_action(action)),
                )
            } else {
                Reply::Error(
                    ErrorCode::NotImplemented,
                    format!("action {} is not implemented", sanitize_action(action)),
                )
            };
        }
        let transaction_id = (action == "StartTransaction").then(|| state.tx_ids.allocate());
        default_reply_for(
            protocol,
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
    let pending = Arc::new(Semaphore::new(inner.config.max_pending_handshakes));
    loop {
        tokio::select! {
            () = async { let _ = stop.wait_for(|stopped| *stopped).await; } => break,
            accepted = listener.accept() => match accepted {
                Ok((tcp, peer)) => {
                    // Too many half-open sockets: drop this one (closes it) instead of
                    // letting a single host exhaust file descriptors.
                    if let Ok(permit) = Arc::clone(&pending).try_acquire_owned() {
                        sessions.spawn(serve_tcp(
                            tcp,
                            peer,
                            acceptor.clone(),
                            Arc::clone(&inner),
                            permit,
                        ));
                    }
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
    handshake: OwnedSemaphorePermit,
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
                    run_session(tls, peer, identity, inner, handshake).await;
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
        None => run_session(tcp, peer, PeerIdentity::Plain, inner, handshake).await,
    }
}

async fn run_session<S>(
    stream: S,
    peer: SocketAddr,
    identity: PeerIdentity,
    inner: Arc<Inner>,
    handshake: OwnedSemaphorePermit,
) where
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
        let Some(id) = station_id_from_path(request.uri().path()) else {
            // Reported with the fixed phrase only: the id is attacker controlled and unusable.
            inner.emit(CsmsEvent::AuthFailed {
                station: None,
                remote_addr: peer,
                reason: "refused: no valid station id in the URL path (1 to 128 bytes, no control characters)".to_owned(),
            });
            return Err(refusal("no valid charge point id in the URL path"));
        };
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
    // The socket is established: it no longer counts as a pending handshake.
    drop(handshake);
    let Some((station, subprotocol)) = target else {
        return;
    };

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let slots = Arc::new(CallSlots::default());
    let protocol = session_protocol(subprotocol.as_deref());
    let session_id = match inner.register(&station, peer, subprotocol, cmd_tx, Arc::clone(&slots)) {
        Ok(session_id) => session_id,
        Err(RegisterError::Shutdown) => return,
        Err(RegisterError::TooManyStations) => {
            // The upgrade is done, so there is no HTTP 503 any more: say it in the close frame.
            let mut socket = socket;
            let close = Message::Close(Some(CloseFrame {
                code: CloseCode::Again,
                reason: "too many stations".into(),
            }));
            let _ = timeout(CLOSE_WRITE_TIMEOUT, socket.send(close)).await;
            return;
        }
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
        protocol,
        station,
        socket,
        slots,
        queue: VecDeque::new(),
        in_flight: None,
        last_seen: Instant::now(),
        ping_sent: None,
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
    /// The caller dropped its future; the deadline was shortened to the default call timeout.
    abandoned: bool,
    reply: oneshot::Sender<Result<Frame, CsmsError>>,
}

struct Session<S> {
    inner: Arc<Inner>,
    link_id: u64,
    /// The protocol of the negotiated subprotocol (1.6 when none or an unknown one).
    protocol: Protocol,
    station: String,
    socket: WebSocketStream<S>,
    slots: Arc<CallSlots>,
    queue: VecDeque<QueuedCall>,
    in_flight: Option<InFlight>,
    /// When the station last sent anything (a frame of any kind, a pong included).
    last_seen: Instant,
    /// When the outstanding watchdog ping was written; cleared by any inbound message.
    ping_sent: Option<Instant>,
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
            let deadline = self.next_deadline();
            let watchdog = self.watchdog_deadline();
            let ended = tokio::select! {
                () = async { let _ = stop.wait_for(|stopped| *stopped).await; } => {
                    self.close(1001, "server shutting down").await;
                    Some(DisconnectReason::Shutdown)
                }
                inbound = self.socket.next() => {
                    if matches!(inbound, Some(Ok(_))) {
                        self.last_seen = Instant::now();
                        self.ping_sent = None;
                    }
                    match inbound {
                        Some(Ok(Message::Text(text))) => self.on_text(text.as_str()).await,
                        Some(Ok(Message::Close(_))) => {
                            // RFC 6455 5.5.1: answer a Close with a Close. tungstenite queued
                            // the reply when it read the frame; flushing writes it.
                            let _ = timeout(CLOSE_WRITE_TIMEOUT, self.socket.flush()).await;
                            Some(DisconnectReason::Closed)
                        }
                        Some(Err(_)) | None => Some(DisconnectReason::Closed),
                        Some(Ok(_)) => None,
                    }
                }
                () = async {
                    match watchdog {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => self.watchdog_fired().await,
                command = commands.recv() => match command {
                    // `call()` reserved a slot, so the queue cannot outgrow its limit.
                    Some(SessionCmd::Call(call)) => {
                        self.queue.push_back(call);
                        None
                    }
                    Some(SessionCmd::Prune) => {
                        // Callers that gave up hold a slot until they are popped or pruned.
                        self.slots.prune.store(false, Ordering::Release);
                        self.queue.retain(|queued| !queued.reply.is_closed());
                        None
                    }
                    Some(SessionCmd::Close { code, reason }) => {
                        self.close(code, reason).await;
                        Some(DisconnectReason::Closed)
                    }
                    None => Some(DisconnectReason::Closed),
                },
                // The caller of the outstanding CALL is gone (its future was dropped): wait at
                // most the default call timeout more for the station, so an abandoned long
                // timeout cannot block the queue for hours. The answer, if it comes, is
                // discarded in `on_text`.
                () = async {
                    match self.in_flight.as_mut() {
                        Some(flight) if !flight.abandoned => flight.reply.closed().await,
                        _ => std::future::pending().await,
                    }
                } => {
                    let grace = self.inner.config.call_timeout.min(MAX_CALL_TIMEOUT);
                    if let Some(flight) = self.in_flight.as_mut() {
                        flight.abandoned = true;
                        flight.deadline = flight.deadline.min(Instant::now() + grace);
                    }
                    None
                }
                () = async {
                    match deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.expire_calls();
                    None
                }
            };
            if let Some(reason) = ended {
                return reason;
            }
        }
    }

    /// When the watchdog next acts: send a ping after `ping_interval` of silence, or give up
    /// `pong_timeout` after a ping. `None` if it is disabled.
    fn watchdog_deadline(&self) -> Option<Instant> {
        let config = &self.inner.config;
        if config.ping_interval.is_zero() {
            return None;
        }
        match self.ping_sent {
            Some(sent) => sent.checked_add(config.pong_timeout),
            None => self.last_seen.checked_add(config.ping_interval),
        }
    }

    async fn watchdog_fired(&mut self) -> Option<DisconnectReason> {
        if self.ping_sent.is_some() {
            return Some(DisconnectReason::PingTimeout);
        }
        let write = self.socket.send(Message::Ping(Vec::new().into()));
        match timeout(self.inner.config.write_timeout, write).await {
            Ok(Ok(())) => {
                self.ping_sent = Some(Instant::now());
                None
            }
            Ok(Err(_)) | Err(_) => Some(DisconnectReason::Closed),
        }
    }

    /// Writes the next queued CALL if none is outstanding.
    async fn pump(&mut self) -> Option<DisconnectReason> {
        while self.in_flight.is_none() {
            let call = self.queue.pop_front()?;
            if call.reply.is_closed() {
                continue; // The caller gave up before it was sent.
            }
            if call.deadline <= Instant::now() {
                self.fail_timeout(call.reply, call.action, call.timeout);
                continue;
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
                deadline: call.deadline,
                abandoned: false,
                reply: call.reply,
            });
        }
        None
    }

    /// The earliest deadline of the outstanding CALL and of the queued ones.
    fn next_deadline(&self) -> Option<Instant> {
        self.in_flight
            .iter()
            .map(|flight| flight.deadline)
            .chain(self.queue.iter().map(|queued| queued.deadline))
            .min()
    }

    /// Fails every call (outstanding or queued) whose deadline has passed with a timeout.
    fn expire_calls(&mut self) {
        let now = Instant::now();
        if self
            .in_flight
            .as_ref()
            .is_some_and(|flight| flight.deadline <= now)
            && let Some(flight) = self.in_flight.take()
        {
            self.fail_timeout(flight.reply, flight.action, flight.timeout);
        }
        let mut index = 0;
        while let Some(queued) = self.queue.get(index) {
            if queued.deadline <= now {
                if let Some(queued) = self.queue.remove(index) {
                    self.fail_timeout(queued.reply, queued.action, queued.timeout);
                }
            } else {
                index += 1;
            }
        }
    }

    fn fail_timeout(
        &self,
        reply: oneshot::Sender<Result<Frame, CsmsError>>,
        action: String,
        timeout: Duration,
    ) {
        let _ = reply.send(Err(CsmsError::Timeout {
            station: self.station.clone(),
            action,
            timeout,
        }));
    }

    async fn on_text(&mut self, text: &str) -> Option<DisconnectReason> {
        let frame = match Frame::parse(text) {
            Ok(frame) => frame,
            Err(error) => {
                if let Some(sink) = &self.inner.recording {
                    let _state = self.inner.lock();
                    sink.frame_for_station(
                        &self.station,
                        self.link_id,
                        &FrameRecord {
                            ts: Utc::now(),
                            direction: RecordDirection::In,
                            msg_id: None,
                            action: None,
                            frame: Value::String(text.to_owned()),
                        },
                    );
                }
                self.inner.emit(CsmsEvent::Malformed {
                    station: self.station.clone(),
                    text: malformed_text(text, self.inner.config.show_secrets),
                });
                // Only a CALL with a readable id gets a FormationViolation (FormatViolation
                // in 2.x); the rest is dropped.
                return match error.reply_for(self.protocol) {
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
                let reply = self.inner.decide(&self.station, &action, &payload);
                self.inner
                    .track(&self.station, self.link_id, &action, &payload, &reply);
                self.inner
                    .observe_inbound(&self.station, self.link_id, &action, &payload, &reply);
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
            // A frame kind added later is not a reply either.
            _ => None,
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

/// The action name for a CALLERROR text: at most 64 characters, control characters replaced.
fn sanitize_action(action: &str) -> String {
    truncate_chars(action, MAX_ACTION_KEY_CHARS)
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// The text kept in a [`CsmsEvent::Malformed`]: redacted, then cut to 512 characters.
/// Invalid JSON is omitted unless secrets are shown.
fn malformed_text(text: &str, show_secrets: bool) -> String {
    kirby_ocpp_core::ocpp::frame::redact_malformed_text(text, show_secrets)
        .chars()
        .take(MALFORMED_TEXT_CHARS)
        .collect()
}

/// Largest frame (as JSON text) that an event carries in full; bigger ones are replaced by the
/// marker of [`truncated_frame`], so a stalled event consumer cannot pin large frames.
const EVENT_FRAME_MAX_BYTES: usize = 64 * 1024;

/// The form of a frame in a [`CsmsEvent::Frame`]: the frame itself, or for a frame above
/// 64 KiB as JSON the same frame with its payload replaced by
/// `{"truncated": true, "originalBytes": n}`. The frame history keeps the full frame (within
/// its byte cap), so to tell a history entry from the event of the same frame, compare
/// `event_frame(&entry.frame)` with the event's frame.
#[must_use]
pub fn event_frame(frame: &Frame) -> Frame {
    let bytes = frame.to_json_string().len();
    if bytes > EVENT_FRAME_MAX_BYTES && !is_truncated_marker(frame) {
        truncated_frame(frame, bytes)
    } else {
        frame.clone()
    }
}

/// Whether the frame already carries the truncation marker.
fn is_truncated_marker(frame: &Frame) -> bool {
    let payload = match frame {
        Frame::Call { payload, .. } | Frame::CallResult { payload, .. } => payload,
        Frame::CallError { details, .. } => details,
        _ => return false,
    };
    payload["truncated"] == true
}

/// Replaces the payload of a frame that is too big for the history by a small marker object
/// (`{"truncated": true, "originalBytes": n}`); a CALLERROR keeps its code and a cut description.
fn truncated_frame(frame: &Frame, original_bytes: usize) -> Frame {
    let marker = json!({ "truncated": true, "originalBytes": original_bytes });
    match frame {
        Frame::Call { id, action, .. } => Frame::call(id.clone(), action.clone(), marker),
        Frame::CallResult { id, .. } => Frame::call_result(id.clone(), marker),
        Frame::CallError {
            id,
            code,
            description,
            ..
        } => Frame::CallError {
            id: id.clone(),
            code: code.clone(),
            description: truncate_chars(description, 128),
            details: marker,
        },
        other => other.clone(),
    }
}

// ---- handshake helpers --------------------------------------------------------------

/// Longest accepted station id, in bytes after percent-decoding.
pub const MAX_STATION_ID_LEN: usize = 128;

/// The station id of a URL path: the last non-empty segment, percent-decoded. `None` if
/// there is none, it is not valid UTF-8 after decoding, it is longer than 128 bytes, it is `.`
/// or `..` (a station could not use them in a URL) or it contains a control character.
pub fn station_id_from_path(path: &str) -> Option<String> {
    let segment = path.rsplit('/').find(|segment| !segment.is_empty())?;
    let decoded = percent_encoding::percent_decode_str(segment)
        .decode_utf8()
        .ok()?;
    (!decoded.is_empty()
        && decoded != "."
        && decoded != ".."
        && decoded.len() <= MAX_STATION_ID_LEN
        && !decoded.chars().any(kirby_ocpp_core::text::is_unsafe_char))
    .then(|| decoded.into_owned())
}

/// Longest CALLERROR description the engine generates, in characters.
const MAX_ERROR_DESCRIPTION: usize = 255;

fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
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

    #[test]
    fn station_id_rejects_long_and_control_characters() {
        assert!(station_id_from_path(&format!("/{}", "a".repeat(128))).is_some());
        assert_eq!(station_id_from_path(&format!("/{}", "a".repeat(129))), None);
        assert_eq!(station_id_from_path("/CP%0A1"), None);
        // Bidi overrides and line separators are refused like controls.
        for bad in [
            "/CP%E2%80%AE1",
            "/CP%E2%80%A81",
            "/CP%E2%81%A61",
            "/%2E%2E",
            "/x/%2e",
            "/..",
        ] {
            assert_eq!(station_id_from_path(bad), None, "{bad}");
        }
        assert_eq!(station_id_from_path("/CP%001"), None);
        assert_eq!(station_id_from_path("/CP%1B%5B1"), None);
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let text = "é".repeat(300);
        assert_eq!(truncate_chars(&text, 255).chars().count(), 255);
        assert_eq!(truncate_chars("short", 255), "short");
    }
}
