//! Smart charging profile.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::common::{ChargingProfile, ChargingSchedule};
use crate::ids::ConnectorId;
use crate::ocpp::types::{ChargingProfilePurposeType, ChargingRateUnitType, wire_enum};

wire_enum! {
    /// `ChargingProfileStatus` of `SetChargingProfile.conf`.
    ChargingProfileStatus {
        Accepted => "Accepted",
        Rejected => "Rejected",
        NotSupported => "NotSupported",
    }
}

/// `SetChargingProfile.req`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetChargingProfileRequest {
    pub connector_id: ConnectorId,
    pub cs_charging_profiles: ChargingProfile,
}

/// `SetChargingProfile.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetChargingProfileResponse {
    pub status: ChargingProfileStatus,
}

action!(
    /// `SetChargingProfile` (central system -> charge point).
    SetChargingProfile = "SetChargingProfile", SetChargingProfileRequest => SetChargingProfileResponse, InboundAction
);

wire_enum! {
    /// `ClearChargingProfileStatus`.
    ClearChargingProfileStatus {
        Accepted => "Accepted",
        Unknown => "Unknown",
    }
}

/// `ClearChargingProfile.req`. All fields optional; they act as a conjunctive filter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearChargingProfileRequest {
    /// Id of the profile to clear (wire name `id`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connector_id: Option<ConnectorId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charging_profile_purpose: Option<ChargingProfilePurposeType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_level: Option<i32>,
}

/// `ClearChargingProfile.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearChargingProfileResponse {
    pub status: ClearChargingProfileStatus,
}

action!(
    /// `ClearChargingProfile` (central system -> charge point).
    ClearChargingProfile = "ClearChargingProfile", ClearChargingProfileRequest => ClearChargingProfileResponse, InboundAction
);

wire_enum! {
    /// `GetCompositeScheduleStatus`.
    GetCompositeScheduleStatus {
        Accepted => "Accepted",
        Rejected => "Rejected",
    }
}

/// `GetCompositeSchedule.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetCompositeScheduleRequest {
    pub connector_id: ConnectorId,
    /// Length of the requested schedule in seconds.
    pub duration: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charging_rate_unit: Option<ChargingRateUnitType>,
}

/// `GetCompositeSchedule.conf`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetCompositeScheduleResponse {
    pub status: GetCompositeScheduleStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connector_id: Option<ConnectorId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule_start: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charging_schedule: Option<ChargingSchedule>,
}

action!(
    /// `GetCompositeSchedule` (central system -> charge point).
    GetCompositeSchedule = "GetCompositeSchedule", GetCompositeScheduleRequest => GetCompositeScheduleResponse, InboundAction
);
