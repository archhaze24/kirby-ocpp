//! Size, count and time limits of the API server.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// Largest request body accepted, bytes.
pub(super) const MAX_BODY_BYTES: usize = 1024 * 1024;
/// How long the shutdown waits for open requests.
pub(super) const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
/// Most event streams open at once.
pub(super) const MAX_STREAMS: usize = 16;
/// Most connections open at once; further ones are closed at once.
pub(super) const MAX_CONNECTIONS: usize = 512;
/// Time a client gets to send the complete request head after connecting (hyper's
/// `header_read_timeout`, which only a real request line and a complete head satisfy).
pub(super) const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
/// A write that makes no progress for this long ends the connection.
pub(super) const WRITE_STALL: Duration = Duration::from_secs(30);
/// Most time a response (an event stream excepted) may take to be written, from its first
/// byte; a client that drains a byte at a time cannot hold a connection longer.
pub(super) const RESPONSE_DEADLINE: Duration = Duration::from_secs(60);
/// An event stream ends after this long; the last event tells the client to reconnect.
pub(super) const STREAM_MAX_AGE: Duration = Duration::from_secs(60 * 60);
/// After a response the connection waits at most this long, and reads at most
/// [`DRAIN_BYTES`], for the client to finish sending the request before it closes. Closing
/// with unread request data would reset the connection and could destroy an early error
/// response (`401`, `403`, `413`) the client has not read yet.
pub(super) const DRAIN_TIME: Duration = Duration::from_millis(100);
pub(super) const DRAIN_BYTES: usize = 64 * 1024;
/// Most request-body bytes held by `POST .../calls` requests at once (the body of each, as
/// received, while its call runs); further requests get `429`.
#[cfg(test)]
pub(super) const PAYLOAD_BUDGET: usize = crate::commands::PAYLOAD_BUDGET;
/// Time a client gets to send the whole request body.
pub(super) const BODY_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a TLS handshake may take, from accept to the first application byte; a peer that
/// is slower (or silent) is dropped.
pub(super) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Handshakes running at the same time. Over the cap a new connection is closed at once, so
/// idle or hostile peers cannot use up the CPU or all connection slots.
pub(super) const MAX_HANDSHAKES: usize = 64;

/// Timing limits of the server; tests use short ones.
#[derive(Debug, Clone, Copy)]
pub(super) struct Limits {
    pub(super) write_stall: Duration,
    pub(super) response_deadline: Duration,
    pub(super) stream_max_age: Duration,
    #[cfg(test)]
    pub(super) payload_budget: usize,
}

impl Limits {
    pub(super) const DEFAULT: Self = Self {
        write_stall: WRITE_STALL,
        response_deadline: RESPONSE_DEADLINE,
        stream_max_age: STREAM_MAX_AGE,
        #[cfg(test)]
        payload_budget: PAYLOAD_BUDGET,
    };
}

/// Set by the handler of a response that is meant to last (an event stream): the connection
/// is then exempt from [`RESPONSE_DEADLINE`].
#[derive(Clone)]
pub(super) struct Streaming(pub(super) Arc<AtomicBool>);
