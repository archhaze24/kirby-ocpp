//! OCPP configuration keys (`GetConfiguration` / `ChangeConfiguration`).
//!
//! TS reference: `src/station/configuration.ts`, `configuration-registry.ts`,
//! `configuration-commands.ts`.
//!
//! The registry holds every key the emulator advertises: typed writable keys
//! (boolean, non-negative and positive integers, sampled-data lists,
//! phase-rotation lists), read-only identity and limit keys, and inert keys that
//! are stored but have no effect. Behaviour that differs from the TS code:
//!
//! * Key lookup is ASCII case-insensitive, as OCPP requires. Responses use the
//!   canonical spelling for known keys and echo the requested spelling for
//!   unknown ones.
//! * The six smart-charging and reservation keys the TS code lacks are present
//!   (`ChargeProfileMaxStackLevel`, `ChargingScheduleAllowedChargingRateUnit`,
//!   `ChargingScheduleMaxPeriods`, `MaxChargingProfilesInstalled`,
//!   `ConnectorSwitch3to1PhaseSupported`, `ReserveConnectorZeroSupported`).
//! * `*MaxLength` keys are enforced for the list-valued keys they describe.
//! * `ConnectorPhaseRotation` accepts the spec form `<connectorId>.<rotation>`
//!   and the bare form the TS code accepted. Connectors added later get an entry
//!   in the spec form (see [`ConfigRegistry::set_connector_count`]).
//! * Only writable values are persisted. Restoring re-validates every value, so
//!   a value that is no longer valid is dropped and the default is kept.
//!
//! No key needs a reboot in this emulator, so `ChangeConfiguration` never answers
//! `RebootRequired`; the variant exists only for the wire type.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::StationConfig;
use crate::ocpp::types::{ChargingRateUnitType, ConfigurationStatus, Measurand};

// Canonical spelling of every registry key.
pub const ALLOW_OFFLINE_TX_FOR_UNKNOWN_ID: &str = "AllowOfflineTxForUnknownId";
pub const AUTHORIZATION_CACHE_ENABLED: &str = "AuthorizationCacheEnabled";
pub const AUTHORIZATION_KEY: &str = "AuthorizationKey";
pub const AUTHORIZE_REMOTE_TX_REQUESTS: &str = "AuthorizeRemoteTxRequests";
pub const BLINK_REPEAT: &str = "BlinkRepeat";
pub const CERTIFICATE_SIGNED_MAX_CHAIN_SIZE: &str = "CertificateSignedMaxChainSize";
pub const CERTIFICATE_STORE_MAX_LENGTH: &str = "CertificateStoreMaxLength";
pub const ADDITIONAL_ROOT_CERTIFICATE_CHECK: &str = "AdditionalRootCertificateCheck";
pub const CPO_NAME: &str = "CpoName";
pub const SECURITY_PROFILE: &str = "SecurityProfile";
pub const ALLOW_SECURITY_PROFILE_DOWNGRADE: &str = "AllowSecurityProfileDowngrade";
pub const ALLOW_CENTRAL_SYSTEM_TLS_WILDCARDS: &str = "AllowCentralSystemTLSWildcards";
pub const CERT_SIGNING_WAIT_MINIMUM: &str = "CertSigningWaitMinimum";
pub const CERT_SIGNING_REPEAT_TIMES: &str = "CertSigningRepeatTimes";
pub const CHARGE_POINT_MODEL: &str = "ChargePointModel";
pub const CHARGE_POINT_SERIAL_NUMBER: &str = "ChargePointSerialNumber";
pub const CHARGE_POINT_VENDOR: &str = "ChargePointVendor";
pub const CHARGE_PROFILE_MAX_STACK_LEVEL: &str = "ChargeProfileMaxStackLevel";
pub const CHARGING_SCHEDULE_ALLOWED_CHARGING_RATE_UNIT: &str =
    "ChargingScheduleAllowedChargingRateUnit";
pub const CHARGING_SCHEDULE_MAX_PERIODS: &str = "ChargingScheduleMaxPeriods";
pub const CLOCK_ALIGNED_DATA_INTERVAL: &str = "ClockAlignedDataInterval";
pub const CONNECTION_TIME_OUT: &str = "ConnectionTimeOut";
pub const CONNECTOR_PHASE_ROTATION: &str = "ConnectorPhaseRotation";
pub const CONNECTOR_PHASE_ROTATION_MAX_LENGTH: &str = "ConnectorPhaseRotationMaxLength";
pub const CONNECTOR_SWITCH_3TO1_PHASE_SUPPORTED: &str = "ConnectorSwitch3to1PhaseSupported";
pub const GET_CONFIGURATION_MAX_KEYS: &str = "GetConfigurationMaxKeys";
pub const HEARTBEAT_INTERVAL: &str = "HeartbeatInterval";
pub const LIGHT_INTENSITY: &str = "LightIntensity";
pub const LOCAL_AUTH_LIST_ENABLED: &str = "LocalAuthListEnabled";
pub const LOCAL_AUTH_LIST_MAX_LENGTH: &str = "LocalAuthListMaxLength";
pub const LOCAL_AUTHORIZE_OFFLINE: &str = "LocalAuthorizeOffline";
pub const LOCAL_PRE_AUTHORIZE: &str = "LocalPreAuthorize";
pub const MAX_CHARGING_PROFILES_INSTALLED: &str = "MaxChargingProfilesInstalled";
pub const MAX_ENERGY_ON_INVALID_ID: &str = "MaxEnergyOnInvalidId";
pub const METER_VALUES_ALIGNED_DATA: &str = "MeterValuesAlignedData";
pub const METER_VALUES_ALIGNED_DATA_MAX_LENGTH: &str = "MeterValuesAlignedDataMaxLength";
pub const METER_VALUES_SAMPLED_DATA: &str = "MeterValuesSampledData";
pub const METER_VALUES_SAMPLED_DATA_MAX_LENGTH: &str = "MeterValuesSampledDataMaxLength";
pub const METER_VALUE_SAMPLE_INTERVAL: &str = "MeterValueSampleInterval";
pub const MINIMUM_STATUS_DURATION: &str = "MinimumStatusDuration";
pub const NUMBER_OF_CONNECTORS: &str = "NumberOfConnectors";
pub const RESERVE_CONNECTOR_ZERO_SUPPORTED: &str = "ReserveConnectorZeroSupported";
pub const RESET_RETRIES: &str = "ResetRetries";
pub const SEND_LOCAL_LIST_MAX_LENGTH: &str = "SendLocalListMaxLength";
pub const STOP_TRANSACTION_ON_EV_SIDE_DISCONNECT: &str = "StopTransactionOnEVSideDisconnect";
pub const STOP_TRANSACTION_ON_INVALID_ID: &str = "StopTransactionOnInvalidId";
pub const STOP_TXN_ALIGNED_DATA: &str = "StopTxnAlignedData";
pub const STOP_TXN_ALIGNED_DATA_MAX_LENGTH: &str = "StopTxnAlignedDataMaxLength";
pub const STOP_TXN_SAMPLED_DATA: &str = "StopTxnSampledData";
pub const STOP_TXN_SAMPLED_DATA_MAX_LENGTH: &str = "StopTxnSampledDataMaxLength";
pub const SUPPORTED_FEATURE_PROFILES: &str = "SupportedFeatureProfiles";
pub const SUPPORTED_FEATURE_PROFILES_MAX_LENGTH: &str = "SupportedFeatureProfilesMaxLength";
pub const TRANSACTION_MESSAGE_ATTEMPTS: &str = "TransactionMessageAttempts";
pub const TRANSACTION_MESSAGE_RETRY_INTERVAL: &str = "TransactionMessageRetryInterval";
pub const UNLOCK_CONNECTOR_ON_EV_SIDE_DISCONNECT: &str = "UnlockConnectorOnEVSideDisconnect";
pub const WEB_SOCKET_PING_INTERVAL: &str = "WebSocketPingInterval";

