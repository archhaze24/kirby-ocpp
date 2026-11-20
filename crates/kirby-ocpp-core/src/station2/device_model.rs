//! The device model: variables, their values and the typed accessors the other modules read.
//!
//! The variables are described by one static table ([`TABLE`]); values live in
//! [`DeviceModel`]. A variable is `Station` scoped (one row), `Evse` scoped (one row per EVSE)
//! or `Connector` scoped (one row per EVSE, each EVSE has one connector). Only the `Actual`
//! attribute is supported. Live variables are computed on read from the actor state.
//!
//! `BasicAuthPassword` is write-only: it never appears in a read, a report, a snapshot view,
//! a persisted value or a `Debug` print.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use super::actor::{Station2Actor, TimerKey2};
use super::snapshot::VariableView;
use super::types::{ItemsPer, LiveVar, Reply2, StartStopPoint};
use crate::config::StationConfig;
use crate::error::StationError;
use crate::ocpp::v2::messages::{
    ComponentCriterion, ComponentVariable, DataType, Mutability, NotifyReportRequest, ReportBase,
    ReportData, VariableAttribute, VariableCharacteristics,
};
use crate::ocpp::v2::types::{
    AttributeType, Component, Evse as EvseRef, Measurand, Variable, WireEnum,
};
use crate::station::LogLevel;
use crate::station2::command::Outcome2;

/// The values that differ from their defaults and are not secret, keyed by
/// `Component.Variable` text, with `.Instance` appended for an instanced variable.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceModelPersist {
    pub values: BTreeMap<String, String>,
}

/// Longest accepted `BasicAuthPassword` (the schema limit).
const MAX_PASSWORD_CHARS: usize = 64;
/// Shortest `BasicAuthPassword` without a warning (A00.FR.205).
const RECOMMENDED_PASSWORD_CHARS: usize = 16;
/// Bound of `Snapshot2::variables`.
const MAX_VIEWS: usize = 256;

const AVAILABILITY: &str = "Available,Occupied,Reserved,Unavailable,Faulted";
const POINTS: &str = "Authorized,EVConnected,PowerPathClosed,EnergyTransfer";
const MEASURANDS: &str = "Energy.Active.Import.Register,Power.Active.Import,Current.Import,Voltage";

/// Which components a variable belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Station,
    Evse,
    Connector,
}

/// Where the default of a variable comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dflt {
    Fixed(&'static str),
    /// `--heartbeat`.
    Heartbeat,
    /// `--call-timeout-ms`, in seconds.
    CallTimeout,
    /// `--ws-ping`.
    WsPing,
    /// `--phases`.
    Phases,
}
use Dflt::{CallTimeout, Fixed, Heartbeat, Phases, WsPing};

/// One variable of the device model (without a description).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct VarSpec {
    pub component: &'static str,
    pub scope: Scope,
    pub instance: Option<&'static str>,
    pub variable: &'static str,
    pub data_type: DataType,
    pub mutability: Mutability,
    pub unit: Option<&'static str>,
    pub min: Option<i32>,
    pub max: Option<i32>,
    pub values_list: Option<&'static str>,
    pub default: Dflt,
    pub supports_monitoring: bool,
    /// Computed from the actor state on every read. The ids are bound per EVSE and connector.
    pub live: Option<LiveVar>,
    /// A list value may be empty.
    pub empty_ok: bool,
}

impl VarSpec {
    const fn rw(mut self) -> Self {
        self.mutability = Mutability::ReadWrite;
        self
    }
    const fn wo(mut self) -> Self {
        self.mutability = Mutability::WriteOnly;
        self
    }
    const fn at(mut self, instance: &'static str) -> Self {
        self.instance = Some(instance);
        self
    }
    const fn u(mut self, unit: &'static str) -> Self {
        self.unit = Some(unit);
        self
    }
    const fn lo(mut self, min: i32) -> Self {
        self.min = Some(min);
        self
    }
    const fn hi(mut self, max: i32) -> Self {
        self.max = Some(max);
        self
    }
    const fn opts(mut self, values: &'static str) -> Self {
        self.values_list = Some(values);
        self
    }
    const fn live(mut self, live: LiveVar) -> Self {
        self.live = Some(live);
        self
    }
    const fn empty(mut self) -> Self {
        self.empty_ok = true;
        self
    }

    /// `Component.Variable[.Instance]`: the key of persisted values.
    fn key(&self) -> String {
        match self.instance {
            Some(instance) => format!("{}.{}.{instance}", self.component, self.variable),
            None => format!("{}.{}", self.component, self.variable),
        }
    }

    fn is_list(&self) -> bool {
        matches!(
            self.data_type,
            DataType::MemberList | DataType::SequenceList
        )
    }
}

const fn row(
    component: &'static str,
    scope: Scope,
    variable: &'static str,
    data_type: DataType,
    default: Dflt,
) -> VarSpec {
    VarSpec {
        component,
        scope,
        instance: None,
        variable,
        data_type,
        mutability: Mutability::ReadOnly,
        unit: None,
        min: None,
        max: None,
        values_list: None,
        default,
        supports_monitoring: false,
        live: None,
        empty_ok: false,
    }
}

/// A station-level boolean.
const fn flag(c: &'static str, v: &'static str, d: &'static str) -> VarSpec {
    row(c, Scope::Station, v, DataType::Boolean, Fixed(d))
}
/// A station-level integer, at least 0.
const fn int(c: &'static str, v: &'static str, d: Dflt) -> VarSpec {
    row(c, Scope::Station, v, DataType::Integer, d).lo(0)
}
/// A station-level text.
const fn text(c: &'static str, v: &'static str, d: &'static str) -> VarSpec {
    row(c, Scope::Station, v, DataType::String, Fixed(d))
}
/// A station-level member list.
const fn members(c: &'static str, v: &'static str, d: &'static str, all: &'static str) -> VarSpec {
    row(c, Scope::Station, v, DataType::MemberList, Fixed(d)).opts(all)
}

const CS: &str = "ChargingStation";
const OCPP: &str = "OCPPCommCtrlr";
const AUTH: &str = "AuthCtrlr";
const CACHE: &str = "AuthCacheCtrlr";
const LOCAL_LIST: &str = "LocalAuthListCtrlr";
const RESERVATION: &str = "ReservationCtrlr";
const TX: &str = "TxCtrlr";
const SAMPLED: &str = "SampledDataCtrlr";
const ALIGNED: &str = "AlignedDataCtrlr";
const DEVICE: &str = "DeviceDataCtrlr";
const SECURITY: &str = "SecurityCtrlr";
const CLOCK: &str = "ClockCtrlr";

/// Whether `spec` is `SecurityCtrlr.BasicAuthPassword`.
pub(super) fn is_basic_auth_password(spec: &VarSpec) -> bool {
    spec.component == SECURITY && spec.variable == "BasicAuthPassword"
}

