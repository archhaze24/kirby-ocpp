//! The connection layer: one WebSocket (optionally TLS) to the CSMS.
//!
//! # Model
//!
//! [`Connection::connect`] performs the handshake and returns a cloneable
//! [`Connection`] handle plus the receiver of [`ConnectionEvent`]s. A background
//! task owns the socket. Everything that must happen in order goes through it:
//!
//! * **Outbound CALLs are serialized.** OCPP-J says a charge point should not
//!   send a new CALL before the previous one is answered, so
//!   [`Connection::call`] enqueues and the task sends the next CALL only after
//!   the current one got its CALLRESULT/CALLERROR or timed out. Callers just
//!   `await`. [`ConnectOptions::serialize_calls`] `= false` is the opt-out for
//!   negative testing (send immediately, several in flight).
//! * Requests we send and responses we receive are validated against the schemas
//!   ([`crate::ocpp::SchemaRegistry`]); violations come back as [`CallError::Schema`].
//! * Incoming CALLs are validated, then delivered as [`ConnectionEvent::Call`]
//!   with a [`ReplyHandle`] (one-shot, see [`crate::ocpp::responder`]). Invalid
//!   ones are answered by the task itself and never reach the station.
//! * On close, every pending and queued CALL fails with [`CallError::Closed`],
//!   and outstanding reply handles are invalidated.
//! * The WebSocket URL is `<base>/<percent-encoded id>` built by
//!   [`charge_point_url`]; the negotiated subprotocol must equal the requested one
//!   (the connection is closed with 1002 otherwise).
//!
//! # Owner obligations
//!
//! The event receiver is bounded and the task waits for it when it has an event to
//! deliver. An owner must therefore drain the receiver **while** it awaits
//! [`Connection::call`]; awaiting a call without draining deadlocks.
//!
//! # Timeouts
//!
//! A CALL's timeout starts when the CALL is written to the socket, not when it is
//! queued behind an earlier CALL. The connection attempt (TCP, TLS and the HTTP
//! upgrade) is bounded by [`ConnectOptions::call_timeout`] as well.

mod handshake;
pub mod probe;
mod task;
mod tls;

pub(crate) use tls::WILDCARD_REFUSED;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use url::Url;

use crate::config::TlsConfig;
use crate::error::CallError;
use crate::ocpp::action::OutboundAction;
use crate::ocpp::frame::FrameRecord;
use crate::ocpp::responder::ReplyHandle;
use crate::ocpp::{Protocol, SchemaError, SchemaRegistry};

/// Bounded capacity of the event channel.
const EVENT_CAPACITY: usize = 64;

/// The schema registry a connection validates against, shared by pointer.
///
/// Two sets are equal when they are the same registry (not merely equal contents), which
/// keeps [`ConnectOptions`] `Eq`. The default is the OCPP 1.6J set.
#[derive(Clone)]
pub struct SchemaSet(Arc<SchemaRegistry>);

impl SchemaSet {
    /// Wraps a registry.
    pub fn new(registry: Arc<SchemaRegistry>) -> Self {
        Self(registry)
    }

    /// The shared set of `protocol` ([`SchemaRegistry::for_protocol`]).
    ///
    /// # Errors
    /// [`SchemaError`] when this build has no schemas for the version.
    pub fn for_protocol(protocol: Protocol) -> Result<Self, SchemaError> {
        SchemaRegistry::for_protocol(protocol).map(Self)
    }

    /// The registry.
    pub fn registry(&self) -> &SchemaRegistry {
        &self.0
    }
}

impl Default for SchemaSet {
    /// The OCPP 1.6J set.
    ///
    /// # Panics
    /// If a bundled schema does not compile (a build-time bug, covered by tests).
    fn default() -> Self {
        Self::for_protocol(Protocol::V16).expect("bundled OCPP 1.6J schemas compile")
    }
}

impl PartialEq for SchemaSet {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for SchemaSet {}

impl std::fmt::Debug for SchemaSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SchemaSet")
            .field(&self.0.protocol().token())
            .finish()
    }
}

