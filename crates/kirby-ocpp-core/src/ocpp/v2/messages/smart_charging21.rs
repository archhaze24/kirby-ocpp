use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ChargingProfileStatus;
use crate::ocpp::v2::types::{CustomData, StatusInfo, payload, wire_enum};

wire_enum! {
    PriorityChargingStatus {
        Accepted = "Accepted",
        Rejected = "Rejected",
        NoProfile = "NoProfile",
    }
}

payload! {
    UsePriorityChargingRequest {
        pub transaction_id: String,
        pub activate: bool,
    }
}

payload! {
    UsePriorityChargingResponse {
        pub status: PriorityChargingStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

action! {
    UsePriorityCharging = "UsePriorityCharging", UsePriorityChargingRequest => UsePriorityChargingResponse, InboundAction
}

payload! {
    NotifyPriorityChargingRequest {
        pub transaction_id: String,
        pub activated: bool,
    }
}

payload! {
    NotifyPriorityChargingResponse {}
}

action! {
    NotifyPriorityCharging = "NotifyPriorityCharging", NotifyPriorityChargingRequest => NotifyPriorityChargingResponse, OutboundAction
}

payload! {
    UpdateDynamicScheduleRequest {
        pub charging_profile_id: i32,
        pub schedule_update: Value,
    }
}

payload! {
    UpdateDynamicScheduleResponse {
        pub status: ChargingProfileStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

action! {
    UpdateDynamicSchedule = "UpdateDynamicSchedule", UpdateDynamicScheduleRequest => UpdateDynamicScheduleResponse, InboundAction
}

payload! {
    PullDynamicScheduleUpdateRequest {
        pub charging_profile_id: i32,
    }
}

payload! {
    PullDynamicScheduleUpdateResponse {
        pub status: ChargingProfileStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub schedule_update: Option<Value>,
    }
}

action! {
    PullDynamicScheduleUpdate = "PullDynamicScheduleUpdate", PullDynamicScheduleUpdateRequest => PullDynamicScheduleUpdateResponse, OutboundAction
}