/// Every variable of the station. Stored (non-live) values of `Station` rows are the only
/// ones that can change.
pub(crate) static TABLE: &[VarSpec] = &[
    // ChargingStation
    row(
        CS,
        Scope::Station,
        "AvailabilityState",
        DataType::OptionList,
        Fixed("Available"),
    )
    .opts(AVAILABILITY)
    .live(LiveVar::StationAvailability),
    row(
        CS,
        Scope::Station,
        "Available",
        DataType::Boolean,
        Fixed("true"),
    ),
    row(
        CS,
        Scope::Station,
        "SupplyPhases",
        DataType::Integer,
        Phases,
    )
    .lo(1)
    .hi(3),
    // EVSE
    row(
        "EVSE",
        Scope::Evse,
        "AvailabilityState",
        DataType::OptionList,
        Fixed("Available"),
    )
    .opts(AVAILABILITY)
    .live(LiveVar::EvseAvailability(0)),
    row(
        "EVSE",
        Scope::Evse,
        "Available",
        DataType::Boolean,
        Fixed("true"),
    ),
    row("EVSE", Scope::Evse, "Power", DataType::Decimal, Fixed("0"))
        .u("W")
        .lo(0)
        .live(LiveVar::EvsePower(0)),
    row(
        "EVSE",
        Scope::Evse,
        "SupplyPhases",
        DataType::Integer,
        Phases,
    )
    .lo(1)
    .hi(3),
    row(
        "EVSE",
        Scope::Evse,
        "AllowReset",
        DataType::Boolean,
        Fixed("false"),
    ),
    // Connector
    row(
        "Connector",
        Scope::Connector,
        "AvailabilityState",
        DataType::OptionList,
        Fixed("Available"),
    )
    .opts(AVAILABILITY)
    .live(LiveVar::ConnectorAvailability(0, 0)),
    row(
        "Connector",
        Scope::Connector,
        "Available",
        DataType::Boolean,
        Fixed("true"),
    ),
    row(
        "Connector",
        Scope::Connector,
        "ConnectorType",
        DataType::String,
        Fixed("cType2"),
    ),
    row(
        "Connector",
        Scope::Connector,
        "SupplyPhases",
        DataType::Integer,
        Phases,
    )
    .lo(1)
    .hi(3),
    // ClockCtrlr
    row(
        CLOCK,
        Scope::Station,
        "DateTime",
        DataType::DateTime,
        Fixed(""),
    )
    .live(LiveVar::DateTime),
    row(
        CLOCK,
        Scope::Station,
        "TimeSource",
        DataType::SequenceList,
        Fixed("Heartbeat"),
    )
    .opts("Heartbeat,NTP,GPS,RealTimeClock,MobileNetwork,RadioTimeTransmitter")
    .rw(),
    // OCPPCommCtrlr
    int(OCPP, "HeartbeatInterval", Heartbeat).rw().u("s").lo(1),
    int(OCPP, "MessageTimeout", CallTimeout)
        .at("Default")
        .u("s")
        .lo(1)
        .hi(86_400),
    int(OCPP, "MessageAttempts", Fixed("3"))
        .at("TransactionEvent")
        .rw()
        .lo(1)
        .hi(1_000),
    int(OCPP, "MessageAttemptInterval", Fixed("60"))
        .at("TransactionEvent")
        .rw()
        .u("s")
        .hi(86_400),
    int(OCPP, "OfflineThreshold", Fixed("60")).rw().u("s"),
    int(OCPP, "WebSocketPingInterval", WsPing).rw().u("s"),
    int(OCPP, "ResetRetries", Fixed("3")).rw().hi(1_000),
    flag(OCPP, "UnlockOnEVSideDisconnect", "true").rw(),
    members(
        OCPP,
        "FileTransferProtocols",
        "HTTPS",
        "FTP,FTPS,HTTP,HTTPS,SFTP",
    ),
    text(OCPP, "NetworkConfigurationPriority", "0").rw(),
    int(OCPP, "NetworkProfileConnectionAttempts", Fixed("1")).rw(),
    flag(OCPP, "QueueAllMessages", "false").rw(),
    // AuthCtrlr
    flag(AUTH, "Enabled", "true").rw(),
    flag(AUTH, "AuthorizeRemoteStart", "true").rw(),
    flag(AUTH, "LocalAuthorizeOffline", "true").rw(),
    flag(AUTH, "LocalPreAuthorize", "false").rw(),
    flag(AUTH, "OfflineTxForUnknownIdEnabled", "false").rw(),
    text(AUTH, "MasterPassGroupId", "").rw().empty(),
    // LocalAuthListCtrlr
    flag(LOCAL_LIST, "Enabled", "true").rw(),
    flag(LOCAL_LIST, "Available", "true"),
    int(LOCAL_LIST, "Entries", Fixed("0"))
        .hi(1_000)
        .live(LiveVar::LocalListEntries),
    int(LOCAL_LIST, "ItemsPerMessage", Fixed("1000")),
    int(LOCAL_LIST, "BytesPerMessage", Fixed("262144")),
    flag(LOCAL_LIST, "SupportsExpiryDateTime", "true"),
    flag(LOCAL_LIST, "DisablePostAuthorize", "false").rw(),
    // ReservationCtrlr
    flag(RESERVATION, "Enabled", "true").rw(),
    flag(RESERVATION, "Available", "true"),
    flag(RESERVATION, "NonEvseSpecific", "true"),
    // AuthCacheCtrlr
    flag(CACHE, "Enabled", "true").rw(),
    int(CACHE, "LifeTime", Fixed("86400")).rw().u("s"),
    int(CACHE, "Storage", Fixed("0")).hi(1_000),
    // TxCtrlr
    members(TX, "TxStartPoint", "PowerPathClosed", POINTS).rw(),
    members(TX, "TxStopPoint", "EVConnected,Authorized", POINTS).rw(),
    int(TX, "EVConnectionTimeOut", Fixed("60")).rw().u("s"),
    flag(TX, "StopTxOnEVSideDisconnect", "true").rw(),
    flag(TX, "StopTxOnInvalidId", "true").rw(),
    // SampledDataCtrlr
    flag(SAMPLED, "Enabled", "true").rw(),
    members(
        SAMPLED,
        "TxStartedMeasurands",
        "Energy.Active.Import.Register",
        MEASURANDS,
    )
    .rw()
    .empty(),
    members(
        SAMPLED,
        "TxUpdatedMeasurands",
        "Energy.Active.Import.Register,Power.Active.Import",
        MEASURANDS,
    )
    .rw()
    .empty(),
    members(
        SAMPLED,
        "TxEndedMeasurands",
        "Energy.Active.Import.Register",
        MEASURANDS,
    )
    .rw()
    .empty(),
    int(SAMPLED, "TxUpdatedInterval", Fixed("30")).rw().u("s"),
    int(SAMPLED, "TxEndedInterval", Fixed("0")).rw().u("s"),
    // AlignedDataCtrlr
    members(
        ALIGNED,
        "Measurands",
        "Energy.Active.Import.Register",
        MEASURANDS,
    )
    .rw()
    .empty(),
    int(ALIGNED, "Interval", Fixed("0")).rw().u("s"),
    members(ALIGNED, "TxEndedMeasurands", "", MEASURANDS)
        .rw()
        .empty(),
    int(ALIGNED, "TxEndedInterval", Fixed("0")).rw().u("s"),
    // DeviceDataCtrlr
    int(DEVICE, "ItemsPerMessage", Fixed("100"))
        .at("GetReport")
        .lo(1),
    int(DEVICE, "ItemsPerMessage", Fixed("100"))
        .at("GetVariables")
        .lo(1),
    int(DEVICE, "ItemsPerMessage", Fixed("100"))
        .at("SetVariables")
        .lo(1),
    int(DEVICE, "BytesPerMessage", Fixed("20000"))
        .at("GetReport")
        .lo(1),
    int(DEVICE, "BytesPerMessage", Fixed("20000"))
        .at("GetVariables")
        .lo(1),
    int(DEVICE, "BytesPerMessage", Fixed("20000"))
        .at("SetVariables")
        .lo(1),
    // SecurityCtrlr
    int(SECURITY, "SecurityProfile", Fixed("0")).hi(3),
    text(SECURITY, "OrganizationName", "kirby-ocpp").rw(),
    int(SECURITY, "CertificateEntries", Fixed("0")).live(LiveVar::CertificateEntries),
    text(SECURITY, "BasicAuthPassword", "").wo(),
];

/// A table row addressed by a `GetVariables` or `SetVariables` item.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Target {
    pub spec: &'static VarSpec,
    pub evse: Option<u32>,
    pub connector: Option<u32>,
}

/// What a component and variable name resolves to.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Lookup {
    Found(Target),
    UnknownComponent,
    UnknownVariable,
}

/// Why a write was not applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SetFailure {
    UnknownComponent,
    UnknownVariable,
    /// Only `Actual` exists.
    AttributeType,
    /// The variable exists but the value cannot be set: `reason` is a status reason code.
    Rejected {
        reason: &'static str,
        info: String,
    },
}

impl std::fmt::Display for SetFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownComponent => f.write_str("unknown component"),
            Self::UnknownVariable => f.write_str("unknown variable"),
            Self::AttributeType => f.write_str("only the Actual attribute is supported"),
            Self::Rejected { info, .. } => f.write_str(info),
        }
    }
}

/// A value was stored.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Applied {
    pub spec: &'static VarSpec,
    /// A password shorter than the recommended length was accepted.
    pub weak_secret: bool,
}

fn reject(reason: &'static str, info: impl Into<String>) -> SetFailure {
    SetFailure::Rejected {
        reason,
        info: info.into(),
    }
}

/// Checks `text` against the type and limits of `spec`; returns the value to store.
fn validate(spec: &VarSpec, text: &str) -> Result<String, SetFailure> {
    match spec.data_type {
        DataType::Boolean => match text {
            "true" | "false" => Ok(text.to_owned()),
            _ => Err(reject("InvalidValue", "expected true or false")),
        },
        DataType::Integer => {
            let number: i64 = text
                .parse()
                .map_err(|_| reject("InvalidValue", "expected an integer"))?;
            let min = i64::from(spec.min.unwrap_or(i32::MIN));
            let max = i64::from(spec.max.unwrap_or(i32::MAX));
            if number < min || number > max {
                return Err(reject(
                    "ValueOutOfRange",
                    format!("expected a value from {min} to {max}"),
                ));
            }
            Ok(number.to_string())
        }
        DataType::Decimal => {
            let number: f64 = text
                .parse()
                .ok()
                .filter(|n: &f64| n.is_finite())
                .ok_or_else(|| reject("InvalidValue", "expected a decimal number"))?;
            let min = spec.min.map_or(f64::MIN, f64::from);
            let max = spec.max.map_or(f64::MAX, f64::from);
            if number < min || number > max {
                return Err(reject("ValueOutOfRange", "value out of range"));
            }
            Ok(text.to_owned())
        }
        DataType::DateTime => DateTime::parse_from_rfc3339(text)
            .map(|_| text.to_owned())
            .map_err(|_| reject("InvalidValue", "expected an RFC 3339 date and time")),
        DataType::String => Ok(text.to_owned()),
        DataType::OptionList => {
            if spec
                .values_list
                .is_some_and(|list| list.split(',').any(|m| m == text))
            {
                Ok(text.to_owned())
            } else {
                Err(reject(
                    "InvalidValue",
                    "value is not in the list of allowed values",
                ))
            }
        }
        DataType::MemberList | DataType::SequenceList => validate_list(spec, text),
    }
}

fn validate_list(spec: &VarSpec, text: &str) -> Result<String, SetFailure> {
    let items: Vec<&str> = if text.trim().is_empty() {
        Vec::new()
    } else {
        text.split(',').map(str::trim).collect()
    };
    if items.is_empty() && !spec.empty_ok {
        return Err(reject("InvalidValue", "the list must not be empty"));
    }
    let allowed = spec.values_list.unwrap_or_default();
    for (index, item) in items.iter().enumerate() {
        if !allowed.split(',').any(|member| member == *item) {
            return Err(reject(
                "InvalidValue",
                format!("{item:?} is not an allowed member"),
            ));
        }
        if items[..index].contains(item) {
            return Err(reject("InvalidValue", format!("{item:?} is listed twice")));
        }
    }
    Ok(items.join(","))
}

/// A chunked `NotifyReport` in progress.
#[derive(Debug)]
pub(crate) struct ReportJob {
    pub request_id: i32,
    pub remaining: VecDeque<ReportData>,
    pub seq_no: i32,
    /// A `NotifyReport` of this job awaits its answer.
    pub in_flight: bool,
}

impl ReportJob {
    /// Cuts the next chunk of at most `size` items; `tbc` is set while items remain.
    pub(crate) fn take_chunk(
        &mut self,
        size: usize,
        generated_at: DateTime<Utc>,
    ) -> NotifyReportRequest {
        let count = size.max(1).min(self.remaining.len());
        let data: Vec<ReportData> = self.remaining.drain(..count).collect();
        let request = NotifyReportRequest {
            request_id: self.request_id,
            generated_at,
            report_data: Some(data),
            tbc: (!self.remaining.is_empty()).then_some(true),
            seq_no: self.seq_no,
            custom_data: None,
        };
        self.seq_no = self.seq_no.saturating_add(1);
        request
    }
}

