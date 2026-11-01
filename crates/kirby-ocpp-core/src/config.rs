//! Station configuration.
//!
//! [`StationConfig`] is plain data (serde, no clap): the CLI crate maps flags and
//! environment variables onto it, scenarios or config files can deserialize it.
//! Field names are Rust-style; the mapping to the TS `StationConfig` is noted per field.
//! Durations are stored as integers with the unit in the field name and exposed
//! as [`std::time::Duration`] through helper methods, which keeps the JSON form obvious.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::ids::ConnectorId;
use crate::station::response_rules::{ResponseRule, canonical_action};

/// Defaults shared with the TS CLI (`src/cli.ts`).
pub mod defaults {
    pub const CHARGE_POINT_ID: &str = "CP-001";
    pub const VENDOR: &str = "archhaze24";
    pub const MODEL: &str = "KIRBY";
    pub const HEARTBEAT_INTERVAL_SECS: u32 = 30;
    pub const WS_SUBPROTOCOL: &str = "ocpp1.6";
    pub const WS_PING_INTERVAL_SECS: u32 = 30;
    pub const RECONNECT_INITIAL_MS: u64 = 1_000;
    pub const RECONNECT_MAX_MS: u64 = 30_000;
    pub const CALL_TIMEOUT_MS: u64 = 30_000;
    /// Nominal charging power of one connector (7.2 kW = 230 V x 31.3 A, single phase).
    pub const NOMINAL_POWER_W: u32 = 7_200;
    pub const VOLTAGE_V: u32 = 230;
    pub const PHASES: u8 = 1;
}

/// Automatic reconnect policy after an unexpected disconnect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReconnectConfig {
    /// TS: `webSocketReconnectEnabled`.
    pub enabled: bool,
    /// TS: `webSocketReconnectInitialDelayMs`.
    pub initial_delay_ms: u64,
    /// TS: `webSocketReconnectMaxDelayMs`.
    pub max_delay_ms: u64,
    /// TS: `webSocketReconnectMaxAttempts`; `0` = unlimited.
    pub max_attempts: u32,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            initial_delay_ms: defaults::RECONNECT_INITIAL_MS,
            max_delay_ms: defaults::RECONNECT_MAX_MS,
            max_attempts: 0,
        }
    }
}

impl ReconnectConfig {
    /// Delay before reconnect attempt number `attempt` (1-based):
    /// `min(initial * 2^(attempt-1), max)`. Saturating: huge attempt numbers
    /// cannot overflow. `attempt == 0` is treated as 1. No jitter (same as TS).
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(63);
        let factor = 1_u64 << exponent;
        let millis = self
            .initial_delay_ms
            .saturating_mul(factor)
            .min(self.max_delay_ms);
        Duration::from_millis(millis)
    }

    /// Whether attempt number `attempt` (1-based) may still be made.
    pub fn allows_attempt(&self, attempt: u32) -> bool {
        self.enabled && (self.max_attempts == 0 || attempt <= self.max_attempts)
    }
}

/// TLS settings for `wss://` URLs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TlsConfig {
    /// TS: `tlsCaFile`. Replaces the system roots when set (like Node's `ca`).
    pub ca_file: Option<PathBuf>,
    /// TS: `tlsCertFile`. Client certificate for mutual TLS.
    pub cert_file: Option<PathBuf>,
    /// TS: `tlsKeyFile`.
    pub key_file: Option<PathBuf>,
    /// TS: `tlsServerName`. SNI / verification name override.
    pub server_name: Option<String>,
    /// TS: `!tlsRejectUnauthorized`. Disables certificate verification (testing only).
    pub skip_verify: bool,
}

/// Everything needed to run one emulated charge point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StationConfig {
    /// TS: `centralSystemUrl`. Base URL; the charge point id is appended as the last path segment.
    pub url: String,
    /// TS: `chargePointId`.
    pub charge_point_id: String,
    pub vendor: String,
    pub model: String,
    /// `chargePointSerialNumber` of `BootNotification` (<= 25 characters); omitted when `None`.
    pub serial_number: Option<String>,
    /// `firmwareVersion` of `BootNotification` (<= 50 characters); omitted when `None`.
    pub firmware_version: Option<String>,
    /// TS: `connectorId`. The connector UIs select initially (not a special connector in the core).
    pub initial_connector: ConnectorId,
    /// TS: `connectorCount`. Raised to at least `initial_connector` by [`StationConfig::normalize`].
    pub connector_count: u32,
    /// TS: `heartbeatIntervalSeconds`. Fallback until `BootNotification.conf` provides one.
    pub heartbeat_interval_secs: u32,
    /// TS: `webSocketSubprotocol`.
    pub ws_subprotocol: String,
    /// TS: `webSocketPingIntervalSeconds`; `0` disables pings.
    pub ws_ping_interval_secs: u32,
    pub reconnect: ReconnectConfig,
    /// TS: `callTimeoutMs`.
    pub call_timeout_ms: u64,
    pub tls: TlsConfig,
    /// TS: `persistState`.
    pub persist: bool,
    /// TS: `stateDirectory`. `None` = the per-user default, see [`StationConfig::state_dir_or_default`].
    pub state_dir: Option<PathBuf>,
    /// Nominal charging power of each connector in watts: the simulated power never exceeds it.
    pub nominal_power_w: u32,
    /// Supply voltage per phase in volts (used for A <-> W conversion and the `Voltage` reading).
    pub voltage_v: u32,
    /// Number of phases the station charges with (1..=3).
    pub phases: u8,
    /// Response rules active from the start (see `station::response_rules`); runtime-only.
    pub response_rules: Vec<ResponseRule>,
}