/// Profiles this emulator advertises in `SupportedFeatureProfiles`.
pub const FEATURE_PROFILES: &[&str] = &[
    "Core",
    "FirmwareManagement",
    "LocalAuthListManagement",
    "Reservation",
    "RemoteTrigger",
    "SmartCharging",
];

/// Phase rotations accepted in `ConnectorPhaseRotation`.
const PHASE_ROTATIONS: &[&str] = &[
    "NotApplicable",
    "Unknown",
    "RST",
    "RTS",
    "SRT",
    "STR",
    "TRS",
    "TSR",
];

/// Measurands sampled when a sampled-data key has no valid entry (TS `DEFAULT_SAMPLED_DATA`).
pub const DEFAULT_SAMPLED_DATA: &[Measurand] = &[
    Measurand::EnergyActiveImportRegister,
    Measurand::PowerActiveImport,
    Measurand::CurrentImport,
    Measurand::Voltage,
    Measurand::Temperature,
    Measurand::SoC,
];

// Fixed read-only limits of this emulator.
const STACK_LEVEL_LIMIT: u32 = 10;
const SCHEDULE_PERIOD_LIMIT: u32 = 24;
const PROFILES_INSTALLED_LIMIT: u32 = 10;
const LOCAL_LIST_LIMIT: u32 = 1_000;
const SEND_LOCAL_LIST_LIMIT: u32 = 1_000;
const GET_CONFIGURATION_KEY_LIMIT: u32 = 50;

/// Type and bounds of a key's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Read-only, never validated.
    Text,
    Boolean,
    /// Integer `>= 0`.
    NonNegativeInt,
    /// Integer `>= 1`.
    PositiveInt,
    /// Comma-separated measurand names.
    Measurands,
    /// Comma-separated phase rotations: `<rotation>` or `<connectorId>.<rotation>`.
    PhaseRotations,
    /// Integer 0..=3 (`SecurityProfile`). Transport rules are checked by the station.
    SecurityProfile,
    /// Write-only secret, 16..=40 printable ASCII characters (`AuthorizationKey`).
    Secret,
    /// Free text of at most 50 characters (`CpoName`).
    Name,
}

impl Kind {
    /// Short description of the accepted values, for UIs.
    fn hint(self) -> &'static str {
        match self {
            Self::Text => "read-only",
            Self::Boolean => "true or false",
            Self::NonNegativeInt => "integer, 0 or more",
            Self::PositiveInt => "integer, 1 or more",
            Self::Measurands => "comma-separated measurand names",
            Self::PhaseRotations => "comma-separated phase rotations (RST, 1.RST, ...)",
            Self::SecurityProfile => "0, 1, 2 or 3; can only be raised",
            Self::Secret => "write-only; 16 to 40 printable ASCII characters",
            Self::Name => "text, at most 50 characters",
        }
    }

    fn accepts(self, value: &str) -> bool {
        match self {
            Self::Text => true,
            Self::Boolean => value == "true" || value == "false",
            Self::NonNegativeInt => parse_int(value).is_some_and(|n| n >= 0),
            Self::PositiveInt => parse_int(value).is_some_and(|n| n >= 1),
            Self::Measurands => {
                let mut items = list_items(value).peekable();
                items.peek().is_some() && items.all(|item| Measurand::from_str(item).is_ok())
            }
            Self::PhaseRotations => {
                let mut items = list_items(value).peekable();
                items.peek().is_some() && items.all(is_phase_rotation_item)
            }
            Self::SecurityProfile => parse_int(value).is_some_and(|n| (0..=3).contains(&n)),
            Self::Secret => {
                (16..=40).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_graphic())
            }
            Self::Name => value.chars().count() <= 50,
        }
    }
}

/// One configuration key with its current value.
#[derive(Clone, PartialEq, Eq)]
pub struct ConfigEntry {
    /// Canonical key name.
    pub key: &'static str,
    pub readonly: bool,
    pub value: String,
    kind: Kind,
    /// For list keys: the read-only `*MaxLength` key that bounds the item count.
    max_items_key: Option<&'static str>,
}

impl std::fmt::Debug for ConfigEntry {
    /// The value of a secret key (`AuthorizationKey`) is never printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigEntry")
            .field("key", &self.key)
            .field("readonly", &self.readonly)
            .field("value", &self.display_value())
            .field("kind", &self.kind)
            .field("max_items_key", &self.max_items_key)
            .finish()
    }
}

impl ConfigEntry {
    /// `true` for write-only keys (`AuthorizationKey`): the value must never be shown,
    /// logged or persisted in the state file.
    pub fn secret(&self) -> bool {
        self.kind == Kind::Secret
    }

    /// The value as it may be shown: secrets are masked (`""` when unset).
    pub fn display_value(&self) -> String {
        if self.secret() && !self.value.is_empty() {
            "********".to_owned()
        } else {
            self.value.clone()
        }
    }

    /// Short description of the accepted values ("read-only" for read-only keys).
    pub fn hint(&self) -> &'static str {
        if self.readonly {
            "read-only"
        } else {
            self.kind.hint()
        }
    }
}

/// `GetConfiguration` named more keys than `GetConfigurationMaxKeys` allows.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{requested} keys requested, GetConfigurationMaxKeys is {max}")]
pub struct TooManyKeys {
    pub requested: usize,
    pub max: usize,
}

/// Result of `GetConfiguration`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigurationReport<'a> {
    /// Known keys, canonical spelling, in registry order.
    pub configuration_key: Vec<&'a ConfigEntry>,
    /// Requested keys the registry does not know, as requested.
    pub unknown_key: Vec<String>,
}

