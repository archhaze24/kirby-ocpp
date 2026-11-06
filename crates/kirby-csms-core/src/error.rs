use std::time::Duration;

use kirby_ocpp_core::ocpp::ErrorCode;

/// Errors from the CSMS engine.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CsmsError {
    /// The listener could not be bound.
    #[error("failed to bind the CSMS listener: {0}")]
    Bind(#[source] std::io::Error),
    /// TLS material could not be turned into a server configuration.
    #[error("TLS setup failed: {0}")]
    Tls(String),
    /// The configuration is inconsistent (nothing was started).
    #[error("invalid configuration: {0}")]
    Config(String),
    /// No station with this id is in the registry.
    #[error("unknown station {station}")]
    UnknownStation { station: String },
    /// The station is in the registry but has no open connection.
    #[error("station {station} is not connected")]
    NotConnected { station: String },
    /// Too many CALLs are already waiting for this station (it answers one at a time).
    #[error("too many calls are waiting for station {station}")]
    QueueFull { station: String },
    /// The station did not answer a CALL in time.
    #[error("station {station} did not answer {action} within {timeout:?}")]
    Timeout {
        station: String,
        action: String,
        timeout: Duration,
    },
    /// The connection closed (or was replaced) before the station answered.
    #[error("connection to {station} closed before a reply to {action} arrived")]
    ConnectionClosed { station: String, action: String },
    /// The station answered with a CALLERROR (typed calls only; [`crate::CsmsHandle::call`]
    /// returns the raw frame).
    #[error("station {station} answered {action} with CALLERROR {code}: {description}")]
    CallError {
        station: String,
        action: String,
        code: ErrorCode,
        description: String,
    },
    /// A typed request could not be serialised or violates the OCPP 1.6J schema; nothing
    /// was sent.
    #[error("invalid {action} request: {reason}")]
    InvalidRequest { action: String, reason: String },
    /// A CALLRESULT did not match the schema or the typed response of its action.
    #[error("invalid {action} response from {station}: {reason}")]
    InvalidResponse {
        station: String,
        action: String,
        reason: String,
    },
}