/// Resolves a live variable to its text.
pub(crate) type LiveFn<'a> = &'a dyn Fn(LiveVar) -> String;

/// The values of the variables and the state of the report in progress.
pub(crate) struct DeviceModel {
    /// Values that differ from the default, by [`VarSpec::key`].
    values: BTreeMap<String, String>,
    password: Option<String>,
    evse_count: u32,
    heartbeat: u32,
    call_timeout_secs: u64,
    ws_ping: u32,
    phases: u8,
    /// `SecurityCtrlr.SecurityProfile`, kept in step with the snapshot.
    pub(super) security_profile: u8,
    pub(super) report: Option<ReportJob>,
}

impl std::fmt::Debug for DeviceModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceModel")
            .field("values", &self.values)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .field("evse_count", &self.evse_count)
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}

impl Default for DeviceModel {
    fn default() -> Self {
        Self::new(&StationConfig::default())
    }
}

fn non_empty(text: Option<&String>) -> Option<&str> {
    text.map(String::as_str).filter(|text| !text.is_empty())
}

/// Whether the variable of `target` is the one `selector` names: the component name, instance
/// and EVSE (with connector) must match exactly; an absent variable selects them all.
fn selector_matches(selector: &ComponentVariable, target: &Target) -> bool {
    let component = &selector.component;
    let spec = target.spec;
    if component.name != spec.component || non_empty(component.instance.as_ref()).is_some() {
        return false;
    }
    let place_ok = match &component.evse {
        None => spec.scope == Scope::Station,
        Some(EvseRef {
            id, connector_id, ..
        }) => {
            u32::try_from(*id).is_ok_and(|id| Some(id) == target.evse)
                && match connector_id {
                    None => spec.scope == Scope::Evse,
                    Some(connector) => {
                        spec.scope == Scope::Connector
                            && u32::try_from(*connector).ok() == target.connector
                    }
                }
        }
    };
    place_ok
        && selector.variable.as_ref().is_none_or(|variable| {
            variable.name == spec.variable && non_empty(variable.instance.as_ref()) == spec.instance
        })
}

impl DeviceModel {
    /// A model with the defaults of `config` and one connector per EVSE.
    pub(crate) fn new(config: &StationConfig) -> Self {
        Self {
            values: BTreeMap::new(),
            password: None,
            evse_count: config.connector_count.max(1),
            heartbeat: config.heartbeat_interval_secs.max(1),
            call_timeout_secs: config.call_timeout_ms.div_ceil(1_000).max(1),
            ws_ping: config.ws_ping_interval_secs,
            phases: config.phases.clamp(1, 3),
            security_profile: 0,
            report: None,
        }
    }

    fn default_value(&self, spec: &VarSpec) -> String {
        match spec.default {
            Fixed(text) => text.to_owned(),
            Heartbeat => self.heartbeat.to_string(),
            CallTimeout => self.call_timeout_secs.to_string(),
            WsPing => self.ws_ping.to_string(),
            Phases => self.phases.to_string(),
        }
    }

    /// The stored value of a non-live variable.
    fn value(&self, spec: &VarSpec) -> String {
        if spec.variable == "SecurityProfile" && spec.component == SECURITY {
            return self.security_profile.to_string();
        }
        self.values
            .get(&spec.key())
            .cloned()
            .unwrap_or_else(|| self.default_value(spec))
    }

    /// Resolves a component and variable to a table row (FR.14, FR.15: an instance must match
    /// exactly, an empty instance is none).
    pub(crate) fn find(&self, component: &Component, variable: &Variable) -> Lookup {
        if non_empty(component.instance.as_ref()).is_some() {
            return Lookup::UnknownComponent;
        }
        let (scope, evse, connector) = match &component.evse {
            None => (Scope::Station, None, None),
            Some(EvseRef {
                id, connector_id, ..
            }) => {
                let Some(id) = u32::try_from(*id)
                    .ok()
                    .filter(|id| (1..=self.evse_count).contains(id))
                else {
                    return Lookup::UnknownComponent;
                };
                match connector_id {
                    None => (Scope::Evse, Some(id), None),
                    Some(1) => (Scope::Connector, Some(id), Some(1)),
                    Some(_) => return Lookup::UnknownComponent,
                }
            }
        };
        let instance = non_empty(variable.instance.as_ref());
        let mut component_known = false;
        for spec in TABLE
            .iter()
            .filter(|s| s.scope == scope && s.component == component.name)
        {
            component_known = true;
            if spec.variable == variable.name && spec.instance == instance {
                return Lookup::Found(Target {
                    spec,
                    evse,
                    connector,
                });
            }
        }
        if component_known {
            Lookup::UnknownVariable
        } else {
            Lookup::UnknownComponent
        }
    }

    /// The `Actual` value of a variable; `None` for a write-only one.
    pub(crate) fn read(&self, target: &Target, live: LiveFn<'_>) -> Option<String> {
        let spec = target.spec;
        if spec.mutability == Mutability::WriteOnly {
            return None;
        }
        let Some(live_var) = spec.live else {
            return Some(self.value(spec));
        };
        let evse = target.evse.unwrap_or(0);
        Some(live(match live_var {
            LiveVar::EvseAvailability(_) => LiveVar::EvseAvailability(evse),
            LiveVar::ConnectorAvailability(..) => {
                LiveVar::ConnectorAvailability(evse, target.connector.unwrap_or(0))
            }
            LiveVar::EvsePower(_) => LiveVar::EvsePower(evse),
            other => other,
        }))
    }

    /// Sets the `Actual` value of a variable as `SetVariables` does.
    pub(crate) fn set_value(
        &mut self,
        component: &Component,
        variable: &Variable,
        attribute: Option<AttributeType>,
        text: &str,
    ) -> Result<Applied, SetFailure> {
        let target = match self.find(component, variable) {
            Lookup::Found(target) => target,
            Lookup::UnknownComponent => return Err(SetFailure::UnknownComponent),
            Lookup::UnknownVariable => return Err(SetFailure::UnknownVariable),
        };
        if attribute.is_some_and(|kind| kind != AttributeType::Actual) {
            return Err(SetFailure::AttributeType);
        }
        let spec = target.spec;
        if spec.mutability == Mutability::ReadOnly {
            return Err(reject("ReadOnly", "the variable is read-only"));
        }
        if spec.mutability == Mutability::WriteOnly {
            let length = text.chars().count();
            if length == 0 || length > MAX_PASSWORD_CHARS {
                return Err(reject(
                    "ValueOutOfRange",
                    format!("the password needs 1 to {MAX_PASSWORD_CHARS} characters"),
                ));
            }
            self.password = Some(text.to_owned());
            return Ok(Applied {
                spec,
                weak_secret: length < RECOMMENDED_PASSWORD_CHARS,
            });
        }
        let value = validate(spec, text)?;
        self.store(spec, value);
        Ok(Applied {
            spec,
            weak_secret: false,
        })
    }

    /// Keeps `value` only when it differs from the default.
    fn store(&mut self, spec: &VarSpec, value: String) {
        let key = spec.key();
        if value == self.default_value(spec) {
            self.values.remove(&key);
        } else {
            self.values.insert(key, value);
        }
    }