/// Writable values, for persistence. Read-only keys come from the station config.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigSnapshot {
    pub values: BTreeMap<String, String>,
}

/// All configuration keys of one station.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRegistry {
    entries: Vec<ConfigEntry>,
    connector_count: u32,
}

impl ConfigRegistry {
    /// The registry with defaults derived from `config`.
    pub fn new(config: &StationConfig) -> Self {
        let connector_count = config
            .connector_count
            .max(config.initial_connector.get())
            .max(1);
        Self {
            entries: default_entries(config, connector_count),
            connector_count,
        }
    }

    /// Looks a key up, ignoring ASCII case.
    pub fn get(&self, name: &str) -> Option<&ConfigEntry> {
        self.index_of(name)
            .and_then(|index| self.entries.get(index))
    }

    /// Current value of a key, if the key exists.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.get(name).map(|entry| entry.value.as_str())
    }

    /// Every key in registry order.
    pub fn entries(&self) -> &[ConfigEntry] {
        &self.entries
    }

    /// `ChangeConfiguration`: unknown keys are `NotSupported`, read-only keys and
    /// invalid values are `Rejected`, anything else is stored and `Accepted`.
    pub fn change(&mut self, name: &str, value: &str) -> ConfigurationStatus {
        let status = self.check(name, value);
        if status == ConfigurationStatus::Accepted
            && let Some(index) = self.index_of(name)
        {
            value.clone_into(&mut self.entries[index].value);
        }
        status
    }

    /// What [`Self::change`] would answer, without changing anything.
    pub fn check(&self, name: &str, value: &str) -> ConfigurationStatus {
        let Some(entry) = self.get(name) else {
            return ConfigurationStatus::NotSupported;
        };
        let fits = entry
            .max_items_key
            .is_none_or(|max_key| list_items(value).count() <= self.count_limit(max_key));
        if entry.readonly || !entry.kind.accepts(value) || !fits {
            ConfigurationStatus::Rejected
        } else {
            ConfigurationStatus::Accepted
        }
    }

    /// `GetConfiguration`. An empty `requested` list asks for every key.
    ///
    /// Naming more keys than `GetConfigurationMaxKeys` is an error; the caller
    /// decides how to answer it (the station replies with a CALLERROR).
    pub fn get_configuration(
        &self,
        requested: &[String],
    ) -> Result<ConfigurationReport<'_>, TooManyKeys> {
        if requested.is_empty() {
            return Ok(ConfigurationReport {
                configuration_key: self.entries.iter().collect(),
                unknown_key: Vec::new(),
            });
        }
        let max = usize::try_from(self.number(GET_CONFIGURATION_MAX_KEYS)).unwrap_or(usize::MAX);
        if requested.len() > max {
            return Err(TooManyKeys {
                requested: requested.len(),
                max,
            });
        }
        let mut report = ConfigurationReport {
            configuration_key: Vec::new(),
            unknown_key: Vec::new(),
        };
        for name in requested {
            match self.get(name) {
                Some(entry) => report.configuration_key.push(entry),
                None => report.unknown_key.push(name.clone()),
            }
        }
        Ok(report)
    }

    /// Number of connectors the registry describes. Only grows.
    pub fn connector_count(&self) -> u32 {
        self.connector_count
    }

    /// Called when connectors are added: updates `NumberOfConnectors` and
    /// `ConnectorPhaseRotationMaxLength`, and, if `ConnectorPhaseRotation` uses the
    /// `<connectorId>.<rotation>` form, appends `<id>.Unknown` for every connector
    /// that has no entry yet. A count that is not larger is ignored.
    pub fn set_connector_count(&mut self, count: u32) {
        if count <= self.connector_count {
            return;
        }
        self.connector_count = count;
        self.set_internal(NUMBER_OF_CONNECTORS, count.to_string());
        self.set_internal(CONNECTOR_PHASE_ROTATION_MAX_LENGTH, (count + 1).to_string());

        let Some(current) = self.value(CONNECTOR_PHASE_ROTATION) else {
            return;
        };
        let covered: Vec<u32> = list_items(current)
            .filter_map(|item| item.split_once('.'))
            .filter_map(|(connector, _)| connector.parse().ok())
            .collect();
        if covered.is_empty() {
            return;
        }
        let mut items: Vec<String> = list_items(current).map(str::to_owned).collect();
        for id in 1..=count {
            if !covered.contains(&id) {
                items.push(format!("{id}.Unknown"));
            }
        }
        let joined = items.join(",");
        self.set_internal(CONNECTOR_PHASE_ROTATION, joined);
    }

    /// The writable part of the registry, for persistence.
    pub fn snapshot(&self) -> ConfigSnapshot {
        ConfigSnapshot {
            values: self
                .entries
                .iter()
                .filter(|entry| !entry.readonly && !entry.secret())
                .map(|entry| (entry.key.to_owned(), entry.value.clone()))
                .collect(),
        }
    }

    /// Applies a persisted snapshot. Every value goes through [`Self::change`], so
    /// read-only, unknown and invalid entries are skipped. Returns the skipped keys.
    pub fn restore(&mut self, snapshot: &ConfigSnapshot) -> Vec<String> {
        let mut skipped = Vec::new();
        for (name, value) in &snapshot.values {
            if self.change(name, value) != ConfigurationStatus::Accepted {
                skipped.push(name.clone());
            }
        }
        skipped
    }

    // ---- typed accessors -------------------------------------------------------

    pub fn heartbeat_interval_secs(&self) -> u32 {
        self.number(HEARTBEAT_INTERVAL)
    }

    /// `WebSocketPingInterval`: the WebSocket ping period; `None` when `0` (pings off).
    /// Read when a connection is opened, so a change applies from the next (re)connect.
    pub fn web_socket_ping_interval(&self) -> Option<Duration> {
        let secs = self.number(WEB_SOCKET_PING_INTERVAL);
        (secs > 0).then(|| Duration::from_secs(u64::from(secs)))
    }

    /// Delivery attempts for transaction messages (at least 1: no retry).
    pub fn transaction_message_attempts(&self) -> u32 {
        self.number(TRANSACTION_MESSAGE_ATTEMPTS).max(1)
    }

    pub fn transaction_message_retry_interval(&self) -> Duration {
        Duration::from_secs(u64::from(self.number(TRANSACTION_MESSAGE_RETRY_INTERVAL)))
    }

    pub fn minimum_status_duration(&self) -> Duration {
        Duration::from_secs(u64::from(self.number(MINIMUM_STATUS_DURATION)))
    }

    pub fn meter_value_sample_interval_secs(&self) -> u32 {
        self.number(METER_VALUE_SAMPLE_INTERVAL)
    }

    pub fn clock_aligned_data_interval_secs(&self) -> u32 {
        self.number(CLOCK_ALIGNED_DATA_INTERVAL)
    }

    pub fn reset_retries(&self) -> u32 {
        self.number(RESET_RETRIES)
    }

    pub fn local_auth_list_enabled(&self) -> bool {
        self.boolean(LOCAL_AUTH_LIST_ENABLED)
    }

    /// Most entries the local list may hold after an update (`LocalAuthListMaxLength`).
    pub fn local_auth_list_max_length(&self) -> u32 {
        self.number(LOCAL_AUTH_LIST_MAX_LENGTH)
    }

    /// Most entries one `SendLocalList` may carry (`SendLocalListMaxLength`).
    pub fn send_local_list_max_length(&self) -> u32 {
        self.number(SEND_LOCAL_LIST_MAX_LENGTH)
    }

    pub fn local_authorize_offline(&self) -> bool {
        self.boolean(LOCAL_AUTHORIZE_OFFLINE)
    }

    /// `SecurityProfile` (0..=3).
    pub fn security_profile(&self) -> u8 {
        u8::try_from(self.number(SECURITY_PROFILE)).unwrap_or(0)
    }

    /// `AllowSecurityProfileDowngrade` (default `false`).
    pub fn allow_security_profile_downgrade(&self) -> bool {
        self.boolean(ALLOW_SECURITY_PROFILE_DOWNGRADE)
    }

    /// `AllowCentralSystemTLSWildcards` (default `false`).
    pub fn allow_central_system_tls_wildcards(&self) -> bool {
        self.boolean(ALLOW_CENTRAL_SYSTEM_TLS_WILDCARDS)
    }

    /// `CertSigningWaitMinimum` in seconds, at least 1.
    pub fn cert_signing_wait_minimum_secs(&self) -> u64 {
        u64::from(self.number(CERT_SIGNING_WAIT_MINIMUM)).max(1)
    }

    /// `CertSigningRepeatTimes`.
    pub fn cert_signing_repeat_times(&self) -> u32 {
        self.number(CERT_SIGNING_REPEAT_TIMES)
    }

    /// The `AuthorizationKey` (Basic Auth password), if one is set.
    pub fn authorization_key(&self) -> Option<&str> {
        self.value(AUTHORIZATION_KEY).filter(|v| !v.is_empty())
    }

    /// Installs the `AuthorizationKey` without validation (restored from the secrets file).
    pub fn set_authorization_key(&mut self, key: &str) {
        self.set_internal(AUTHORIZATION_KEY, key.to_owned());
    }

    /// `CpoName`, empty when unset.
    pub fn cpo_name(&self) -> &str {
        self.value(CPO_NAME).unwrap_or_default()
    }

    /// `AdditionalRootCertificateCheck` (read-only; set at start).
    pub fn additional_root_certificate_check(&self) -> bool {
        self.value(ADDITIONAL_ROOT_CERTIFICATE_CHECK) == Some("true")
    }

    pub fn certificate_store_max_length(&self) -> usize {
        usize::try_from(self.number(CERTIFICATE_STORE_MAX_LENGTH)).unwrap_or(usize::MAX)
    }

    pub fn certificate_signed_max_chain_size(&self) -> usize {
        usize::try_from(self.number(CERTIFICATE_SIGNED_MAX_CHAIN_SIZE)).unwrap_or(usize::MAX)
    }

    pub fn local_pre_authorize(&self) -> bool {
        self.boolean(LOCAL_PRE_AUTHORIZE)
    }

    pub fn authorization_cache_enabled(&self) -> bool {
        self.boolean(AUTHORIZATION_CACHE_ENABLED)
    }

    pub fn allow_offline_tx_for_unknown_id(&self) -> bool {
        self.boolean(ALLOW_OFFLINE_TX_FOR_UNKNOWN_ID)
    }

    pub fn authorize_remote_tx_requests(&self) -> bool {
        self.boolean(AUTHORIZE_REMOTE_TX_REQUESTS)
    }

    pub fn stop_transaction_on_ev_side_disconnect(&self) -> bool {
        self.boolean(STOP_TRANSACTION_ON_EV_SIDE_DISCONNECT)
    }

    pub fn stop_transaction_on_invalid_id(&self) -> bool {
        self.boolean(STOP_TRANSACTION_ON_INVALID_ID)
    }

    pub fn unlock_connector_on_ev_side_disconnect(&self) -> bool {
        self.boolean(UNLOCK_CONNECTOR_ON_EV_SIDE_DISCONNECT)
    }

    /// Energy (Wh) after which a transaction with an invalid idTag is stopped; `0` = off.
    pub fn max_energy_on_invalid_id(&self) -> u32 {
        self.number(MAX_ENERGY_ON_INVALID_ID)
    }

    /// Whether ReserveNow may name connector `0` (any connector). When `false`,
    /// such requests are rejected.
    pub fn reserve_connector_zero_supported(&self) -> bool {
        self.boolean(RESERVE_CONNECTOR_ZERO_SUPPORTED)
    }

    pub fn connector_switch_3to1_phase_supported(&self) -> bool {
        self.boolean(CONNECTOR_SWITCH_3TO1_PHASE_SUPPORTED)
    }

    pub fn charge_profile_max_stack_level(&self) -> u32 {
        self.number(CHARGE_PROFILE_MAX_STACK_LEVEL)
    }

    pub fn charging_schedule_max_periods(&self) -> u32 {
        self.number(CHARGING_SCHEDULE_MAX_PERIODS)
    }

    pub fn max_charging_profiles_installed(&self) -> u32 {
        self.number(MAX_CHARGING_PROFILES_INSTALLED)
    }

    /// Whether `ChargingScheduleAllowedChargingRateUnit` lists `unit`
    /// (`A` is `Current`, `W` is `Power`).
    pub fn charging_rate_unit_allowed(&self, unit: ChargingRateUnitType) -> bool {
        let wire = match unit {
            ChargingRateUnitType::A => "Current",
            ChargingRateUnitType::W => "Power",
        };
        self.value(CHARGING_SCHEDULE_ALLOWED_CHARGING_RATE_UNIT)
            .is_some_and(|value| list_items(value).any(|item| item == wire))
    }

    /// Measurands of a sampled-data key. Unknown names are dropped; if nothing
    /// valid remains, [`DEFAULT_SAMPLED_DATA`] is used (TS `readConfiguredMeasurands`).
    pub fn measurands(&self, name: &str) -> Vec<Measurand> {
        let measurands: Vec<Measurand> = self
            .value(name)
            .into_iter()
            .flat_map(list_items)
            .filter_map(|item| Measurand::from_str(item).ok())
            .collect();
        if measurands.is_empty() {
            DEFAULT_SAMPLED_DATA.to_vec()
        } else {
            measurands
        }
    }

    // ---- internals --------------------------------------------------------------

    fn index_of(&self, name: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.key.eq_ignore_ascii_case(name))
    }

    /// Writes a value without validation. Only for values the station derives itself.
    fn set_internal(&mut self, name: &str, value: String) {
        if let Some(index) = self.index_of(name) {
            self.entries[index].value = value;
        }
    }

    /// Integer value of a key, `0` if it is missing or not an integer. Keys are
    /// validated on write, so this only matters for a key the registry does not own.
    fn number(&self, name: &str) -> u32 {
        self.value(name)
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }

    fn boolean(&self, name: &str) -> bool {
        self.value(name) == Some("true")
    }

    /// Item limit of a `*MaxLength` key; `usize::MAX` if the key is missing.
    fn count_limit(&self, max_key: &str) -> usize {
        usize::try_from(self.number(max_key)).unwrap_or(usize::MAX)
    }
}

