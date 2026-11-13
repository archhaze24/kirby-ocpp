//! A bounded, ordered feed of session and transaction facts for one consumer.
//!
//! The event stream truncates large frames, carries no session identity and drops events for a
//! slow receiver; the history is capped and evicted. A consumer that has to correlate
//! transactions across a whole run (the `--replay` runner) needs none of those losses, so the
//! engine can feed it a compact [`Observation`] per fact instead. The feed is attached
//! before the server accepts connections ([`CsmsServer::start_with`]), so no fact is missed,
//! and it never grows: when the consumer falls behind by more than the capacity, the feed
//! turns into an error and stays one ([`ObserveError::Overflow`]).
//!
//! [`CsmsServer::start_with`]: crate::CsmsServer::start_with

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;

use crate::server::ConnectedSession;

/// Identity of one session (one WebSocket connection) of a server run. A replacement
/// connection of the same station has a different id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub(crate) u64);

/// Longest `idTag` kept in an [`Observation::Start`]; a longer one is reported as `None`.
pub const MAX_OBSERVED_ID_TAG: usize = 255;

/// One fact about a session, in the order the engine saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Observation {
    /// A station connected; the token addresses exactly this connection.
    Connected(ConnectedSession),
    /// A session ended: closed, replaced, forgotten or shut down. Nothing more is reported
    /// for it.
    Ended { station: String, session: SessionId },
    /// A `BootNotification` of the session was answered.
    Boot {
        station: String,
        session: SessionId,
        /// The answer was a CALLRESULT with `status` `Accepted`.
        accepted: bool,
    },
    /// A `StartTransaction` of the session was answered (or not).
    Start {
        station: String,
        session: SessionId,
        /// `connectorId` of the request; `None` when it is missing or not a valid number.
        connector: Option<u32>,
        /// `idTag` of the request; `None` when missing or longer than
        /// [`MAX_OBSERVED_ID_TAG`].
        id_tag: Option<String>,
        /// The `transactionId` of the answer actually sent; `None` for a CALLERROR, no
        /// answer or an answer without a valid id.
        transaction_id: Option<i32>,
    },
    /// A `StopTransaction` of the session was received, whatever the answer was.
    Stop {
        station: String,
        session: SessionId,
        /// `transactionId` of the request; `None` when missing or not a valid number.
        transaction_id: Option<i32>,
    },
}

/// Why [`Observer::recv`] has no more observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObserveError {
    /// The consumer fell behind: observations were dropped, so nothing can be trusted any
    /// more.
    Overflow,
    /// The server is gone.
    Closed,
}

impl fmt::Display for ObserveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Overflow => "the observation queue overflowed",
            Self::Closed => "the server stopped",
        })
    }
}

impl std::error::Error for ObserveError {}

/// The receiving end of the feed. See the module docs.
#[derive(Debug)]
pub struct Observer {
    rx: mpsc::Receiver<Observation>,
    overflow: Arc<AtomicBool>,
}

impl Observer {
    /// The next observation, oldest first.
    ///
    /// # Errors
    /// [`ObserveError::Overflow`] once any observation was dropped (it stays an error);
    /// [`ObserveError::Closed`] when the server and its sessions are gone and the queue is
    /// empty.
    pub async fn recv(&mut self) -> Result<Observation, ObserveError> {
        let item = self.rx.recv().await;
        if self.overflow.load(Ordering::Acquire) {
            return Err(ObserveError::Overflow);
        }
        item.ok_or(ObserveError::Closed)
    }
}

/// The sending end, held by the server.
pub(crate) struct ObserveSender {
    tx: mpsc::Sender<Observation>,
    overflow: Arc<AtomicBool>,
}

impl ObserveSender {
    /// Queues an observation; a full or closed queue marks the feed as overflowed.
    pub(crate) fn send(&self, observation: Observation) {
        if self.tx.try_send(observation).is_err() {
            self.overflow.store(true, Ordering::Release);
        }
    }
}

/// A feed with room for `capacity` pending observations (at least 1).
pub(crate) fn channel(capacity: usize) -> (ObserveSender, Observer) {
    let (tx, rx) = mpsc::channel(capacity.max(1));
    let overflow = Arc::new(AtomicBool::new(false));
    (
        ObserveSender {
            tx,
            overflow: Arc::clone(&overflow),
        },
        Observer { rx, overflow },
    )
}
