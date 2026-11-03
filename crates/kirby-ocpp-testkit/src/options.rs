use std::time::Duration;

use kirby_csms_core::{SubprotocolPolicy, TransactionIds};

use crate::tls::TlsOpts;

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