/// State of the link to the CSMS, as shown in snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ConnectionState {
    /// No connection and none being attempted.
    #[default]
    Disconnected,
    /// Handshake in progress.
    Connecting,
    /// Socket open and subprotocol verified.
    Connected,
    /// Waiting to reconnect after an unexpected disconnect; `attempt` is the
    /// number of the attempt that will be made next (1-based).
    Backoff { attempt: u32 },
}

/// Options of a connection (everything not part of the URL/TLS/subprotocol triple).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectOptions {
    /// WebSocket ping interval; `None` (or a zero duration) disables pings. Watchdog (as in the TS client):
    /// if the pong for the previous ping is still missing when the next tick fires,
    /// the socket is terminated.
    pub ping_interval: Option<Duration>,
    /// Default timeout of [`Connection::call`] when none is given. Also bounds the
    /// connection attempt.
    pub call_timeout: Duration,
    /// Serialize outbound CALLs (default `true`). `false` allows several in flight.
    pub serialize_calls: bool,
    /// Record `AuthorizationKey` values in [`FrameRecord`]s as sent (default `false`:
    /// they are replaced by `***`). The wire is never affected.
    pub show_secrets: bool,
    /// Longest time one write to the socket (a frame, a ping, a pong flush or the close
    /// frame) may take. A peer that stops reading is treated as a lost connection when it
    /// expires. Default 10 seconds.
    pub write_timeout: Duration,
    /// The schemas payloads are validated against, and the version whose error code
    /// spellings the connection answers with. Default: OCPP 1.6J.
    pub schemas: SchemaSet,
}

impl ConnectOptions {
    /// These options with `schemas` as the validation set.
    #[must_use]
    pub fn with_schemas(mut self, schemas: SchemaSet) -> Self {
        self.schemas = schemas;
        self
    }
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            ping_interval: Some(Duration::from_secs(30)),
            call_timeout: Duration::from_secs(30),
            serialize_calls: true,
            show_secrets: false,
            write_timeout: DEFAULT_WRITE_TIMEOUT,
            schemas: SchemaSet::default(),
        }
    }
}

/// Default of [`ConnectOptions::write_timeout`].
pub const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Something that happened on the connection.
#[derive(Debug)]
#[non_exhaustive]
pub enum ConnectionEvent {
    /// A frame crossed the wire (either direction), for logs. Emitted for every
    /// text frame, including unparsable ones (whose `frame` is the raw text as a
    /// JSON string).
    Frame(FrameRecord),
    /// A schema-valid CALL from the CSMS awaiting a reply.
    Call(IncomingCall),
    /// A protocol problem that did not close the socket (malformed frame, CALLRESULT
    /// for an unknown id, ...). Informational.
    ProtocolWarning(String),
    /// The socket is gone. After this event no more events arrive.
    Closed {
        /// WebSocket close code: the peer's, or ours for a close we initiated.
        /// `Some(1006)` for an abnormal end without a close handshake.
        code: Option<u16>,
        reason: String,
        /// `true` if we initiated the close (or terminated the socket ourselves).
        by_us: bool,
    },
}

/// A CALL from the CSMS that passed validation.
#[derive(Debug)]
pub struct IncomingCall {
    pub id: String,
    pub action: String,
    pub payload: Value,
    /// Answer with exactly one of accept/reject; dropping it replies `InternalError`.
    pub reply: ReplyHandle,
}

