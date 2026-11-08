//! Error types shared across the crate.

use std::time::Duration;

use serde_json::Value;
use thiserror::Error;

use crate::ids::{ConnectorId, LocalTxIdError};
use crate::ocpp::frame::ErrorCode;
use crate::ocpp::schema::SchemaViolation;
use crate::store::StoreError;

/// Why an outbound CALL did not produce a usable CALLRESULT.
///
/// The transaction-message retry logic treats every variant except
/// [`CallError::NotConnected`] as "attempt failed" (CALLERROR, timeout and a
/// schema-violating response are all retryable, as in the TS station).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CallError {
    /// No open connection when the CALL was submitted.
    #[error("not connected to the central system")]
    NotConnected,
    /// The registration state forbids this message: only `BootNotification` may be sent
    /// until the central system accepted it (CSMS-triggered messages are also allowed
    /// while `Pending`). `state` is the registration status or `unregistered`.
    #[error("{action} not sent: BootNotification is {state}")]
    NotRegistered { action: String, state: String },
    /// The connection closed while the CALL was queued or in flight.
    #[error("connection closed (code {})", .code.map_or_else(|| "none".to_owned(), |c| c.to_string()))]
    Closed { code: Option<u16> },
    /// No CALLRESULT/CALLERROR within the call timeout.
    #[error("{action} timed out after {after:?}")]
    Timeout { action: String, after: Duration },
    /// The caller stopped waiting (`CancelCall`); the CALL may still be answered later.
    #[error("{action} cancelled")]
    Cancelled { action: String },
    /// The central system answered with a CALLERROR.
    #[error("{action} failed: {code} {description}")]
    Rpc {
        action: String,
        code: ErrorCode,
        description: String,
        details: Value,
    },
    /// The request we were about to send, or the response we received, violates the schema.
    #[error(transparent)]
    Schema(#[from] SchemaViolation),
    /// The request payload could not be serialized.
    #[error("cannot serialize the {action} request: {source}")]
    Serialize {
        action: String,
        source: serde_json::Error,
    },
    /// A schema-valid response could not be decoded into the typed response.
    #[error("cannot decode the {action} response: {source}")]
    Decode {
        action: String,
        source: serde_json::Error,
    },
}

/// Error returned by [`crate::station::StationHandle`] operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StationError {
    /// The station actor has stopped (it was shut down or panicked).
    #[error("the station has stopped")]
    Stopped,
    /// The operation needs a connection to the central system.
    #[error("not connected to the central system")]
    NotConnected,
    #[error("connector {0} does not exist")]
    UnknownConnector(ConnectorId),
    /// The command is not valid in the current state (e.g. unplug with no EV connected).
    #[error("{reason}")]
    InvalidState {
        connector: Option<ConnectorId>,
        reason: String,
    },
    /// The command carries an argument that is out of range or malformed.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error(transparent)]
    Call(#[from] CallError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    LocalTransaction(#[from] LocalTxIdError),
    /// Temporary: the actor stub does not implement this command yet. Removed
    /// (with its only producer) once all command handlers exist.
    #[error("command `{command}` is not implemented yet")]
    NotImplemented { command: &'static str },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_error_messages_name_the_action() {
        let timeout = CallError::Timeout {
            action: "Authorize".into(),
            after: Duration::from_secs(30),
        };
        assert_eq!(timeout.to_string(), "Authorize timed out after 30s");

        let rpc = CallError::Rpc {
            action: "Authorize".into(),
            code: ErrorCode::NotSupported,
            description: "no".into(),
            details: Value::Null,
        };
        assert_eq!(rpc.to_string(), "Authorize failed: NotSupported no");
        assert_eq!(
            CallError::Closed { code: Some(1006) }.to_string(),
            "connection closed (code 1006)"
        );
        assert_eq!(
            CallError::Closed { code: None }.to_string(),
            "connection closed (code none)"
        );
    }

    #[test]
    fn station_error_wraps_call_error_transparently() {
        let err: StationError = CallError::NotConnected.into();
        assert_eq!(err.to_string(), "not connected to the central system");
    }
}
