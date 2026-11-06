use std::collections::BTreeMap;
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
    /// Bytes of history kept per station (frames as JSON text; oldest evicted first until the
    /// sum fits). A single frame bigger than this is stored with its payload replaced by the
    /// marker `{"truncated": true, "originalBytes": n}`. `0` keeps no history. Default 8 MiB.
    pub history_bytes_cap: usize,
    /// Stopped transactions kept in each station's [`crate::StationState`] (oldest dropped
    /// first). `0` keeps none; active transactions are always kept. Default 100.
    pub transaction_history_cap: usize,
    /// Capacity of the event broadcast channel. A subscriber that falls further behind gets
    /// `RecvError::Lagged` and loses events. Default 4096.
    pub event_buffer: usize,
    /// Longest time one frame write may take. A peer that stops reading is dropped when it
    /// expires. Default 10 seconds.
    pub write_timeout: Duration,
    /// Largest WebSocket message (and frame) accepted from a station, bytes; a station that
    /// sends more is disconnected. Default 1 MiB.
    pub max_message_size: usize,
    /// Most distinct station ids in the registry. A connection with a new id beyond it is
    /// refused with HTTP 503; known ids may still reconnect. Default 100 000.
    pub max_stations: usize,
    /// Most sockets in their TLS or WebSocket handshake at the same time (each lasts at most
    /// 10 seconds). A new socket beyond it is closed at once; established sessions are not
    /// counted. Default 1024.
    pub max_pending_handshakes: usize,
    /// `Some` serves `wss://` with this rustls configuration; `None` serves plain `ws://`.
    /// Build one from PEM files' contents with [`crate::server_config_from_pem`].
    pub tls: Option<Arc<ServerConfig>>,
    /// Who may connect. Default: everybody (profile 0). See [`SecurityPolicy`].
    pub security: SecurityPolicy,
    /// Keep `AuthorizationKey` values in recorded frames (history, events). Default `false`:
    /// they are replaced by `***`. The wire is unaffected.
    pub show_secrets: bool,
    /// Validate every inbound CALL against the embedded OCPP 1.6J schema; an invalid payload
    /// is answered with a CALLERROR (the code the station uses for the same mistake) instead
    /// of the canned or responder reply, and is not tracked. A [`crate::Fault`] on the same
    /// action is applied first. Unknown actions are not validated. Default `true`.
    pub validate_inbound: bool,
    /// Interval of server WebSocket pings. `Duration::ZERO` disables the watchdog.
    /// Default 30 seconds.
    pub ping_interval: Duration,
    /// A station that sends nothing (no pong, no frame) this long after a ping is dropped
    /// with [`crate::DisconnectReason::PingTimeout`]. Default 10 seconds.
    pub pong_timeout: Duration,
}

/// OCPP 1.6 Security Whitepaper connection security profile of the server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SecurityProfile {
    /// Profile 0: no authentication (plain `ws://` or server-auth-only `wss://`).
    #[default]
    None,
    /// Profile 1: HTTP Basic Auth, over `ws://` or `wss://`.
    BasicAuth,
    /// Profile 2: HTTP Basic Auth over TLS ([`CsmsConfig::tls`] is required).
    BasicAuthTls,
    /// Profile 3: TLS with a required client certificate whose Common Name is the station id
    /// ([`CsmsConfig::tls`] is required and must demand client certificates, see
    /// [`crate::server_config_with_client_ca`]).
    MutualTls,
}

impl SecurityProfile {
    /// The whitepaper's profile number, 0 to 3.
    pub fn number(self) -> u8 {
        match self {
            Self::None => 0,
            Self::BasicAuth => 1,
            Self::BasicAuthTls => 2,
            Self::MutualTls => 3,
        }
    }

    /// The profile with this number, if there is one.
    pub fn from_number(number: u8) -> Option<Self> {
        match number {
            0 => Some(Self::None),
            1 => Some(Self::BasicAuth),
            2 => Some(Self::BasicAuthTls),
            3 => Some(Self::MutualTls),
            _ => None,
        }
    }
}

/// Connection security of a [`crate::CsmsServer`].
///
/// With profiles 1 and 2 a station must send `Authorization: Basic base64(id:key)` where `id`
/// is the station id from the URL and `key` equals the key configured for that id (its entry
/// in [`SecurityPolicy::keys`], else [`SecurityPolicy::default_key`]); a station without a key
/// is refused. With profile 3 the TLS client certificate's Common Name must equal the id.
/// Refusals are HTTP 401 (403 for certificates) and emit [`crate::CsmsEvent::AuthFailed`].
/// Keys never appear in events, logs or the `Debug` output.
#[derive(Clone, Default)]
pub struct SecurityPolicy {
    pub profile: SecurityProfile,
    /// Station id to the key (the "AuthorizationKey" of that station) used as the Basic Auth
    /// password. Compared byte for byte.
    pub keys: BTreeMap<String, String>,
    /// Key for stations without an entry in `keys`.
    pub default_key: Option<String>,
}

impl fmt::Debug for SecurityPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecurityPolicy")
            .field("profile", &self.profile)
            .field("keys", &self.keys.len())
            .field("default_key", &self.default_key.is_some())
            .finish()
    }
}

impl SecurityPolicy {
    /// The key a station must present, if one is configured.
    pub(crate) fn key_for(&self, station: &str) -> Option<&str> {
        self.keys
            .get(station)
            .or(self.default_key.as_ref())
            .map(String::as_str)
            .filter(|key| !key.is_empty())
    }

    /// Whether any configured key is empty (refused by [`crate::CsmsServer::start`]).
    pub(crate) fn has_empty_key(&self) -> bool {
        self.default_key.as_deref() == Some("") || self.keys.values().any(String::is_empty)
    }
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
            history_bytes_cap: 8 * 1024 * 1024,
            transaction_history_cap: 100,
            event_buffer: 4096,
            write_timeout: Duration::from_secs(10),
            max_message_size: 1024 * 1024,
            max_stations: 100_000,
            max_pending_handshakes: 1024,
            tls: None,
            security: SecurityPolicy::default(),
            show_secrets: false,
            validate_inbound: true,
            ping_interval: Duration::from_secs(30),
            pong_timeout: Duration::from_secs(10),
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
            .field("history_bytes_cap", &self.history_bytes_cap)
            .field("transaction_history_cap", &self.transaction_history_cap)
            .field("event_buffer", &self.event_buffer)
            .field("write_timeout", &self.write_timeout)
            .field("max_message_size", &self.max_message_size)
            .field("max_stations", &self.max_stations)
            .field("max_pending_handshakes", &self.max_pending_handshakes)
            .field("tls", &self.tls.is_some())
            .field("security", &self.security)
            .field("show_secrets", &self.show_secrets)
            .field("validate_inbound", &self.validate_inbound)
            .field("ping_interval", &self.ping_interval)
            .field("pong_timeout", &self.pong_timeout)
            .finish()
    }
}