/// Why [`Connection::connect`] failed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConnectError {
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    /// The Basic Auth credentials could not be built; the attempt made no request.
    #[error("invalid credentials: {0}")]
    InvalidCredentials(String),
    /// A TLS setting is inconsistent or the TLS handshake failed (for example the server
    /// certificate is not trusted).
    #[error("TLS error: {0}")]
    Tls(String),
    /// A TLS file (CA certificate, client certificate or key) could not be read or parsed.
    #[error("cannot use TLS file {}: {reason}", path.display())]
    TlsFile { path: PathBuf, reason: String },
    /// The TCP connection could not be established.
    #[error("cannot reach the central system: {0}")]
    Network(String),
    #[error("WebSocket handshake failed: {0}")]
    Handshake(String),
    /// The server answered the upgrade request with an HTTP status other than 101.
    #[error(
        "WebSocket handshake failed: the central system answered `{line}` instead of 101 Switching Protocols"
    )]
    HttpStatus { status: u16, line: String },
    /// The server did not select the requested subprotocol. The connection was
    /// closed with code 1002 before this error is returned.
    #[error("central system did not negotiate required WebSocket subprotocol {expected}")]
    SubprotocolMismatch { expected: String },
    /// The connection attempt (TCP, TLS and upgrade) exceeded [`ConnectOptions::call_timeout`].
    #[error("connection attempt timed out")]
    Timeout,
}

/// Cloneable handle to an open connection.
///
/// Dropping the last handle closes the connection gracefully.
#[derive(Debug, Clone)]
pub struct Connection {
    commands: mpsc::UnboundedSender<task::Command>,
    open: Arc<AtomicBool>,
    default_timeout: Duration,
    schemas: SchemaSet,
}

/// The answer of a CALL queued with [`Connection::enqueue`].
#[derive(Debug)]
pub struct PendingReply(oneshot::Receiver<Result<Value, CallError>>);

impl PendingReply {
    /// Waits for the CALLRESULT payload (or the failure).
    pub async fn wait(self) -> Result<Value, CallError> {
        self.0
            .await
            .unwrap_or(Err(CallError::Closed { code: None }))
    }
}

impl Connection {
    /// A channel-backed connection for deterministic station handoff tests.
    #[cfg(test)]
    pub(crate) fn test_reply_channel() -> (
        Self,
        impl FnMut() -> oneshot::Sender<Result<Value, CallError>>,
    ) {
        Self::test_reply_channel_with(SchemaSet::default())
    }

    /// [`Connection::test_reply_channel`] validating against `schemas`.
    #[cfg(test)]
    pub(crate) fn test_reply_channel_with(
        schemas: SchemaSet,
    ) -> (
        Self,
        impl FnMut() -> oneshot::Sender<Result<Value, CallError>>,
    ) {
        let (commands, mut receiver) = mpsc::unbounded_channel();
        let connection = Self {
            commands,
            open: Arc::new(AtomicBool::new(true)),
            default_timeout: Duration::from_secs(10),
            schemas,
        };
        (connection, move || match receiver.try_recv().unwrap() {
            task::Command::Call(call) => call.reply,
            task::Command::Close => panic!("expected a queued CALL"),
        })
    }

    /// Opens a connection to `<url>/<charge_point_id>` requesting `subprotocol`.
    ///
    /// Returns the handle and the stream of events. The receiver is bounded; the
    /// owner must drain it (the station actor does).
    pub async fn connect(
        url: &str,
        charge_point_id: &str,
        tls: &TlsConfig,
        subprotocol: &str,
        options: ConnectOptions,
    ) -> Result<(Self, mpsc::Receiver<ConnectionEvent>), ConnectError> {
        let url = charge_point_url(url, charge_point_id)?;
        let attempt = options.call_timeout;
        let default_timeout = options.call_timeout;
        let schemas = options.schemas.clone();

        let (open, commands, events) = if url.scheme() == "wss" {
            let config = tls::client_config(tls).await?;
            let ws = timeout(attempt, async {
                let tcp = tcp_connect(&url).await?;
                let server_name = tls::server_name(&url, tls)?;
                let stream = TlsConnector::from(config)
                    .connect(server_name, tcp)
                    .await
                    .map_err(|e| ConnectError::Tls(format!("TLS handshake failed: {e}")))?;
                handshake::upgrade(stream, &url, subprotocol).await
            })
            .await
            .map_err(|_| ConnectError::Timeout)??;
            task::spawn(ws, &options)
        } else {
            let ws = timeout(attempt, async {
                let tcp = tcp_connect(&url).await?;
                handshake::upgrade(tcp, &url, subprotocol).await
            })
            .await
            .map_err(|_| ConnectError::Timeout)??;
            task::spawn(ws, &options)
        };

        Ok((
            Self {
                commands,
                open,
                default_timeout,
                schemas,
            },
            events,
        ))
    }

