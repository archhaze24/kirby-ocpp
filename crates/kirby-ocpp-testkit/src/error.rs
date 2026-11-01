use std::time::Duration;

/// Errors from the mock CSMS and its helpers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TestkitError {
    /// The loopback listener could not be bound.
    #[error("failed to start the mock CSMS: {0}")]
    Start(#[source] std::io::Error),
    /// Certificate generation or TLS configuration failed.
    #[error("TLS setup failed: {0}")]
    Tls(String),
    /// No charge point is connected (or the connection went away).
    #[error("no charge point is connected")]
    NotConnected,
    /// A wait did not complete within its timeout.
    #[error("timed out after {timeout:?} waiting for {what}")]
    Timeout { what: String, timeout: Duration },
    /// The charge point disconnected before answering a CSMS-originated CALL.
    #[error("connection closed before a reply to {action} arrived")]
    ConnectionClosed { action: String },
    /// An `expect_*` helper received a different reply than expected.
    #[error("{action}: expected {expected}, got {got}")]
    UnexpectedReply {
        action: String,
        expected: String,
        got: String,
    },
}

/// Result alias used throughout the crate.
pub type TestkitResult<T> = Result<T, TestkitError>;