    /// The `BasicAuthPassword`, if one was set.
    pub(crate) fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }

    /// Sets or clears the password without validation (restore from the secrets file).
    pub(super) fn set_password(&mut self, password: Option<String>) {
        self.password = password;
    }

    /// The values to write to the state file.
    pub(super) fn persist(&self) -> DeviceModelPersist {
        DeviceModelPersist {
            values: self.values.clone(),
        }
    }

    /// Restores persisted values. Keys that are not writable variables of this station and
    /// values that no longer validate are ignored.
    pub(super) fn restore(&mut self, persisted: &DeviceModelPersist) {
        for (key, text) in &persisted.values {
            let Some(spec) = TABLE.iter().find(|spec| {
                spec.scope == Scope::Station
                    && spec.mutability == Mutability::ReadWrite
                    && spec.live.is_none()
                    && spec.key() == *key
            }) else {
                continue;
            };
            if let Ok(value) = validate(spec, text) {
                self.store(spec, value);
            }
        }
    }

    /// The text of a station-level variable, empty for a name the table does not have.
    fn station_text(&self, component: &str, variable: &str, instance: Option<&str>) -> String {
        TABLE
            .iter()
            .find(|s| {
                s.scope == Scope::Station
                    && s.component == component
                    && s.variable == variable
                    && s.instance == instance
            })
            .map_or_else(String::new, |spec| self.value(spec))
    }

    fn uint(&self, component: &str, variable: &str, instance: Option<&str>) -> u32 {
        self.station_text(component, variable, instance)
            .parse()
            .unwrap_or(0)
    }

    fn flag(&self, component: &str, variable: &str) -> bool {
        self.station_text(component, variable, None) == "true"
    }

    fn list(&self, component: &str, variable: &str) -> Vec<String> {
        self.station_text(component, variable, None)
            .split(',')
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect()
    }

    fn points(&self, variable: &str) -> Vec<StartStopPoint> {
        self.list(TX, variable)
            .iter()
            .filter_map(|item| match item.as_str() {
                "Authorized" => Some(StartStopPoint::Authorized),
                "EVConnected" => Some(StartStopPoint::EvConnected),
                "PowerPathClosed" => Some(StartStopPoint::PowerPathClosed),
                "EnergyTransfer" => Some(StartStopPoint::EnergyTransfer),
                _ => None,
            })
            .collect()
    }

    fn measurands(&self, component: &str, variable: &str) -> Vec<Measurand> {
        self.list(component, variable)
            .iter()
            .filter_map(|item| Measurand::ALL.iter().find(|m| m.as_str() == item).copied())
            .collect()
    }

    /// Station-level variables for the snapshot. `DateTime` is left out: it changes on every
    /// read and would make every snapshot differ from the last.
    pub(crate) fn views(&self, live: LiveFn<'_>) -> Vec<VariableView> {
        TABLE
            .iter()
            .filter(|spec| spec.scope == Scope::Station && spec.live != Some(LiveVar::DateTime))
            .take(MAX_VIEWS)
            .map(|spec| {
                let target = Target {
                    spec,
                    evse: None,
                    connector: None,
                };
                VariableView {
                    component: spec.component.to_owned(),
                    instance: spec.instance.map(str::to_owned),
                    variable: spec.variable.to_owned(),
                    value: self.read(&target, live),
                    mutability: spec.mutability,
                }
            })
            .collect()
    }

    /// The report data of a `GetBaseReport`. Write-only variables are listed without a value.
    pub(crate) fn report_items(&self, base: ReportBase, live: LiveFn<'_>) -> Vec<ReportData> {
        let wanted = |spec: &VarSpec| match base {
            ReportBase::FullInventory => true,
            ReportBase::ConfigurationInventory => spec.mutability != Mutability::ReadOnly,
            ReportBase::SummaryInventory => spec.variable == "AvailabilityState",
        };
        let mut items = Vec::new();
        let mut add = |spec: &'static VarSpec, evse: Option<u32>, connector: Option<u32>| {
            let target = Target {
                spec,
                evse,
                connector,
            };
            items.push(self.report_item(&target, base != ReportBase::SummaryInventory, live));
        };
        for spec in TABLE
            .iter()
            .filter(|s| s.scope == Scope::Station && wanted(s))
        {
            add(spec, None, None);
        }
        for evse in 1..=self.evse_count {
            for spec in TABLE.iter().filter(|s| s.scope == Scope::Evse && wanted(s)) {
                add(spec, Some(evse), None);
            }
            for spec in TABLE
                .iter()
                .filter(|s| s.scope == Scope::Connector && wanted(s))
            {
                add(spec, Some(evse), Some(1));
            }
        }
        items
    }

    /// The report data of a `GetReport` (B08): the variables of the components that match any
    /// of `criteria` (all components when there are none) and one of `selectors` (everything
    /// when there are none). Components are matched exactly like in `GetVariables`.
    pub(crate) fn report_items_for(
        &self,
        criteria: &[ComponentCriterion],
        selectors: &[ComponentVariable],
        live: LiveFn<'_>,
    ) -> Vec<ReportData> {
        let mut targets: Vec<Target> = Vec::new();
        for spec in TABLE.iter().filter(|s| s.scope == Scope::Station) {
            targets.push(Target {
                spec,
                evse: None,
                connector: None,
            });
        }
        for evse in 1..=self.evse_count {
            for (scope, connector) in [(Scope::Evse, None), (Scope::Connector, Some(1))] {
                for spec in TABLE.iter().filter(|s| s.scope == scope) {
                    targets.push(Target {
                        spec,
                        evse: Some(evse),
                        connector,
                    });
                }
            }
        }
        let component_ok = |target: &Target| {
            criteria.is_empty()
                || criteria.iter().any(|criterion| {
                    let name = match criterion {
                        ComponentCriterion::Active => "Active",
                        ComponentCriterion::Available => "Available",
                        ComponentCriterion::Enabled => "Enabled",
                        ComponentCriterion::Problem => "Problem",
                    };
                    // A component without the variable meets `Active`, `Available` and
                    // `Enabled` (B08.FR.07 to FR.09), never `Problem` (B08.FR.10).
                    let flag = TABLE.iter().find(|s| {
                        s.scope == target.spec.scope
                            && s.component == target.spec.component
                            && s.variable == name
                            && s.instance.is_none()
                    });
                    match flag {
                        Some(spec) => {
                            let flag = Target {
                                spec,
                                evse: target.evse,
                                connector: target.connector,
                            };
                            self.read(&flag, live)
                                .is_some_and(|value| value.eq_ignore_ascii_case("true"))
                        }
                        None => !matches!(criterion, ComponentCriterion::Problem),
                    }
                })
        };
        let selected = |target: &Target| {
            selectors.is_empty()
                || selectors
                    .iter()
                    .any(|selector| selector_matches(selector, target))
        };
        targets
            .iter()
            .filter(|target| component_ok(target) && selected(target))
            .map(|target| self.report_item(target, true, live))
            .collect()
    }

    fn report_item(&self, target: &Target, characteristics: bool, live: LiveFn<'_>) -> ReportData {
        let spec = target.spec;
        let component = Component {
            evse: target.evse.map(|id| EvseRef {
                id: i32::try_from(id).unwrap_or(i32::MAX),
                connector_id: target
                    .connector
                    .map(|c| i32::try_from(c).unwrap_or(i32::MAX)),
                custom_data: None,
            }),
            name: spec.component.to_owned(),
            instance: None,
            custom_data: None,
        };
        let attribute = VariableAttribute {
            kind: Some(AttributeType::Actual),
            value: self.read(target, live),
            mutability: Some(spec.mutability),
            persistent: Some(spec.mutability == Mutability::ReadWrite),
            constant: Some(spec.mutability == Mutability::ReadOnly && spec.live.is_none()),
            custom_data: None,
        };
        ReportData {
            component,
            variable: Variable {
                name: spec.variable.to_owned(),
                instance: spec.instance.map(str::to_owned),
                custom_data: None,
            },
            variable_attribute: vec![attribute],
            variable_characteristics: characteristics.then(|| VariableCharacteristics {
                unit: spec.unit.map(str::to_owned),
                data_type: spec.data_type,
                min_limit: spec.min.map(f64::from),
                max_limit: spec.max.map(f64::from),
                values_list: spec
                    .values_list
                    .filter(|_| spec.is_list())
                    .map(str::to_owned),
                supports_monitoring: spec.supports_monitoring,
                custom_data: None,
            }),
            custom_data: None,
        }
    }

    /// Adopts a heartbeat interval the CSMS gave (a boot response).
    pub(super) fn set_heartbeat_interval(&mut self, secs: u32) {
        if secs > 0
            && let Some(spec) = TABLE.iter().find(|s| s.variable == "HeartbeatInterval")
        {
            self.store(spec, secs.to_string());
        }
    }
}

impl Station2Actor {
    /// The text of a live variable.
    pub(super) fn live_value(&self, live: LiveVar) -> String {
        let availability = |operative: bool| {
            if operative {
                "Available"
            } else {
                "Unavailable"
            }
        };
        match live {
            LiveVar::StationAvailability => availability(self.state.availability_operative).into(),
            LiveVar::EvseAvailability(evse) => availability(self.evse_operative(evse)).into(),
            LiveVar::ConnectorAvailability(evse, connector) => self
                .connector_status_text(evse, connector)
                .unwrap_or("Unavailable")
                .into(),
            LiveVar::DateTime => self
                .clock
                .now_utc()
                .to_rfc3339_opts(SecondsFormat::Millis, true),
            LiveVar::EvsePower(evse) => format!("{:.1}", self.evse_power_w(evse)),
            LiveVar::CertificateEntries => "0".into(),
            LiveVar::LocalListEntries => self.local_list_len().to_string(),
        }
    }

    /// Re-applies what a changed variable controls. The password reconnect of A01 is the
    /// `SetVariables` handler's: a local edit does not reconnect.
    pub(super) fn apply_variable_effects(&mut self, spec: &VarSpec) {
        match (spec.component, spec.variable) {
            (OCPP, "HeartbeatInterval") => {
                let secs = self.heartbeat_interval_secs();
                self.state.heartbeat_interval_secs = secs;
                if self.timers.is_armed(&TimerKey2::Heartbeat) {
                    self.timers
                        .set(TimerKey2::Heartbeat, Duration::from_secs(u64::from(secs)));
                }
            }
            (SECURITY, "BasicAuthPassword") => self.mark_secrets_dirty(),
            // Held EVSEs show as reserved only while reservations are enabled.
            (RESERVATION, "Enabled") => self.notify_status_changes(),
            _ => {}
        }
        self.flush_now();
    }

    /// `Command2::SetVariable`.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "signature of the command router"
    )]
    pub(super) fn cmd_set_variable(
        &mut self,
        component: Component,
        variable: Variable,
        value: String,
        reply: Reply2,
    ) {
        let result = self.dm.set_value(&component, &variable, None, &value);
        let answer = match result {
            Ok(applied) => {
                if applied.weak_secret {
                    self.log_weak_password();
                }
                self.apply_variable_effects(applied.spec);
                if is_basic_auth_password(applied.spec) {
                    // A local edit is not known to the CSMS: no reconnect (that is A01, a
                    // SetVariables from the CSMS).
                    self.log(
                        LogLevel::Info,
                        "BasicAuthPassword changed; it applies at the next connect".to_owned(),
                    );
                }
                self.mark_dirty();
                Ok(Outcome2::Done)
            }
            Err(failure) => Err(StationError::InvalidArgument(format!(
                "{}.{}: {failure}",
                component.name, variable.name
            ))),
        };
        // The requester may have gone away; nothing to do about that.
        let _ = reply.send(answer);
    }

    /// Warns about a password below the recommended length, without naming it.
    pub(super) fn log_weak_password(&self) {
        self.log(
            LogLevel::Warn,
            format!("BasicAuthPassword has fewer than {RECOMMENDED_PASSWORD_CHARS} characters"),
        );
    }

    /// The next `NotifyReport` chunk is due.
    pub(super) fn on_report_chunk(&mut self) {
        let size = self.items_per_message(ItemsPer::GetReport);
        let now = self.clock.now_utc();
        let Some(job) = self.dm.report.as_mut() else {
            return;
        };
        if job.in_flight || job.remaining.is_empty() {
            return;
        }
        job.in_flight = true;
        let request = job.take_chunk(size, now);
        self.call::<crate::ocpp::v2::messages::NotifyReport>(
            super::types::Origin2::Triggered,
            &request,
            |actor, result| match result {
                Ok(_) => {
                    let more = actor.dm.report.as_mut().is_some_and(|job| {
                        job.in_flight = false;
                        !job.remaining.is_empty()
                    });
                    if more {
                        actor.timers.set(TimerKey2::ReportChunk, Duration::ZERO);
                    } else {
                        actor.dm.report = None;
                    }
                }
                Err(error) => {
                    actor.dm.report = None;
                    actor.log(
                        LogLevel::Warn,
                        format!("NotifyReport failed, the report is abandoned: {error}"),
                    );
                }
            },
        );
    }

    /// Rebuilds `Snapshot2::variables` from the device model.
    pub(super) fn refresh_variable_views(&mut self) {
        self.dm.security_profile = self.state.security_profile;
        let views = self.dm.views(&|live| self.live_value(live));
        if self.state.variables != views {
            self.state.variables = views;
        }
    }

    /// Adopts a heartbeat interval from the CSMS: the variable and the snapshot follow.
    pub(super) fn adopt_heartbeat_interval(&mut self, secs: u32) {
        self.dm.set_heartbeat_interval(secs);
        self.state.heartbeat_interval_secs = self.heartbeat_interval_secs();
    }
}

