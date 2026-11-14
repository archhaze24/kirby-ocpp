use chrono::{DateTime, Utc};
use serde_json::Value;

/// A CALL that a charge point sent to the mock CSMS.
#[derive(Debug, Clone, PartialEq)]
pub struct Received {
    /// When the CALL was received.
    pub ts: DateTime<Utc>,
    /// Id of the connection it arrived on (see [`ConnectionInfo::id`]).
    pub connection: u64,
    /// Subprotocol selected on that connection, if any (see [`ConnectionInfo::subprotocol`]).
    pub subprotocol: Option<String>,
    /// OCPP-J message id of the CALL.
    pub message_id: String,
    /// Action name, e.g. `StartTransaction`.
    pub action: String,
    /// Payload as sent (before any reply was generated).
    pub payload: Value,
}

/// A WebSocket connection accepted by the mock CSMS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInfo {
    /// Sequential id, starting at 1.
    pub id: u64,
    /// Request path of the upgrade, e.g. `/ocpp/CP-001`.
    pub path: String,
    /// Last non-empty path segment (the charge point id), if any.
    pub charge_point_id: Option<String>,
    /// Subprotocol selected in the handshake, if any.
    pub subprotocol: Option<String>,
    /// `false` once the socket has closed (either side).
    pub open: bool,
}

/// The charge point id is the last non-empty path segment.
pub(crate) fn charge_point_id_from_path(path: &str) -> Option<String> {
    path.rsplit('/')
        .find(|segment| !segment.is_empty())
        .map(str::to_owned)
}