impl Default for StationConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            charge_point_id: defaults::CHARGE_POINT_ID.to_owned(),
            vendor: defaults::VENDOR.to_owned(),
            model: defaults::MODEL.to_owned(),
            serial_number: None,
            firmware_version: None,
            initial_connector: ConnectorId::new(1),
            connector_count: 1,
            heartbeat_interval_secs: defaults::HEARTBEAT_INTERVAL_SECS,
            ws_subprotocol: defaults::WS_SUBPROTOCOL.to_owned(),
            ws_ping_interval_secs: defaults::WS_PING_INTERVAL_SECS,
            reconnect: ReconnectConfig::default(),
            call_timeout_ms: defaults::CALL_TIMEOUT_MS,
            tls: TlsConfig::default(),
            persist: true,
            state_dir: None,
            nominal_power_w: defaults::NOMINAL_POWER_W,
            voltage_v: defaults::VOLTAGE_V,
            phases: defaults::PHASES,
            response_rules: Vec::new(),
        }
    }
}

/// An invalid [`StationConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigError {
    #[error("url: {0}")]
    InvalidUrl(String),
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} must be greater than zero")]
    NotPositive { field: &'static str },
    #[error("tls.cert_file and tls.key_file must be given together")]
    TlsCertKeyMismatch,
    #[error(
        "reconnect: a zero initial or maximum delay with unlimited attempts would retry in a \
         busy loop; use a delay of at least 1 ms, limit the attempts or disable reconnect"
    )]
    ReconnectBusyLoop,
    #[error("phases must be 1, 2 or 3")]
    InvalidPhases,
    #[error("response rule `{0}`: action is not an inbound CSMS command or count is zero")]
    InvalidResponseRule(String),
}

impl StationConfig {
    /// Config for `url` with every other field at its default.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ..Self::default()
        }
    }

    pub fn with_charge_point_id(mut self, id: impl Into<String>) -> Self {
        self.charge_point_id = id.into();
        self
    }

    pub fn with_vendor_model(
        mut self,
        vendor: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        self.vendor = vendor.into();
        self.model = model.into();
        self
    }

    pub fn with_connectors(mut self, count: u32) -> Self {
        self.connector_count = count;
        self
    }

    pub fn with_heartbeat_interval_secs(mut self, secs: u32) -> Self {
        self.heartbeat_interval_secs = secs;
        self
    }

    pub fn with_reconnect(mut self, reconnect: ReconnectConfig) -> Self {
        self.reconnect = reconnect;
        self
    }

    pub fn with_call_timeout_ms(mut self, ms: u64) -> Self {
        self.call_timeout_ms = ms;
        self
    }

    pub fn with_tls(mut self, tls: TlsConfig) -> Self {
        self.tls = tls;
        self
    }

    /// Disables persistence (an ephemeral station).
    pub fn without_persistence(mut self) -> Self {
        self.persist = false;
        self
    }

    pub fn with_state_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.state_dir = Some(dir.into());
        self
    }

    pub fn call_timeout(&self) -> Duration {
        Duration::from_millis(self.call_timeout_ms)
    }

    pub fn heartbeat_interval(&self) -> Duration {
        Duration::from_secs(u64::from(self.heartbeat_interval_secs))
    }

    /// `None` when pings are disabled (`ws_ping_interval_secs == 0`).
    pub fn ws_ping_interval(&self) -> Option<Duration> {
        (self.ws_ping_interval_secs > 0)
            .then(|| Duration::from_secs(u64::from(self.ws_ping_interval_secs)))
    }

    /// Raises `connector_count` to at least `initial_connector`, as the TS CLI does.
    pub fn normalize(&mut self) {
        self.connector_count = self.connector_count.max(self.initial_connector.get());
    }

    /// Checks the invariants the rest of the code relies on.
    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_ws_url(&self.url).map_err(ConfigError::InvalidUrl)?;
        for (field, value) in [
            ("charge_point_id", &self.charge_point_id),
            ("vendor", &self.vendor),
            ("model", &self.model),
            ("ws_subprotocol", &self.ws_subprotocol),
        ] {
            if value.is_empty() {
                return Err(ConfigError::Empty { field });
            }
        }
        for (field, value) in [
            ("connector_count", u64::from(self.connector_count)),
            ("initial_connector", u64::from(self.initial_connector.get())),
            (
                "heartbeat_interval_secs",
                u64::from(self.heartbeat_interval_secs),
            ),
            ("call_timeout_ms", self.call_timeout_ms),
            ("nominal_power_w", u64::from(self.nominal_power_w)),
            ("voltage_v", u64::from(self.voltage_v)),
        ] {
            if value == 0 {
                return Err(ConfigError::NotPositive { field });
            }
        }
        if !(1..=3).contains(&self.phases) {
            return Err(ConfigError::InvalidPhases);
        }
        if self.tls.cert_file.is_some() != self.tls.key_file.is_some() {
            return Err(ConfigError::TlsCertKeyMismatch);
        }
        let reconnect = &self.reconnect;
        if reconnect.enabled
            && reconnect.max_attempts == 0
            && reconnect.initial_delay_ms.min(reconnect.max_delay_ms) == 0
        {
            return Err(ConfigError::ReconnectBusyLoop);
        }
        for rule in &self.response_rules {
            if canonical_action(&rule.action) != Ok(rule.action.as_str())
                || rule.remaining == Some(0)
            {
                return Err(ConfigError::InvalidResponseRule(rule.to_string()));
            }
        }
        Ok(())
    }

    /// The state directory: `state_dir` if set, else `~/.kirby-ocpp/stations`
    /// (`%USERPROFILE%` on Windows). `None` if no home directory can be determined.
    pub fn state_dir_or_default(&self) -> Option<PathBuf> {
        self.state_dir.clone().or_else(|| {
            let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
            Some(PathBuf::from(home).join(".kirby-ocpp").join("stations"))
        })
    }
}

