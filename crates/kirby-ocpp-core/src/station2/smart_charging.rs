//! Smart charging of the OCPP 2.x station (use case group K).
//!
//! The CSMS shapes the power of an EVSE (or the whole station) with charging profiles:
//! `SetChargingProfile` installs one, `ClearChargingProfile` removes some,
//! `GetChargingProfiles` reports the installed ones and `GetCompositeSchedule` reports the
//! limit that results from all of them. A `RequestStartTransaction` may carry a `TxProfile`
//! (K05). A simulated external system (an energy management system) limits the station with
//! `ChargingStationExternalConstraints` profiles (K11 to K13).
//!
//! # Evaluation
//!
//! The limit math is the one of the 1.6 station ([`crate::station::smart_charging`]). An
//! adapter turns each stored 2.x profile into the 1.6 form and runs that code:
//!
//! * the EVSE id is the connector id (EVSE 0 is the station);
//!   `ChargingStationMaxProfile` is the 1.6 `ChargePointMaxProfile`;
//! * only `chargingSchedule[0]` counts (a request with more schedules is rejected);
//! * `validTo` is exclusive in 2.x (K01.FR.37) and inclusive in 1.6, so the adapter hands
//!   over `validTo` minus one nanosecond;
//! * a `TxProfile` is evaluated only for the transaction it names; the 2.x transaction id is a
//!   string, so the adapter binds every `TxProfile` to one fixed 1.6 id and drops the ones of
//!   other transactions before the call;
//! * `ChargingStationExternalConstraints` profiles are evaluated apart from the rest and cap
//!   the result: at every instant the limit is the lower of both. On EVSE 0 such a profile
//!   acts like a max profile, on another EVSE like a default profile of that EVSE.
//!
//! The composite schedule has the rules of the 1.6 one: a profile in the other rate unit than
//! the one asked for does not take part (there is no conversion between `A` and `W`), while the
//! simulated meter converts both. Where no profile applies, the schedule shows the station's
//! nominal power. `SmartChargingCtrlr.Enabled` false makes every profile inert.
//!
//! # Admission
//!
//! See [`admit`]: the first rule that fails decides the `reasonCode` of the `Rejected` answer.
//! Profiles that the station does not implement are refused with the code OCPP 2.1 names for
//! them: the purposes `PriorityCharging` and `LocalGeneration`, the kind `Dynamic`, any
//! operation mode but `ChargingOnly`, the ISO 15118 and V2X fields.
//!
//! # State
//!
//! A profile is stored as the validated JSON that arrived (`StoredProfile2`), so a
//! `ReportChargingProfiles` repeats it unchanged, together with the parsed copy the
//! evaluation uses. A new profile replaces the stored one with the same id, and the one with
//! the same EVSE, purpose and stack level. A `TxProfile` ends with its transaction. Everything
//! else survives a restart (K01.FR.27).

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::actor::{Station2Actor, TimerKey2};
use super::calls::call_failure;
use super::command::Outcome2;
use super::link::sanitize;
use super::types::{Origin2, Reply2};
use crate::error::StationError;
use crate::ids::{ConnectorId, TxId};
use crate::ocpp::frame::ErrorCode;
use crate::ocpp::messages::{
    ChargingProfile as LegacyProfile, ChargingSchedule as LegacySchedule,
    ChargingSchedulePeriod as LegacyPeriod,
};
use crate::ocpp::responder::Responder;
use crate::ocpp::types::{
    ChargingProfileKindType, ChargingProfilePurposeType, ChargingRateUnitType, RecurrencyKindType,
};
use crate::ocpp::v2::messages::{
    ChargingLimit, ChargingProfile, ChargingProfileKind, ChargingProfilePurpose,
    ChargingProfileStatus, ChargingRateUnit, ChargingSchedule, ChargingSchedulePeriod,
    ClearChargingProfile, ClearChargingProfileRequest, ClearChargingProfileResponse,
    ClearChargingProfileStatus, ClearedChargingLimit, ClearedChargingLimitRequest,
    CompositeSchedule, CompositeScheduleStatus, GetChargingProfileStatus, GetChargingProfiles,
    GetChargingProfilesRequest, GetChargingProfilesResponse, GetCompositeSchedule,
    GetCompositeScheduleRequest, GetCompositeScheduleResponse, NotifyChargingLimit,
    NotifyChargingLimitRequest, RecurrencyKind, ReportChargingProfiles,
    ReportChargingProfilesRequest, SetChargingProfile, SetChargingProfileRequest,
    SetChargingProfileResponse,
};
use crate::ocpp::v2::types::{ChargingState, StatusInfo, WireEnum};
use crate::ocpp::{Protocol, SchemaRegistry};
use crate::station::LogLevel;
use crate::station::charging_profiles::StoredProfile as LegacyStored;
use crate::station::metering::{PowerModel, energy_wh_for};
use crate::station::smart_charging::{
    CompositeError, CompositeRequest, CompositeSchedule as LegacyComposite,
    MAX_COMPOSITE_DURATION_S, MAX_COMPOSITE_PERIODS, PowerRequest, change_instants,
    composite_schedule_full, limit_w_at, schedule_base, shape_error, truncate_to_second,
};

/// The most profiles the station holds (`SmartChargingCtrlr.Entries` maximum).
pub(super) const MAX_PROFILES: i32 = 10;
const MAX_PROFILE_BYTES: usize = 64 * 1024;

/// `chargingLimitSource` of the profiles the CSMS sets.
const CSMS_SOURCE: &str = "CSO";
/// `chargingLimitSource` of the limits of the simulated external system.
const EXTERNAL_SOURCE: &str = "EMS";

/// The 1.6 transaction every `TxProfile` is bound to during evaluation.
const BOUND: TxId = TxId::Local(0);

/// A charging profile as stored and persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredProfile2 {
    /// The EVSE the profile was set for; 0 is the station.
    pub evse: u32,
    /// The `ChargingProfileType` as validated, reported back unchanged.
    pub profile: Value,
    /// The transaction a `TxProfile` belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction_id: Option<String>,
    /// When the profile was installed: the start of a schedule that names none.
    pub installed_at: DateTime<Utc>,
    /// The `chargingLimitSource`: `CSO` for the CSMS, `EMS` for an external limit.
    pub source: String,
}

/// The charging profiles as persisted. `TxProfile`s are left out: the transactions they
/// belong to do not survive a restart.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmartChargingPersist {
    pub profiles: Vec<StoredProfile2>,
    #[serde(default)]
    pub protocol: Option<String>,
}

/// An installed profile with the parsed copies the evaluation reads.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    stored: StoredProfile2,
    typed: ChargingProfile,
    legacy: LegacyStored,
}

impl Entry {
    /// Parses a stored profile; `Err` for one the station cannot evaluate.
    fn new(stored: StoredProfile2) -> Result<Self, String> {
        let typed: ChargingProfile =
            serde_json::from_value(stored.profile.clone()).map_err(|error| error.to_string())?;
        let legacy = legacy_of(&stored, &typed).ok_or("the profile cannot be evaluated")?;
        Ok(Self {
            stored,
            typed,
            legacy,
        })
    }

    fn id(&self) -> i32 {
        self.typed.id
    }

    fn purpose(&self) -> ChargingProfilePurpose {
        self.typed.charging_profile_purpose
    }

    fn is_external(&self) -> bool {
        self.purpose() == ChargingProfilePurpose::ChargingStationExternalConstraints
    }

    /// `validTo` is exclusive (K01.FR.37).
    fn expired(&self, now: DateTime<Utc>) -> bool {
        self.typed.valid_to.is_some_and(|to| to <= now)
    }

    /// The same profile for the transaction `tx_id`, installed at `now`.
    fn bound(mut self, tx_id: &str, now: DateTime<Utc>) -> Self {
        self.stored.transaction_id = Some(tx_id.to_owned());
        self.stored.installed_at = now;
        self.legacy.installed_at = Some(now);
        if let Some(object) = self.stored.profile.as_object_mut() {
            object.insert("transactionId".to_owned(), Value::String(tx_id.to_owned()));
        }
        self.typed.transaction_id = Some(tx_id.to_owned());
        self
    }
}

fn legacy_unit(unit: ChargingRateUnit) -> ChargingRateUnitType {
    match unit {
        ChargingRateUnit::W => ChargingRateUnitType::W,
        ChargingRateUnit::A => ChargingRateUnitType::A,
    }
}

fn wire_unit(unit: ChargingRateUnitType) -> ChargingRateUnit {
    match unit {
        ChargingRateUnitType::W => ChargingRateUnit::W,
        ChargingRateUnitType::A => ChargingRateUnit::A,
    }
}

