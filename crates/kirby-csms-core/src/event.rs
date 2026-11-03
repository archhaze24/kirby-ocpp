use std::collections::BTreeMap;
use std::net::SocketAddr;

use chrono::{DateTime, Utc};
use kirby_ocpp_core::ocpp::Frame;

/// Direction of a frame relative to the central system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Received from the station.
    In,
    /// Sent to the station.
    Out,
}

/// One frame in a station's history.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameEntry {
    pub ts: DateTime<Utc>,
    pub direction: Direction,
    pub frame: Frame,
}

/// Why a station's connection ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    /// The peer closed the socket, or it failed.
    Closed,
    /// A newer connection with the same station id took over.
    Replaced,
    /// The server shut down.
    Shutdown,
    /// A [`crate::Reply::Close`] fault or responder closed the socket.
    Injected,
    /// The station was removed with [`crate::CsmsHandle::forget`].
    Forgotten,
}

/// What happens on the server. Delivered through a `tokio::sync::broadcast` channel: a
/// subscriber that is too slow gets `RecvError::Lagged(n)` and misses `n` events; resync
/// with [`crate::CsmsHandle::stations`] and [`crate::CsmsHandle::history`] if that matters.
#[derive(Debug, Clone, PartialEq)]
pub enum CsmsEvent {
    /// A station connected (also emitted when it replaces an older connection).
    Connected { station: String },
    /// A station's connection ended.
    Disconnected {
        station: String,
        reason: DisconnectReason,
    },
    /// A frame crossed the wire.
    Frame {
        station: String,
        direction: Direction,
        frame: Frame,
    },
    /// Text that is not a valid OCPP-J frame arrived (truncated to 512 characters).
    Malformed { station: String, text: String },
    /// A station was removed from the registry.
    Forgotten { station: String },
}

/// What the registry knows about one station.
#[derive(Debug, Clone, PartialEq)]
pub struct StationInfo {
    pub id: String,
    pub connected: bool,
    /// When the current (or last) connection was established.
    pub connected_at: Option<DateTime<Utc>>,
    /// When the last connection ended; `None` while connected or never connected.
    pub disconnected_at: Option<DateTime<Utc>>,
    /// Remote address of the current (or last) connection.
    pub remote_addr: Option<SocketAddr>,
    /// Subprotocol negotiated for the current (or last) connection.
    pub subprotocol: Option<String>,
    /// CALLs received from the station over its whole life in the registry.
    pub received_calls: u64,
    /// The same, per action.
    pub received_by_action: BTreeMap<String, u64>,
}
