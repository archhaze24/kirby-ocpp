use std::time::Duration;

use crate::tls::TlsOpts;

/// How the mock CSMS answers the WebSocket subprotocol negotiation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubprotocolPolicy {
    /// Select `name` if the client offered it; refuse the upgrade (HTTP 400) otherwise.
    /// The default is `Require("ocpp1.6")`.
    Require(String),
    /// Always select `name`, even if the client did not offer it. Use this to test
    /// a charge point that must notice a protocol it did not ask for.
    Select(String),
    /// Accept the upgrade without selecting any subprotocol (no
    /// `Sec-WebSocket-Protocol` response header).
    Omit,
    /// Refuse every upgrade with HTTP 400.
    Reject,
}

impl Default for SubprotocolPolicy {
    fn default() -> Self {
        Self::Require("ocpp1.6".to_owned())
    }
}

/// How `StartTransaction` transaction ids are allocated by the default responder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionIds {
    /// Every `StartTransaction` gets this id (the TS harness uses 123).
    Fixed(i32),
    /// The first `StartTransaction` gets this id, each following one gets the next integer.
    Incrementing(i32),
}

impl Default for TransactionIds {
    fn default() -> Self {
        Self::Fixed(123)
    }
}

/// Configuration of a [`crate::MockCsms`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsmsOptions {
    /// Subprotocol negotiation policy. Default: `Require("ocpp1.6")`.
    pub subprotocol: SubprotocolPolicy,
    /// `interval` (seconds) returned by the default `BootNotification` answer. Default 5.
    pub heartbeat_interval: u32,
    /// Transaction id allocation for the default `StartTransaction` answer. Default `Fixed(123)`.
    pub transaction_ids: TransactionIds,
    /// Timeout used by [`crate::MockCsms::send_call`]. Default 3 seconds.
    pub call_timeout: Duration,
    /// Path prefix of the central system URL. [`crate::MockCsms::url`] includes it, so
    /// `url()` can be used as the emulator's `centralSystemUrl` (the charge point id
    /// is appended as a path segment). Default `/ocpp`.
    pub base_path: String,
    /// `Some` serves `wss://` with generated certificates; `None` serves plain `ws://`.
    pub tls: Option<TlsOpts>,
}

impl Default for CsmsOptions {
    fn default() -> Self {
        Self {
            subprotocol: SubprotocolPolicy::default(),
            heartbeat_interval: 5,
            transaction_ids: TransactionIds::default(),
            call_timeout: Duration::from_secs(3),
            base_path: "/ocpp".to_owned(),
            tls: None,
        }
    }
}