/// The 1.6 form of a profile (see the module docs), or `None` for one the station cannot
/// evaluate.
fn legacy_of(stored: &StoredProfile2, typed: &ChargingProfile) -> Option<LegacyStored> {
    let schedule = typed.charging_schedule.first()?;
    let (connector, purpose) = match (typed.charging_profile_purpose, stored.evse) {
        (ChargingProfilePurpose::ChargingStationMaxProfile, _)
        | (ChargingProfilePurpose::ChargingStationExternalConstraints, 0) => {
            (0, ChargingProfilePurposeType::ChargePointMaxProfile)
        }
        (
            ChargingProfilePurpose::ChargingStationExternalConstraints
            | ChargingProfilePurpose::TxDefaultProfile,
            evse,
        ) => (evse, ChargingProfilePurposeType::TxDefaultProfile),
        (ChargingProfilePurpose::TxProfile, evse) => (evse, ChargingProfilePurposeType::TxProfile),
        _ => return None,
    };
    let kind = match typed.charging_profile_kind {
        ChargingProfileKind::Absolute => ChargingProfileKindType::Absolute,
        ChargingProfileKind::Recurring => ChargingProfileKindType::Recurring,
        ChargingProfileKind::Relative => ChargingProfileKindType::Relative,
        ChargingProfileKind::Dynamic => return None,
    };
    let periods = schedule
        .charging_schedule_period
        .iter()
        .map(|period| {
            Some(LegacyPeriod {
                start_period: period.start_period,
                limit: period.limit?,
                number_phases: period.number_phases,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(LegacyStored {
        connector_id: ConnectorId::new(connector),
        profile: LegacyProfile {
            charging_profile_id: typed.id,
            transaction_id: None,
            stack_level: typed.stack_level,
            charging_profile_purpose: purpose,
            charging_profile_kind: kind,
            recurrency_kind: typed.recurrency_kind.map(|kind| match kind {
                RecurrencyKind::Daily => RecurrencyKindType::Daily,
                RecurrencyKind::Weekly => RecurrencyKindType::Weekly,
            }),
            valid_from: typed.valid_from,
            valid_to: typed.valid_to.map(|to| to - TimeDelta::nanoseconds(1)),
            charging_schedule: LegacySchedule {
                duration: schedule.duration,
                start_schedule: schedule.start_schedule,
                charging_rate_unit: legacy_unit(schedule.charging_rate_unit),
                charging_schedule_period: periods,
                min_charging_rate: schedule.min_charging_rate,
            },
        },
        bound_tx: (purpose == ChargingProfilePurposeType::TxProfile).then_some(BOUND),
        installed_at: Some(stored.installed_at),
    })
}

// ---- evaluation ----------------------------------------------------------------------

/// The transaction on the EVSE an evaluation is for.
#[derive(Debug, Clone, Copy)]
pub(super) struct TxView<'a> {
    pub id: &'a str,
    pub started_at: DateTime<Utc>,
    pub relative_start: Option<DateTime<Utc>>,
}

/// Evaluates installed profiles with the 1.6 math (see the module docs).
pub(super) struct Eval<'a> {
    pub entries: &'a [Entry],
    pub model: PowerModel,
    pub evse_count: u32,
}

fn min_f64(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

fn min_i32(left: Option<i32>, right: Option<i32>) -> Option<i32> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

fn round_tenth(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

impl Eval<'_> {
    /// The 1.6 form of the profiles that can apply: the regular ones, or the external
    /// constraints. A `TxProfile` stays only when it belongs to the running transaction.
    fn legacy(&self, tx: Option<TxView<'_>>, external: bool) -> Vec<LegacyStored> {
        self.entries
            .iter()
            .filter(|entry| entry.is_external() == external)
            .filter(|entry| {
                entry.purpose() != ChargingProfilePurpose::TxProfile
                    || tx.is_some_and(|tx| entry.stored.transaction_id.as_deref() == Some(tx.id))
            })
            .filter_map(|entry| {
                let mut legacy = entry.legacy.clone();
                if !external
                    && legacy.profile.charging_profile_kind == ChargingProfileKindType::Relative
                    && let Some(tx) = tx
                {
                    // A bound relative schedule has no periods until its anchor is known.
                    legacy.profile.charging_schedule.start_schedule = Some(tx.relative_start?);
                    legacy.profile.charging_profile_kind = ChargingProfileKindType::Absolute;
                }
                Some(legacy)
            })
            .collect()
    }

    fn power_request(&self, evse: u32, tx: Option<TxView<'_>>, charging: u32) -> PowerRequest {
        PowerRequest {
            connector: ConnectorId::new(evse),
            active_tx: tx.map(|_| BOUND),
            tx_started_at: tx.map(|tx| tx.started_at),
            model: self.model,
            charging_connectors: charging.max(1),
        }
    }

    /// The limit in watts at `at`; `None` when no profile applies. `charging` is the number
    /// of EVSEs that draw power: they share the limit of a max profile.
    pub(super) fn limit_w(
        &self,
        evse: u32,
        tx: Option<TxView<'_>>,
        charging: u32,
        at: DateTime<Utc>,
    ) -> Option<f64> {
        let request = self.power_request(evse, tx, charging);
        min_f64(
            limit_w_at(&self.legacy(tx, false), &request, at),
            limit_w_at(&self.legacy(tx, true), &request, at),
        )
    }

    /// The energy in Wh an EVSE draws in `[from, to]`: the integral of the power under the
    /// limit, over the stretches where the limit is constant.
    pub(super) fn energy_wh(
        &self,
        evse: u32,
        tx: Option<TxView<'_>>,
        charging: u32,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> f64 {
        if to <= from {
            return 0.0;
        }
        let request = self.power_request(evse, tx, charging);
        let regular = self.legacy(tx, false);
        let external = self.legacy(tx, true);
        let mut cuts = vec![from, to];
        for entry in regular.iter().chain(&external) {
            let base = schedule_base(
                &entry.profile,
                request.tx_started_at,
                entry.installed_at,
                from,
            );
            cuts.extend(change_instants(&entry.profile, base, from, to));
        }
        cuts.sort_unstable();
        cuts.dedup();
        cuts.windows(2)
            .map(|pair| {
                let span = pair[1] - pair[0];
                let middle = pair[0] + span / 2;
                let limit = min_f64(
                    limit_w_at(&regular, &request, middle),
                    limit_w_at(&external, &request, middle),
                );
                self.model.effective_w(limit) * span.as_seconds_f64() / 3600.0
            })
            .sum()
    }

    /// The composite schedule of an EVSE. With no profile that applies, it is the station's
    /// nominal power (K08, TC_K_39).
    pub(super) fn composite(
        &self,
        evse: u32,
        duration_s: i32,
        unit: Option<ChargingRateUnitType>,
        now: DateTime<Utc>,
        tx: Option<TxView<'_>>,
    ) -> Result<LegacyComposite, CompositeError> {
        let request = |unit| CompositeRequest {
            connector: ConnectorId::new(evse),
            duration_s,
            unit,
            now,
            connector_count: self.evse_count,
            active_tx: tx.map(|_| BOUND),
            tx_started_at: tx.map(|tx| tx.started_at),
            model: self.model,
        };
        let regular = composite_schedule_full(&self.legacy(tx, false), &request(unit));
        let external_unit = unit.or_else(|| {
            regular
                .as_ref()
                .ok()
                .map(|schedule| schedule.charging_schedule.charging_rate_unit)
        });
        let external = composite_schedule_full(&self.legacy(tx, true), &request(external_unit));
        match (regular, external) {
            (
                Err(
                    error @ (CompositeError::UnknownConnector(_)
                    | CompositeError::NonPositiveDuration),
                ),
                _,
            ) => Err(error),
            (Ok(first), Ok(second)) => Ok(merge_min(&first, &second)),
            (Ok(only), Err(_)) | (Err(_), Ok(only)) => Ok(only),
            (Err(_), Err(_)) => Ok(self.station_schedule(evse, duration_s, unit, now)),
        }
    }

    /// A schedule of the station's nominal power for the whole window.
    fn station_schedule(
        &self,
        evse: u32,
        duration_s: i32,
        unit: Option<ChargingRateUnitType>,
        now: DateTime<Utc>,
    ) -> LegacyComposite {
        let unit = unit.unwrap_or(ChargingRateUnitType::W);
        let limit = match unit {
            ChargingRateUnitType::W => self.model.nominal_w,
            ChargingRateUnitType::A => round_tenth(self.model.watts_to_amps(self.model.nominal_w)),
        };
        let start = truncate_to_second(now);
        LegacyComposite {
            connector_id: ConnectorId::new(evse),
            schedule_start: start,
            charging_schedule: LegacySchedule {
                duration: Some(duration_s.min(MAX_COMPOSITE_DURATION_S)),
                start_schedule: Some(start),
                charging_rate_unit: unit,
                charging_schedule_period: vec![LegacyPeriod {
                    start_period: 0,
                    limit,
                    number_phases: None,
                }],
                min_charging_rate: None,
            },
        }
    }
}

/// The period of `schedule` in force `offset` seconds after its start.
fn period_at(schedule: &LegacySchedule, offset: i32) -> Option<&LegacyPeriod> {
    let periods = &schedule.charging_schedule_period;
    let next = periods.partition_point(|period| period.start_period <= offset);
    next.checked_sub(1).and_then(|index| periods.get(index))
}

/// The lower of two composite schedules over the same window (same start and unit).
fn merge_min(first: &LegacyComposite, second: &LegacyComposite) -> LegacyComposite {
    let (a, b) = (&first.charging_schedule, &second.charging_schedule);
    let duration = match (a.duration, b.duration) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (only, None) | (None, only) => only,
    };
    let mut starts: Vec<i32> = a
        .charging_schedule_period
        .iter()
        .chain(&b.charging_schedule_period)
        .map(|period| period.start_period)
        .filter(|start| duration.is_none_or(|duration| *start < duration))
        .collect();
    starts.sort_unstable();
    starts.dedup();
    let mut periods: Vec<LegacyPeriod> = Vec::new();
    for start in starts {
        let (limit, number_phases) = match (period_at(a, start), period_at(b, start)) {
            (Some(x), Some(y)) => (
                x.limit.min(y.limit),
                min_i32(x.number_phases, y.number_phases),
            ),
            (Some(only), None) | (None, Some(only)) => (only.limit, only.number_phases),
            (None, None) => continue,
        };
        if periods.last().is_some_and(|last| {
            last.limit.to_bits() == limit.to_bits() && last.number_phases == number_phases
        }) {
            continue;
        }
        periods.push(LegacyPeriod {
            start_period: start,
            limit,
            number_phases,
        });
    }
    let min_charging_rate = match (a.min_charging_rate, b.min_charging_rate) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (only, None) | (None, only) => only,
    };
    LegacyComposite {
        connector_id: first.connector_id,
        schedule_start: first.schedule_start,
        charging_schedule: LegacySchedule {
            duration,
            start_schedule: first.charging_schedule.start_schedule,
            charging_rate_unit: first.charging_schedule.charging_rate_unit,
            charging_schedule_period: periods,
            min_charging_rate,
        },
    }
}

// ---- admission -----------------------------------------------------------------------

/// Why a profile is refused: the `reasonCode` and a short explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Rejection {
    pub reason: &'static str,
    pub info: String,
}

fn reject(reason: &'static str, info: impl Into<String>) -> Rejection {
    Rejection {
        reason,
        info: info.into(),
    }
}

impl Rejection {
    fn status_info(&self) -> StatusInfo {
        StatusInfo {
            reason_code: self.reason.to_owned(),
            additional_info: Some(self.info.clone()),
            custom_data: None,
        }
    }
}

/// Where a profile comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Origin {
    /// `SetChargingProfile`.
    Csms,
    /// `RequestStartTransaction`: a `TxProfile` of a transaction that does not exist yet.
    RemoteStart,
    /// The simulated external system: a `ChargingStationExternalConstraints` profile.
    External,
}

/// The limits the device model sets.
#[derive(Debug, Clone)]
pub(super) struct Limits {
    pub evse_count: u32,
    pub phases: u8,
    pub max_stack: i32,
    pub max_periods: usize,
    pub max_entries: usize,
    /// `SmartChargingCtrlr.RateUnit`.
    pub units: Vec<String>,
}

/// What the admission of one profile depends on.
pub(super) struct Admit<'a> {
    pub origin: Origin,
    pub limits: &'a Limits,
    /// The transaction running on the EVSE.
    pub active_tx: Option<&'a str>,
}

/// Whether the installed profile `existing` makes way for `new` (it has the same id, or the
/// same EVSE, purpose and stack level).
fn replaces(existing: &Entry, new: &Entry) -> bool {
    existing.id() == new.id()
        || (existing.stored.evse == new.stored.evse
            && existing.purpose() == new.purpose()
            && existing.typed.stack_level == new.typed.stack_level)
}

fn has_room(entries: &[Entry], new: &Entry, max: usize) -> bool {
    entries
        .iter()
        .filter(|existing| !replaces(existing, new))
        .count()
        < max
}

/// Installs `entry`, replacing what it replaces.
fn install(entries: &mut Vec<Entry>, entry: Entry) {
    entries.retain(|existing| !replaces(existing, &entry));
    entries.push(entry);
}

/// Profile fields of OCPP 2.1 the station does not implement. The 2.0.1 schema rejects them
/// before a payload gets here.
fn unsupported_fields(raw: &Value) -> Option<Rejection> {
    let has = |value: &Value, key: &str| value.get(key).is_some();
    if has(raw, "maxOfflineDuration") || has(raw, "invalidAfterOfflineDuration") {
        return Some(reject(
            "UnsupportedParam",
            "maxOfflineDuration is not supported",
        ));
    }
    if has(raw, "dynUpdateInterval") || has(raw, "dynUpdateTime") {
        return Some(reject(
            "InvalidProfile",
            "dynUpdateInterval and dynUpdateTime need a Dynamic profile",
        ));
    }
    let schedule = raw.get("chargingSchedule")?.as_array()?.first()?;
    if has(schedule, "limitAtSoC") {
        return Some(reject("InvalidSchedule", "limitAtSoC is not supported"));
    }
    if schedule.get("useLocalTime").and_then(Value::as_bool) == Some(true) {
        return Some(reject("InvalidSchedule", "useLocalTime is not supported"));
    }
    if schedule
        .get("randomizedDelay")
        .and_then(Value::as_i64)
        .is_some_and(|delay| delay > 0)
    {
        return Some(reject(
            "InvalidSchedule",
            "randomizedDelay is not supported",
        ));
    }
    for period in schedule.get("chargingSchedulePeriod")?.as_array()? {
        if has(period, "limit_L2") || has(period, "limit_L3") {
            return Some(reject(
                "PhaseConflict",
                "limits per phase are not supported",
            ));
        }
        if period.get("evseSleep").and_then(Value::as_bool) == Some(true) {
            return Some(reject("InvalidSchedule", "evseSleep is not supported"));
        }
        if period
            .get("operationMode")
            .and_then(Value::as_str)
            .is_some_and(|mode| mode != "ChargingOnly")
        {
            return Some(reject(
                "InvalidOperationMode",
                "only the operation mode ChargingOnly is supported",
            ));
        }
        for key in [
            "dischargeLimit",
            "dischargeLimit_L2",
            "dischargeLimit_L3",
            "setpoint",
            "setpoint_L2",
            "setpoint_L3",
            "setpointReactive",
            "setpointReactive_L2",
            "setpointReactive_L3",
            "v2xBaseline",
            "v2xFreqWattCurve",
            "v2xSignalWattCurve",
            "preconditioningRequest",
        ] {
            if has(period, key) {
                return Some(reject(
                    "UnsupportedParam",
                    format!("{key} is not supported"),
                ));
            }
        }
    }
    None
}

/// Rules 2 and 3 of [`admit`]: what purpose and kind the origin and the EVSE allow.
fn check_placement(
    purpose: ChargingProfilePurpose,
    kind: ChargingProfileKind,
    evse: u32,
    origin: Origin,
) -> Result<(), Rejection> {
    match (purpose, origin) {
        (ChargingProfilePurpose::PriorityCharging | ChargingProfilePurpose::LocalGeneration, _) => {
            return Err(reject(
                "UnsupportedPurpose",
                format!("{} is not supported", purpose.as_str()),
            ));
        }
        (ChargingProfilePurpose::ChargingStationExternalConstraints, Origin::Csms) => {
            return Err(reject(
                "InvalidProfile",
                "ChargingStationExternalConstraints cannot be set by the CSMS",
            ));
        }
        (ChargingProfilePurpose::TxProfile, Origin::RemoteStart)
        | (ChargingProfilePurpose::ChargingStationExternalConstraints, Origin::External)
        | (_, Origin::Csms) => {}
        (_, Origin::RemoteStart) => {
            return Err(reject(
                "InvalidProfile",
                "a remote start takes a TxProfile only",
            ));
        }
        (_, Origin::External) => {
            return Err(reject("InvalidProfile", "not an external constraint"));
        }
    }
    if kind == ChargingProfileKind::Dynamic {
        return Err(reject(
            "UnsupportedKind",
            "Dynamic profiles are not supported",
        ));
    }
    match purpose {
        ChargingProfilePurpose::ChargingStationMaxProfile => {
            if evse != 0 {
                return Err(reject(
                    "InvalidProfile",
                    "ChargingStationMaxProfile belongs on EVSE 0",
                ));
            }
            if kind == ChargingProfileKind::Relative {
                return Err(reject(
                    "InvalidProfile",
                    "ChargingStationMaxProfile cannot be Relative",
                ));
            }
        }
        ChargingProfilePurpose::TxProfile if evse == 0 => {
            return Err(reject("InvalidProfile", "TxProfile needs an EVSE above 0"));
        }
        _ => {}
    }
    Ok(())
}