/// Checks that `value` is an absolute `ws://` or `wss://` URL with a host.
/// Used by [`StationConfig::validate`] and by CLI value parsers.
pub fn validate_ws_url(value: &str) -> Result<(), String> {
    let url = Url::parse(value).map_err(|e| format!("invalid URL `{value}`: {e}"))?;
    if !matches!(url.scheme(), "ws" | "wss") {
        return Err(format!("`{value}` must use the ws:// or wss:// scheme"));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(format!("`{value}` has no host"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> StationConfig {
        StationConfig::new("ws://localhost:9000/ocpp")
    }

    #[test]
    fn defaults_match_the_ts_cli() {
        let c = StationConfig::default();
        assert_eq!(c.charge_point_id, "CP-001");
        assert_eq!(c.vendor, "archhaze24");
        assert_eq!(c.model, "KIRBY");
        assert_eq!(c.connector_count, 1);
        assert_eq!(c.heartbeat_interval(), Duration::from_secs(30));
        assert_eq!(c.ws_subprotocol, "ocpp1.6");
        assert_eq!(c.ws_ping_interval(), Some(Duration::from_secs(30)));
        assert_eq!(c.call_timeout(), Duration::from_secs(30));
        assert!(c.persist);
        assert!(c.reconnect.enabled);
        assert!(!c.tls.skip_verify);
    }

    #[test]
    fn zero_ping_interval_disables_pings() {
        let mut c = valid();
        c.ws_ping_interval_secs = 0;
        assert_eq!(c.ws_ping_interval(), None);
    }

    #[test]
    fn valid_config_validates() {
        assert_eq!(valid().validate(), Ok(()));
        assert_eq!(
            StationConfig::new("wss://csms.example.com/ocpp").validate(),
            Ok(())
        );
    }

    #[test]
    fn rejects_bad_urls() {
        for url in [
            "",
            "http://localhost",
            "localhost:9000",
            "ws://",
            "not a url",
        ] {
            assert!(
                matches!(
                    StationConfig::new(url).validate(),
                    Err(ConfigError::InvalidUrl(_))
                ),
                "{url:?}"
            );
        }
    }

    #[test]
    fn zero_reconnect_delay_with_unlimited_attempts_is_rejected() {
        let with = |enabled, initial_delay_ms, max_delay_ms, max_attempts| {
            StationConfig::new("ws://h/p")
                .with_reconnect(ReconnectConfig {
                    enabled,
                    initial_delay_ms,
                    max_delay_ms,
                    max_attempts,
                })
                .validate()
        };
        assert_eq!(
            with(true, 0, 30_000, 0),
            Err(ConfigError::ReconnectBusyLoop)
        );
        assert_eq!(with(true, 1_000, 0, 0), Err(ConfigError::ReconnectBusyLoop));
        // Bounded attempts, a real delay or reconnect off are all fine.
        assert_eq!(with(true, 0, 30_000, 5), Ok(()));
        assert_eq!(with(true, 1, 30_000, 0), Ok(()));
        assert_eq!(with(false, 0, 0, 0), Ok(()));
    }

    #[test]
    fn power_model_defaults_and_validation() {
        let c = valid();
        assert_eq!((c.nominal_power_w, c.voltage_v, c.phases), (7200, 230, 1));
        let with = |f: fn(&mut StationConfig)| {
            let mut c = valid();
            f(&mut c);
            c.validate()
        };
        assert_eq!(with(|c| c.phases = 3), Ok(()));
        assert_eq!(with(|c| c.phases = 0), Err(ConfigError::InvalidPhases));
        assert_eq!(with(|c| c.phases = 4), Err(ConfigError::InvalidPhases));
        assert_eq!(
            with(|c| c.nominal_power_w = 0),
            Err(ConfigError::NotPositive {
                field: "nominal_power_w"
            })
        );
        assert_eq!(
            with(|c| c.voltage_v = 0),
            Err(ConfigError::NotPositive { field: "voltage_v" })
        );
    }

    #[test]
    fn rejects_empty_and_zero_fields() {
        assert_eq!(
            valid().with_charge_point_id("").validate(),
            Err(ConfigError::Empty {
                field: "charge_point_id"
            })
        );
        assert_eq!(
            valid().with_connectors(0).validate(),
            Err(ConfigError::NotPositive {
                field: "connector_count"
            })
        );
        assert_eq!(
            valid().with_heartbeat_interval_secs(0).validate(),
            Err(ConfigError::NotPositive {
                field: "heartbeat_interval_secs"
            })
        );
        assert_eq!(
            valid().with_call_timeout_ms(0).validate(),
            Err(ConfigError::NotPositive {
                field: "call_timeout_ms"
            })
        );
    }

    #[test]
    fn tls_cert_and_key_must_come_together() {
        let tls = TlsConfig {
            cert_file: Some("c.pem".into()),
            ..TlsConfig::default()
        };
        assert_eq!(
            valid().with_tls(tls).validate(),
            Err(ConfigError::TlsCertKeyMismatch)
        );
    }

    #[test]
    fn normalize_raises_connector_count_to_initial_connector() {
        let mut c = valid();
        c.initial_connector = ConnectorId::new(3);
        c.connector_count = 1;
        c.normalize();
        assert_eq!(c.connector_count, 3);
        c.connector_count = 5;
        c.normalize();
        assert_eq!(c.connector_count, 5);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let r = ReconnectConfig::default();
        let ms: Vec<u128> = (1..=7)
            .map(|n| r.delay_for_attempt(n).as_millis())
            .collect();
        assert_eq!(ms, [1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000]);
        assert_eq!(r.delay_for_attempt(0), r.delay_for_attempt(1));
    }

    #[test]
    fn backoff_saturates_for_huge_attempts() {
        let r = ReconnectConfig::default();
        assert_eq!(r.delay_for_attempt(u32::MAX), Duration::from_secs(30));
        let wide = ReconnectConfig {
            initial_delay_ms: u64::MAX,
            max_delay_ms: u64::MAX,
            ..ReconnectConfig::default()
        };
        assert_eq!(wide.delay_for_attempt(200), Duration::from_millis(u64::MAX));
    }

    #[test]
    fn max_attempts_zero_means_unlimited() {
        let unlimited = ReconnectConfig::default();
        assert!(unlimited.allows_attempt(1_000_000));
        let limited = ReconnectConfig {
            max_attempts: 3,
            ..ReconnectConfig::default()
        };
        assert!(limited.allows_attempt(3));
        assert!(!limited.allows_attempt(4));
        let disabled = ReconnectConfig {
            enabled: false,
            ..ReconnectConfig::default()
        };
        assert!(!disabled.allows_attempt(1));
    }

    #[test]
    fn serde_roundtrip_and_partial_json_uses_defaults() {
        let c = valid().with_connectors(4).without_persistence();
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<StationConfig>(&json).unwrap(), c);

        let partial: StationConfig =
            serde_json::from_str(r#"{"url":"ws://h/p","reconnect":{"enabled":false}}"#).unwrap();
        assert_eq!(partial.charge_point_id, "CP-001");
        assert!(!partial.reconnect.enabled);
        assert_eq!(partial.reconnect.max_delay_ms, 30_000);
    }

    #[test]
    fn explicit_state_dir_wins() {
        let c = valid().with_state_dir("/tmp/x");
        assert_eq!(c.state_dir_or_default(), Some(PathBuf::from("/tmp/x")));
    }
}
