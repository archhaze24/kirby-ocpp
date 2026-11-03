use std::time::Duration;

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
    /// No station with this id is in the registry.
    #[error("unknown station {station}")]
    UnknownStation { station: String },
    /// The station is in the registry but has no open connection.
    #[error("station {station} is not connected")]
    NotConnected { station: String },
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
}