/// Rules 5 and 6 of [`admit`] (without the number of schedules): the schedule itself.
fn check_schedule(
    typed: &ChargingProfile,
    schedule: &ChargingSchedule,
    limits: &Limits,
) -> Result<(), Rejection> {
    let kind = typed.charging_profile_kind;
    if typed.stack_level < 0 || typed.stack_level > limits.max_stack {
        return Err(reject(
            "InvalidStackLevel",
            format!("stackLevel must be from 0 to {}", limits.max_stack),
        ));
    }
    if schedule.charging_schedule_period.len() > limits.max_periods {
        return Err(reject(
            "TooManyElements",
            format!("at most {} periods per schedule", limits.max_periods),
        ));
    }
    if schedule
        .charging_schedule_period
        .iter()
        .any(|period| period.limit.is_none())
    {
        return Err(reject("InvalidSchedule", "every period needs a limit"));
    }
    match (kind, schedule.start_schedule.is_some()) {
        (ChargingProfileKind::Absolute | ChargingProfileKind::Recurring, false) => {
            return Err(reject(
                "InvalidSchedule",
                "Absolute and Recurring profiles need startSchedule",
            ));
        }
        (ChargingProfileKind::Relative, true) => {
            return Err(reject(
                "InvalidSchedule",
                "a Relative profile has no startSchedule",
            ));
        }
        _ => {}
    }
    if kind == ChargingProfileKind::Recurring && typed.recurrency_kind.is_none() {
        return Err(reject(
            "InvalidProfile",
            "a Recurring profile needs recurrencyKind",
        ));
    }
    if !limits
        .units
        .iter()
        .any(|unit| unit == schedule.charging_rate_unit.as_str())
    {
        return Err(reject(
            "UnsupportedRateUnit",
            format!("{} is not supported", schedule.charging_rate_unit.as_str()),
        ));
    }
    for period in &schedule.charging_schedule_period {
        if period
            .number_phases
            .is_some_and(|phases| phases < 1 || phases > i32::from(limits.phases))
        {
            return Err(reject(
                "InvalidSchedule",
                format!("the station has {} phase(s)", limits.phases),
            ));
        }
        if period.phase_to_use.is_some() {
            return Err(reject(
                "InvalidSchedule",
                "phaseToUse is not supported (no phase switching)",
            ));
        }
    }
    Ok(())
}

/// Rule 7 of [`admit`]: the transaction a `TxProfile` names.
fn check_transaction(typed: &ChargingProfile, ctx: &Admit<'_>) -> Result<(), Rejection> {
    if typed.charging_profile_purpose != ChargingProfilePurpose::TxProfile {
        return Ok(());
    }
    match (ctx.origin, typed.transaction_id.as_deref()) {
        (Origin::Csms, None) => Err(reject(
            "InvalidProfile",
            "a TxProfile needs a transactionId",
        )),
        (Origin::Csms, Some(named)) if ctx.active_tx != Some(named) => {
            Err(reject("TxNotFound", "no such transaction on the EVSE"))
        }
        (Origin::RemoteStart, Some(_)) => Err(reject(
            "InvalidProfile",
            "the profile of a remote start has no transactionId",
        )),
        _ => Ok(()),
    }
}

/// Checks a profile for `evse` against the rules of K01 and builds the entry to install.
///
/// The rules, in the order they are checked (the first failure decides):
///
/// 1. The profile parses; the EVSE exists (`UnknownEVSE`).
/// 2. The purpose: `PriorityCharging` and `LocalGeneration` are `UnsupportedPurpose`; the
///    CSMS may not set `ChargingStationExternalConstraints` (K01.FR.22); a remote start
///    carries a `TxProfile` only (K05.FR.02). The kind `Dynamic` is `UnsupportedKind`.
/// 3. A max profile belongs on EVSE 0 and is not `Relative` (K01.FR.38); a `TxProfile` on an
///    EVSE above 0 (K01.FR.16).
/// 4. The 2.1 fields the station does not implement ([`unsupported_fields`]).
/// 5. One schedule only (K01.FR.34); stack level within `ProfileStackLevel`; at most
///    `PeriodsPerSchedule` periods (`TooManyElements`); every period has a limit; the first
///    starts at 0 and starts increase (K01.FR.31/35); `Absolute` and `Recurring` need
///    `startSchedule` (K01.FR.40), `Relative` has none (K01.FR.41); `Recurring` needs
///    `recurrencyKind` (K01.FR.60). `InvalidSchedule` unless named otherwise.
/// 6. The rate unit is listed in `SmartChargingCtrlr.RateUnit` (`UnsupportedRateUnit`,
///    K01.FR.26); `numberPhases` is within the station's phases and `phaseToUse` is absent
///    (K01.FR.43/48).
/// 7. A `TxProfile` of the CSMS names the transaction running on the EVSE (`TxNotFound`,
///    K01.FR.09/33); from a remote start it names none.
/// 8. The id is not one of an external constraint (`InvalidProfileId`, K01.FR.05); a
///    `TxDefaultProfile` does not share its stack level with another id on EVSE 0 or on a
///    specific EVSE (`DuplicateProfile`, K01.FR.52/53).
/// 9. Room: at most `Entries` maximum profiles after what the new one replaces.
pub(super) fn admit(
    entries: &[Entry],
    evse: i32,
    raw: &Value,
    ctx: &Admit<'_>,
    now: DateTime<Utc>,
    source: &str,
) -> Result<Entry, Rejection> {
    if raw.to_string().len() > MAX_PROFILE_BYTES {
        return Err(reject("TooLarge", "the charging profile exceeds 64 KiB"));
    }
    let limits = ctx.limits;
    let typed: ChargingProfile = serde_json::from_value(raw.clone())
        .map_err(|error| reject("InvalidProfile", format!("the profile is invalid: {error}")))?;
    let evse = u32::try_from(evse)
        .ok()
        .filter(|evse| *evse <= limits.evse_count)
        .ok_or_else(|| reject("UnknownEVSE", "the EVSE does not exist"))?;
    let purpose = typed.charging_profile_purpose;
    check_placement(purpose, typed.charging_profile_kind, evse, ctx.origin)?;
    if let Some(rejection) = unsupported_fields(raw) {
        return Err(rejection);
    }
    let [schedule] = typed.charging_schedule.as_slice() else {
        return Err(reject(
            "InvalidSchedule",
            "the profile needs exactly one charging schedule",
        ));
    };
    check_schedule(&typed, schedule, limits)?;
    check_transaction(&typed, ctx)?;
    let stored = StoredProfile2 {
        evse,
        profile: raw.clone(),
        transaction_id: (purpose == ChargingProfilePurpose::TxProfile)
            .then(|| typed.transaction_id.clone())
            .flatten(),
        installed_at: now,
        source: source.to_owned(),
    };
    let legacy = legacy_of(&stored, &typed)
        .ok_or_else(|| reject("InvalidProfile", "the profile cannot be evaluated"))?;
    if let Some(error) = shape_error(&legacy.profile) {
        return Err(reject("InvalidSchedule", error.to_string()));
    }
    let entry = Entry {
        stored,
        typed,
        legacy,
    };
    if entries.iter().any(|existing| {
        existing.id() == entry.id() && existing.is_external() != entry.is_external()
    }) {
        return Err(reject(
            "InvalidProfileId",
            if entry.is_external() {
                "the id belongs to a CSMS profile"
            } else {
                "the id belongs to an external constraint"
            },
        ));
    }
    if purpose == ChargingProfilePurpose::TxDefaultProfile
        && entries.iter().any(|existing| {
            existing.purpose() == ChargingProfilePurpose::TxDefaultProfile
                && existing.typed.stack_level == entry.typed.stack_level
                && existing.id() != entry.id()
                && (existing.stored.evse == 0) != (entry.stored.evse == 0)
        })
    {
        return Err(reject(
            "DuplicateProfile",
            "another TxDefaultProfile has this stack level on EVSE 0 or on a specific EVSE",
        ));
    }
    if !has_room(entries, &entry, limits.max_entries) {
        return Err(reject(
            "TooManyElements",
            format!("at most {} profiles", limits.max_entries),
        ));
    }
    Ok(entry)
}

// ---- state ---------------------------------------------------------------------------

/// A `TxProfile` of an accepted remote start that waits for its transaction.
#[derive(Debug)]
struct PendingStart {
    remote_start_id: i32,
    entry: Entry,
}

/// The installed profiles and the reports in progress.
#[derive(Debug, Default)]
pub(crate) struct SmartCharging {
    entries: Vec<Entry>,
    pending: BTreeMap<u32, PendingStart>,
    /// `ReportChargingProfiles` requests still to send, in order.
    reports: VecDeque<ReportChargingProfilesRequest>,
    report_in_flight: bool,
}

impl SmartCharging {
    fn admit(
        &self,
        evse: i32,
        raw: &Value,
        ctx: &Admit<'_>,
        now: DateTime<Utc>,
        source: &str,
    ) -> Result<Entry, Rejection> {
        let entry = admit(&self.entries, evse, raw, ctx, now, source)?;
        if self
            .pending
            .values()
            .any(|pending| pending.entry.id() == entry.id())
        {
            return Err(reject(
                "InvalidProfileId",
                "the id belongs to a pending remote start",
            ));
        }
        let max = ctx.limits.max_entries;
        let available = max.saturating_sub(self.pending.len());
        let has_room = if ctx.origin == Origin::RemoteStart {
            self.entries.len() < available
        } else {
            has_room(&self.entries, &entry, available)
        };
        if !has_room {
            return Err(reject("TooManyElements", format!("at most {max} profiles")));
        }
        Ok(entry)
    }

    /// A report is queued or on its way.
    fn report_busy(&self) -> bool {
        self.report_in_flight || !self.reports.is_empty()
    }
}