impl Station2Actor {
    /// `OCPPCommCtrlr.HeartbeatInterval`.
    pub(super) fn heartbeat_interval_secs(&self) -> u32 {
        self.dm.uint(OCPP, "HeartbeatInterval", None)
    }

    /// `OCPPCommCtrlr.MessageTimeout`.
    pub(super) fn message_timeout(&self) -> Duration {
        Duration::from_secs(u64::from(self.dm.uint(
            OCPP,
            "MessageTimeout",
            Some("Default"),
        )))
    }

    /// `TxCtrlr.TxStartPoint`.
    pub(super) fn tx_start_points(&self) -> Vec<StartStopPoint> {
        self.dm.points("TxStartPoint")
    }

    /// `TxCtrlr.TxStopPoint`.
    pub(super) fn tx_stop_points(&self) -> Vec<StartStopPoint> {
        self.dm.points("TxStopPoint")
    }

    /// `OCPPCommCtrlr.MessageAttempts` of `TransactionEvent`.
    pub(super) fn tx_attempts(&self) -> u32 {
        self.dm
            .uint(OCPP, "MessageAttempts", Some("TransactionEvent"))
    }

    /// `OCPPCommCtrlr.MessageAttemptInterval` of `TransactionEvent`.
    pub(super) fn tx_attempt_interval(&self) -> Duration {
        Duration::from_secs(u64::from(self.dm.uint(
            OCPP,
            "MessageAttemptInterval",
            Some("TransactionEvent"),
        )))
    }

    /// `SampledDataCtrlr.TxUpdatedInterval`, seconds.
    pub(super) fn tx_updated_interval(&self) -> u32 {
        self.dm.uint(SAMPLED, "TxUpdatedInterval", None)
    }

    /// `SampledDataCtrlr.TxStartedMeasurands`.
    pub(super) fn tx_started_measurands(&self) -> Vec<Measurand> {
        self.dm.measurands(SAMPLED, "TxStartedMeasurands")
    }

    /// `SampledDataCtrlr.TxUpdatedMeasurands`.
    pub(super) fn tx_updated_measurands(&self) -> Vec<Measurand> {
        self.dm.measurands(SAMPLED, "TxUpdatedMeasurands")
    }

    /// `SampledDataCtrlr.TxEndedMeasurands`.
    pub(super) fn tx_ended_measurands(&self) -> Vec<Measurand> {
        self.dm.measurands(SAMPLED, "TxEndedMeasurands")
    }

    /// `SampledDataCtrlr.TxEndedInterval`, seconds; 0 means only the values at the start and
    /// the end of a transaction go into the `Ended` event.
    pub(super) fn tx_ended_interval(&self) -> u32 {
        self.dm.uint(SAMPLED, "TxEndedInterval", None)
    }

    /// `AlignedDataCtrlr.TxEndedMeasurands`.
    pub(super) fn aligned_tx_ended_measurands(&self) -> Vec<Measurand> {
        self.dm.measurands(ALIGNED, "TxEndedMeasurands")
    }

    /// `AlignedDataCtrlr.TxEndedInterval`, seconds; 0 is off.
    pub(super) fn aligned_tx_ended_interval(&self) -> u32 {
        self.dm.uint(ALIGNED, "TxEndedInterval", None)
    }

    /// `AlignedDataCtrlr.Measurands`.
    pub(super) fn aligned_measurands(&self) -> Vec<Measurand> {
        self.dm.measurands(ALIGNED, "Measurands")
    }

    /// `AlignedDataCtrlr.Interval`, seconds; 0 is off.
    pub(super) fn aligned_interval(&self) -> u32 {
        self.dm.uint(ALIGNED, "Interval", None)
    }

    /// `OCPPCommCtrlr.OfflineThreshold`.
    pub(super) fn offline_threshold(&self) -> Duration {
        Duration::from_secs(u64::from(self.dm.uint(OCPP, "OfflineThreshold", None)))
    }

    /// `AuthCtrlr.AuthorizeRemoteStart`.
    pub(super) fn authorize_remote_start(&self) -> bool {
        self.dm.flag(AUTH, "AuthorizeRemoteStart")
    }

    /// `AuthCtrlr.LocalAuthorizeOffline`.
    pub(super) fn local_authorize_offline(&self) -> bool {
        self.dm.flag(AUTH, "LocalAuthorizeOffline")
    }

    /// `AuthCtrlr.LocalPreAuthorize`.
    pub(super) fn local_pre_authorize(&self) -> bool {
        self.dm.flag(AUTH, "LocalPreAuthorize")
    }

    /// `AuthCtrlr.MasterPassGroupId`; `None` when empty (the feature is off).
    pub(super) fn master_pass_group_id(&self) -> Option<String> {
        let id = self.dm.station_text(AUTH, "MasterPassGroupId", None);
        (!id.is_empty()).then_some(id)
    }

    /// `AuthCtrlr.OfflineTxForUnknownIdEnabled`.
    pub(super) fn offline_tx_for_unknown_id(&self) -> bool {
        self.dm.flag(AUTH, "OfflineTxForUnknownIdEnabled")
    }

    /// `TxCtrlr.EVConnectionTimeOut`.
    pub(super) fn ev_connection_timeout(&self) -> Duration {
        Duration::from_secs(u64::from(self.dm.uint(TX, "EVConnectionTimeOut", None)))
    }

    /// `TxCtrlr.StopTxOnEVSideDisconnect`.
    pub(super) fn stop_tx_on_ev_side_disconnect(&self) -> bool {
        self.dm.flag(TX, "StopTxOnEVSideDisconnect")
    }

    /// `TxCtrlr.StopTxOnInvalidId`.
    pub(super) fn stop_tx_on_invalid_id(&self) -> bool {
        self.dm.flag(TX, "StopTxOnInvalidId")
    }

    /// `SecurityCtrlr.BasicAuthPassword`.
    pub(super) fn basic_auth_password(&self) -> Option<&str> {
        self.dm.password()
    }

    /// `DeviceDataCtrlr.ItemsPerMessage` of one message type; at least 1.
    pub(super) fn items_per_message(&self, which: ItemsPer) -> usize {
        let instance = match which {
            ItemsPer::GetReport => "GetReport",
            ItemsPer::GetVariables => "GetVariables",
            ItemsPer::SetVariables => "SetVariables",
        };
        usize::try_from(self.dm.uint(DEVICE, "ItemsPerMessage", Some(instance)))
            .unwrap_or(usize::MAX)
            .max(1)
    }

