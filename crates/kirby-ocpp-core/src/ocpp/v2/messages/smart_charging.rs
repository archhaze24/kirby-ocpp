use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ocpp::v2::types::{CustomData, StatusInfo, helpers, payload, wire_enum};

wire_enum! {
    /// Result of `SetChargingProfile` (`ChargingProfileStatusEnumType`).
    ChargingProfileStatus {
        Accepted = "Accepted",
        Rejected = "Rejected",
    }
}

wire_enum! {
    /// Result of `ClearChargingProfile` (`ClearChargingProfileStatusEnumType`).
    ClearChargingProfileStatus {
        Accepted = "Accepted",
        Unknown = "Unknown",
    }
}

wire_enum! {
    /// Result of `GetChargingProfiles` (`GetChargingProfileStatusEnumType`).
    GetChargingProfileStatus {
        Accepted = "Accepted",
        NoProfiles = "NoProfiles",
    }
}

wire_enum! {
    /// Result of `GetCompositeSchedule` (`GenericStatusEnumType`).
    CompositeScheduleStatus {
        Accepted = "Accepted",
        Rejected = "Rejected",
    }
}

wire_enum! {
    /// `ChargingProfilePurposeEnumType`. `PriorityCharging` and `LocalGeneration` exist in
    /// OCPP 2.1 only.
    ChargingProfilePurpose {
        ChargingStationExternalConstraints = "ChargingStationExternalConstraints",
        ChargingStationMaxProfile = "ChargingStationMaxProfile",
        TxDefaultProfile = "TxDefaultProfile",
        TxProfile = "TxProfile",
        PriorityCharging = "PriorityCharging",
        LocalGeneration = "LocalGeneration",
    }
}

wire_enum! {
    /// `ChargingProfileKindEnumType`. `Dynamic` exists in OCPP 2.1 only.
    ChargingProfileKind {
        Absolute = "Absolute",
        Recurring = "Recurring",
        Relative = "Relative",
        Dynamic = "Dynamic",
    }
}

wire_enum! {
    /// `RecurrencyKindEnumType`.
    RecurrencyKind {
        Daily = "Daily",
        Weekly = "Weekly",
    }
}

wire_enum! {
    /// `ChargingRateUnitEnumType`.
    ChargingRateUnit {
        W = "W",
        A = "A",
    }
}

payload! {
    /// `ChargingSchedulePeriodType`, the fields the station evaluates. The OCPP 2.1 fields
    /// besides `limit` and `operationMode` are read from the raw profile.
    ChargingSchedulePeriod {
        pub start_period: i32,
        /// Optional in OCPP 2.1 only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub limit: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub number_phases: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub phase_to_use: Option<i32>,
        /// `OperationModeEnumType` (OCPP 2.1); the station supports `ChargingOnly` only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub operation_mode: Option<String>,
    }
}

payload! {
    /// `ChargingScheduleType`, the fields the station evaluates.
    ChargingSchedule {
        pub id: i32,
        #[serde(default, skip_serializing_if = "Option::is_none", with = "helpers::ts_opt")]
        pub start_schedule: Option<DateTime<Utc>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub duration: Option<i32>,
        pub charging_rate_unit: ChargingRateUnit,
        pub charging_schedule_period: Vec<ChargingSchedulePeriod>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub min_charging_rate: Option<f64>,
    }
}

payload! {
    /// `ChargingProfileType`, the fields the station evaluates. The station keeps the profile
    /// as received too (see `station2::smart_charging`), so fields it does not know survive.
    ChargingProfile {
        pub id: i32,
        pub stack_level: i32,
        pub charging_profile_purpose: ChargingProfilePurpose,
        pub charging_profile_kind: ChargingProfileKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub recurrency_kind: Option<RecurrencyKind>,
        #[serde(default, skip_serializing_if = "Option::is_none", with = "helpers::ts_opt")]
        pub valid_from: Option<DateTime<Utc>>,
        #[serde(default, skip_serializing_if = "Option::is_none", with = "helpers::ts_opt")]
        pub valid_to: Option<DateTime<Utc>>,
        pub charging_schedule: Vec<ChargingSchedule>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub transaction_id: Option<String>,
    }
}

payload! {
    /// `SetChargingProfile.req`. The profile stays raw JSON: the station validates it and
    /// parses it into [`ChargingProfile`] itself, and reports it back unchanged.
    SetChargingProfileRequest {
        pub evse_id: i32,
        pub charging_profile: Value,
    }
}