fn status_info(reason: &str, info: &str) -> StatusInfo {
    StatusInfo {
        reason_code: reason.to_owned(),
        additional_info: Some(info.to_owned()),
        custom_data: None,
    }
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

impl Station2Actor {
    fn sc_limits(&self) -> Limits {
        Limits {
            evse_count: u32::try_from(self.evses.len()).unwrap_or(u32::MAX),
            phases: self.config.phases.clamp(1, 3),
            max_stack: self.smart_stack_limit(),
            max_periods: self.smart_period_limit(),
            max_entries: usize::try_from(MAX_PROFILES).unwrap_or(usize::MAX),
            units: self.smart_rate_units(),
        }
    }

    /// The evaluation of the installed profiles; inert while smart charging is disabled.
    fn sc_eval(&self) -> Eval<'_> {
        let entries: &[Entry] = if self.smart_charging_enabled() {
            &self.charging.entries
        } else {
            &[]
        };
        Eval {
            entries,
            model: PowerModel::from_config(&self.config),
            evse_count: u32::try_from(self.evses.len()).unwrap_or(u32::MAX),
        }
    }

    fn sc_tx_view(&self, evse: u32) -> Option<TxView<'_>> {
        self.active_tx(evse).map(|tx| TxView {
            id: &tx.id.0,
            started_at: tx.started_at,
            relative_start: if self.protocol() == Protocol::V21 {
                tx.ready_at
            } else {
                Some(tx.started_at)
            },
        })
    }

    /// The power in watts an EVSE may draw now: the nominal power under the limit in force.
    pub(super) fn sc_offered_w(&self, evse: u32) -> f64 {
        if self.active_tx(evse).is_some_and(|tx| {
            tx.limit_reached.is_some()
                || (self.protocol() == Protocol::V21
                    && tx.charging_state == ChargingState::SuspendedEvse)
        }) {
            return 0.0;
        }
        let eval = self.sc_eval();
        let now = self.clock.now_utc();
        eval.model.effective_w(eval.limit_w(
            evse,
            self.sc_tx_view(evse),
            self.charging_evse_count(),
            now,
        ))
    }

    /// What the snapshot shows of the profiles of an EVSE: the power they allow when that is
    /// below the nominal power, and whether an external constraint covers it.
    pub(super) fn sc_view(&self, evse: u32) -> (Option<f64>, bool) {
        let eval = self.sc_eval();
        let offered = self.sc_offered_w(evse);
        let limited = offered < eval.model.nominal_w;
        let external = eval.entries.iter().any(|entry| {
            entry.is_external() && (entry.stored.evse == evse || entry.stored.evse == 0)
        });
        (limited.then_some(offered), external)
    }

    /// Profiles installed.
    pub(super) fn sc_profile_count(&self) -> usize {
        self.charging.entries.len()
    }

    /// The energy in Wh an EVSE draws in `elapsed` from `from`, under the limit in force.
    pub(super) fn sc_energy_wh(&self, evse: u32, from: DateTime<Utc>, elapsed: Duration) -> f64 {
        let eval = self.sc_eval();
        if eval.entries.is_empty() {
            return energy_wh_for(eval.model.nominal_w, elapsed);
        }
        let to = chrono::Duration::from_std(elapsed)
            .ok()
            .and_then(|elapsed| from.checked_add_signed(elapsed))
            .unwrap_or(from);
        eval.energy_wh(
            evse,
            self.sc_tx_view(evse),
            self.charging_evse_count(),
            from,
            to,
        )
    }

    /// `SmartChargingCtrlr.Entries`: the profiles installed and not expired.
    pub(super) fn charging_profile_count(&self) -> usize {
        let now = self.clock.now_utc();
        self.charging
            .entries
            .iter()
            .filter(|entry| !entry.expired(now))
            .count()
    }

    /// Drops the profiles whose `validTo` has passed.
    fn sc_prune(&mut self, now: DateTime<Utc>) {
        let before = self.charging.entries.len();
        self.charging.entries.retain(|entry| !entry.expired(now));
        if self.charging.entries.len() != before {
            self.mark_dirty();
        }
    }

    /// A transaction ended: its `TxProfile`s end with it (K01.FR.04).
    pub(super) fn sc_tx_ended(&mut self, tx_id: &str) {
        self.charging.entries.retain(|entry| {
            !(entry.purpose() == ChargingProfilePurpose::TxProfile
                && entry.stored.transaction_id.as_deref() == Some(tx_id))
        });
    }

    /// A transaction started on `evse`: binds the `TxProfile` its remote start carried (K05).
    pub(super) fn sc_tx_started(&mut self, evse: u32, tx_id: &str, remote_start_id: Option<i32>) {
        if let Some(id) = remote_start_id
            && let Some(pending) = self.sc_take_pending(evse, id)
        {
            self.sc_bind(pending.entry, tx_id);
        }
    }

    fn sc_bind(&mut self, entry: Entry, tx_id: &str) {
        let now = self.clock.now_utc();
        let entry = entry.bound(tx_id, now);
        self.log(
            LogLevel::Info,
            format!("TxProfile {} installed for the remote start", entry.id()),
        );
        install(&mut self.charging.entries, entry);
        // The profile may have replaced a stored one.
        self.mark_dirty();
    }

    fn sc_take_pending(&mut self, evse: u32, remote_start_id: i32) -> Option<PendingStart> {
        if self
            .charging
            .pending
            .get(&evse)
            .is_some_and(|pending| pending.remote_start_id == remote_start_id)
        {
            self.charging.pending.remove(&evse)
        } else {
            None
        }
    }

    /// Releases the profile reservation of a remote start that will not use it.
    pub(super) fn sc_forget_pending(&mut self, evse: u32, remote_start_id: i32) {
        self.sc_take_pending(evse, remote_start_id);
    }

    /// The profile of a `RequestStartTransaction` (K05). `Ok` when it is accepted or ignored
    /// (smart charging is disabled, K05.FR.04); the profile is installed when the transaction
    /// starts. `Err` carries the answer to send (K05.FR.05).
    pub(super) fn sc_remote_start_profile(
        &mut self,
        evse: u32,
        raw: &Value,
        remote_start_id: i32,
    ) -> Result<(), StatusInfo> {
        if !self.smart_charging_enabled() {
            self.log(
                LogLevel::Info,
                "RequestStartTransaction: the charging profile is ignored (smart charging is disabled)"
                    .to_owned(),
            );
            return Ok(());
        }
        let now = self.clock.now_utc();
        self.sc_prune(now);
        let limits = self.sc_limits();
        let ctx = Admit {
            origin: Origin::RemoteStart,
            limits: &limits,
            active_tx: None,
        };
        let entry = self
            .charging
            .admit(
                i32::try_from(evse).unwrap_or(i32::MAX),
                raw,
                &ctx,
                now,
                CSMS_SOURCE,
            )
            .map_err(|rejection| rejection.status_info())?;
        if self.charging.pending.contains_key(&evse) {
            return Err(status_info(
                "Occupied",
                "the EVSE has a pending remote start",
            ));
        }
        self.charging.pending.insert(
            evse,
            PendingStart {
                remote_start_id,
                entry,
            },
        );
        Ok(())
    }

    // ---- persistence ------------------------------------------------------------------

    /// The profiles for the state file.
    pub(super) fn sc_persist(&self) -> SmartChargingPersist {
        SmartChargingPersist {
            protocol: Some(self.protocol().token().to_owned()),
            profiles: self
                .charging
                .entries
                .iter()
                .filter(|entry| entry.purpose() != ChargingProfilePurpose::TxProfile)
                .map(|entry| entry.stored.clone())
                .collect(),
        }
    }

    /// Takes back the persisted profiles at start: the ones that parse, for an EVSE the
    /// station has, with ids that are unique, up to the limit. A `TxProfile` is dropped: its
    /// transaction ended with the restart.
    pub(super) fn sc_restore(&mut self, persisted: SmartChargingPersist) {
        let protocol = self.protocol().token();
        if let Some(stored) = persisted
            .protocol
            .as_deref()
            .filter(|stored| *stored != protocol)
        {
            self.log(
                LogLevel::Warn,
                format!(
                    "charging profiles stored by {} are not restored on {protocol}",
                    sanitize(stored)
                ),
            );
            return;
        }
        let evse_count = u32::try_from(self.evses.len()).unwrap_or(u32::MAX);
        let max = usize::try_from(MAX_PROFILES).unwrap_or(usize::MAX);
        let now = self.clock.now_utc();
        let mut entries: Vec<Entry> = Vec::new();
        for stored in persisted.profiles {
            if stored.evse > evse_count || entries.len() >= max {
                continue;
            }
            let Ok(entry) = Entry::new(stored) else {
                continue;
            };
            if entry.purpose() == ChargingProfilePurpose::TxProfile
                || entry.expired(now)
                || entries.iter().any(|known| known.id() == entry.id())
            {
                continue;
            }
            entries.push(entry);
        }
        self.charging.entries = entries;
    }

    // ---- reports ----------------------------------------------------------------------

    /// Sends the next queued `ReportChargingProfiles`.
    pub(super) fn on_profile_report_timer(&mut self) {
        if self.charging.report_in_flight {
            return;
        }
        let Some(request) = self.charging.reports.pop_front() else {
            return;
        };
        self.charging.report_in_flight = true;
        self.call::<ReportChargingProfiles>(Origin2::Triggered, &request, |actor, result| {
            actor.charging.report_in_flight = false;
            match result {
                Ok(_) => {
                    if !actor.charging.reports.is_empty() {
                        actor.timers.set(TimerKey2::ProfileReport, Duration::ZERO);
                    }
                }
                Err(error) => {
                    actor.charging.reports.clear();
                    actor.log(
                        LogLevel::Warn,
                        format!("ReportChargingProfiles failed, the report is abandoned: {error}"),
                    );
                }
            }
        });
    }

    // ---- external limits (K11 to K13) -------------------------------------------------

    /// `Command2::SetExternalLimit`: a limit of the external system for `evse` (0 is the
    /// whole station). `value` is a `ChargingScheduleType`, or a whole `ChargingProfileType`
    /// whose purpose is set to `ChargingStationExternalConstraints`. The limit is in force at
    /// once and the CSMS is told with `NotifyChargingLimit`; the command completes with the
    /// answer.
    pub(super) fn cmd_set_external_limit(&mut self, evse: u32, value: &Value, reply: Reply2) {
        match self.install_external_limit(evse, value) {
            Ok(schedules) => {
                let request = NotifyChargingLimitRequest {
                    charging_schedule: self.smart_notify_with_schedules().then_some(schedules),
                    evse_id: (evse > 0).then(|| i32::try_from(evse).unwrap_or(i32::MAX)),
                    charging_limit: ChargingLimit {
                        charging_limit_source: EXTERNAL_SOURCE.to_owned(),
                        is_grid_critical: None,
                        custom_data: None,
                    },
                    custom_data: None,
                };
                self.call::<NotifyChargingLimit>(Origin2::Station, &request, move |_, result| {
                    let answer = result
                        .map(|response| Outcome2::Response {
                            payload: serde_json::to_value(&response).unwrap_or(Value::Null),
                        })
                        .map_err(call_failure);
                    let _ = reply.send(answer);
                });
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    /// Builds, checks and installs the external profile; returns its schedules.
    fn install_external_limit(
        &mut self,
        evse: u32,
        value: &Value,
    ) -> Result<Vec<Value>, StationError> {
        if !self.smart_charging_enabled() || !self.smart_external_signals() {
            return Err(StationError::InvalidState {
                connector: None,
                reason: "external limits are off (SmartChargingCtrlr.Enabled or \
                         ExternalControlSignalsEnabled is false)"
                    .to_owned(),
            });
        }
        let now = self.clock.now_utc();
        self.sc_prune(now);
        let raw = self.external_profile(value, now)?;
        // What is stored is reported and notified as it is: it must be a valid profile.
        let schemas = SchemaRegistry::for_protocol(self.protocol()).map_err(|error| {
            StationError::InvalidState {
                connector: None,
                reason: error.to_string(),
            }
        })?;
        schemas
            .validate_request(
                "SetChargingProfile",
                &json!({"evseId": evse, "chargingProfile": raw}),
            )
            .map_err(|error| {
                StationError::InvalidArgument(format!(
                    "the external limit is not a valid charging profile: {error}"
                ))
            })?;
        let limits = self.sc_limits();
        let ctx = Admit {
            origin: Origin::External,
            limits: &limits,
            active_tx: None,
        };
        let entry = self
            .charging
            .admit(
                i32::try_from(evse).unwrap_or(i32::MAX),
                &raw,
                &ctx,
                now,
                EXTERNAL_SOURCE,
            )
            .map_err(|rejection| {
                StationError::InvalidArgument(format!(
                    "the external limit is refused: {} ({})",
                    rejection.reason, rejection.info
                ))
            })?;
        let schedules = raw
            .get("chargingSchedule")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        self.log(
            LogLevel::Info,
            format!("external limit {} set on EVSE {evse}", entry.id()),
        );
        install(&mut self.charging.entries, entry);
        self.mark_dirty();
        Ok(schedules)
    }

    /// The `ChargingProfileType` of an external limit given as a schedule or as a profile.
    fn external_profile(&self, value: &Value, now: DateTime<Utc>) -> Result<Value, StationError> {
        let mut profile = if value.get("chargingSchedule").is_some() {
            value.clone()
        } else if value.get("chargingSchedulePeriod").is_some() {
            json!({"chargingSchedule": [value]})
        } else {
            return Err(StationError::InvalidArgument(
                "an external limit is a chargingSchedule or a charging profile".to_owned(),
            ));
        };
        let Some(object) = profile.as_object_mut() else {
            return Err(StationError::InvalidArgument(
                "the profile must be a JSON object".to_owned(),
            ));
        };
        object.insert(
            "chargingProfilePurpose".to_owned(),
            json!(ChargingProfilePurpose::ChargingStationExternalConstraints.as_str()),
        );
        if let serde_json::map::Entry::Vacant(id) = object.entry("id") {
            let entries = &self.charging.entries;
            let pending = &self.charging.pending;
            let used = |id| {
                entries.iter().any(|entry| entry.id() == id)
                    || pending.values().any(|start| start.entry.id() == id)
            };
            let lowest = entries
                .iter()
                .filter(|entry| entry.is_external())
                .map(Entry::id)
                .min()
                .unwrap_or(0)
                .min(0);
            let next = (i32::MIN..lowest)
                .rev()
                .chain((lowest..0).rev())
                .find(|id| !used(*id))
                .ok_or_else(|| {
                    StationError::InvalidArgument("no external profile id is available".to_owned())
                })?;
            id.insert(json!(next));
        }
        object.entry("stackLevel").or_insert_with(|| json!(0));
        object
            .entry("chargingProfileKind")
            .or_insert_with(|| json!(ChargingProfileKind::Absolute.as_str()));
        let relative = object.get("chargingProfileKind").and_then(Value::as_str)
            == Some(ChargingProfileKind::Relative.as_str());
        if let Some(schedules) = object
            .get_mut("chargingSchedule")
            .and_then(Value::as_array_mut)
        {
            for schedule in schedules {
                if let Some(schedule) = schedule.as_object_mut() {
                    schedule.entry("id").or_insert_with(|| json!(1));
                    if !relative {
                        schedule
                            .entry("startSchedule")
                            .or_insert_with(|| json!(stamp(truncate_to_second(now))));
                    }
                }
            }
        }
        Ok(profile)
    }

    /// `Command2::ClearExternalLimit`: the external limits of `evse` end; the CSMS is told
    /// with `ClearedChargingLimit`.
    pub(super) fn cmd_clear_external_limit(&mut self, evse: u32, reply: Reply2) {
        let before = self.charging.entries.len();
        self.charging
            .entries
            .retain(|entry| !(entry.is_external() && entry.stored.evse == evse));
        if self.charging.entries.len() == before {
            let _ = reply.send(Err(StationError::InvalidState {
                connector: None,
                reason: format!("no external limit on EVSE {evse}"),
            }));
            return;
        }
        self.mark_dirty();
        self.log(
            LogLevel::Info,
            format!("external limit on EVSE {evse} cleared"),
        );
        let request = ClearedChargingLimitRequest {
            charging_limit_source: EXTERNAL_SOURCE.to_owned(),
            evse_id: (evse > 0).then(|| i32::try_from(evse).unwrap_or(i32::MAX)),
            custom_data: None,
        };
        self.call::<ClearedChargingLimit>(Origin2::Station, &request, move |_, result| {
            let answer = result
                .map(|response| Outcome2::Response {
                    payload: serde_json::to_value(&response).unwrap_or(Value::Null),
                })
                .map_err(call_failure);
            let _ = reply.send(answer);
        });
    }
}

// ---- CALL handlers -------------------------------------------------------------------

/// Answers a CSMS request with `NotSupported` while smart charging is off (K01.FR.29).
fn refuse_when_disabled<A: crate::ocpp::action::Action>(
    actor: &Station2Actor,
    responder: Responder<A>,
) -> Option<Responder<A>> {
    if actor.smart_charging_enabled() {
        return Some(responder);
    }
    actor.respond_error(
        responder,
        ErrorCode::NotSupported,
        "smart charging is disabled (SmartChargingCtrlr.Enabled)",
    );
    None
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "handlers share the signature `fn(&mut Station2Actor, Responder<A>, A::Req)`"
)]
pub(super) fn set_charging_profile(
    actor: &mut Station2Actor,
    responder: Responder<SetChargingProfile>,
    request: SetChargingProfileRequest,
) {
    let Some(responder) = refuse_when_disabled(actor, responder) else {
        return;
    };
    let now = actor.clock.now_utc();
    actor.sc_prune(now);
    let limits = actor.sc_limits();
    let evse = u32::try_from(request.evse_id).ok();
    let active_tx = evse
        .and_then(|evse| actor.active_tx(evse))
        .map(|tx| tx.id.0.clone());
    let ctx = Admit {
        origin: Origin::Csms,
        limits: &limits,
        active_tx: active_tx.as_deref(),
    };
    let outcome = actor.charging.admit(
        request.evse_id,
        &request.charging_profile,
        &ctx,
        now,
        CSMS_SOURCE,
    );
    let response = match outcome {
        Ok(entry) => {
            actor.log(
                LogLevel::Info,
                format!(
                    "SetChargingProfile {} on EVSE {}: Accepted",
                    entry.id(),
                    request.evse_id
                ),
            );
            install(&mut actor.charging.entries, entry);
            actor.mark_dirty();
            SetChargingProfileResponse {
                status: ChargingProfileStatus::Accepted,
                status_info: None,
                custom_data: None,
            }
        }
        Err(rejection) => {
            actor.log(
                LogLevel::Info,
                format!(
                    "SetChargingProfile on EVSE {} rejected: {}",
                    request.evse_id, rejection.reason
                ),
            );
            SetChargingProfileResponse {
                status: ChargingProfileStatus::Rejected,
                status_info: Some(rejection.status_info()),
                custom_data: None,
            }
        }
    };
    actor.respond(responder, response);
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "handlers share the signature `fn(&mut Station2Actor, Responder<A>, A::Req)`"
)]
pub(super) fn clear_charging_profile(
    actor: &mut Station2Actor,
    responder: Responder<ClearChargingProfile>,
    request: ClearChargingProfileRequest,
) {
    let Some(responder) = refuse_when_disabled(actor, responder) else {
        return;
    };
    let now = actor.clock.now_utc();
    actor.sc_prune(now);
    let criteria = request.charging_profile_criteria.as_ref();
    let wanted_evse = criteria.and_then(|criteria| criteria.evse_id);
    let wanted_purpose = criteria.and_then(|criteria| criteria.charging_profile_purpose);
    let wanted_stack = criteria.and_then(|criteria| criteria.stack_level);
    let before = actor.charging.entries.len();
    // External constraints are the external system's: a clear never touches them
    // (K10.FR.04, FR.08, FR.09). A request without a selection clears every other profile.
    actor.charging.entries.retain(|entry| {
        let matches = !entry.is_external()
            && request
                .charging_profile_id
                .is_none_or(|id| entry.id() == id)
            && wanted_evse.is_none_or(|evse| u32::try_from(evse).ok() == Some(entry.stored.evse))
            && wanted_purpose.is_none_or(|purpose| entry.purpose() == purpose)
            && wanted_stack.is_none_or(|stack| entry.typed.stack_level == stack);
        !matches
    });
    let removed = before - actor.charging.entries.len();
    let response = if removed > 0 {
        actor.mark_dirty();
        actor.log(
            LogLevel::Info,
            format!("ClearChargingProfile: {removed} profile(s) cleared"),
        );
        ClearChargingProfileResponse {
            status: ClearChargingProfileStatus::Accepted,
            status_info: None,
            custom_data: None,
        }
    } else {
        ClearChargingProfileResponse {
            status: ClearChargingProfileStatus::Unknown,
            status_info: Some(status_info("NotFound", "no profile matches")),
            custom_data: None,
        }
    };
    actor.respond(responder, response);
}

