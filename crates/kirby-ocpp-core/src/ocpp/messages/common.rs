//! Sub-structures shared by several actions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::IdTag;
use crate::ocpp::types::{
    AuthorizationStatus, ChargingProfileKindType, ChargingProfilePurposeType, ChargingRateUnitType,
    Location, Measurand, Phase, ReadingContext, RecurrencyKindType, UnitOfMeasure, ValueFormat,
};

/// `IdTagInfo`: the CSMS' verdict on an `idTag`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdTagInfo {
    pub status: AuthorizationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiry_date: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id_tag: Option<IdTag>,
}

impl IdTagInfo {
    /// An `IdTagInfo` with only a status.
    pub const fn with_status(status: AuthorizationStatus) -> Self {
        Self {
            status,
            expiry_date: None,
            parent_id_tag: None,
        }
    }
}

/// `SampledValue` of a `MeterValue`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SampledValue {
    /// Decimal string (or signed data, see `format`).
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ReadingContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<ValueFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measurand: Option<Measurand>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<UnitOfMeasure>,
}

impl SampledValue {
    /// A sample with only a value; all descriptive fields unset.
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            context: None,
            format: None,
            measurand: None,
            phase: None,
            location: None,
            unit: None,
        }
    }
}

/// `MeterValue`: samples taken at one instant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeterValue {
    pub timestamp: DateTime<Utc>,
    pub sampled_value: Vec<SampledValue>,
}

/// One entry of `ChargingSchedule.chargingSchedulePeriod`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChargingSchedulePeriod {
    /// Seconds from the start of the schedule.
    pub start_period: i32,
    /// Limit in the schedule's `chargingRateUnit` (the schema demands a multiple of 0.1).
    pub limit: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number_phases: Option<i32>,
}

/// `ChargingSchedule`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChargingSchedule {
    /// Duration of the schedule in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_schedule: Option<DateTime<Utc>>,
    pub charging_rate_unit: ChargingRateUnitType,
    pub charging_schedule_period: Vec<ChargingSchedulePeriod>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_charging_rate: Option<f64>,
}

/// `ChargingProfile` (`csChargingProfiles` of `SetChargingProfile`, `chargingProfile` of `RemoteStartTransaction`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChargingProfile {
    pub charging_profile_id: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction_id: Option<i32>,
    pub stack_level: i32,
    pub charging_profile_purpose: ChargingProfilePurposeType,
    pub charging_profile_kind: ChargingProfileKindType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recurrency_kind: Option<RecurrencyKindType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<DateTime<Utc>>,
    pub charging_schedule: ChargingSchedule,
}

/// `AuthorizationData`: one entry of a `SendLocalList` list.
///
/// Without `idTagInfo` a differential update deletes the entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationData {
    pub id_tag: IdTag,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_tag_info: Option<IdTagInfo>,
}
