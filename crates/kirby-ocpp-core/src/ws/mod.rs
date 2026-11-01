//! The connection layer: one WebSocket (optionally TLS) to the CSMS.
//!
//! **Contract only.** The types and signatures below are final; the transport
//! (tokio-tungstenite + rustls, ping/pong watchdog, subprotocol enforcement,
//! pending-call table, outbound CALL queue) is filled in by the ocpp stage.
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
//! * The WebSocket URL is `<base>/<percent-encoded charge point id>` built by
//!   [`charge_point_url`]; the negotiated subprotocol must equal the requested one
//!   (close code 1002 otherwise).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc;
use url::Url;

use crate::config::TlsConfig;
use crate::error::CallError;
use crate::ocpp::action::OutboundAction;
use crate::ocpp::frame::FrameRecord;
use crate::ocpp::responder::ReplyHandle;

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
    /// WebSocket ping interval; `None` disables pings. Watchdog (as in the TS client):
    /// if the pong for the previous ping is still missing when the next tick fires,
    /// the socket is terminated.
    pub ping_interval: Option<Duration>,
    /// Default timeout of [`Connection::call`] when none is given.
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
    /// text frame, including unparsable ones.
    Frame(FrameRecord),
    /// A schema-valid CALL from the CSMS awaiting a reply.
    Call(IncomingCall),
    /// A protocol problem that did not close the socket (malformed frame, CALLRESULT
    /// for an unknown id, ...). Informational.
    ProtocolWarning(String),
    /// The socket is gone. After this event no more events arrive.
    Closed {
        code: Option<u16>,
        reason: String,
        /// `true` if we initiated the close.
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
    #[error("TLS configuration error: {0}")]
    Tls(String),
    #[error("WebSocket handshake failed: {0}")]
    Handshake(String),
    /// The server did not select the requested subprotocol.
    #[error("central system did not negotiate required WebSocket subprotocol {expected}")]
    SubprotocolMismatch { expected: String },
    #[error("connection attempt timed out")]
    Timeout,
}

/// Cloneable handle to an open connection.
#[derive(Debug, Clone)]
pub struct Connection {
    _task: (),
}

#[allow(
    unused_variables,
    clippy::unused_async,
    clippy::unused_self,
    reason = "signatures are final; bodies arrive with the ocpp stage"
)]
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
        unimplemented!("ocpp stage: websocket transport")
    }

    /// Sends a CALL and waits for its CALLRESULT payload. `timeout` defaults to
    /// [`ConnectOptions::call_timeout`].
    pub async fn call(
        &self,
        action: &str,
        payload: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, CallError> {
        unimplemented!("ocpp stage: outbound CALL queue")
    }

    /// Typed variant of [`Connection::call`] for charge-point-initiated actions.
    pub async fn call_typed<A: OutboundAction>(
        &self,
        request: &A::Req,
        timeout: Option<Duration>,
    ) -> Result<A::Res, CallError> {
        unimplemented!("ocpp stage: typed CALL")
    }

    /// Starts a graceful close (WebSocket Close frame). Idempotent.
    pub fn close(&self) {
        unimplemented!("ocpp stage: close")
    }

    /// `true` while the socket is open.
    pub fn is_open(&self) -> bool {
        unimplemented!("ocpp stage: state")
    }
}

/// Builds the WebSocket URL for a charge point: `<base>/<percent-encoded id>`.
///
/// Trailing slashes of the base path are dropped; a query string and fragment of the
/// base are preserved (the TS code appended the id after the query, producing a broken
/// URL). If the base path already ends with the (encoded) id it is used as is.
pub fn charge_point_url(base: &str, charge_point_id: &str) -> Result<Url, ConnectError> {
    let mut url = Url::parse(base).map_err(|e| ConnectError::InvalidUrl(format!("{base}: {e}")))?;
    if !matches!(url.scheme(), "ws" | "wss") {
        return Err(ConnectError::InvalidUrl(format!(
            "{base}: scheme must be ws or wss"
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
    url.path_segments_mut()
        .map_err(|()| ConnectError::InvalidUrl(format!("{base}: cannot be a base URL")))?
        .pop_if_empty()
        .push(charge_point_id);
    Ok(url)
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
    fn connection_state_serializes_with_a_tag() {
        let json = serde_json::to_value(ConnectionState::Backoff { attempt: 2 }).unwrap();
        assert_eq!(json, serde_json::json!({"state": "backoff", "attempt": 2}));
        assert_eq!(
            serde_json::to_value(ConnectionState::Connected).unwrap(),
            serde_json::json!({"state": "connected"})
        );
    }
}