/// The `ReportChargingProfiles` messages that answer `request`: the profiles that match all
/// of its criteria, grouped by EVSE and limit source in that order, `tbc` on all but the last.
fn report_requests(
    entries: &[Entry],
    request: &GetChargingProfilesRequest,
) -> Vec<ReportChargingProfilesRequest> {
    let criterion = &request.charging_profile;
    let mut groups: BTreeMap<(u32, String), Vec<Value>> = BTreeMap::new();
    for entry in entries {
        let matches = request
            .evse_id
            .is_none_or(|evse| u32::try_from(evse).ok() == Some(entry.stored.evse))
            && criterion
                .charging_profile_purpose
                .is_none_or(|purpose| entry.purpose() == purpose)
            && criterion
                .stack_level
                .is_none_or(|stack| entry.typed.stack_level == stack)
            && criterion
                .charging_profile_id
                .as_ref()
                .is_none_or(|ids| ids.contains(&entry.id()))
            && criterion
                .charging_limit_source
                .as_ref()
                .is_none_or(|sources| sources.contains(&entry.stored.source));
        if matches {
            groups
                .entry((entry.stored.evse, entry.stored.source.clone()))
                .or_default()
                .push(entry.stored.profile.clone());
        }
    }
    let total = groups.len();
    groups
        .into_iter()
        .enumerate()
        .map(
            |(index, ((evse, source), profiles))| ReportChargingProfilesRequest {
                request_id: request.request_id,
                charging_limit_source: source,
                charging_profile: profiles,
                tbc: (index + 1 < total).then_some(true),
                evse_id: i32::try_from(evse).unwrap_or(i32::MAX),
                custom_data: None,
            },
        )
        .collect()
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "handlers share the signature `fn(&mut Station2Actor, Responder<A>, A::Req)`"
)]
pub(super) fn get_charging_profiles(
    actor: &mut Station2Actor,
    responder: Responder<GetChargingProfiles>,
    request: GetChargingProfilesRequest,
) {
    let Some(responder) = refuse_when_disabled(actor, responder) else {
        return;
    };
    let now = actor.clock.now_utc();
    actor.sc_prune(now);
    // One report at a time keeps the queue as short as the profiles of one request.
    if actor.charging.report_busy() {
        actor.respond(
            responder,
            GetChargingProfilesResponse {
                status: GetChargingProfileStatus::NoProfiles,
                status_info: Some(status_info(
                    "Unspecified",
                    "an earlier report is still being sent",
                )),
                custom_data: None,
            },
        );
        return;
    }
    let reports = report_requests(&actor.charging.entries, &request);
    if reports.is_empty() {
        actor.respond(
            responder,
            GetChargingProfilesResponse {
                status: GetChargingProfileStatus::NoProfiles,
                status_info: Some(status_info("NotFound", "no profile matches")),
                custom_data: None,
            },
        );
        return;
    }
    actor.charging.reports.extend(reports);
    actor.respond(
        responder,
        GetChargingProfilesResponse {
            status: GetChargingProfileStatus::Accepted,
            status_info: None,
            custom_data: None,
        },
    );
    // The reports follow the answer.
    actor.timers.set(TimerKey2::ProfileReport, Duration::ZERO);
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "handlers share the signature `fn(&mut Station2Actor, Responder<A>, A::Req)`"
)]
pub(super) fn get_composite_schedule(
    actor: &mut Station2Actor,
    responder: Responder<GetCompositeSchedule>,
    request: GetCompositeScheduleRequest,
) {
    let Some(responder) = refuse_when_disabled(actor, responder) else {
        return;
    };
    let reject_with = |actor: &Station2Actor, responder, reason: &str, info: &str| {
        actor.respond(
            responder,
            GetCompositeScheduleResponse {
                status: CompositeScheduleStatus::Rejected,
                status_info: Some(status_info(reason, info)),
                schedule: None,
                custom_data: None,
            },
        );
    };
    let now = actor.clock.now_utc();
    actor.sc_prune(now);
    let evse = u32::try_from(request.evse_id)
        .ok()
        .filter(|evse| *evse as usize <= actor.evses.len());
    let Some(evse) = evse else {
        reject_with(actor, responder, "UnknownEVSE", "the EVSE does not exist");
        return;
    };
    if request.duration <= 0 {
        reject_with(
            actor,
            responder,
            "InvalidValue",
            "the duration must be positive",
        );
        return;
    }
    if request.duration > MAX_COMPOSITE_DURATION_S {
        reject_with(
            actor,
            responder,
            "UnsupportedRequest",
            "the requested duration exceeds the composite schedule limit",
        );
        return;
    }
    if request.charging_rate_unit.is_some_and(|unit| {
        !actor
            .smart_rate_units()
            .iter()
            .any(|allowed| allowed == unit.as_str())
    }) {
        reject_with(
            actor,
            responder,
            "UnsupportedRateUnit",
            "the rate unit is not supported",
        );
        return;
    }
    let composite = actor.sc_eval().composite(
        evse,
        request.duration,
        request.charging_rate_unit.map(legacy_unit),
        now,
        actor.sc_tx_view(evse),
    );
    let Ok(composite) = composite else {
        reject_with(
            actor,
            responder,
            "InvalidValue",
            "no schedule for the request",
        );
        return;
    };
    if composite.charging_schedule.charging_schedule_period.len() > MAX_COMPOSITE_PERIODS {
        reject_with(
            actor,
            responder,
            "UnsupportedRequest",
            "the composite schedule has too many periods",
        );
        return;
    }
    let response = GetCompositeScheduleResponse {
        status: CompositeScheduleStatus::Accepted,
        status_info: None,
        schedule: Some(wire_composite(composite, &request)),
        custom_data: None,
    };
    actor.respond(responder, response);
}