payload! {
    /// `SetChargingProfile.conf`.
    SetChargingProfileResponse {
        pub status: ChargingProfileStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

payload! {
    /// `ClearChargingProfileType`: the selection of `ClearChargingProfile`.
    ClearChargingProfileCriteria {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub evse_id: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub charging_profile_purpose: Option<ChargingProfilePurpose>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub stack_level: Option<i32>,
    }
}

payload! {
    /// `ClearChargingProfile.req`.
    ClearChargingProfileRequest {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub charging_profile_id: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub charging_profile_criteria: Option<ClearChargingProfileCriteria>,
    }
}

payload! {
    /// `ClearChargingProfile.conf`.
    ClearChargingProfileResponse {
        pub status: ClearChargingProfileStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

payload! {
    /// `ChargingProfileCriterionType`: the filter of `GetChargingProfiles`.
    ChargingProfileCriterion {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub charging_profile_purpose: Option<ChargingProfilePurpose>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub stack_level: Option<i32>,
        #[serde(default, skip_serializing_if = "helpers::omit_empty")]
        pub charging_profile_id: Option<Vec<i32>>,
        /// `ChargingLimitSourceEnumType` in 2.0.1, a string of at most 20 characters in 2.1;
        /// the schema of the connection checks it.
        #[serde(default, skip_serializing_if = "helpers::omit_empty")]
        pub charging_limit_source: Option<Vec<String>>,
    }
}

payload! {
    /// `GetChargingProfiles.req`.
    GetChargingProfilesRequest {
        pub request_id: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub evse_id: Option<i32>,
        pub charging_profile: ChargingProfileCriterion,
    }
}

payload! {
    /// `GetChargingProfiles.conf`.
    GetChargingProfilesResponse {
        pub status: GetChargingProfileStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

payload! {
    /// `ReportChargingProfiles.req`: the profiles of one EVSE and one limit source. The
    /// profiles are the stored raw JSON.
    ReportChargingProfilesRequest {
        pub request_id: i32,
        /// `ChargingLimitSourceEnumType` in 2.0.1, a string of at most 20 characters in 2.1.
        pub charging_limit_source: String,
        pub charging_profile: Vec<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tbc: Option<bool>,
        pub evse_id: i32,
    }
}

payload! {
    /// `ReportChargingProfiles.conf`: no fields besides `customData`.
    ReportChargingProfilesResponse {}
}

payload! {
    /// `GetCompositeSchedule.req`.
    GetCompositeScheduleRequest {
        pub duration: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub charging_rate_unit: Option<ChargingRateUnit>,
        pub evse_id: i32,
    }
}

payload! {
    /// `CompositeScheduleType`.
    CompositeSchedule {
        pub charging_schedule_period: Vec<ChargingSchedulePeriod>,
        pub evse_id: i32,
        pub duration: i32,
        #[serde(with = "helpers::ts")]
        pub schedule_start: DateTime<Utc>,
        pub charging_rate_unit: ChargingRateUnit,
    }
}

payload! {
    /// `GetCompositeSchedule.conf`.
    GetCompositeScheduleResponse {
        pub status: CompositeScheduleStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub schedule: Option<CompositeSchedule>,
    }
}

payload! {
    /// `ChargingLimitType`.
    ChargingLimit {
        /// `ChargingLimitSourceEnumType`.
        pub charging_limit_source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub is_grid_critical: Option<bool>,
    }
}

payload! {
    /// `NotifyChargingLimit.req`. The schedules are raw `ChargingScheduleType` JSON.
    NotifyChargingLimitRequest {
        #[serde(default, skip_serializing_if = "helpers::omit_empty")]
        pub charging_schedule: Option<Vec<Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub evse_id: Option<i32>,
        pub charging_limit: ChargingLimit,
    }
}

payload! {
    /// `NotifyChargingLimit.conf`: no fields besides `customData`.
    NotifyChargingLimitResponse {}
}

payload! {
    /// `ClearedChargingLimit.req`.
    ClearedChargingLimitRequest {
        /// `ChargingLimitSourceEnumType` in 2.0.1, a string of at most 20 characters in 2.1.
        pub charging_limit_source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub evse_id: Option<i32>,
    }
}

payload! {
    /// `ClearedChargingLimit.conf`: no fields besides `customData`.
    ClearedChargingLimitResponse {}
}

action! {
    /// `SetChargingProfile` (CSMS -> station).
    SetChargingProfile = "SetChargingProfile", SetChargingProfileRequest => SetChargingProfileResponse, InboundAction
}
action! {
    /// `ClearChargingProfile` (CSMS -> station).
    ClearChargingProfile = "ClearChargingProfile", ClearChargingProfileRequest => ClearChargingProfileResponse, InboundAction
}
action! {
    /// `GetChargingProfiles` (CSMS -> station).
    GetChargingProfiles = "GetChargingProfiles", GetChargingProfilesRequest => GetChargingProfilesResponse, InboundAction
}
action! {
    /// `GetCompositeSchedule` (CSMS -> station).
    GetCompositeSchedule = "GetCompositeSchedule", GetCompositeScheduleRequest => GetCompositeScheduleResponse, InboundAction
}
action! {
    /// `ReportChargingProfiles` (station -> CSMS).
    ReportChargingProfiles = "ReportChargingProfiles", ReportChargingProfilesRequest => ReportChargingProfilesResponse, OutboundAction
}
action! {
    /// `NotifyChargingLimit` (station -> CSMS).
    NotifyChargingLimit = "NotifyChargingLimit", NotifyChargingLimitRequest => NotifyChargingLimitResponse, OutboundAction
}
action! {
    /// `ClearedChargingLimit` (station -> CSMS).
    ClearedChargingLimit = "ClearedChargingLimit", ClearedChargingLimitRequest => ClearedChargingLimitResponse, OutboundAction
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::ocpp::v2::test_support::{check_action, check_enum};

    fn cd() -> Value {
        json!({"vendorId": "acme", "x": {"y": [1, null]}})
    }

    fn profile() -> Value {
        json!({
            "customData": cd(),
            "id": 4,
            "stackLevel": 2,
            "chargingProfilePurpose": "TxProfile",
            "chargingProfileKind": "Recurring",
            "recurrencyKind": "Weekly",
            "validFrom": "2026-05-06T07:08:09.000Z",
            "validTo": "2026-06-06T07:08:09.500Z",
            "transactionId": "tx-1",
            "chargingSchedule": [{
                "customData": cd(),
                "id": 1,
                "startSchedule": "2026-05-06T07:08:09.000Z",
                "duration": 3600,
                "chargingRateUnit": "A",
                "minChargingRate": 6.5,
                "chargingSchedulePeriod": [
                    {"customData": cd(), "startPeriod": 0, "limit": 16.5, "numberPhases": 1, "phaseToUse": 1},
                    {"startPeriod": 600, "limit": 8.0}
                ]
            }]
        })
    }

    #[test]
    fn enums_match_schema() {
        check_enum::<ChargingProfileStatus>(
            "SetChargingProfileResponse.json",
            "ChargingProfileStatusEnumType",
        );
        check_enum::<ClearChargingProfileStatus>(
            "ClearChargingProfileResponse.json",
            "ClearChargingProfileStatusEnumType",
        );
        check_enum::<GetChargingProfileStatus>(
            "GetChargingProfilesResponse.json",
            "GetChargingProfileStatusEnumType",
        );
        check_enum::<CompositeScheduleStatus>(
            "GetCompositeScheduleResponse.json",
            "GenericStatusEnumType",
        );
        check_enum::<ChargingRateUnit>(
            "GetCompositeScheduleRequest.json",
            "ChargingRateUnitEnumType",
        );
        check_enum::<RecurrencyKind>("SetChargingProfileRequest.json", "RecurrencyKindEnumType");
    }

    #[test]
    fn the_typed_profile_reads_the_evaluated_fields() {
        let typed: ChargingProfile = serde_json::from_value(profile()).expect("decodes");
        assert_eq!(typed.id, 4);
        assert_eq!(typed.stack_level, 2);
        assert_eq!(
            typed.charging_profile_purpose,
            ChargingProfilePurpose::TxProfile
        );
        assert_eq!(typed.charging_profile_kind, ChargingProfileKind::Recurring);
        assert_eq!(typed.recurrency_kind, Some(RecurrencyKind::Weekly));
        assert_eq!(typed.transaction_id.as_deref(), Some("tx-1"));
        let schedule = &typed.charging_schedule[0];
        assert_eq!(schedule.duration, Some(3600));
        assert_eq!(schedule.charging_rate_unit, ChargingRateUnit::A);
        assert_eq!(schedule.charging_schedule_period[0].limit, Some(16.5));
        assert_eq!(schedule.charging_schedule_period[0].number_phases, Some(1));
        assert_eq!(schedule.charging_schedule_period[1].number_phases, None);
        // Only the 2.0.1 fields: the typed form roundtrips them.
        assert_eq!(serde_json::to_value(&typed).expect("encodes"), profile());
    }

    #[test]
    fn set_charging_profile() {
        check_action::<SetChargingProfile>(
            &json!({"customData": cd(), "evseId": 1, "chargingProfile": profile()}),
            &json!({
                "customData": cd(),
                "status": "Rejected",
                "statusInfo": {"reasonCode": "TxNotFound", "additionalInfo": "x"}
            }),
        );
        check_action::<SetChargingProfile>(
            &json!({"evseId": 0, "chargingProfile": {
                "id": 1, "stackLevel": 0,
                "chargingProfilePurpose": "ChargingStationMaxProfile",
                "chargingProfileKind": "Absolute",
                "chargingSchedule": [{"id": 1, "chargingRateUnit": "W",
                    "chargingSchedulePeriod": [{"startPeriod": 0, "limit": 7000}]}]
            }}),
            &json!({"status": "Accepted"}),
        );
    }

    #[test]
    fn clear_charging_profile() {
        check_action::<ClearChargingProfile>(
            &json!({
                "customData": cd(),
                "chargingProfileId": 3,
                "chargingProfileCriteria": {
                    "customData": cd(),
                    "evseId": 1,
                    "chargingProfilePurpose": "TxDefaultProfile",
                    "stackLevel": 2
                }
            }),
            &json!({
                "customData": cd(),
                "status": "Unknown",
                "statusInfo": {"reasonCode": "NotFound"}
            }),
        );
        check_action::<ClearChargingProfile>(&json!({}), &json!({"status": "Accepted"}));
    }

    #[test]
    fn get_charging_profiles() {
        check_action::<GetChargingProfiles>(
            &json!({
                "customData": cd(),
                "requestId": 9,
                "evseId": 0,
                "chargingProfile": {
                    "customData": cd(),
                    "chargingProfilePurpose": "ChargingStationExternalConstraints",
                    "stackLevel": 1,
                    "chargingProfileId": [1, 2],
                    "chargingLimitSource": ["EMS", "CSO"]
                }
            }),
            &json!({
                "customData": cd(),
                "status": "NoProfiles",
                "statusInfo": {"reasonCode": "NotFound"}
            }),
        );
        check_action::<GetChargingProfiles>(
            &json!({"requestId": 0, "chargingProfile": {}}),
            &json!({"status": "Accepted"}),
        );
    }

    #[test]
    fn report_charging_profiles() {
        check_action::<ReportChargingProfiles>(
            &json!({
                "customData": cd(),
                "requestId": 9,
                "chargingLimitSource": "CSO",
                "chargingProfile": [profile()],
                "tbc": true,
                "evseId": 1
            }),
            &json!({"customData": cd()}),
        );
        check_action::<ReportChargingProfiles>(
            &json!({
                "requestId": 0,
                "chargingLimitSource": "EMS",
                "chargingProfile": [profile(), profile()],
                "evseId": 0
            }),
            &json!({}),
        );
    }

    #[test]
    fn get_composite_schedule() {
        check_action::<GetCompositeSchedule>(
            &json!({"customData": cd(), "duration": 300, "chargingRateUnit": "W", "evseId": 1}),
            &json!({
                "customData": cd(),
                "status": "Accepted",
                "statusInfo": {"reasonCode": "x"},
                "schedule": {
                    "customData": cd(),
                    "chargingSchedulePeriod": [
                        {"startPeriod": 0, "limit": 7000.0, "numberPhases": 3},
                        {"startPeriod": 100, "limit": 3000.0}
                    ],
                    "evseId": 1,
                    "duration": 300,
                    "scheduleStart": "2026-05-06T07:08:09.000Z",
                    "chargingRateUnit": "W"
                }
            }),
        );
        check_action::<GetCompositeSchedule>(
            &json!({"duration": 1, "evseId": 0}),
            &json!({"status": "Rejected"}),
        );
    }

    #[test]
    fn notify_charging_limit() {
        check_action::<NotifyChargingLimit>(
            &json!({
                "customData": cd(),
                "chargingSchedule": [{
                    "id": 1, "chargingRateUnit": "W",
                    "chargingSchedulePeriod": [{"startPeriod": 0, "limit": 5000.0}]
                }],
                "evseId": 2,
                "chargingLimit": {
                    "customData": cd(),
                    "chargingLimitSource": "EMS",
                    "isGridCritical": true
                }
            }),
            &json!({"customData": cd()}),
        );
        check_action::<NotifyChargingLimit>(
            &json!({"chargingLimit": {"chargingLimitSource": "Other"}}),
            &json!({}),
        );
    }

    #[test]
    fn cleared_charging_limit() {
        check_action::<ClearedChargingLimit>(
            &json!({"customData": cd(), "chargingLimitSource": "SO", "evseId": 1}),
            &json!({"customData": cd()}),
        );
        check_action::<ClearedChargingLimit>(&json!({"chargingLimitSource": "EMS"}), &json!({}));
    }
}
