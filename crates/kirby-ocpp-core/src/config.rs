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

/// Longest `chargePointVendor` and `chargePointModel` of `BootNotification` (OCPP 1.6 schema).
const BOOT_VENDOR_MAX: usize = 20;
const BOOT_MODEL_MAX: usize = 20;
/// Longest `chargePointSerialNumber` of `BootNotification`.
const BOOT_SERIAL_MAX: usize = 25;
/// Longest `firmwareVersion` of `BootNotification`.
const BOOT_FIRMWARE_MAX: usize = 50;

/// Upper bound of `call_timeout_ms`: 24 hours.
pub const MAX_CALL_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;

/// Defaults shared with the TS CLI (`src/cli.ts`).
pub mod defaults {
    pub const CHARGE_POINT_ID: &str = "CP-001";
    pub const VENDOR: &str = "archhaze24";
    pub const MODEL: &str = "kirby-ocpp";
    pub const HEARTBEAT_INTERVAL_SECS: u32 = 30;
    pub const WS_SUBPROTOCOL: &str = crate::ocpp::Protocol::V16.token();
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
    /// Client identity held in memory (the certificate the CSMS signed for us, Security Profile 3).
    /// Filled in by the station at connect time; never serialized.
    #[serde(skip)]
    pub identity_pem: Option<IdentityPem>,
    /// Extra trust anchors (installed Central System root certificates, PEM); filled in by the
    /// station at connect time and used in addition to `ca_file` / the system roots.
    #[serde(skip)]
    pub extra_roots_pem: Vec<String>,
    /// Accept a wildcard server certificate (`AllowCentralSystemTLSWildcards`); filled in by the
    /// station at connect time. Without it a certificate with `*` in a subject alternative DNS
    /// name or the Common Name is refused.
    #[serde(skip)]
    pub allow_wildcard_certificates: bool,
}

/// A client certificate chain and its private key as PEM text.
#[derive(Clone, PartialEq, Eq)]
pub struct IdentityPem {
    pub chain_pem: String,
    pub key_pem: String,
}

impl std::fmt::Debug for IdentityPem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdentityPem").finish_non_exhaustive()
    }
}

/// How the `AuthorizationKey` configuration value becomes the Basic Auth password (OCPP 1.6J
/// registry key only; a password written in the URL is always sent as the URL says).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthorizationKeyEncoding {
    /// The key text is the password (the OCPP 2.0.1 A00.FR.205 form).
    #[default]
    Literal,
    /// The key is hexadecimal text and the password is the bytes it encodes (OCPP-J 1.6 section
    /// 6.2.2).
    Hex,
}

impl AuthorizationKeyEncoding {
    /// The configuration value spelled as the flag, environment variable and file key do.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Literal => "literal",
            Self::Hex => "hex",
        }
    }

    /// Whether `key` is a valid `AuthorizationKey` value for this encoding.
    #[must_use]
    pub fn accepts(self, key: &str) -> bool {
        match self {
            Self::Literal => true,
            Self::Hex => decode_hex(key).is_some(),
        }
    }

    /// The Basic Auth password bytes for `key`; `None` when a hex key is not valid hex.
    #[must_use]
    pub fn password_bytes(self, key: &str) -> Option<Vec<u8>> {
        match self {
            Self::Literal => Some(key.as_bytes().to_vec()),
            Self::Hex => decode_hex(key),
        }
    }
}

/// Decodes a nonempty, even-length string of hexadecimal digits (either case).
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let raw = text.as_bytes();
    if raw.is_empty() || !raw.len().is_multiple_of(2) || !raw.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    raw.as_chunks::<2>()
        .0
        .iter()
        .map(|&[high, low]| {
            let high = char::from(high).to_digit(16)?;
            let low = char::from(low).to_digit(16)?;
            u8::try_from(high * 16 + low).ok()
        })
        .collect()
}