    /// Sends a CALL and waits for its CALLRESULT payload. `timeout` defaults to
    /// [`ConnectOptions::call_timeout`].
    ///
    /// The request is validated first: a schema violation returns
    /// [`CallError::Schema`] and nothing is sent. Dropping the returned future
    /// withdraws the CALL if it is still queued behind an earlier one; once it has been
    /// written to the socket it is not recalled (its response is discarded).
    pub async fn call(
        &self,
        action: &str,
        payload: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, CallError> {
        self.call_checked(action, payload, timeout, true).await
    }

    /// Like [`Connection::call`] but the request is sent **without** schema validation
    /// (negative testing of a central system). The response is still validated.
    pub async fn call_unvalidated(
        &self,
        action: &str,
        payload: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, CallError> {
        self.call_checked(action, payload, timeout, false).await
    }

    async fn call_checked(
        &self,
        action: &str,
        payload: Value,
        timeout: Option<Duration>,
        validate: bool,
    ) -> Result<Value, CallError> {
        self.enqueue(action, payload, timeout, validate)?
            .wait()
            .await
    }

    /// Validates and queues a CALL **synchronously** and returns the handle to wait for its
    /// answer. Callers that need an order between several CALLs enqueue them in that order
    /// (the connection task sends its queue front to back) and only then spawn the waiters;
    /// awaiting [`Connection::call`] from separate tasks leaves the order to the scheduler.
    /// Dropping the handle withdraws the CALL while it is still queued.
    ///
    /// # Errors
    /// [`CallError::NotConnected`] when the socket is closed, [`CallError::Schema`] when
    /// `validate` is set and the request violates its schema.
    pub fn enqueue(
        &self,
        action: &str,
        payload: Value,
        timeout: Option<Duration>,
        validate: bool,
    ) -> Result<PendingReply, CallError> {
        if !self.is_open() {
            return Err(CallError::NotConnected);
        }
        if validate {
            self.schemas.registry().validate_request(action, &payload)?;
        }

        let (reply, response) = oneshot::channel();
        let call = task::OutboundCall {
            action: action.to_owned(),
            payload,
            timeout: timeout.unwrap_or(self.default_timeout),
            reply,
        };
        self.commands
            .send(task::Command::Call(call))
            .map_err(|_| CallError::NotConnected)?;
        Ok(PendingReply(response))
    }

    /// Typed variant of [`Connection::call`] for charge-point-initiated actions.
    pub async fn call_typed<A: OutboundAction>(
        &self,
        request: &A::Req,
        timeout: Option<Duration>,
    ) -> Result<A::Res, CallError> {
        let payload = serde_json::to_value(request).map_err(|source| CallError::Serialize {
            action: A::NAME.to_owned(),
            source,
        })?;
        let value = self.call(A::NAME, payload, timeout).await?;
        serde_json::from_value(value).map_err(|source| CallError::Decode {
            action: A::NAME.to_owned(),
            source,
        })
    }

    /// Starts a graceful close (WebSocket Close frame). Idempotent.
    ///
    /// The task sends the Close frame and waits (bounded) for the peer's reply, then
    /// emits [`ConnectionEvent::Closed`] with `by_us: true`. Calls not yet answered
    /// fail with [`CallError::Closed`].
    pub fn close(&self) {
        self.open.store(false, Ordering::SeqCst);
        let _ = self.commands.send(task::Command::Close);
    }

    /// `true` while the socket is open (and no close has been requested).
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }
}

/// Opens the TCP connection to the URL's host and port.
async fn tcp_connect(url: &Url) -> Result<TcpStream, ConnectError> {
    let host = url
        .host_str()
        .ok_or_else(|| ConnectError::InvalidUrl(format!("{}: no host", redact(url))))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| ConnectError::InvalidUrl(format!("{}: no port", redact(url))))?;
    TcpStream::connect((host, port))
        .await
        .map_err(|e| ConnectError::Network(format!("{host}:{port}: {e}")))
}