/// Trimmed, non-empty items of a comma-separated list value (TS semantics).
fn list_items(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
}

/// Integer in the OCPP `int` range, digits only (no sign, no whitespace).
fn parse_int(value: &str) -> Option<i32> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

/// `<rotation>` or `<connectorId>.<rotation>`.
fn is_phase_rotation_item(item: &str) -> bool {
    let rotation = match item.split_once('.') {
        Some((connector, rotation)) => {
            if connector.is_empty() || !connector.bytes().all(|byte| byte.is_ascii_digit()) {
                return false;
            }
            rotation
        }
        None => item,
    };
    PHASE_ROTATIONS.contains(&rotation)
}

fn writable(name: &'static str, kind: Kind, value: impl Into<String>) -> ConfigEntry {
    ConfigEntry {
        key: name,
        readonly: false,
        value: value.into(),
        kind,
        max_items_key: None,
    }
}

fn writable_list(
    name: &'static str,
    kind: Kind,
    max_key: &'static str,
    value: impl Into<String>,
) -> ConfigEntry {
    ConfigEntry {
        max_items_key: Some(max_key),
        ..writable(name, kind, value)
    }
}

fn read_only(name: &'static str, value: impl Into<String>) -> ConfigEntry {
    ConfigEntry {
        key: name,
        readonly: true,
        value: value.into(),
        kind: Kind::Text,
        max_items_key: None,
    }
}

