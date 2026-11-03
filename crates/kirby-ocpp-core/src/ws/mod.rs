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
mod task;
mod tls;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
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
use crate::ocpp::SchemaRegistry;
use crate::ocpp::action::OutboundAction;
use crate::ocpp::frame::FrameRecord;
use crate::ocpp::responder::ReplyHandle;

/// Bounded capacity of the event channel.
const EVENT_CAPACITY: usize = 64;

/// The bundled OCPP schemas, compiled once for the whole process.
static SCHEMAS: LazyLock<SchemaRegistry> =
    LazyLock::new(|| SchemaRegistry::new().expect("bundled OCPP 1.6J schemas compile"));

fn schemas() -> &'static SchemaRegistry {
    &SCHEMAS
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
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            ping_interval: Some(Duration::from_secs(30)),
            call_timeout: Duration::from_secs(30),
            serialize_calls: true,
        }
    }
}

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
    /// A TLS setting is inconsistent or the TLS handshake failed.
    #[error("TLS configuration error: {0}")]
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
}

impl Connection {
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
        if !self.is_open() {
            return Err(CallError::NotConnected);
        }
        if validate {
            schemas().validate_request(action, &payload)?;
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
        response
            .await
            .unwrap_or(Err(CallError::Closed { code: None }))
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

/// Parse errors have no parsed URL; drop anything before `@` in the authority.
fn redact_text(base: &str) -> String {
    match base.split_once("://") {
        Some((scheme, rest)) => match rest.split_once('@') {
            Some((_, after)) => format!("{scheme}://<redacted>@{after}"),
            None => base.to_owned(),
        },
        None => base.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(base: &str, id: &str) -> String {
        charge_point_url(base, id).unwrap().to_string()
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