/// The URL without its userinfo, for error messages (passwords must not reach logs).
fn redact(url: &Url) -> String {
    let mut shown = url.clone();
    let _ = shown.set_username("");
    let _ = shown.set_password(None);
    shown.to_string()
}

/// Builds the WebSocket URL for a charge point: `<base>/<percent-encoded id>`.
///
/// Trailing slashes of the base path are dropped; a query string and fragment of the
/// base are preserved (the TS code appended the id after the query, producing a broken
/// URL). If the base path already ends with the (encoded) id it is used as is.
pub fn charge_point_url(base: &str, charge_point_id: &str) -> Result<Url, ConnectError> {
    let mut url = Url::parse(base)
        .map_err(|e| ConnectError::InvalidUrl(format!("{}: {e}", redact_text(base))))?;
    if !matches!(url.scheme(), "ws" | "wss") {
        return Err(ConnectError::InvalidUrl(format!(
            "{}: scheme must be ws or wss",
            redact(&url)
        )));
    }
    // The url crate drops `.` and `..` segments (also as `%2E`), so the id would never
    // reach the server.
    if matches!(charge_point_id, "." | "..") {
        return Err(ConnectError::InvalidUrl(format!(
            "charge point id `{charge_point_id}` cannot be used as a URL path segment"
        )));
    }
    let mut encoded = Url::parse("ws://h/").expect("static URL is valid");
    encoded
        .path_segments_mut()
        .expect("ws URLs can be a base")
        .push(charge_point_id);
    let encoded_id = encoded.path().trim_start_matches('/').to_owned();

    let already_there = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .is_some_and(|last| last == encoded_id);
    if already_there {
        return Ok(url);
    }
    let shown = redact(&url);
    url.path_segments_mut()
        .map_err(|()| ConnectError::InvalidUrl(format!("{shown}: cannot be a base URL")))?
        .pop_if_empty()
        .push(charge_point_id);
    Ok(url)
}