fn default_measurands() -> String {
    DEFAULT_SAMPLED_DATA
        .iter()
        .map(|measurand| measurand.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// All defaults, in the alphabetical order the TS registry uses.
fn default_entries(config: &StationConfig, connectors: u32) -> Vec<ConfigEntry> {
    let mut entries = first_half_entries(config, connectors);
    entries.extend(second_half_entries(config, connectors));
    entries
}

fn first_half_entries(config: &StationConfig, connectors: u32) -> Vec<ConfigEntry> {
    let phase_rotation = (1..=connectors)
        .map(|id| format!("{id}.Unknown"))
        .collect::<Vec<_>>()
        .join(",");

    vec![
        writable(ALLOW_CENTRAL_SYSTEM_TLS_WILDCARDS, Kind::Boolean, "false"),
        writable(ALLOW_OFFLINE_TX_FOR_UNKNOWN_ID, Kind::Boolean, "true"),
        writable(ALLOW_SECURITY_PROFILE_DOWNGRADE, Kind::Boolean, "false"),
        read_only(
            ADDITIONAL_ROOT_CERTIFICATE_CHECK,
            config.additional_root_certificate_check.to_string(),
        ),
        writable(AUTHORIZATION_CACHE_ENABLED, Kind::Boolean, "true"),
        writable(AUTHORIZATION_KEY, Kind::Secret, ""),
        writable(AUTHORIZE_REMOTE_TX_REQUESTS, Kind::Boolean, "false"),
        writable(BLINK_REPEAT, Kind::NonNegativeInt, "0"),
        read_only(CERTIFICATE_SIGNED_MAX_CHAIN_SIZE, "10000"),
        read_only(CERTIFICATE_STORE_MAX_LENGTH, "8"),
        writable(CERT_SIGNING_REPEAT_TIMES, Kind::NonNegativeInt, "5"),
        writable(CERT_SIGNING_WAIT_MINIMUM, Kind::NonNegativeInt, "20"),
        read_only(CHARGE_POINT_MODEL, config.model.clone()),
        read_only(CHARGE_POINT_SERIAL_NUMBER, config.charge_point_id.clone()),
        read_only(CHARGE_POINT_VENDOR, config.vendor.clone()),
        read_only(
            CHARGE_PROFILE_MAX_STACK_LEVEL,
            STACK_LEVEL_LIMIT.to_string(),
        ),
        read_only(
            CHARGING_SCHEDULE_ALLOWED_CHARGING_RATE_UNIT,
            "Current,Power",
        ),
        read_only(
            CHARGING_SCHEDULE_MAX_PERIODS,
            SCHEDULE_PERIOD_LIMIT.to_string(),
        ),
        writable(CLOCK_ALIGNED_DATA_INTERVAL, Kind::NonNegativeInt, "0"),
        writable(CONNECTION_TIME_OUT, Kind::PositiveInt, "60"),
        writable_list(
            CONNECTOR_PHASE_ROTATION,
            Kind::PhaseRotations,
            CONNECTOR_PHASE_ROTATION_MAX_LENGTH,
            phase_rotation,
        ),
        read_only(
            CONNECTOR_PHASE_ROTATION_MAX_LENGTH,
            (connectors + 1).to_string(),
        ),
        writable(
            CONNECTOR_SWITCH_3TO1_PHASE_SUPPORTED,
            Kind::Boolean,
            "false",
        ),
        read_only(
            GET_CONFIGURATION_MAX_KEYS,
            GET_CONFIGURATION_KEY_LIMIT.to_string(),
        ),
        writable(
            HEARTBEAT_INTERVAL,
            Kind::PositiveInt,
            config.heartbeat_interval_secs.to_string(),
        ),
        writable(CPO_NAME, Kind::Name, ""),
        writable(LIGHT_INTENSITY, Kind::NonNegativeInt, "0"),
        writable(LOCAL_AUTH_LIST_ENABLED, Kind::Boolean, "true"),
        read_only(LOCAL_AUTH_LIST_MAX_LENGTH, LOCAL_LIST_LIMIT.to_string()),
        writable(LOCAL_AUTHORIZE_OFFLINE, Kind::Boolean, "true"),
        writable(LOCAL_PRE_AUTHORIZE, Kind::Boolean, "false"),
    ]
}

fn second_half_entries(config: &StationConfig, connectors: u32) -> Vec<ConfigEntry> {
    let measurands = default_measurands();
    let measurand_max = Measurand::ALL.len().to_string();

    vec![
        read_only(
            MAX_CHARGING_PROFILES_INSTALLED,
            PROFILES_INSTALLED_LIMIT.to_string(),
        ),
        writable(MAX_ENERGY_ON_INVALID_ID, Kind::NonNegativeInt, "0"),
        writable_list(
            METER_VALUES_ALIGNED_DATA,
            Kind::Measurands,
            METER_VALUES_ALIGNED_DATA_MAX_LENGTH,
            measurands.clone(),
        ),
        read_only(METER_VALUES_ALIGNED_DATA_MAX_LENGTH, measurand_max.clone()),
        writable_list(
            METER_VALUES_SAMPLED_DATA,
            Kind::Measurands,
            METER_VALUES_SAMPLED_DATA_MAX_LENGTH,
            measurands.clone(),
        ),
        read_only(METER_VALUES_SAMPLED_DATA_MAX_LENGTH, measurand_max.clone()),
        writable(METER_VALUE_SAMPLE_INTERVAL, Kind::NonNegativeInt, "60"),
        writable(MINIMUM_STATUS_DURATION, Kind::NonNegativeInt, "0"),
        read_only(NUMBER_OF_CONNECTORS, connectors.to_string()),
        read_only(RESERVE_CONNECTOR_ZERO_SUPPORTED, "true"),
        writable(RESET_RETRIES, Kind::NonNegativeInt, "0"),
        writable(SECURITY_PROFILE, Kind::SecurityProfile, "0"),
        read_only(
            SEND_LOCAL_LIST_MAX_LENGTH,
            SEND_LOCAL_LIST_LIMIT.to_string(),
        ),
        writable(
            STOP_TRANSACTION_ON_EV_SIDE_DISCONNECT,
            Kind::Boolean,
            "true",
        ),
        writable(STOP_TRANSACTION_ON_INVALID_ID, Kind::Boolean, "true"),
        writable_list(
            STOP_TXN_ALIGNED_DATA,
            Kind::Measurands,
            STOP_TXN_ALIGNED_DATA_MAX_LENGTH,
            measurands.clone(),
        ),
        read_only(STOP_TXN_ALIGNED_DATA_MAX_LENGTH, measurand_max.clone()),
        writable_list(
            STOP_TXN_SAMPLED_DATA,
            Kind::Measurands,
            STOP_TXN_SAMPLED_DATA_MAX_LENGTH,
            measurands,
        ),
        read_only(STOP_TXN_SAMPLED_DATA_MAX_LENGTH, measurand_max),
        read_only(SUPPORTED_FEATURE_PROFILES, FEATURE_PROFILES.join(",")),
        read_only(
            SUPPORTED_FEATURE_PROFILES_MAX_LENGTH,
            FEATURE_PROFILES.len().to_string(),
        ),
        writable(TRANSACTION_MESSAGE_ATTEMPTS, Kind::PositiveInt, "1"),
        writable(TRANSACTION_MESSAGE_RETRY_INTERVAL, Kind::PositiveInt, "30"),
        writable(
            UNLOCK_CONNECTOR_ON_EV_SIDE_DISCONNECT,
            Kind::Boolean,
            "false",
        ),
        writable(
            WEB_SOCKET_PING_INTERVAL,
            Kind::NonNegativeInt,
            config.ws_ping_interval_secs.to_string(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ConfigRegistry {
        ConfigRegistry::new(&StationConfig::new("ws://h/p").with_connectors(2))
    }

    fn keys_of(report: &ConfigurationReport<'_>) -> Vec<&'static str> {
        report.configuration_key.iter().map(|e| e.key).collect()
    }

    #[test]
    fn web_socket_ping_interval_follows_the_station_config_and_can_be_changed() {
        let mut config = StationConfig::new("ws://localhost:9000/ocpp");
        config.ws_ping_interval_secs = 45;
        let mut registry = ConfigRegistry::new(&config);
        assert_eq!(registry.value("WebSocketPingInterval"), Some("45"));
        assert_eq!(
            registry.web_socket_ping_interval(),
            Some(Duration::from_secs(45))
        );
        assert_eq!(
            registry.change("WebSocketPingInterval", "0"),
            ConfigurationStatus::Accepted
        );
        assert_eq!(registry.web_socket_ping_interval(), None);
        config.ws_ping_interval_secs = 0;
        assert_eq!(
            ConfigRegistry::new(&config).web_socket_ping_interval(),
            None
        );
    }

    #[test]
    fn offline_authorization_is_allowed_by_default() {
        let registry = registry();
        assert_eq!(registry.value(LOCAL_AUTHORIZE_OFFLINE), Some("true"));
        assert_eq!(
            registry.value(ALLOW_OFFLINE_TX_FOR_UNKNOWN_ID),
            Some("true")
        );
    }

    #[test]
    fn registry_has_every_advertised_key_once() {
        let registry = registry();
        assert_eq!(registry.entries().len(), 56);
        let mut names: Vec<String> = registry
            .entries()
            .iter()
            .map(|e| e.key.to_ascii_lowercase())
            .collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 56, "duplicate key names");
    }

    #[test]
    fn missing_smart_charging_and_reservation_keys_are_present() {
        let registry = registry();
        for name in [
            "ChargeProfileMaxStackLevel",
            "ChargingScheduleAllowedChargingRateUnit",
            "ChargingScheduleMaxPeriods",
            "MaxChargingProfilesInstalled",
            "ConnectorSwitch3to1PhaseSupported",
            "ReserveConnectorZeroSupported",
        ] {
            assert!(registry.get(name).is_some(), "{name}");
        }
        assert!(
            registry
                .get("ReserveConnectorZeroSupported")
                .unwrap()
                .readonly
        );
        assert!(
            !registry
                .get("ConnectorSwitch3to1PhaseSupported")
                .unwrap()
                .readonly
        );
    }

    #[test]
    fn get_configuration_without_keys_returns_everything() {
        let registry = registry();
        let report = registry.get_configuration(&[]).unwrap();
        assert_eq!(report.configuration_key.len(), registry.entries().len());
        assert_eq!(report.unknown_key, Vec::<String>::new());
        let keys = keys_of(&report);
        for expected in [
            "ClockAlignedDataInterval",
            "SupportedFeatureProfiles",
            "StopTransactionOnEVSideDisconnect",
        ] {
            assert!(keys.contains(&expected), "{expected}");
        }
    }

    #[test]
    fn get_configuration_reports_known_and_unknown_keys_separately() {
        let registry = registry();
        let requested = vec![
            "heartbeatinterval".to_owned(),
            "TotallyUnknownConfigurationKey".to_owned(),
        ];
        let report = registry.get_configuration(&requested).unwrap();
        assert_eq!(keys_of(&report), ["HeartbeatInterval"]);
        assert_eq!(report.unknown_key, ["TotallyUnknownConfigurationKey"]);
    }

    #[test]
    fn get_configuration_enforces_max_keys() {
        let registry = registry();
        let requested: Vec<String> = (0..51).map(|i| format!("Key{i}")).collect();
        assert_eq!(
            registry.get_configuration(&requested),
            Err(TooManyKeys {
                requested: 51,
                max: 50
            })
        );
        assert!(registry.get_configuration(&requested[..50]).is_ok());
    }

    #[test]
    fn unknown_key_change_is_not_supported() {
        let mut registry = registry();
        assert_eq!(
            registry.change("TotallyUnknownConfigurationKey", "1"),
            ConfigurationStatus::NotSupported
        );
    }

    #[test]
    fn read_only_key_change_is_rejected() {
        let mut registry = registry();
        assert_eq!(
            registry.change("NumberOfConnectors", "7"),
            ConfigurationStatus::Rejected
        );
        assert_eq!(registry.value("NumberOfConnectors"), Some("2"));
    }

    #[test]
    fn invalid_values_are_rejected_and_keep_the_old_value() {
        let mut registry = registry();
        let cases = [
            ("StopTransactionOnEVSideDisconnect", "maybe"),
            ("StopTransactionOnEVSideDisconnect", "TRUE"),
            ("StopTransactionOnEVSideDisconnect", ""),
            (
                "MeterValuesSampledData",
                "Energy.Active.Import.Register,Totally.Not.Real",
            ),
            ("MeterValuesSampledData", ""),
            ("MeterValuesSampledData", " , "),
            ("HeartbeatInterval", "0"),
            ("HeartbeatInterval", "-5"),
            ("HeartbeatInterval", "+5"),
            ("HeartbeatInterval", "1.5"),
            ("HeartbeatInterval", "3000000000"),
            ("ConnectorPhaseRotation", "1.XYZ"),
            ("ConnectorPhaseRotation", "x.RST"),
        ];
        for (name, value) in cases {
            let before = registry.value(name).unwrap().to_owned();
            assert_eq!(
                registry.change(name, value),
                ConfigurationStatus::Rejected,
                "{name}={value:?}"
            );
            assert_eq!(registry.value(name), Some(before.as_str()));
        }
    }

    #[test]
    fn valid_values_are_accepted_and_stored_as_given() {
        let mut registry = registry();
        assert_eq!(
            registry.change("lightintensity", "10"),
            ConfigurationStatus::Accepted
        );
        assert_eq!(registry.value("LightIntensity"), Some("10"));
        assert_eq!(
            registry.change("StopTransactionOnEVSideDisconnect", "false"),
            ConfigurationStatus::Accepted
        );
        assert!(!registry.stop_transaction_on_ev_side_disconnect());
        assert_eq!(
            registry.change("MeterValuesSampledData", "Voltage, Current.Import"),
            ConfigurationStatus::Accepted
        );
        assert_eq!(
            registry.measurands("MeterValuesSampledData"),
            [Measurand::Voltage, Measurand::CurrentImport]
        );
    }

    #[test]
    fn list_keys_respect_their_max_length() {
        let mut registry = registry();
        let too_many = vec!["Voltage"; 23].join(",");
        assert_eq!(
            registry.change("MeterValuesSampledData", &too_many),
            ConfigurationStatus::Rejected
        );
        let exactly = vec!["Voltage"; 22].join(",");
        assert_eq!(
            registry.change("MeterValuesSampledData", &exactly),
            ConfigurationStatus::Accepted
        );
    }

    #[test]
    fn measurands_fall_back_to_defaults_when_nothing_valid_is_left() {
        let mut registry = registry();
        registry.set_internal("MeterValuesSampledData", "Bogus".to_owned());
        assert_eq!(
            registry.measurands("MeterValuesSampledData"),
            DEFAULT_SAMPLED_DATA
        );
    }

    #[test]
    fn phase_rotation_accepts_both_forms() {
        let mut registry = registry();
        for value in ["0.RST,1.RTS", "Unknown", "1.NotApplicable"] {
            assert_eq!(
                registry.change("ConnectorPhaseRotation", value),
                ConfigurationStatus::Accepted,
                "{value}"
            );
        }
    }

    #[test]
    fn default_phase_rotation_has_one_entry_per_connector() {
        let registry = registry();
        assert_eq!(
            registry.value("ConnectorPhaseRotation"),
            Some("1.Unknown,2.Unknown")
        );
        assert_eq!(registry.value("ConnectorPhaseRotationMaxLength"), Some("3"));
    }

    #[test]
    fn adding_connectors_updates_phase_rotation_and_counts() {
        let mut registry = registry();
        registry.set_connector_count(4);
        assert_eq!(
            registry.value("ConnectorPhaseRotation"),
            Some("1.Unknown,2.Unknown,3.Unknown,4.Unknown")
        );
        assert_eq!(registry.value("NumberOfConnectors"), Some("4"));
        assert_eq!(registry.value("ConnectorPhaseRotationMaxLength"), Some("5"));
        assert_eq!(registry.connector_count(), 4);

        // Shrinking is ignored.
        registry.set_connector_count(1);
        assert_eq!(registry.connector_count(), 4);
    }

    #[test]
    fn adding_connectors_leaves_bare_phase_rotation_alone() {
        let mut registry = registry();
        assert_eq!(
            registry.change("ConnectorPhaseRotation", "RST"),
            ConfigurationStatus::Accepted
        );
        registry.set_connector_count(3);
        assert_eq!(registry.value("ConnectorPhaseRotation"), Some("RST"));
    }

    #[test]
    fn typed_accessors_read_the_defaults() {
        let registry = registry();
        assert_eq!(registry.heartbeat_interval_secs(), 30);
        assert_eq!(registry.transaction_message_attempts(), 1);
        assert_eq!(
            registry.transaction_message_retry_interval(),
            Duration::from_secs(30)
        );
        assert!(registry.local_auth_list_enabled());
        assert_eq!(registry.local_auth_list_max_length(), 1000);
        assert_eq!(registry.send_local_list_max_length(), 1000);
        assert!(registry.local_authorize_offline());
        assert!(!registry.local_pre_authorize());
        assert!(registry.authorization_cache_enabled());
        assert!(registry.allow_offline_tx_for_unknown_id());
        assert!(!registry.authorize_remote_tx_requests());
        assert!(registry.stop_transaction_on_ev_side_disconnect());
        assert!(registry.stop_transaction_on_invalid_id());
        assert!(registry.reserve_connector_zero_supported());
        assert_eq!(registry.charge_profile_max_stack_level(), 10);
        assert_eq!(registry.charging_schedule_max_periods(), 24);
        assert_eq!(registry.max_charging_profiles_installed(), 10);
        assert!(registry.charging_rate_unit_allowed(ChargingRateUnitType::A));
        assert!(registry.charging_rate_unit_allowed(ChargingRateUnitType::W));
        assert_eq!(registry.minimum_status_duration(), Duration::ZERO);
    }

    #[test]
    fn charging_rate_unit_filter_follows_the_configured_list() {
        let mut registry = registry();
        // Read-only, so the only way to change it is the internal writer.
        registry.set_internal(
            "ChargingScheduleAllowedChargingRateUnit",
            "Power".to_owned(),
        );
        assert!(!registry.charging_rate_unit_allowed(ChargingRateUnitType::A));
        assert!(registry.charging_rate_unit_allowed(ChargingRateUnitType::W));
    }

    #[test]
    fn transaction_message_attempts_never_drops_below_one() {
        let mut registry = registry();
        assert_eq!(
            registry.change("TransactionMessageAttempts", "0"),
            ConfigurationStatus::Rejected
        );
        assert_eq!(registry.transaction_message_attempts(), 1);
    }

    #[test]
    fn snapshot_holds_writable_values_only() {
        let registry = registry();
        let snapshot = registry.snapshot();
        assert!(snapshot.values.contains_key("HeartbeatInterval"));
        assert!(!snapshot.values.contains_key("NumberOfConnectors"));
        assert!(!snapshot.values.contains_key("ChargePointVendor"));
        assert_eq!(
            snapshot.values.len(),
            registry
                .entries()
                .iter()
                .filter(|e| !e.readonly && !e.secret())
                .count()
        );
        assert!(!snapshot.values.contains_key("AuthorizationKey"));
    }

    #[test]
    fn security_keys_validate_and_mask() {
        let mut registry = registry();
        assert_eq!(registry.security_profile(), 0);
        assert_eq!(
            registry.change("SecurityProfile", "4"),
            ConfigurationStatus::Rejected
        );
        assert_eq!(
            registry.change("SecurityProfile", "2"),
            ConfigurationStatus::Accepted
        );
        assert_eq!(registry.security_profile(), 2);
        assert_eq!(
            registry.change("AuthorizationKey", "short"),
            ConfigurationStatus::Rejected
        );
        assert_eq!(
            registry.change("AuthorizationKey", &"k".repeat(41)),
            ConfigurationStatus::Rejected
        );
        assert_eq!(
            registry.change("authorizationkey", "0123456789abcdef"),
            ConfigurationStatus::Accepted
        );
        assert_eq!(registry.authorization_key(), Some("0123456789abcdef"));
        let entry = registry.get("AuthorizationKey").unwrap();
        assert!(entry.secret());
        assert_eq!(entry.display_value(), "********");
        let printed = format!("{registry:?} {entry:?}");
        assert!(!printed.contains("0123456789abcdef"), "Debug leaks the key");
        for key in [
            "CertificateStoreMaxLength",
            "AdditionalRootCertificateCheck",
            "CertificateSignedMaxChainSize",
        ] {
            assert_eq!(
                registry.change(key, "1"),
                ConfigurationStatus::Rejected,
                "{key}"
            );
        }
        assert_eq!(
            registry.change("CpoName", &"x".repeat(51)),
            ConfigurationStatus::Rejected
        );
        assert_eq!(
            registry.change("CpoName", "ACME"),
            ConfigurationStatus::Accepted
        );
    }

    #[test]
    fn restore_applies_valid_values_and_reports_the_rest() {
        let mut source = registry();
        source.change("LightIntensity", "42");
        source.change("MeterValuesSampledData", "Voltage");
        let mut snapshot = source.snapshot();
        snapshot
            .values
            .insert("NumberOfConnectors".to_owned(), "9".to_owned());
        snapshot
            .values
            .insert("HeartbeatInterval".to_owned(), "not a number".to_owned());
        snapshot.values.insert("GoneKey".to_owned(), "1".to_owned());

        let mut target = registry();
        let skipped = target.restore(&snapshot);
        assert_eq!(
            skipped,
            ["GoneKey", "HeartbeatInterval", "NumberOfConnectors"]
        );
        assert_eq!(target.value("LightIntensity"), Some("42"));
        assert_eq!(target.value("MeterValuesSampledData"), Some("Voltage"));
        assert_eq!(target.value("NumberOfConnectors"), Some("2"));
        assert_eq!(target.value("HeartbeatInterval"), Some("30"));
    }

    #[test]
    fn snapshot_roundtrips_through_json() {
        let mut registry = registry();
        registry.change("AuthorizationCacheEnabled", "false");
        let json = serde_json::to_string(&registry.snapshot()).unwrap();
        let back: ConfigSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, registry.snapshot());
        assert_eq!(
            serde_json::from_str::<ConfigSnapshot>("{}").unwrap(),
            ConfigSnapshot::default()
        );
    }

    #[test]
    fn identity_keys_come_from_the_station_config() {
        let config = StationConfig::new("ws://h/p")
            .with_charge_point_id("CP-9")
            .with_vendor_model("acme", "wall-box");
        let registry = ConfigRegistry::new(&config);
        assert_eq!(registry.value("ChargePointSerialNumber"), Some("CP-9"));
        assert_eq!(registry.value("ChargePointVendor"), Some("acme"));
        assert_eq!(registry.value("ChargePointModel"), Some("wall-box"));
    }

    #[test]
    fn parse_int_rejects_signs_and_whitespace() {
        assert_eq!(parse_int("0"), Some(0));
        assert_eq!(parse_int("007"), Some(7));
        for bad in ["", "+1", "-1", " 1", "1 ", "1e3", "2147483648"] {
            assert_eq!(parse_int(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn default_sampled_data_is_the_ts_list() {
        assert_eq!(
            default_measurands(),
            "Energy.Active.Import.Register,Power.Active.Import,Current.Import,Voltage,Temperature,SoC"
        );
        assert_eq!(Measurand::ALL.len(), 22);
    }
}

#[cfg(test)]
mod property_tests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        /// Any ASCII case spelling of a key finds the same entry.
        #[test]
        fn key_lookup_ignores_ascii_case(
            index in 0..46_usize,
            upper in proptest::collection::vec(any::<bool>(), 0..64),
        ) {
            let registry = ConfigRegistry::new(&StationConfig::new("ws://h/p"));
            let canonical = registry.entries()[index].key;
            let spelled: String = canonical
                .chars()
                .enumerate()
                .map(|(i, c)| {
                    if upper.get(i).copied().unwrap_or(false) {
                        c.to_ascii_uppercase()
                    } else {
                        c.to_ascii_lowercase()
                    }
                })
                .collect();
            let found = registry.get(&spelled).expect("known key");
            prop_assert_eq!(found.key, canonical);
        }

        /// A rejected change never alters any stored value.
        #[test]
        fn rejected_changes_keep_values(value in "[ -~]{0,30}") {
            let mut registry = ConfigRegistry::new(&StationConfig::new("ws://h/p"));
            let before = registry.snapshot();
            let status = registry.change("HeartbeatInterval", &value);
            if status != ConfigurationStatus::Accepted {
                prop_assert_eq!(registry.snapshot(), before);
            }
        }
    }
}