fn wire_composite(
    composite: LegacyComposite,
    request: &GetCompositeScheduleRequest,
) -> CompositeSchedule {
    let schedule = composite.charging_schedule;
    CompositeSchedule {
        charging_schedule_period: schedule
            .charging_schedule_period
            .into_iter()
            .map(|period| ChargingSchedulePeriod {
                start_period: period.start_period,
                limit: Some(period.limit),
                number_phases: period.number_phases,
                phase_to_use: None,
                operation_mode: None,
                custom_data: None,
            })
            .collect(),
        evse_id: request.evse_id,
        duration: schedule.duration.unwrap_or(request.duration),
        schedule_start: composite.schedule_start,
        charging_rate_unit: wire_unit(schedule.charging_rate_unit),
        custom_data: None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::TimeZone;
    use tokio::sync::{broadcast, mpsc};

    use super::*;
    use crate::clock::ManualClock;
    use crate::config::StationConfig;
    use crate::ocpp::Protocol;
    use crate::ocpp::v2::messages::{ChargingProfileCriterion, TriggerReason};
    use crate::ocpp::v2::types::{Component, Variable};
    use crate::station2::transactions::StartSpec;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0)
            .single()
            .expect("a valid time")
    }

    fn at(seconds: i64) -> DateTime<Utc> {
        t0() + TimeDelta::seconds(seconds)
    }

    fn limits() -> Limits {
        Limits {
            evse_count: 2,
            phases: 3,
            max_stack: 10,
            max_periods: 24,
            max_entries: usize::try_from(MAX_PROFILES).expect("positive"),
            units: vec!["A".to_owned(), "W".to_owned()],
        }
    }

    fn model() -> PowerModel {
        PowerModel {
            nominal_w: 11_000.0,
            voltage_v: 230.0,
            phases: 3,
        }
    }

    fn eval(entries: &[Entry]) -> Eval<'_> {
        Eval {
            entries,
            model: model(),
            evse_count: 2,
        }
    }

    /// An absolute profile that starts at `t0` with the given `(startPeriod, limit)` periods.
    fn profile(id: i32, stack: i32, purpose: &str, unit: &str, periods: &[(i32, f64)]) -> Value {
        json!({
            "id": id,
            "stackLevel": stack,
            "chargingProfilePurpose": purpose,
            "chargingProfileKind": "Absolute",
            "chargingSchedule": [{
                "id": 1,
                "startSchedule": stamp(t0()),
                "chargingRateUnit": unit,
                "chargingSchedulePeriod": periods
                    .iter()
                    .map(|(start, limit)| json!({"startPeriod": start, "limit": limit}))
                    .collect::<Vec<_>>(),
            }],
        })
    }

    fn with(mut raw: Value, key: &str, value: Value) -> Value {
        raw[key] = value;
        raw
    }

    fn with_schedule(mut raw: Value, key: &str, value: Value) -> Value {
        raw["chargingSchedule"][0][key] = value;
        raw
    }

    fn with_period(mut raw: Value, key: &str, value: Value) -> Value {
        raw["chargingSchedule"][0]["chargingSchedulePeriod"][0][key] = value;
        raw
    }

    fn tx_profile(id: i32, tx: &str, limit: f64) -> Value {
        with(
            profile(id, 0, "TxProfile", "A", &[(0, limit)]),
            "transactionId",
            json!(tx),
        )
    }

    fn put_with(
        entries: &mut Vec<Entry>,
        evse: i32,
        raw: &Value,
        origin: Origin,
        tx: Option<&str>,
        limits: &Limits,
    ) -> Result<(), Rejection> {
        let ctx = Admit {
            origin,
            limits,
            active_tx: tx,
        };
        let source = if origin == Origin::External {
            EXTERNAL_SOURCE
        } else {
            CSMS_SOURCE
        };
        let entry = admit(entries, evse, raw, &ctx, t0(), source)?;
        install(entries, entry);
        Ok(())
    }

    fn put(entries: &mut Vec<Entry>, evse: i32, raw: &Value) -> Result<(), Rejection> {
        put_with(entries, evse, raw, Origin::Csms, None, &limits())
    }

    fn put_tx(entries: &mut Vec<Entry>, evse: i32, raw: &Value, tx: &str) -> Result<(), Rejection> {
        put_with(entries, evse, raw, Origin::Csms, Some(tx), &limits())
    }

    fn put_external(entries: &mut Vec<Entry>, evse: i32, raw: &Value) -> Result<(), Rejection> {
        put_with(entries, evse, raw, Origin::External, None, &limits())
    }

    fn reason(result: Result<(), Rejection>) -> &'static str {
        result.expect_err("the profile is refused").reason
    }

    fn close(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("a limit applies");
        assert!(
            (actual - expected).abs() < 1e-6,
            "expected {expected}, got {actual}"
        );
    }

    fn view(id: &str) -> TxView<'_> {
        TxView {
            id,
            started_at: t0(),
            relative_start: Some(t0()),
        }
    }

    // ---- evaluation --------------------------------------------------------------------

    #[test]
    fn a_tx_profile_overrides_the_tx_default_profile_of_its_transaction() {
        let mut entries = Vec::new();
        put(
            &mut entries,
            1,
            &profile(1, 0, "TxDefaultProfile", "A", &[(0, 6.0)]),
        )
        .expect("default");
        put_tx(&mut entries, 1, &tx_profile(2, "T1", 10.0), "T1").expect("tx profile");
        let eval = eval(&entries);
        // 10 A on three phases at 230 V.
        close(eval.limit_w(1, Some(view("T1")), 1, t0()), 6900.0);
        // Without the transaction, or in another one, only the default applies.
        close(eval.limit_w(1, None, 1, t0()), 4140.0);
        close(eval.limit_w(1, Some(view("T2")), 1, t0()), 4140.0);
        // Another EVSE has neither.
        assert_eq!(eval.limit_w(2, None, 1, t0()), None);
    }

    #[test]
    fn a_tx_default_profile_on_evse_0_applies_to_every_evse() {
        let mut entries = Vec::new();
        put(
            &mut entries,
            0,
            &profile(1, 0, "TxDefaultProfile", "A", &[(0, 8.0)]),
        )
        .expect("default");
        let eval = eval(&entries);
        close(eval.limit_w(1, None, 1, t0()), 5520.0);
        close(eval.limit_w(2, None, 1, t0()), 5520.0);
        // A higher stack level of an EVSE wins over the station's profile on that EVSE only.
        put(
            &mut entries,
            1,
            &profile(2, 1, "TxDefaultProfile", "A", &[(0, 16.0)]),
        )
        .expect("evse default");
        let eval = self::eval(&entries);
        close(eval.limit_w(1, None, 1, t0()), 11_040.0);
        close(eval.limit_w(2, None, 1, t0()), 5520.0);
    }

    #[test]
    fn the_max_profile_caps_the_result_and_is_shared_by_the_charging_evses() {
        let mut entries = Vec::new();
        put(
            &mut entries,
            1,
            &profile(1, 0, "TxDefaultProfile", "W", &[(0, 9000.0)]),
        )
        .expect("default");
        put(
            &mut entries,
            0,
            &profile(2, 0, "ChargingStationMaxProfile", "W", &[(0, 6000.0)]),
        )
        .expect("max");
        let eval = eval(&entries);
        close(eval.limit_w(1, None, 1, t0()), 6000.0);
        close(eval.limit_w(1, None, 2, t0()), 3000.0);
        // The max profile alone limits an EVSE that has no default.
        close(eval.limit_w(2, None, 1, t0()), 6000.0);
    }

    #[test]
    fn external_constraints_cap_like_a_max_profile() {
        let mut entries = Vec::new();
        put(
            &mut entries,
            1,
            &profile(1, 0, "TxDefaultProfile", "W", &[(0, 9000.0)]),
        )
        .expect("default");
        put_external(
            &mut entries,
            0,
            &profile(
                -1,
                0,
                "ChargingStationExternalConstraints",
                "W",
                &[(0, 4000.0)],
            ),
        )
        .expect("station constraint");
        {
            let eval = eval(&entries);
            close(eval.limit_w(1, None, 1, t0()), 4000.0);
            close(eval.limit_w(2, None, 1, t0()), 4000.0);
        }
        // A constraint on one EVSE caps that EVSE only, and the lowest of all wins.
        put_external(
            &mut entries,
            1,
            &profile(
                -2,
                0,
                "ChargingStationExternalConstraints",
                "W",
                &[(0, 2500.0)],
            ),
        )
        .expect("evse constraint");
        let eval = eval(&entries);
        close(eval.limit_w(1, None, 1, t0()), 2500.0);
        close(eval.limit_w(2, None, 1, t0()), 4000.0);
        // The constraint is the only limit where no other profile applies.
        let none: Vec<Entry> = entries
            .iter()
            .filter(|entry| entry.is_external())
            .cloned()
            .collect();
        close(self::eval(&none).limit_w(2, None, 1, t0()), 4000.0);
    }

    #[test]
    fn valid_to_is_exclusive() {
        let mut entries = Vec::new();
        let raw = with(
            profile(1, 0, "TxDefaultProfile", "W", &[(0, 3000.0)]),
            "validTo",
            json!(stamp(at(60))),
        );
        put(&mut entries, 1, &raw).expect("profile");
        let eval = eval(&entries);
        let just_before = at(60) - TimeDelta::nanoseconds(1);
        close(eval.limit_w(1, None, 1, just_before), 3000.0);
        assert_eq!(
            eval.limit_w(1, None, 1, at(60)),
            None,
            "valid while now < validTo"
        );
        let entry = entries.first().expect("installed");
        assert!(!entry.expired(just_before));
        assert!(entry.expired(at(60)));
    }

    #[test]
    fn valid_from_is_inclusive_and_a_future_start_waits() {
        let mut entries = Vec::new();
        let raw = with(
            profile(1, 0, "TxDefaultProfile", "W", &[(0, 3000.0)]),
            "validFrom",
            json!(stamp(at(30))),
        );
        put(&mut entries, 1, &raw).expect("profile");
        let eval = eval(&entries);
        assert_eq!(eval.limit_w(1, None, 1, at(29)), None);
        close(eval.limit_w(1, None, 1, at(30)), 3000.0);
        let mut entries = Vec::new();
        let raw = with_schedule(
            profile(2, 0, "TxDefaultProfile", "W", &[(0, 3000.0)]),
            "startSchedule",
            json!(stamp(at(100))),
        );
        put(&mut entries, 1, &raw).expect("profile");
        assert_eq!(self::eval(&entries).limit_w(1, None, 1, at(99)), None);
        close(self::eval(&entries).limit_w(1, None, 1, at(100)), 3000.0);
    }

    #[test]
    fn the_composite_is_the_lowest_of_the_profiles_and_the_external_constraints() {
        let mut entries = Vec::new();
        put(
            &mut entries,
            0,
            &profile(1, 0, "TxDefaultProfile", "W", &[(0, 5000.0), (120, 4000.0)]),
        )
        .expect("default");
        put_external(
            &mut entries,
            0,
            &profile(
                -1,
                0,
                "ChargingStationExternalConstraints",
                "W",
                &[(0, 4500.0)],
            ),
        )
        .expect("constraint");
        let composite = eval(&entries)
            .composite(1, 300, None, t0(), None)
            .expect("a schedule");
        let schedule = composite.charging_schedule;
        assert_eq!(schedule.charging_rate_unit, ChargingRateUnitType::W);
        assert_eq!(schedule.duration, Some(300));
        let periods: Vec<(i32, f64)> = schedule
            .charging_schedule_period
            .iter()
            .map(|period| (period.start_period, period.limit))
            .collect();
        assert_eq!(periods, [(0, 4500.0), (120, 4000.0)]);
    }

    #[test]
    fn without_profiles_the_composite_shows_the_nominal_power() {
        let entries = Vec::new();
        let watts = eval(&entries)
            .composite(1, 300, None, t0(), None)
            .expect("a schedule")
            .charging_schedule;
        assert_eq!(watts.charging_rate_unit, ChargingRateUnitType::W);
        assert_eq!(watts.duration, Some(300));
        assert_eq!(watts.charging_schedule_period.len(), 1);
        close(
            watts.charging_schedule_period.first().map(|p| p.limit),
            11_000.0,
        );
        let amps = eval(&entries)
            .composite(1, 60, Some(ChargingRateUnitType::A), t0(), None)
            .expect("a schedule")
            .charging_schedule;
        close(amps.charging_schedule_period.first().map(|p| p.limit), 15.9);
        assert!(matches!(
            eval(&entries).composite(7, 60, None, t0(), None),
            Err(CompositeError::UnknownConnector(_))
        ));
    }

    #[test]
    fn a_constraint_alone_shapes_the_composite() {
        let mut entries = Vec::new();
        put_external(
            &mut entries,
            1,
            &profile(
                -1,
                0,
                "ChargingStationExternalConstraints",
                "W",
                &[(0, 2000.0)],
            ),
        )
        .expect("constraint");
        let composite = eval(&entries)
            .composite(1, 60, None, t0(), None)
            .expect("a schedule")
            .charging_schedule;
        close(
            composite.charging_schedule_period.first().map(|p| p.limit),
            2000.0,
        );
        // Another EVSE is not limited.
        let other = eval(&entries)
            .composite(2, 60, None, t0(), None)
            .expect("a schedule")
            .charging_schedule;
        close(
            other.charging_schedule_period.first().map(|p| p.limit),
            11_000.0,
        );
    }

    #[test]
    fn the_energy_follows_the_periods_and_the_nominal_power_caps_it() {
        let mut entries = Vec::new();
        put(
            &mut entries,
            1,
            &profile(
                1,
                0,
                "TxDefaultProfile",
                "W",
                &[(0, 3600.0), (1800, 0.0), (2700, 50_000.0)],
            ),
        )
        .expect("profile");
        let eval = eval(&entries);
        // 3600 W for half an hour, nothing for fifteen minutes, then the nominal 11 kW for
        // fifteen minutes.
        let wh = eval.energy_wh(1, None, 1, at(0), at(3600));
        assert!((wh - (1800.0 + 0.0 + 2750.0)).abs() < 1e-6, "{wh}");
        assert_eq!(eval.energy_wh(1, None, 1, at(10), at(10)), 0.0);
        // A constraint lowers the same span.
        put_external(
            &mut entries,
            0,
            &profile(
                -1,
                0,
                "ChargingStationExternalConstraints",
                "W",
                &[(0, 1000.0)],
            ),
        )
        .expect("constraint");
        // 1000 W for half an hour, nothing for fifteen minutes, 1000 W for fifteen.
        let wh = self::eval(&entries).energy_wh(1, None, 1, at(0), at(3600));
        assert!((wh - (500.0 + 0.0 + 250.0)).abs() < 1e-6, "{wh}");
    }

    #[test]
    fn a_relative_tx_profile_counts_from_the_start_of_the_transaction() {
        let mut entries = Vec::new();
        let mut raw = tx_profile(1, "T1", 10.0);
        raw["chargingProfileKind"] = json!("Relative");
        raw["chargingSchedule"][0]
            .as_object_mut()
            .expect("an object")
            .remove("startSchedule");
        raw["chargingSchedule"][0]["chargingSchedulePeriod"] =
            json!([{"startPeriod": 0, "limit": 10.0}, {"startPeriod": 60, "limit": 6.0}]);
        put_tx(&mut entries, 1, &raw, "T1").expect("a Relative TxProfile");
        let eval = eval(&entries);
        let started = TxView {
            id: "T1",
            started_at: at(1000),
            relative_start: Some(at(1000)),
        };
        close(eval.limit_w(1, Some(started), 1, at(1030)), 6900.0);
        close(eval.limit_w(1, Some(started), 1, at(1060)), 4140.0);
        // Before the transaction started, nothing is in force.
        assert_eq!(eval.limit_w(1, Some(started), 1, at(999)), None);
    }

    #[test]
    fn reports_are_grouped_by_evse_and_source_and_carry_tbc_but_the_last() {
        let mut entries = Vec::new();
        put(
            &mut entries,
            1,
            &profile(1, 0, "TxDefaultProfile", "A", &[(0, 6.0)]),
        )
        .expect("evse 1");
        put(
            &mut entries,
            1,
            &profile(2, 1, "TxDefaultProfile", "A", &[(0, 8.0)]),
        )
        .expect("evse 1");
        put(
            &mut entries,
            0,
            &profile(3, 0, "ChargingStationMaxProfile", "A", &[(0, 9.0)]),
        )
        .expect("evse 0");
        put_external(
            &mut entries,
            1,
            &profile(
                -1,
                0,
                "ChargingStationExternalConstraints",
                "W",
                &[(0, 3000.0)],
            ),
        )
        .expect("external");
        let request =
            |evse: Option<i32>, criterion: ChargingProfileCriterion| GetChargingProfilesRequest {
                request_id: 4,
                evse_id: evse,
                charging_profile: criterion,
                custom_data: None,
            };
        let any = || ChargingProfileCriterion {
            charging_profile_purpose: None,
            stack_level: None,
            charging_profile_id: None,
            charging_limit_source: None,
            custom_data: None,
        };
        let reports = report_requests(&entries, &request(None, any()));
        let shape: Vec<(i32, &str, usize, Option<bool>)> = reports
            .iter()
            .map(|r| {
                (
                    r.evse_id,
                    r.charging_limit_source.as_str(),
                    r.charging_profile.len(),
                    r.tbc,
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                (0, "CSO", 1, Some(true)),
                (1, "CSO", 2, Some(true)),
                (1, "EMS", 1, None)
            ]
        );
        assert!(reports.iter().all(|r| r.request_id == 4));
        let by_source = ChargingProfileCriterion {
            charging_limit_source: Some(vec!["EMS".to_owned()]),
            ..any()
        };
        assert_eq!(
            report_requests(&entries, &request(None, by_source)).len(),
            1
        );
        let none = ChargingProfileCriterion {
            stack_level: Some(7),
            ..any()
        };
        assert_eq!(report_requests(&entries, &request(None, none)).len(), 0);
    }

    #[test]
    fn one_report_runs_at_a_time() {
        let mut state = SmartCharging::default();
        assert!(!state.report_busy());
        state.report_in_flight = true;
        assert!(state.report_busy());
        state.report_in_flight = false;
        state.reports.push_back(ReportChargingProfilesRequest {
            request_id: 1,
            charging_limit_source: "CSO".to_owned(),
            charging_profile: Vec::new(),
            tbc: None,
            evse_id: 0,
            custom_data: None,
        });
        assert!(state.report_busy());
    }

    // ---- admission ---------------------------------------------------------------------

    #[test]
    fn an_unknown_evse_is_refused() {
        let raw = profile(1, 0, "TxDefaultProfile", "A", &[(0, 6.0)]);
        assert_eq!(reason(put(&mut Vec::new(), 3, &raw)), "UnknownEVSE");
        assert_eq!(reason(put(&mut Vec::new(), -1, &raw)), "UnknownEVSE");
    }

    #[test]
    fn purposes_and_places_follow_k01() {
        let mut entries = Vec::new();
        let external = profile(1, 0, "ChargingStationExternalConstraints", "A", &[(0, 6.0)]);
        assert_eq!(reason(put(&mut entries, 0, &external)), "InvalidProfile");
        let max = profile(2, 0, "ChargingStationMaxProfile", "A", &[(0, 6.0)]);
        assert_eq!(reason(put(&mut entries, 1, &max)), "InvalidProfile");
        let relative_max = with_schedule(
            with(max.clone(), "chargingProfileKind", json!("Relative")),
            "startSchedule",
            Value::Null,
        );
        let mut relative_max = relative_max;
        relative_max["chargingSchedule"][0]
            .as_object_mut()
            .expect("an object")
            .remove("startSchedule");
        assert_eq!(
            reason(put(&mut entries, 0, &relative_max)),
            "InvalidProfile"
        );
        put(&mut entries, 0, &max).expect("a max profile on EVSE 0");
        let tx = tx_profile(3, "T1", 6.0);
        assert_eq!(
            reason(put_tx(&mut entries, 0, &tx, "T1")),
            "InvalidProfile",
            "a TxProfile needs an EVSE above 0"
        );
    }

    #[test]
    fn a_tx_profile_needs_the_running_transaction() {
        let mut entries = Vec::new();
        let raw = tx_profile(1, "T1", 6.0);
        assert_eq!(reason(put(&mut entries, 1, &raw)), "TxNotFound");
        assert_eq!(reason(put_tx(&mut entries, 1, &raw, "OTHER")), "TxNotFound");
        let nameless = profile(2, 0, "TxProfile", "A", &[(0, 6.0)]);
        assert_eq!(
            reason(put_tx(&mut entries, 1, &nameless, "T1")),
            "InvalidProfile"
        );
        put_tx(&mut entries, 1, &raw, "T1").expect("the transaction runs");
        assert_eq!(
            entries
                .first()
                .and_then(|e| e.stored.transaction_id.as_deref()),
            Some("T1")
        );
    }

    #[test]
    fn the_schedule_is_checked() {
        let base = || profile(1, 0, "TxDefaultProfile", "A", &[(0, 6.0), (60, 8.0)]);
        let refused = |raw: Value| reason(put(&mut Vec::new(), 1, &raw));
        let no_start = {
            let mut raw = base();
            raw["chargingSchedule"][0]
                .as_object_mut()
                .expect("an object")
                .remove("startSchedule");
            raw
        };
        assert_eq!(refused(no_start.clone()), "InvalidSchedule");
        assert_eq!(
            refused(with(no_start, "chargingProfileKind", json!("Recurring"))),
            "InvalidSchedule"
        );
        assert_eq!(
            refused(with(base(), "chargingProfileKind", json!("Relative"))),
            "InvalidSchedule",
            "a Relative profile has no startSchedule"
        );
        assert_eq!(
            refused(with(base(), "chargingProfileKind", json!("Recurring"))),
            "InvalidProfile",
            "a Recurring profile needs recurrencyKind"
        );
        assert_eq!(
            refused(profile(1, 0, "TxDefaultProfile", "A", &[(5, 6.0)])),
            "InvalidSchedule",
            "the first period starts at 0"
        );
        assert_eq!(
            refused(profile(
                1,
                0,
                "TxDefaultProfile",
                "A",
                &[(0, 6.0), (0, 8.0)]
            )),
            "InvalidSchedule",
            "periods increase strictly"
        );
        assert_eq!(
            refused(profile(
                1,
                0,
                "TxDefaultProfile",
                "A",
                &[(0, 6.0), (60, 8.0), (30, 1.0)]
            )),
            "InvalidSchedule"
        );
        let two = {
            let mut raw = base();
            let schedule = raw["chargingSchedule"][0].clone();
            raw["chargingSchedule"]
                .as_array_mut()
                .expect("an array")
                .push(schedule);
            raw
        };
        assert_eq!(refused(two), "InvalidSchedule", "one schedule only");
        assert_eq!(
            refused(with_schedule(base(), "duration", json!(0))),
            "InvalidSchedule"
        );
    }

    #[test]
    fn limits_of_the_device_model_hold() {
        let mut entries = Vec::new();
        let strict = Limits {
            max_stack: 2,
            max_periods: 2,
            units: vec!["A".to_owned()],
            ..limits()
        };
        let try_put = |entries: &mut Vec<Entry>, raw: &Value| {
            reason(put_with(entries, 1, raw, Origin::Csms, None, &strict))
        };
        assert_eq!(
            try_put(
                &mut entries,
                &profile(1, 3, "TxDefaultProfile", "A", &[(0, 6.0)])
            ),
            "InvalidStackLevel"
        );
        assert_eq!(
            try_put(
                &mut entries,
                &profile(1, -1, "TxDefaultProfile", "A", &[(0, 6.0)])
            ),
            "InvalidStackLevel"
        );
        assert_eq!(
            try_put(
                &mut entries,
                &profile(
                    1,
                    0,
                    "TxDefaultProfile",
                    "A",
                    &[(0, 6.0), (1, 6.0), (2, 6.0)]
                )
            ),
            "TooManyElements"
        );
        assert_eq!(
            try_put(
                &mut entries,
                &profile(1, 0, "TxDefaultProfile", "W", &[(0, 6.0)])
            ),
            "UnsupportedRateUnit"
        );
        put_with(
            &mut entries,
            1,
            &profile(1, 2, "TxDefaultProfile", "A", &[(0, 6.0), (1, 5.0)]),
            Origin::Csms,
            None,
            &strict,
        )
        .expect("inside the limits");
    }

    #[test]
    fn phases_are_checked_and_phase_switching_is_not_offered() {
        let refused = |raw: Value| reason(put(&mut Vec::new(), 1, &raw));
        let base = || profile(1, 0, "TxDefaultProfile", "A", &[(0, 6.0)]);
        assert_eq!(
            refused(with_period(base(), "numberPhases", json!(4))),
            "InvalidSchedule"
        );
        assert_eq!(
            refused(with_period(base(), "numberPhases", json!(0))),
            "InvalidSchedule"
        );
        assert_eq!(
            refused(with_period(base(), "phaseToUse", json!(1))),
            "InvalidSchedule"
        );
        put(
            &mut Vec::new(),
            1,
            &with_period(base(), "numberPhases", json!(3)),
        )
        .expect("the station has three phases");
    }

    #[test]
    fn a_profile_replaces_the_same_id_and_the_same_slot() {
        let mut entries = Vec::new();
        put(
            &mut entries,
            1,
            &profile(1, 0, "TxDefaultProfile", "A", &[(0, 6.0)]),
        )
        .expect("first");
        // The same id moves to another EVSE (K01.FR.05).
        put(
            &mut entries,
            2,
            &profile(1, 0, "TxDefaultProfile", "A", &[(0, 8.0)]),
        )
        .expect("moved");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries.first().map(|e| e.stored.evse), Some(2));
        // Another id in the same EVSE, purpose and stack level replaces it.
        put(
            &mut entries,
            2,
            &profile(5, 0, "TxDefaultProfile", "A", &[(0, 9.0)]),
        )
        .expect("slot");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries.first().map(Entry::id), Some(5));
        // A different stack level coexists.
        put(
            &mut entries,
            2,
            &profile(6, 1, "TxDefaultProfile", "A", &[(0, 9.0)]),
        )
        .expect("stack");
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn a_tx_default_stack_level_is_not_shared_between_evse_0_and_an_evse() {
        let mut entries = Vec::new();
        put(
            &mut entries,
            1,
            &profile(1, 1, "TxDefaultProfile", "A", &[(0, 6.0)]),
        )
        .expect("evse");
        let station = profile(2, 1, "TxDefaultProfile", "A", &[(0, 8.0)]);
        assert_eq!(reason(put(&mut entries, 0, &station)), "DuplicateProfile");
        // The same id is a replacement, not a clash.
        put(&mut entries, 0, &with(station.clone(), "id", json!(1))).expect("replacement");
        assert_eq!(entries.len(), 1);
        // The station's profile now holds stack level 1 against every EVSE.
        assert_eq!(
            reason(put(
                &mut entries,
                2,
                &profile(9, 1, "TxDefaultProfile", "A", &[(0, 6.0)])
            )),
            "DuplicateProfile"
        );
        // Other stack levels are free on EVSE 0 and on the EVSEs.
        put(&mut entries, 0, &with(station, "stackLevel", json!(2))).expect("other stack");
        put(
            &mut entries,
            2,
            &profile(9, 3, "TxDefaultProfile", "A", &[(0, 6.0)]),
        )
        .expect("evse 2");
        let mut entries = Vec::new();
        put(
            &mut entries,
            0,
            &profile(1, 1, "TxDefaultProfile", "A", &[(0, 6.0)]),
        )
        .expect("station");
        assert_eq!(
            reason(put(
                &mut entries,
                1,
                &profile(2, 1, "TxDefaultProfile", "A", &[(0, 6.0)])
            )),
            "DuplicateProfile"
        );
    }

    #[test]
    fn the_store_has_room_for_ten_and_replacements_need_none() {
        let mut entries = Vec::new();
        for stack in 0..10 {
            put(
                &mut entries,
                1,
                &profile(stack + 1, stack, "TxDefaultProfile", "A", &[(0, 6.0)]),
            )
            .expect("room");
        }
        let eleventh = profile(50, 10, "TxDefaultProfile", "A", &[(0, 6.0)]);
        assert_eq!(reason(put(&mut entries, 1, &eleventh)), "TooManyElements");
        put(
            &mut entries,
            1,
            &profile(60, 3, "TxDefaultProfile", "A", &[(0, 7.0)]),
        )
        .expect("a replacement takes a slot that exists");
        assert_eq!(entries.len(), 10);
    }

    #[test]
    fn the_csms_cannot_take_an_id_of_an_external_constraint() {
        let mut entries = Vec::new();
        put_external(
            &mut entries,
            0,
            &profile(
                -1,
                0,
                "ChargingStationExternalConstraints",
                "W",
                &[(0, 4000.0)],
            ),
        )
        .expect("constraint");
        let raw = profile(-1, 0, "TxDefaultProfile", "W", &[(0, 4000.0)]);
        assert_eq!(reason(put(&mut entries, 1, &raw)), "InvalidProfileId");
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn what_the_station_does_not_implement_is_refused_with_the_code_of_ocpp_21() {
        let base = || profile(1, 0, "TxDefaultProfile", "A", &[(0, 6.0)]);
        let refused = |raw: Value| reason(put(&mut Vec::new(), 1, &raw));
        assert_eq!(
            refused(with(
                base(),
                "chargingProfilePurpose",
                json!("PriorityCharging")
            )),
            "UnsupportedPurpose"
        );
        assert_eq!(
            refused(with(
                base(),
                "chargingProfilePurpose",
                json!("LocalGeneration")
            )),
            "UnsupportedPurpose"
        );
        assert_eq!(
            refused(with(base(), "chargingProfileKind", json!("Dynamic"))),
            "UnsupportedKind"
        );
        assert_eq!(
            refused(with_period(
                base(),
                "operationMode",
                json!("CentralSetpoint")
            )),
            "InvalidOperationMode"
        );
        assert_eq!(
            refused(with_period(base(), "limit_L2", json!(5.0))),
            "PhaseConflict"
        );
        assert_eq!(
            refused(with_period(base(), "dischargeLimit", json!(-5.0))),
            "UnsupportedParam"
        );
        assert_eq!(
            refused(with_period(base(), "evseSleep", json!(true))),
            "InvalidSchedule"
        );
        assert_eq!(
            refused(with_schedule(base(), "useLocalTime", json!(true))),
            "InvalidSchedule"
        );
        assert_eq!(
            refused(with_schedule(base(), "randomizedDelay", json!(30))),
            "InvalidSchedule"
        );
        assert_eq!(
            refused(with_schedule(
                base(),
                "limitAtSoC",
                json!({"soc": 80, "limit": 3.0})
            )),
            "InvalidSchedule"
        );
        assert_eq!(
            refused(with(base(), "maxOfflineDuration", json!(60))),
            "UnsupportedParam"
        );
        assert_eq!(
            refused(with(base(), "dynUpdateInterval", json!(60))),
            "InvalidProfile"
        );
        let mut no_limit = base();
        no_limit["chargingSchedule"][0]["chargingSchedulePeriod"][0]
            .as_object_mut()
            .expect("an object")
            .remove("limit");
        assert_eq!(refused(no_limit), "InvalidSchedule");
        // What the station does implement is accepted, unknown 2.1 fields included.
        let mut entries = Vec::new();
        let accepted = with_schedule(
            with_period(base(), "operationMode", json!("ChargingOnly")),
            "powerTolerance",
            json!(1.5),
        );
        put(&mut entries, 1, &accepted).expect("ChargingOnly is the supported mode");
        assert_eq!(
            entries.first().map(|e| e.stored.profile.clone()),
            Some(accepted),
            "the stored profile is the one that arrived"
        );
    }

    #[test]
    fn a_remote_start_takes_a_tx_profile_without_a_transaction_id() {
        let mut entries = Vec::new();
        let raw = profile(1, 0, "TxProfile", "A", &[(0, 6.0)]);
        put_with(&mut entries, 1, &raw, Origin::RemoteStart, None, &limits()).expect("accepted");
        let named = with(raw, "transactionId", json!("T1"));
        assert_eq!(
            reason(put_with(
                &mut Vec::new(),
                1,
                &named,
                Origin::RemoteStart,
                None,
                &limits()
            )),
            "InvalidProfile"
        );
        let default = profile(2, 0, "TxDefaultProfile", "A", &[(0, 6.0)]);
        assert_eq!(
            reason(put_with(
                &mut Vec::new(),
                1,
                &default,
                Origin::RemoteStart,
                None,
                &limits()
            )),
            "InvalidProfile"
        );
    }

    // ---- the actor ---------------------------------------------------------------------

    fn actor() -> Station2Actor {
        let mut config = StationConfig::new("ws://127.0.0.1:1")
            .with_connectors(2)
            .without_persistence();
        config.protocol = Protocol::V201;
        let (_commands, receiver) = mpsc::channel(4);
        let (events, _) = broadcast::channel(64);
        Station2Actor::new(
            config,
            Arc::new(ManualClock::new(Utc::now())),
            receiver,
            events,
            None,
        )
        .0
    }

    fn set_var(actor: &mut Station2Actor, variable: &str, value: &str) {
        let component = Component {
            evse: None,
            name: "SmartChargingCtrlr".to_owned(),
            instance: None,
            custom_data: None,
        };
        let variable = Variable {
            name: variable.to_owned(),
            instance: None,
            custom_data: None,
        };
        actor
            .dm
            .set_value(&component, &variable, None, value)
            .expect("variable accepted");
    }

    fn install_in(actor: &mut Station2Actor, evse: i32, raw: &Value) {
        let limits = actor.sc_limits();
        let ctx = Admit {
            origin: Origin::Csms,
            limits: &limits,
            active_tx: None,
        };
        let now = actor.clock.now_utc();
        let entry = admit(&actor.charging.entries, evse, raw, &ctx, now, CSMS_SOURCE)
            .expect("the profile is accepted");
        install(&mut actor.charging.entries, entry);
    }

    fn start_spec(remote_start_id: Option<i32>) -> StartSpec {
        StartSpec {
            token: None,
            remote_start_id,
            trigger: TriggerReason::RemoteStart,
            authorized: true,
        }
    }

    #[tokio::test]
    async fn profiles_survive_a_restart_except_tx_profiles() {
        let mut a = actor();
        let now = a.clock.now_utc();
        let start = stamp(now);
        let default = with_schedule(
            profile(1, 0, "TxDefaultProfile", "W", &[(0, 3000.0)]),
            "startSchedule",
            json!(start),
        );
        install_in(&mut a, 1, &default);
        install_in(
            &mut a,
            0,
            &with_schedule(
                profile(2, 0, "ChargingStationMaxProfile", "W", &[(0, 9000.0)]),
                "startSchedule",
                json!(start),
            ),
        );
        a.charging.entries.push(
            Entry::new(StoredProfile2 {
                evse: 1,
                profile: with_schedule(tx_profile(3, "T1", 6.0), "startSchedule", json!(start)),
                transaction_id: Some("T1".to_owned()),
                installed_at: now,
                source: CSMS_SOURCE.to_owned(),
            })
            .expect("a TxProfile"),
        );
        let persisted = a.sc_persist();
        assert_eq!(persisted.profiles.len(), 2, "the TxProfile stays out");
        let json = serde_json::to_value(&persisted).expect("serializes");
        let mut stored: SmartChargingPersist = serde_json::from_value(json).expect("loads");
        // Entries a restore must not trust: an EVSE that does not exist, a repeated id, a
        // profile that does not parse and a TxProfile.
        let extra = |evse: u32, id: i32, raw: Value| StoredProfile2 {
            evse,
            profile: with(raw, "id", json!(id)),
            transaction_id: None,
            installed_at: now,
            source: CSMS_SOURCE.to_owned(),
        };
        stored.profiles.push(extra(9, 20, default.clone()));
        stored.profiles.push(extra(1, 1, default.clone()));
        stored.profiles.push(StoredProfile2 {
            evse: 1,
            profile: json!({"id": 30}),
            transaction_id: None,
            installed_at: now,
            source: CSMS_SOURCE.to_owned(),
        });
        stored
            .profiles
            .push(extra(1, 31, tx_profile(31, "T9", 6.0)));
        let mut b = actor();
        b.sc_restore(stored);
        let ids: Vec<i32> = b.charging.entries.iter().map(Entry::id).collect();
        assert_eq!(ids, [1, 2]);
        // A file from before the profiles were stored has none.
        let old: SmartChargingPersist = serde_json::from_value(json!({})).expect("old file loads");
        assert_eq!(old.profiles.len(), 0);
    }

    #[tokio::test]
    async fn pending_remote_start_profile_is_released_on_connect_timeout() {
        let mut a = actor();
        let raw = profile(7, 0, "TxProfile", "A", &[(0, 6.0)]);
        a.sc_remote_start_profile(1, &raw, 5).expect("accepted");
        a.on_authorized(
            1,
            super::super::types::IdTokenRef {
                id_token: "REMOTE".to_owned(),
                kind: crate::ocpp::v2::types::IdTokenType::Central,
            },
            Some(5),
            TriggerReason::RemoteStart,
        )
        .expect("authorization waits for the EV");
        assert_eq!(a.charging.pending.len(), 1);
        a.on_ev_connect_timeout(1);
        assert!(a.charging.pending.is_empty());
    }

    #[tokio::test]
    async fn a_remote_start_profile_binds_to_the_transaction_that_starts() {
        let mut a = actor();
        let raw = profile(7, 0, "TxProfile", "A", &[(0, 6.0)]);
        a.sc_remote_start_profile(1, &raw, 5).expect("accepted");
        assert!(a.charging.entries.is_empty(), "nothing is installed yet");
        let tx = a
            .begin_transaction(1, start_spec(Some(5)))
            .expect("transaction starts");
        let entry = a.charging.entries.first().expect("installed");
        assert_eq!(entry.stored.transaction_id.as_deref(), Some(tx.0.as_str()));
        assert_eq!(
            entry.stored.profile["transactionId"],
            json!(tx.0),
            "the reported profile names the transaction"
        );
        a.sc_tx_ended(&tx.0);
        assert!(
            a.charging.entries.is_empty(),
            "it ends with the transaction"
        );
    }

    #[tokio::test]
    async fn a_remote_start_profile_is_kept_for_its_own_start() {
        let mut a = actor();
        let raw = profile(7, 0, "TxProfile", "A", &[(0, 6.0)]);
        a.sc_remote_start_profile(1, &raw, 5).expect("accepted");
        a.begin_transaction(1, start_spec(None))
            .expect("a local start");
        assert!(a.charging.entries.is_empty());
        assert_eq!(a.charging.pending.len(), 1);
        a.sc_forget_pending(1, 6);
        assert_eq!(a.charging.pending.len(), 1);
        a.sc_forget_pending(1, 5);
        assert!(a.charging.pending.is_empty());
    }

    #[tokio::test]
    async fn a_remote_start_profile_is_checked_before_the_start() {
        let mut a = actor();
        let default = profile(7, 0, "TxDefaultProfile", "A", &[(0, 6.0)]);
        let refused = a
            .sc_remote_start_profile(1, &default, 5)
            .expect_err("only a TxProfile");
        assert_eq!(refused.reason_code, "InvalidProfile");
        // With smart charging off the profile is ignored (K05.FR.04).
        set_var(&mut a, "Enabled", "false");
        a.sc_remote_start_profile(1, &default, 5)
            .expect("ignored, not refused");
        assert!(a.charging.pending.is_empty());
    }

    #[tokio::test]
    async fn the_meter_energy_follows_the_limit() {
        let mut a = actor();
        let hour = Duration::from_secs(3600);
        let nominal = PowerModel::from_config(&a.config).nominal_w;
        let now = a.clock.now_utc();
        let free = a.sc_energy_wh(1, now, hour);
        assert!((free - nominal).abs() < 1e-6, "no profile: nominal power");
        let low = with_schedule(
            profile(1, 0, "TxDefaultProfile", "W", &[(0, 1000.0)]),
            "startSchedule",
            json!(stamp(now)),
        );
        install_in(&mut a, 1, &low);
        let limited = a.sc_energy_wh(1, now, hour);
        assert!((limited - 1000.0).abs() < 1e-6, "{limited}");
        assert!((a.sc_offered_w(1) - 1000.0).abs() < 1e-6);
        // Another EVSE is not limited.
        assert!((a.sc_energy_wh(2, now, hour) - nominal).abs() < 1e-6);
        let zero = with_schedule(
            profile(2, 1, "TxDefaultProfile", "A", &[(0, 0.0)]),
            "startSchedule",
            json!(stamp(now)),
        );
        install_in(&mut a, 1, &zero);
        assert_eq!(a.sc_energy_wh(1, now, hour), 0.0);
        // Smart charging off: the profiles are inert.
        set_var(&mut a, "Enabled", "false");
        assert!((a.sc_energy_wh(1, now, hour) - nominal).abs() < 1e-6);
        assert!((a.sc_offered_w(1) - nominal).abs() < 1e-6);
    }

    #[tokio::test]
    async fn expired_profiles_are_pruned_and_not_counted() {
        let mut a = actor();
        let now = a.clock.now_utc();
        let raw = with_schedule(
            with(
                profile(1, 0, "TxDefaultProfile", "W", &[(0, 3000.0)]),
                "validTo",
                json!(stamp(now + TimeDelta::seconds(10))),
            ),
            "startSchedule",
            json!(stamp(now)),
        );
        install_in(&mut a, 1, &raw);
        assert_eq!(a.charging_profile_count(), 1);
        a.clock.now_utc();
        let clock = ManualClock::new(now + TimeDelta::seconds(10));
        a.clock = Arc::new(clock);
        assert_eq!(a.charging_profile_count(), 0);
        a.sc_prune(a.clock.now_utc());
        assert!(a.charging.entries.is_empty());
    }
}