/// Parse errors have no parsed URL; everything before the last `@` is dropped.
fn redact_text(base: &str) -> String {
    crate::config::redact_unparsable_url(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(base: &str, id: &str) -> String {
        charge_point_url(base, id).unwrap().to_string()
    }

    #[test]
    fn unparsable_urls_lose_everything_before_the_last_at() {
        let shown = redact_text("ws://cp:pa@ss@[bad/x");
        assert_eq!(shown, "ws://[bad/x");
        assert!(!shown.contains("ss"));
    }

    #[test]
    fn appends_the_id_as_last_path_segment() {
        assert_eq!(url("ws://h:9000/ocpp", "CP-1"), "ws://h:9000/ocpp/CP-1");
        assert_eq!(url("ws://h:9000/ocpp/", "CP-1"), "ws://h:9000/ocpp/CP-1");
        assert_eq!(url("ws://h:9000", "CP-1"), "ws://h:9000/CP-1");
    }

    #[test]
    fn percent_encodes_the_id() {
        assert_eq!(url("ws://h/ocpp", "a b/c"), "ws://h/ocpp/a%20b%2Fc");
    }

    #[test]
    fn keeps_the_query_string_after_the_id() {
        assert_eq!(
            url("wss://h/ocpp?token=x", "CP-1"),
            "wss://h/ocpp/CP-1?token=x"
        );
    }

    #[test]
    fn does_not_append_the_id_twice() {
        assert_eq!(url("ws://h/ocpp/CP-1", "CP-1"), "ws://h/ocpp/CP-1");
        assert_eq!(url("ws://h/ocpp/a%20b", "a b"), "ws://h/ocpp/a%20b");
    }

    #[test]
    fn rejects_non_websocket_urls() {
        assert!(matches!(
            charge_point_url("http://h/x", "id"),
            Err(ConnectError::InvalidUrl(_))
        ));
        assert!(charge_point_url("nonsense", "id").is_err());
    }

    #[test]
    fn error_messages_do_not_show_the_password() {
        let parse_error = charge_point_url("ws://user:secret@[bad/ocpp", "CP-1").unwrap_err();
        assert!(!parse_error.to_string().contains("secret"), "{parse_error}");
        let scheme_error = charge_point_url("http://user:secret@h/ocpp", "CP-1").unwrap_err();
        assert!(
            !scheme_error.to_string().contains("secret"),
            "{scheme_error}"
        );
    }

    /// A set that knows one action, `Ping`, whose request must carry an integer `n`.
    pub(super) fn ping_schemas(protocol: Protocol) -> SchemaSet {
        const TABLE: crate::ocpp::schema::SchemaTable = &[(
            "Ping",
            r#"{"type":"object","required":["n"],"properties":{"n":{"type":"integer"}}}"#,
            r#"{"type":"object"}"#,
        )];
        SchemaSet::new(Arc::new(SchemaRegistry::build(protocol, TABLE).unwrap()))
    }

    #[test]
    fn the_default_schema_set_is_the_shared_1_6_registry() {
        let default = SchemaSet::default();
        assert_eq!(default, SchemaSet::default());
        assert_eq!(default, SchemaSet::for_protocol(Protocol::V16).unwrap());
        assert_eq!(default.registry().protocol(), Protocol::V16);
        assert_eq!(format!("{default:?}"), r#"SchemaSet("ocpp1.6")"#);
        assert!(matches!(
            SchemaSet::for_protocol(Protocol::V201),
            Err(SchemaError::Unavailable(Protocol::V201))
        ));
    }

    #[test]
    fn schema_sets_compare_by_identity() {
        let one = ping_schemas(Protocol::V201);
        assert_eq!(one, one.clone());
        assert_ne!(
            one,
            ping_schemas(Protocol::V201),
            "equal content, other registry"
        );
        assert_ne!(one, SchemaSet::default());
    }

    #[test]
    fn connect_options_default_to_1_6_and_compare_by_schema_identity() {
        let options = ConnectOptions::default();
        assert_eq!(options, ConnectOptions::default());
        assert_eq!(options.schemas, SchemaSet::default());
        let custom = ping_schemas(Protocol::V201);
        let with_custom = options.clone().with_schemas(custom.clone());
        assert_eq!(with_custom.schemas, custom);
        assert_ne!(with_custom, options);
        assert_eq!(with_custom, options.with_schemas(custom));
    }

    #[test]
    fn a_connection_validates_requests_against_the_schemas_of_its_options() {
        let valid = serde_json::json!({"n": 1});
        let invalid = serde_json::json!({});

        let (default_connection, _) = Connection::test_reply_channel();
        let error = default_connection
            .enqueue("Ping", valid.clone(), None, true)
            .unwrap_err();
        assert!(
            matches!(&error, CallError::Schema(v) if v.error_code() == crate::ocpp::ErrorCode::NotImplemented),
            "the 1.6 set does not know Ping: {error:?}"
        );

        // The closure owns the command receiver; dropping it would make every send fail.
        let (connection, _queued) =
            Connection::test_reply_channel_with(ping_schemas(Protocol::V201));
        assert!(
            connection
                .enqueue("Ping", valid.clone(), None, true)
                .is_ok()
        );
        let error = connection
            .enqueue("Ping", invalid.clone(), None, true)
            .unwrap_err();
        assert!(matches!(error, CallError::Schema(_)), "{error:?}");
        // Without validation the payload goes out regardless of the set.
        assert!(connection.enqueue("Ping", invalid, None, false).is_ok());
        // And the 1.6 actions are unknown to this set.
        assert!(
            connection
                .enqueue("Heartbeat", serde_json::json!({}), None, true)
                .is_err()
        );
    }

    #[test]
    fn connection_state_serializes_with_a_tag() {
        let json = serde_json::to_value(ConnectionState::Backoff { attempt: 2 }).unwrap();
        assert_eq!(json, serde_json::json!({"state": "backoff", "attempt": 2}));
        assert_eq!(
            serde_json::to_value(ConnectionState::Connected).unwrap(),
            serde_json::json!({"state": "connected"})
        );
    }
}