    /// `OCPPCommCtrlr.WebSocketPingInterval`; `None` is off.
    pub(super) fn ws_ping_interval(&self) -> Option<Duration> {
        match self.dm.uint(OCPP, "WebSocketPingInterval", None) {
            0 => None,
            secs => Some(Duration::from_secs(u64::from(secs))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use serde_json::json;
    use tokio::sync::{broadcast, mpsc, oneshot};

    use super::*;
    use crate::clock::ManualClock;
    use crate::ocpp::v2::messages::{
        GetVariableData, GetVariableResult, GetVariableStatus, SetVariableData, SetVariableResult,
        SetVariableStatus,
    };
    use crate::station2::command::Command2;
    use crate::station2::handlers::device_model::{answer_get, answer_set};

    /// Rows with `Required? = yes` in `dm_components_vars.csv` (OCPP 2.0.1 appendices v1.5)
    /// for the blocks the station implements: `(component, variable, instance)`.
    const REQUIRED: &[(&str, &str, &str)] = &[
        ("AlignedDataCtrlr", "Interval", ""),
        ("AlignedDataCtrlr", "Measurands", ""),
        ("AlignedDataCtrlr", "TxEndedInterval", ""),
        ("AlignedDataCtrlr", "TxEndedMeasurands", ""),
        ("AuthCtrlr", "AuthorizeRemoteStart", ""),
        ("AuthCtrlr", "LocalAuthorizeOffline", ""),
        ("AuthCtrlr", "LocalPreAuthorize", ""),
        ("ChargingStation", "AvailabilityState", ""),
        ("ChargingStation", "Available", ""),
        ("ChargingStation", "SupplyPhases", ""),
        ("ClockCtrlr", "DateTime", ""),
        ("ClockCtrlr", "TimeSource", ""),
        ("Connector", "Available", ""),
        ("Connector", "ConnectorType", ""),
        ("Connector", "SupplyPhases", ""),
        ("DeviceDataCtrlr", "BytesPerMessage", "GetReport"),
        ("DeviceDataCtrlr", "BytesPerMessage", "GetVariables"),
        ("DeviceDataCtrlr", "BytesPerMessage", "SetVariables"),
        ("DeviceDataCtrlr", "ItemsPerMessage", "GetReport"),
        ("DeviceDataCtrlr", "ItemsPerMessage", "GetVariables"),
        ("DeviceDataCtrlr", "ItemsPerMessage", "SetVariables"),
        ("EVSE", "AvailabilityState", ""),
        ("EVSE", "Available", ""),
        ("EVSE", "Power", ""),
        ("EVSE", "SupplyPhases", ""),
        ("OCPPCommCtrlr", "FileTransferProtocols", ""),
        ("OCPPCommCtrlr", "MessageTimeout", "Default"),
        (
            "OCPPCommCtrlr",
            "MessageAttemptInterval",
            "TransactionEvent",
        ),
        ("OCPPCommCtrlr", "MessageAttempts", "TransactionEvent"),
        ("OCPPCommCtrlr", "NetworkConfigurationPriority", ""),
        ("OCPPCommCtrlr", "NetworkProfileConnectionAttempts", ""),
        ("OCPPCommCtrlr", "OfflineThreshold", ""),
        ("OCPPCommCtrlr", "ResetRetries", ""),
        ("OCPPCommCtrlr", "UnlockOnEVSideDisconnect", ""),
        ("SampledDataCtrlr", "TxEndedInterval", ""),
        ("SampledDataCtrlr", "TxEndedMeasurands", ""),
        ("SampledDataCtrlr", "TxStartedMeasurands", ""),
        ("SampledDataCtrlr", "TxUpdatedInterval", ""),
        ("SampledDataCtrlr", "TxUpdatedMeasurands", ""),
        ("SecurityCtrlr", "CertificateEntries", ""),
        ("SecurityCtrlr", "OrganizationName", ""),
        ("SecurityCtrlr", "SecurityProfile", ""),
        ("TxCtrlr", "EVConnectionTimeOut", ""),
        ("TxCtrlr", "StopTxOnEVSideDisconnect", ""),
        ("TxCtrlr", "StopTxOnInvalidId", ""),
        ("TxCtrlr", "TxStartPoint", ""),
        ("TxCtrlr", "TxStopPoint", ""),
    ];

    const SECRET: &str = "sentinel-Password-0123456789-xyz";

    fn config(evses: u32) -> StationConfig {
        StationConfig::default()
            .with_connectors(evses)
            .without_persistence()
    }

    fn model() -> DeviceModel {
        DeviceModel::new(&config(2))
    }

    fn comp(name: &str) -> Component {
        Component {
            evse: None,
            name: name.to_owned(),
            instance: None,
            custom_data: None,
        }
    }

    fn evse_comp(name: &str, id: i32, connector: Option<i32>) -> Component {
        Component {
            evse: Some(EvseRef {
                id,
                connector_id: connector,
                custom_data: None,
            }),
            ..comp(name)
        }
    }

    fn var(name: &str, instance: Option<&str>) -> Variable {
        Variable {
            name: name.to_owned(),
            instance: instance.map(str::to_owned),
            custom_data: None,
        }
    }

    fn live(var: LiveVar) -> String {
        match var {
            LiveVar::EvseAvailability(id) => format!("evse{id}"),
            LiveVar::ConnectorAvailability(e, c) => format!("conn{e}.{c}"),
            LiveVar::EvsePower(id) => format!("power{id}"),
            other => format!("{other:?}"),
        }
    }

    fn get(
        dm: &DeviceModel,
        component: Component,
        variable: Variable,
        kind: Option<AttributeType>,
    ) -> GetVariableResult {
        let data = GetVariableData {
            attribute_type: kind,
            component,
            variable,
            custom_data: None,
        };
        answer_get(dm, &data, &live)
    }

    fn set(
        dm: &mut DeviceModel,
        component: Component,
        variable: Variable,
        value: &str,
        kind: Option<AttributeType>,
    ) -> SetVariableResult {
        let data = SetVariableData {
            attribute_type: kind,
            attribute_value: value.to_owned(),
            component,
            variable,
            custom_data: None,
        };
        answer_set(dm, &data).0
    }

    fn set_ok(dm: &mut DeviceModel, c: &str, v: &str, value: &str) {
        let result = set(dm, comp(c), var(v, None), value, None);
        assert_eq!(
            result.attribute_status,
            SetVariableStatus::Accepted,
            "{c}.{v}"
        );
    }

    fn actor() -> Station2Actor {
        let (_commands_tx, commands_rx) = mpsc::channel(1);
        let (events, _) = broadcast::channel(8);
        let clock = Arc::new(ManualClock::new(Utc::now()));
        Station2Actor::new(config(2), clock, commands_rx, events, None).0
    }

    #[test]
    fn table_keys_are_unique() {
        let mut seen = HashSet::new();
        for spec in TABLE {
            assert!(
                seen.insert((
                    spec.scope as u8,
                    spec.component,
                    spec.variable,
                    spec.instance
                )),
                "duplicate row {}.{}",
                spec.component,
                spec.variable
            );
        }
    }

    #[test]
    fn defaults_satisfy_their_own_limits() {
        let mut odd = StationConfig::default().with_connectors(3);
        odd.heartbeat_interval_secs = 1;
        odd.ws_ping_interval_secs = 0;
        odd.call_timeout_ms = 1;
        odd.phases = 3;
        for dm in [model(), DeviceModel::new(&odd)] {
            for spec in TABLE {
                let default = dm.default_value(spec);
                if spec.live.is_some()
                    || spec.mutability == Mutability::WriteOnly
                    || spec.data_type == DataType::DateTime
                {
                    continue;
                }
                let checked = validate(spec, &default);
                if spec.is_list() && default.is_empty() {
                    assert!(
                        spec.empty_ok,
                        "{}.{} default is empty",
                        spec.component, spec.variable
                    );
                    continue;
                }
                assert_eq!(
                    checked.as_deref().ok(),
                    Some(default.as_str()),
                    "{}.{}",
                    spec.component,
                    spec.variable
                );
            }
        }
    }

    #[test]
    fn required_rows_are_present() {
        for (component, variable, instance) in REQUIRED {
            let instance = (!instance.is_empty()).then_some(*instance);
            assert!(
                TABLE.iter().any(|s| s.component == *component
                    && s.variable == *variable
                    && s.instance == instance),
                "missing {component}.{variable} {instance:?}"
            );
        }
    }

    #[test]
    fn every_listed_value_is_known_to_the_model() {
        for spec in TABLE.iter().filter(|s| s.values_list.is_some()) {
            assert!(spec.is_list() || spec.data_type == DataType::OptionList);
        }
        let dm = model();
        for name in ["TxStartPoint", "TxStopPoint"] {
            let allowed = TABLE
                .iter()
                .find(|s| s.variable == name)
                .unwrap()
                .values_list;
            assert_eq!(allowed.unwrap().split(',').count(), 4);
            assert_ne!(dm.points(name).len(), 0);
        }
        for member in MEASURANDS.split(',') {
            assert!(
                Measurand::ALL.iter().any(|m| m.as_str() == member),
                "{member}"
            );
        }
    }

    #[tokio::test]
    async fn accessors_return_the_defaults() {
        let actor = actor();
        assert_eq!(actor.heartbeat_interval_secs(), 30);
        assert_eq!(actor.message_timeout(), Duration::from_secs(30));
        assert_eq!(actor.tx_start_points(), [StartStopPoint::PowerPathClosed]);
        assert_eq!(
            actor.tx_stop_points(),
            [StartStopPoint::EvConnected, StartStopPoint::Authorized]
        );
        assert_eq!(actor.tx_attempts(), 3);
        assert_eq!(actor.tx_attempt_interval(), Duration::from_secs(60));
        assert_eq!(actor.tx_updated_interval(), 30);
        assert_eq!(
            actor.tx_started_measurands(),
            [Measurand::EnergyActiveImportRegister]
        );
        assert_eq!(
            actor.tx_updated_measurands(),
            [
                Measurand::EnergyActiveImportRegister,
                Measurand::PowerActiveImport
            ]
        );
        assert_eq!(
            actor.tx_ended_measurands(),
            [Measurand::EnergyActiveImportRegister]
        );
        assert_eq!(actor.tx_ended_interval(), 0);
        assert_eq!(actor.aligned_tx_ended_interval(), 0);
        assert_eq!(actor.aligned_tx_ended_measurands(), []);
        assert_eq!(actor.aligned_interval(), 0);
        assert_eq!(actor.offline_threshold(), Duration::from_secs(60));
        assert!(actor.authorize_remote_start());
        assert!(actor.local_authorize_offline());
        assert!(!actor.local_pre_authorize());
        assert!(!actor.offline_tx_for_unknown_id());
        assert_eq!(actor.ev_connection_timeout(), Duration::from_secs(60));
        assert!(actor.stop_tx_on_ev_side_disconnect());
        assert!(actor.stop_tx_on_invalid_id());
        assert_eq!(actor.basic_auth_password(), None);
        for which in [
            ItemsPer::GetReport,
            ItemsPer::GetVariables,
            ItemsPer::SetVariables,
        ] {
            assert_eq!(actor.items_per_message(which), 100);
        }
        assert_eq!(actor.ws_ping_interval(), Some(Duration::from_secs(30)));
    }

    #[test]
    fn get_echoes_the_item_and_defaults_the_attribute_type() {
        let dm = model();
        let result = get(
            &dm,
            comp("OCPPCommCtrlr"),
            var("HeartbeatInterval", None),
            None,
        );
        assert_eq!(result.attribute_status, GetVariableStatus::Accepted);
        assert_eq!(result.attribute_type, Some(AttributeType::Actual));
        assert_eq!(result.attribute_value.as_deref(), Some("30"));
        assert_eq!(result.component.name, "OCPPCommCtrlr");
        assert_eq!(result.variable.name, "HeartbeatInterval");
    }

    #[test]
    fn get_reports_unknown_items_without_a_value() {
        let dm = model();
        let cases = [
            (
                comp("Nope"),
                var("X", None),
                None,
                GetVariableStatus::UnknownComponent,
            ),
            (
                comp("OCPPCommCtrlr"),
                var("Nope", None),
                None,
                GetVariableStatus::UnknownVariable,
            ),
            (
                comp("OCPPCommCtrlr"),
                var("HeartbeatInterval", None),
                Some(AttributeType::Target),
                GetVariableStatus::NotSupportedAttributeType,
            ),
            (
                comp("OCPPCommCtrlr"),
                var("HeartbeatInterval", None),
                Some(AttributeType::MaxSet),
                GetVariableStatus::NotSupportedAttributeType,
            ),
        ];
        for (component, variable, kind, status) in cases {
            let result = get(&dm, component, variable, kind);
            assert_eq!(result.attribute_status, status);
            assert_eq!(result.attribute_value, None);
        }
    }

    #[test]
    fn get_matches_instances_exactly() {
        let dm = model();
        let attempts = |instance| {
            get(
                &dm,
                comp("OCPPCommCtrlr"),
                var("MessageAttempts", instance),
                None,
            )
            .attribute_status
        };
        assert_eq!(
            attempts(Some("TransactionEvent")),
            GetVariableStatus::Accepted
        );
        assert_eq!(attempts(None), GetVariableStatus::UnknownVariable);
        assert_eq!(attempts(Some("")), GetVariableStatus::UnknownVariable);
        assert_eq!(attempts(Some("Other")), GetVariableStatus::UnknownVariable);
        let plain = get(
            &dm,
            comp("OCPPCommCtrlr"),
            var("HeartbeatInterval", Some("")),
            None,
        );
        assert_eq!(plain.attribute_status, GetVariableStatus::Accepted);
        let with_instance = Component {
            instance: Some("x".to_owned()),
            ..comp("OCPPCommCtrlr")
        };
        let result = get(&dm, with_instance, var("HeartbeatInterval", None), None);
        assert_eq!(result.attribute_status, GetVariableStatus::UnknownComponent);
    }

    #[test]
    fn get_addresses_evses_and_connectors() {
        let dm = model();
        let status = |component| get(&dm, component, var("AvailabilityState", None), None);
        let evse = status(evse_comp("EVSE", 2, None));
        assert_eq!(evse.attribute_value.as_deref(), Some("evse2"));
        let connector = status(evse_comp("Connector", 1, Some(1)));
        assert_eq!(connector.attribute_value.as_deref(), Some("conn1.1"));
        assert_eq!(
            status(comp("ChargingStation")).attribute_value.as_deref(),
            Some("StationAvailability")
        );
        for component in [
            evse_comp("EVSE", 3, None),
            evse_comp("EVSE", 0, None),
            evse_comp("EVSE", -1, None),
            evse_comp("Connector", 1, Some(2)),
            comp("EVSE"),
            comp("Connector"),
            evse_comp("ChargingStation", 1, None),
        ] {
            assert_eq!(
                status(component).attribute_status,
                GetVariableStatus::UnknownComponent
            );
        }
        let power = get(&dm, evse_comp("EVSE", 1, None), var("Power", None), None);
        assert_eq!(power.attribute_value.as_deref(), Some("power1"));
    }

    #[test]
    fn get_rejects_the_write_only_password() {
        let mut dm = model();
        set_ok(&mut dm, "SecurityCtrlr", "BasicAuthPassword", SECRET);
        let result = get(
            &dm,
            comp("SecurityCtrlr"),
            var("BasicAuthPassword", None),
            None,
        );
        assert_eq!(result.attribute_status, GetVariableStatus::Rejected);
        assert_eq!(result.attribute_value, None);
        assert!(!serde_json::to_string(&result).unwrap().contains(SECRET));
    }

    #[test]
    fn set_accepts_a_valid_value_and_echoes_the_item() {
        let mut dm = model();
        let result = set(
            &mut dm,
            comp("OCPPCommCtrlr"),
            var("HeartbeatInterval", None),
            "45",
            None,
        );
        assert_eq!(result.attribute_status, SetVariableStatus::Accepted);
        assert_eq!(result.attribute_type, Some(AttributeType::Actual));
        assert_eq!(result.variable.name, "HeartbeatInterval");
        let read = get(
            &dm,
            comp("OCPPCommCtrlr"),
            var("HeartbeatInterval", None),
            None,
        );
        assert_eq!(read.attribute_value.as_deref(), Some("45"));
    }

    #[test]
    fn set_reports_unknown_items() {
        let mut dm = model();
        let status = |dm: &mut DeviceModel, c: &str, v: &str, kind| {
            set(dm, comp(c), var(v, None), "1", kind).attribute_status
        };
        assert_eq!(
            status(&mut dm, "Nope", "X", None),
            SetVariableStatus::UnknownComponent
        );
        assert_eq!(
            status(&mut dm, "OCPPCommCtrlr", "Nope", None),
            SetVariableStatus::UnknownVariable
        );
        assert_eq!(
            status(
                &mut dm,
                "OCPPCommCtrlr",
                "ResetRetries",
                Some(AttributeType::MinSet)
            ),
            SetVariableStatus::NotSupportedAttributeType
        );
    }

    #[test]
    fn set_rejects_bad_formats_ranges_and_read_only_variables() {
        let mut dm = model();
        let cases = [
            ("OCPPCommCtrlr", "HeartbeatInterval", "abc"),
            ("OCPPCommCtrlr", "HeartbeatInterval", "1.5"),
            ("OCPPCommCtrlr", "HeartbeatInterval", "0"),
            ("OCPPCommCtrlr", "HeartbeatInterval", ""),
            ("OCPPCommCtrlr", "ResetRetries", "-1"),
            ("OCPPCommCtrlr", "ResetRetries", "1001"),
            ("AuthCtrlr", "LocalPreAuthorize", "yes"),
            ("AuthCtrlr", "LocalPreAuthorize", "TRUE"),
            ("TxCtrlr", "TxStartPoint", "ParkingBayOccupancy"),
            ("TxCtrlr", "TxStartPoint", "DataSigned,Authorized"),
            ("TxCtrlr", "TxStartPoint", ""),
            ("TxCtrlr", "TxStopPoint", "Authorized,Authorized"),
            ("SampledDataCtrlr", "TxUpdatedMeasurands", "Nonsense"),
            ("DeviceDataCtrlr", "ItemsPerMessage", "5"),
            ("OCPPCommCtrlr", "MessageTimeout", "30"),
            ("SecurityCtrlr", "SecurityProfile", "1"),
            ("EVSE", "AllowReset", "true"),
        ];
        for (component, variable, value) in cases {
            let instance = match variable {
                "ItemsPerMessage" => Some("GetReport"),
                "MessageTimeout" => Some("Default"),
                _ => None,
            };
            let result = set(
                &mut dm,
                comp(component),
                var(variable, instance),
                value,
                None,
            );
            // `EVSE` needs an EVSE number, so it is unknown at the station level.
            let expected = if component == "EVSE" {
                SetVariableStatus::UnknownComponent
            } else {
                SetVariableStatus::Rejected
            };
            assert_eq!(
                result.attribute_status, expected,
                "{component}.{variable}={value:?}"
            );
        }
        let read_only = set(
            &mut dm,
            evse_comp("EVSE", 1, None),
            var("AllowReset", None),
            "true",
            None,
        );
        assert_eq!(read_only.attribute_status, SetVariableStatus::Rejected);
        assert_eq!(
            read_only
                .attribute_status_info
                .map(|i| i.reason_code)
                .as_deref(),
            Some("ReadOnly")
        );
        assert!(dm.persist().values.is_empty());
    }

    #[test]
    fn message_timeout_is_read_only_and_keeps_its_value() {
        let mut dm = model();
        let before = dm.uint(OCPP, "MessageTimeout", Some("Default"));
        let result = set(
            &mut dm,
            comp("OCPPCommCtrlr"),
            var("MessageTimeout", Some("Default")),
            &(before + 5).to_string(),
            None,
        );
        assert_eq!(result.attribute_status, SetVariableStatus::Rejected);
        assert_eq!(dm.uint(OCPP, "MessageTimeout", Some("Default")), before);
        assert!(dm.persist().values.is_empty());
    }

    #[test]
    fn set_normalizes_member_lists() {
        let mut dm = model();
        set_ok(
            &mut dm,
            "TxCtrlr",
            "TxStartPoint",
            "Authorized, EVConnected",
        );
        assert_eq!(
            dm.points("TxStartPoint"),
            [StartStopPoint::Authorized, StartStopPoint::EvConnected]
        );
        set_ok(&mut dm, "AlignedDataCtrlr", "Measurands", "");
        assert_eq!(dm.measurands(ALIGNED, "Measurands").len(), 0);
    }

    #[test]
    fn password_length_rules() {
        let mut dm = model();
        let attempt = |dm: &mut DeviceModel, len: usize| {
            dm.set_value(
                &comp("SecurityCtrlr"),
                &var("BasicAuthPassword", None),
                None,
                &"p".repeat(len),
            )
        };
        assert!(attempt(&mut dm, 0).is_err());
        assert!(attempt(&mut dm, 65).is_err());
        assert_eq!(dm.password(), None);
        assert!(attempt(&mut dm, 1).unwrap().weak_secret);
        assert!(attempt(&mut dm, 15).unwrap().weak_secret);
        assert!(!attempt(&mut dm, 16).unwrap().weak_secret);
        assert!(!attempt(&mut dm, 64).unwrap().weak_secret);
        assert_eq!(dm.password().map(str::len), Some(64));
    }

    #[test]
    fn persist_keeps_only_changed_non_secret_values() {
        let mut dm = model();
        assert!(dm.persist().values.is_empty());
        set_ok(&mut dm, "OCPPCommCtrlr", "HeartbeatInterval", "45");
        set_ok(&mut dm, "AuthCtrlr", "LocalPreAuthorize", "true");
        set_ok(&mut dm, "SecurityCtrlr", "BasicAuthPassword", SECRET);
        let persisted = dm.persist();
        assert_eq!(persisted.values.len(), 2);
        assert_eq!(persisted.values["OCPPCommCtrlr.HeartbeatInterval"], "45");
        assert!(!serde_json::to_string(&persisted).unwrap().contains(SECRET));
        set_ok(&mut dm, "OCPPCommCtrlr", "HeartbeatInterval", "30");
        assert_eq!(dm.persist().values.len(), 1);
    }

    #[test]
    fn restore_ignores_unknown_keys_and_invalid_values() {
        let mut dm = model();
        let mut persisted = DeviceModelPersist::default();
        for (key, value) in [
            ("OCPPCommCtrlr.HeartbeatInterval", "45"),
            ("OCPPCommCtrlr.MessageAttempts.TransactionEvent", "5"),
            ("OCPPCommCtrlr.ResetRetries", "bad"),
            ("OCPPCommCtrlr.NoSuchVariable", "1"),
            ("OCPPCommCtrlr.MessageTimeout.Default", "99"),
            ("ClockCtrlr.DateTime", "2020-01-01T00:00:00Z"),
            ("SecurityCtrlr.BasicAuthPassword", SECRET),
            ("EVSE.AllowReset", "true"),
        ] {
            persisted.values.insert(key.to_owned(), value.to_owned());
        }
        dm.restore(&persisted);
        assert_eq!(dm.uint(OCPP, "HeartbeatInterval", None), 45);
        assert_eq!(
            dm.uint(OCPP, "MessageAttempts", Some("TransactionEvent")),
            5
        );
        assert_eq!(dm.uint(OCPP, "ResetRetries", None), 3);
        assert_eq!(
            dm.uint(OCPP, "MessageTimeout", Some("Default")),
            u32::try_from(StationConfig::default().call_timeout().as_secs()).unwrap()
        );
        assert_eq!(dm.password(), None);
        assert_eq!(dm.persist().values.len(), 2);
    }

    fn report_text(dm: &DeviceModel, base: ReportBase) -> Vec<ReportData> {
        dm.report_items(base, &live)
    }

    #[test]
    fn configuration_report_lists_the_settable_variables_with_characteristics() {
        let dm = model();
        let items = report_text(&dm, ReportBase::ConfigurationInventory);
        assert!(items.len() > 1);
        for item in &items {
            let attribute = &item.variable_attribute[0];
            assert_ne!(attribute.mutability, Some(Mutability::ReadOnly));
            assert!(item.variable_characteristics.is_some());
            assert!(item.component.evse.is_none());
        }
        assert!(items.iter().any(|i| i.variable.name == "BasicAuthPassword"));
    }

    #[test]
    fn full_report_covers_every_component_and_attribute() {
        let dm = model();
        let items = report_text(&dm, ReportBase::FullInventory);
        let station = TABLE.iter().filter(|s| s.scope == Scope::Station).count();
        let evse = TABLE.iter().filter(|s| s.scope == Scope::Evse).count();
        let connector = TABLE.iter().filter(|s| s.scope == Scope::Connector).count();
        assert_eq!(items.len(), station + 2 * (evse + connector));
        for item in &items {
            assert_eq!(item.variable_attribute.len(), 1);
            assert_eq!(item.variable_attribute[0].kind, Some(AttributeType::Actual));
            assert!(item.variable_characteristics.is_some());
        }
        let heartbeat = items
            .iter()
            .find(|i| i.variable.name == "HeartbeatInterval")
            .and_then(|i| i.variable_characteristics.as_ref())
            .unwrap();
        assert_eq!(heartbeat.data_type, DataType::Integer);
        assert_eq!(heartbeat.unit.as_deref(), Some("s"));
        assert_eq!(heartbeat.min_limit, Some(1.0));
        let points = items
            .iter()
            .find(|i| i.variable.name == "TxStartPoint")
            .and_then(|i| i.variable_characteristics.as_ref())
            .unwrap();
        assert_eq!(points.values_list.as_deref(), Some(POINTS));
        let connector = items
            .iter()
            .find(|i| {
                i.component.name == "Connector"
                    && i.variable.name == "AvailabilityState"
                    && i.component.evse.as_ref().is_some_and(|e| e.id == 2)
            })
            .unwrap();
        assert_eq!(
            connector.component.evse.as_ref().unwrap().connector_id,
            Some(1)
        );
        assert_eq!(
            connector.variable_attribute[0].value.as_deref(),
            Some("conn2.1")
        );
    }

    #[test]
    fn summary_report_holds_the_availability_states() {
        let dm = model();
        let items = report_text(&dm, ReportBase::SummaryInventory);
        assert_eq!(items.len(), 1 + 2 + 2);
        assert!(items.iter().all(|i| i.variable.name == "AvailabilityState"));
        assert!(items.iter().all(|i| i.variable_characteristics.is_none()));
    }

    #[test]
    fn report_never_carries_the_password() {
        let mut dm = model();
        set_ok(&mut dm, "SecurityCtrlr", "BasicAuthPassword", SECRET);
        for base in [
            ReportBase::ConfigurationInventory,
            ReportBase::FullInventory,
            ReportBase::SummaryInventory,
        ] {
            let items = report_text(&dm, base);
            assert!(!serde_json::to_string(&items).unwrap().contains(SECRET));
        }
        let items = report_text(&dm, ReportBase::FullInventory);
        let password = items
            .iter()
            .find(|i| i.variable.name == "BasicAuthPassword")
            .unwrap();
        assert_eq!(password.variable_attribute[0].value, None);
        assert_eq!(
            password.variable_attribute[0].mutability,
            Some(Mutability::WriteOnly)
        );
    }

    #[test]
    fn chunks_carry_increasing_sequence_numbers_and_tbc() {
        let dm = model();
        let items = report_text(&dm, ReportBase::FullInventory);
        let total = items.len();
        let mut job = ReportJob {
            request_id: 7,
            remaining: VecDeque::from(items),
            seq_no: 0,
            in_flight: false,
        };
        let at = Utc::now();
        let mut seen = 0;
        let mut seq = 0;
        loop {
            let chunk = job.take_chunk(3, at);
            assert_eq!(chunk.request_id, 7);
            assert_eq!(chunk.seq_no, seq);
            let count = chunk.report_data.as_ref().unwrap().len();
            assert!((1..=3).contains(&count));
            seen += count;
            seq += 1;
            if job.remaining.is_empty() {
                assert_eq!(chunk.tbc, None);
                break;
            }
            assert_eq!(chunk.tbc, Some(true));
        }
        assert_eq!(seen, total);
        assert_eq!(seq, i32::try_from(total.div_ceil(3)).unwrap());
    }

    #[test]
    fn debug_output_hides_the_password() {
        let mut dm = model();
        set_ok(&mut dm, "SecurityCtrlr", "BasicAuthPassword", SECRET);
        assert!(!format!("{dm:?}").contains(SECRET));
    }

    #[tokio::test]
    async fn snapshot_views_hide_the_password_and_skip_the_clock() {
        let mut actor = actor();
        set_ok(&mut actor.dm, "SecurityCtrlr", "BasicAuthPassword", SECRET);
        set_ok(&mut actor.dm, "OCPPCommCtrlr", "HeartbeatInterval", "45");
        actor.state.security_profile = 2;
        actor.refresh_variable_views();
        let views = &actor.state.variables;
        assert!(!views.is_empty() && views.len() <= MAX_VIEWS);
        assert!(!serde_json::to_string(views).unwrap().contains(SECRET));
        let find = |name: &str| views.iter().find(|v| v.variable == name).unwrap();
        let password = find("BasicAuthPassword");
        assert_eq!(password.value, None);
        assert_eq!(password.mutability, Mutability::WriteOnly);
        assert_eq!(find("HeartbeatInterval").value.as_deref(), Some("45"));
        assert_eq!(find("SecurityProfile").value.as_deref(), Some("2"));
        assert!(views.iter().all(|v| v.variable != "DateTime"));
        assert!(
            views
                .iter()
                .all(|v| v.component != "EVSE" && v.component != "Connector")
        );
        let before = views.clone();
        actor.refresh_variable_views();
        assert_eq!(actor.state.variables, before);
    }

    #[tokio::test]
    async fn local_set_variable_applies_and_reports_failures() {
        let mut actor = actor();
        let (tx, rx) = oneshot::channel();
        actor.cmd_set_variable(
            comp("OCPPCommCtrlr"),
            var("HeartbeatInterval", None),
            "12".to_owned(),
            tx,
        );
        assert!(matches!(rx.await, Ok(Ok(Outcome2::Done))));
        assert_eq!(actor.heartbeat_interval_secs(), 12);
        assert_eq!(actor.state.heartbeat_interval_secs, 12);

        let (tx, rx) = oneshot::channel();
        actor.cmd_set_variable(
            comp("OCPPCommCtrlr"),
            var("HeartbeatInterval", None),
            "0".to_owned(),
            tx,
        );
        assert!(matches!(
            rx.await,
            Ok(Err(StationError::InvalidArgument(_)))
        ));
        assert_eq!(actor.heartbeat_interval_secs(), 12);

        let (tx, rx) = oneshot::channel();
        actor.cmd_set_variable(
            comp("SecurityCtrlr"),
            var("BasicAuthPassword", None),
            SECRET.to_owned(),
            tx,
        );
        assert!(matches!(rx.await, Ok(Ok(Outcome2::Done))));
        assert_eq!(actor.basic_auth_password(), Some(SECRET));
    }

    #[test]
    fn recording_line_of_a_local_command_is_redacted() {
        let command = Command2::SetVariable {
            component: comp("SecurityCtrlr"),
            variable: var("BasicAuthPassword", None),
            value: SECRET.to_owned(),
        };
        let mut line = serde_json::to_value(&command).unwrap();
        assert!(
            line.to_string().contains(SECRET),
            "sentinel is in the raw command"
        );
        crate::recording::redact_value(&mut line);
        assert!(!line.to_string().contains(SECRET), "leaked: {line}");
        assert_eq!(line["command"], json!("set_variable"));
        // Other variables stay readable in a recording.
        let other = Command2::SetVariable {
            component: comp("OCPPCommCtrlr"),
            variable: var("HeartbeatInterval", None),
            value: "45".to_owned(),
        };
        let mut line = serde_json::to_value(&other).unwrap();
        crate::recording::redact_value(&mut line);
        assert_eq!(line["attributeValue"], json!("45"));
    }

    #[test]
    fn set_variables_frame_is_redacted_by_the_payload_rules() {
        let mut payload = json!({"setVariableData": [{
            "attributeValue": SECRET,
            "component": {"name": "SecurityCtrlr"},
            "variable": {"name": "BasicAuthPassword"}
        }]});
        crate::ocpp::frame::redact_payload(&mut payload);
        assert!(!payload.to_string().contains(SECRET));
    }
}