/// Everything needed to run one emulated charge point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent switches of one configuration record"
)]
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
    /// Record `AuthorizationKey` values in frame logs as they are (default `false`: they
    /// are replaced by `***` in `Event::Frame`; the wire is unaffected).
    pub show_secrets: bool,
    /// Initial value of the read-only `AdditionalRootCertificateCheck` key (Security Whitepaper
    /// `M05.FR.08` to `M05.FR.13`): a new Central System root must be signed by the installed one.
    pub additional_root_certificate_check: bool,
    /// Security Whitepaper `A05.FR.08`: after a change of `SecurityProfile` that the Central
    /// System does not accept (repeated profile-attributable connection failures before the
    /// first successful connection) the station goes back to the previous profile.
    pub security_profile_fallback: bool,
    /// How the `AuthorizationKey` registry value is turned into the Basic Auth password
    /// (OCPP 1.6J); `hex` also makes the key accept only hexadecimal digits.
    pub authorization_key_encoding: AuthorizationKeyEncoding,
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
            show_secrets: false,
            additional_root_certificate_check: false,
            security_profile_fallback: true,
            authorization_key_encoding: AuthorizationKeyEncoding::Literal,
        }
    }
}

/// An invalid [`StationConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigError {
    #[error("url: {0}")]
    InvalidUrl(String),
    #[error("charge point id `{0}` cannot be used as a URL path segment")]
    InvalidChargePointId(String),
    #[error("ws_subprotocol `{0}` must consist of RFC 6455 token characters")]
    InvalidSubprotocol(String),
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} must be greater than zero")]
    NotPositive { field: &'static str },
    #[error("{field} must be at most {max}")]
    TooLarge { field: &'static str, max: u64 },
    #[error("--tls-cert and --tls-key must be given together")]
    TlsCertKeyMismatch,
    #[error("{flag} must be at most {max} characters")]
    TooLong { flag: &'static str, max: usize },
    #[error("--id `{0}` contains control or invisible characters")]
    UnsafeChargePointId(String),
    #[error("{flag} {}: {reason}", path.display())]
    TlsFile {
        flag: &'static str,
        path: PathBuf,
        reason: String,
    },
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
        if matches!(self.charge_point_id.as_str(), "." | "..") {
            return Err(ConfigError::InvalidChargePointId(
                self.charge_point_id.clone(),
            ));
        }
        if self
            .charge_point_id
            .chars()
            .any(crate::text::is_unsafe_char)
        {
            return Err(ConfigError::UnsafeChargePointId(
                crate::text::escape_unsafe(&self.charge_point_id),
            ));
        }
        for (flag, value, max) in [
            ("--vendor", Some(&self.vendor), BOOT_VENDOR_MAX),
            ("--model", Some(&self.model), BOOT_MODEL_MAX),
            ("--serial", self.serial_number.as_ref(), BOOT_SERIAL_MAX),
            (
                "--firmware-version",
                self.firmware_version.as_ref(),
                BOOT_FIRMWARE_MAX,
            ),
        ] {
            if value.is_some_and(|text| text.chars().count() > max) {
                return Err(ConfigError::TooLong { flag, max });
            }
        }
        if !is_http_token(&self.ws_subprotocol) {
            return Err(ConfigError::InvalidSubprotocol(self.ws_subprotocol.clone()));
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
        if self.call_timeout_ms > MAX_CALL_TIMEOUT_MS {
            return Err(ConfigError::TooLarge {
                field: "call_timeout_ms",
                max: MAX_CALL_TIMEOUT_MS,
            });
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

    /// Checks that the configured TLS files (`--tls-ca`, `--tls-cert`, `--tls-key`) can be
    /// opened, so a wrong path fails at startup instead of on every reconnect attempt.
    /// Separate from [`StationConfig::validate`], which does no I/O.
    ///
    /// # Errors
    /// [`ConfigError::TlsFile`] naming the flag, the path and the I/O error.
    pub fn check_tls_files(&self) -> Result<(), ConfigError> {
        for (flag, path) in [
            ("--tls-ca", &self.tls.ca_file),
            ("--tls-cert", &self.tls.cert_file),
            ("--tls-key", &self.tls.key_file),
        ] {
            let Some(path) = path else { continue };
            let opened = std::fs::File::open(path).and_then(|file| {
                if file.metadata()?.is_dir() {
                    Err(std::io::Error::other("is a directory"))
                } else {
                    Ok(())
                }
            });
            if let Err(error) = opened {
                return Err(ConfigError::TlsFile {
                    flag,
                    path: path.clone(),
                    reason: error.to_string(),
                });
            }
        }
        Ok(())
    }

    /// The state directory: `state_dir` if set, else `~/.kirby-ocpp/stations`
    /// (`%USERPROFILE%` on Windows). `None` if no home directory can be determined.
    pub fn state_dir_or_default(&self) -> Option<PathBuf> {
        self.state_dir.clone().or_else(default_state_dir)
    }
}

/// The per-user state directory: `~/.kirby-ocpp/stations` (`%USERPROFILE%` on Windows).
/// `None` if no home directory can be determined.
#[must_use]
pub fn default_state_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".kirby-ocpp").join("stations"))
}

/// RFC 7230 token: non-empty, visible ASCII without separators.
fn is_http_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// Checks that `value` is an absolute `ws://` or `wss://` URL with a host.
/// Used by [`StationConfig::validate`] and by CLI value parsers.
pub fn validate_ws_url(value: &str) -> Result<(), String> {
    let url = Url::parse(value).map_err(|e| format!("invalid URL `{}`: {e}", redact_url(value)))?;
    // Error texts name the URL without credentials: it may carry `user:password@`.
    let shown = redact_url(value);
    if !matches!(url.scheme(), "ws" | "wss") {
        return Err(format!("`{shown}` must use the ws:// or wss:// scheme"));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(format!("`{shown}` has no host"));
    }
    Ok(())
}

/// `value` without userinfo (user name and password) for messages. A string that does not
/// parse loses everything before its last `@`, so a password never survives in an error text.
fn redact_url(value: &str) -> String {
    match Url::parse(value) {
        Ok(parsed) if parsed.cannot_be_a_base() => redact_unparsable_url(value),
        Ok(mut parsed) => {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.to_string()
        }
        Err(_) => redact_unparsable_url(value),
    }
}

/// `value` for display with the user name and password of its authority removed. A URL
/// without userinfo comes back unchanged; one that does not parse loses everything before its
/// last `@`, so no credential survives odd forms such as `ws:///user:pw@host`.
#[must_use]
pub fn redact_url_userinfo(value: &str) -> String {
    match Url::parse(value) {
        Ok(parsed) if !parsed.cannot_be_a_base() => {
            if parsed.username().is_empty() && parsed.password().is_none() {
                value.to_owned()
            } else {
                redact_url(value)
            }
        }
        _ => redact_unparsable_url(value),
    }
}

pub(crate) fn redact_unparsable_url(value: &str) -> String {
    let (scheme, rest) = value.split_once("://").unwrap_or(("", value));
    // Everything up to the last `@` is dropped, even past a `/`: a malformed URL may hold a
    // password with raw delimiters.
    let rest = rest.rsplit_once('@').map_or(rest, |(_, after)| after);
    if scheme.is_empty() {
        rest.to_owned()
    } else {
        format!("{scheme}://{rest}")
    }
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
        assert_eq!(c.model, "kirby-ocpp");
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
    fn the_authorization_key_encoding_turns_a_key_into_password_bytes() {
        use AuthorizationKeyEncoding::{Hex, Literal};
        assert_eq!(AuthorizationKeyEncoding::default(), Literal);
        assert_eq!(Literal.password_bytes("zz%41"), Some(b"zz%41".to_vec()));
        assert_eq!(
            Hex.password_bytes("00ff7Fa0"),
            Some(vec![0x00, 0xff, 0x7f, 0xa0])
        );
        for invalid in [
            "",
            "abc",
            "zz",
            "0g",
            "é1",
            "00 0",
            "00é",
            "00000000000000é",
        ] {
            assert_eq!(Hex.password_bytes(invalid), None, "{invalid:?}");
            assert!(!Hex.accepts(invalid), "{invalid:?}");
            assert!(Literal.accepts(invalid));
        }
        assert_eq!(
            serde_json::to_string(&Hex).unwrap(),
            "\"hex\"",
            "the JSON form is the flag value"
        );
        let config: StationConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.authorization_key_encoding, Literal);
    }

    #[test]
    fn url_errors_never_show_the_password() {
        for url in [
            "ws://CP-1:s3cret@host:99999/x",
            "ws://CP-1:s3cret@/x",
            "http://CP-1:s3cret@host/x",
            "ws://CP-1:s3/cret@host:99999/x",
            "CP-1:s3cret@host",
        ] {
            let error = validate_ws_url(url).unwrap_err();
            assert!(
                !error.contains("s3cret") && !error.contains("cret"),
                "{error}"
            );
            let config = StationConfig::new(url).validate().unwrap_err().to_string();
            assert!(!config.contains("cret"), "{config}");
        }
    }

    #[test]
    fn call_timeout_has_an_upper_bound() {
        assert_eq!(
            valid().with_call_timeout_ms(MAX_CALL_TIMEOUT_MS).validate(),
            Ok(())
        );
        assert_eq!(
            valid().with_call_timeout_ms(u64::MAX).validate(),
            Err(ConfigError::TooLarge {
                field: "call_timeout_ms",
                max: MAX_CALL_TIMEOUT_MS
            })
        );
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
    fn rejects_dot_ids_and_non_token_subprotocols() {
        for id in [".", ".."] {
            assert_eq!(
                valid().with_charge_point_id(id).validate(),
                Err(ConfigError::InvalidChargePointId(id.to_owned()))
            );
        }
        for protocol in ["ocpp 1.6", "ocpp1.6\r\nX: y", "a,b", "\"q\"", "ocpp1.6é"] {
            let mut config = valid();
            config.ws_subprotocol = protocol.to_owned();
            assert!(
                matches!(config.validate(), Err(ConfigError::InvalidSubprotocol(_))),
                "{protocol:?}"
            );
        }
        assert!(valid().validate().is_ok());
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
    fn boot_notification_strings_respect_the_schema_limits() {
        let limit = |flag, max| Err(ConfigError::TooLong { flag, max });
        let mut c = valid();
        c.serial_number = Some("s".repeat(26));
        assert_eq!(c.validate(), limit("--serial", 25));
        c.serial_number = Some("s".repeat(25));
        assert_eq!(c.validate(), Ok(()));
        c.firmware_version = Some("f".repeat(51));
        assert_eq!(c.validate(), limit("--firmware-version", 50));
        c.firmware_version = None;
        c.vendor = "v".repeat(21);
        assert_eq!(c.validate(), limit("--vendor", 20));
        c.vendor = "v".to_owned();
        c.model = "m".repeat(21);
        assert_eq!(c.validate(), limit("--model", 20));
    }

    #[test]
    fn unsafe_charge_point_ids_are_refused() {
        for id in ["CP\n1", "CP\u{1b}[2J", "CP\u{200b}1"] {
            assert!(
                matches!(
                    valid().with_charge_point_id(id).validate(),
                    Err(ConfigError::UnsafeChargePointId(_))
                ),
                "{id:?}"
            );
        }
        assert_eq!(valid().with_charge_point_id("CP-ä 1").validate(), Ok(()));
    }

    #[test]
    fn unreadable_tls_files_name_their_flag() {
        let dir = std::env::temp_dir();
        let mut c = valid();
        c.tls.ca_file = Some(dir.join("kirby-ocpp-no-such-ca.pem"));
        assert!(matches!(
            c.check_tls_files(),
            Err(ConfigError::TlsFile {
                flag: "--tls-ca",
                ..
            })
        ));
        c.tls.ca_file = Some(dir);
        assert!(c.check_tls_files().is_err(), "a directory is not a file");
        c.tls.ca_file = None;
        assert_eq!(c.check_tls_files(), Ok(()));
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
