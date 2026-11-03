use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use rustls::ServerConfig;

use crate::policy::{SubprotocolPolicy, TransactionIds};

/// Configuration of a [`crate::CsmsServer`].
#[derive(Clone)]
pub struct CsmsConfig {
    /// Address to listen on. Default `127.0.0.1:9000`; use port 0 for an ephemeral port.
    pub listen: SocketAddr,
    /// Subprotocol negotiation policy. Default `Require("ocpp1.6")`.
    pub subprotocol: SubprotocolPolicy,
    /// `interval` (seconds) in the default `BootNotification` answer. Default 300.
    pub heartbeat_interval: u32,
    /// Transaction id allocation for the default `StartTransaction` answer. Default `Fixed(123)`.
    pub transaction_ids: TransactionIds,
    /// Default timeout of [`crate::CsmsHandle::call`], counted from the moment the CALL is
    /// written to the socket. Default 30 seconds.
    pub call_timeout: Duration,
    /// Frames kept per station (oldest dropped first). `0` keeps none. Default 1000.
    pub history_cap: usize,
    /// Capacity of the event broadcast channel. A subscriber that falls further behind gets
    /// `RecvError::Lagged` and loses events. Default 4096.
    pub event_buffer: usize,
    /// `Some` serves `wss://` with this rustls configuration; `None` serves plain `ws://`.
    /// Build one from PEM files' contents with [`crate::server_config_from_pem`].
    pub tls: Option<Arc<ServerConfig>>,
}

impl CsmsConfig {
    /// A configuration listening on `listen`, everything else default.
    pub fn new(listen: SocketAddr) -> Self {
        Self {
            listen,
            ..Self::default()
        }
    }
}

impl Default for CsmsConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from((Ipv4Addr::LOCALHOST, 9000)),
            subprotocol: SubprotocolPolicy::default(),
            heartbeat_interval: 300,
            transaction_ids: TransactionIds::default(),
            call_timeout: Duration::from_secs(30),
            history_cap: 1000,
            event_buffer: 4096,
            tls: None,
        }
    }
}

impl fmt::Debug for CsmsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CsmsConfig")
            .field("listen", &self.listen)
            .field("subprotocol", &self.subprotocol)
            .field("heartbeat_interval", &self.heartbeat_interval)
            .field("transaction_ids", &self.transaction_ids)
            .field("call_timeout", &self.call_timeout)
            .field("history_cap", &self.history_cap)
            .field("event_buffer", &self.event_buffer)
            .field("tls", &self.tls.is_some())
            .finish()
    }
}
