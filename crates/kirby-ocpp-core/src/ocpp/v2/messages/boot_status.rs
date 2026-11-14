use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ocpp::v2::types::{
    ChargingStation, CustomData, StatusInfo, helpers, payload, wire_enum,
};

wire_enum! {
    /// Why a boot notification was sent (`BootReasonEnumType`).
    BootReason {
        ApplicationReset = "ApplicationReset",
        FirmwareUpdate = "FirmwareUpdate",
        LocalReset = "LocalReset",
        PowerUp = "PowerUp",
        RemoteReset = "RemoteReset",
        ScheduledReset = "ScheduledReset",
        Triggered = "Triggered",
        Unknown = "Unknown",
        Watchdog = "Watchdog",
    }
}

wire_enum! {
    /// Registration result (`RegistrationStatusEnumType`).
    RegistrationStatus {
        Accepted = "Accepted",
        Pending = "Pending",
        Rejected = "Rejected",
    }
}

wire_enum! {
    /// Connector state (`ConnectorStatusEnumType`).
    ConnectorStatus {
        Available = "Available",
        Occupied = "Occupied",
        Reserved = "Reserved",
        Unavailable = "Unavailable",
        Faulted = "Faulted",
    }
}

payload! {
    /// `BootNotification.req`.
    BootNotificationRequest {
        pub charging_station: ChargingStation,
        pub reason: BootReason,
    }
}

payload! {
    /// `BootNotification.conf`.
    BootNotificationResponse {
        #[serde(with = "helpers::ts")]
        pub current_time: DateTime<Utc>,
        /// Heartbeat interval in seconds.
        pub interval: i32,
        pub status: RegistrationStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

payload! {
    /// `Heartbeat.req`: no fields besides `customData`.
    HeartbeatRequest {}
}

payload! {
    /// `Heartbeat.conf`.
    HeartbeatResponse {
        #[serde(with = "helpers::ts")]
        pub current_time: DateTime<Utc>,
    }
}

payload! {
    /// `StatusNotification.req`.
    StatusNotificationRequest {
        #[serde(with = "helpers::ts")]
        pub timestamp: DateTime<Utc>,
        pub connector_status: ConnectorStatus,
        pub evse_id: i32,
        pub connector_id: i32,
    }
}

payload! {
    /// `StatusNotification.conf`: no fields besides `customData`.
    StatusNotificationResponse {}
}

action! {
    /// `BootNotification` (station -> CSMS).
    BootNotification = "BootNotification", BootNotificationRequest => BootNotificationResponse, OutboundAction
}
action! {
    /// `Heartbeat` (station -> CSMS).
    Heartbeat = "Heartbeat", HeartbeatRequest => HeartbeatResponse, OutboundAction
}
action! {
    /// `StatusNotification` (station -> CSMS).
    StatusNotification = "StatusNotification", StatusNotificationRequest => StatusNotificationResponse, OutboundAction
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::ocpp::v2::test_support::{check_action, check_enum};

    fn cd() -> Value {
        json!({"vendorId": "acme", "flag": true, "nested": {"a": [1, 2, {"b": null}]}})
    }

    #[test]
    fn enums_match_schema() {
        check_enum::<BootReason>("BootNotificationRequest.json", "BootReasonEnumType");
        check_enum::<RegistrationStatus>(
            "BootNotificationResponse.json",
            "RegistrationStatusEnumType",
        );
        check_enum::<ConnectorStatus>("StatusNotificationRequest.json", "ConnectorStatusEnumType");
    }

    #[test]
    fn boot_notification() {
        let request_full = json!({
            "customData": cd(),
            "chargingStation": {
                "customData": cd(),
                "serialNumber": "SN-1",
                "model": "Model",
                "modem": {"customData": cd(), "iccid": "8931", "imsi": "2620"},
                "vendorName": "Vendor",
                "firmwareVersion": "1.2.3"
            },
            "reason": "PowerUp"
        });
        let response_full = json!({
            "customData": cd(),
            "currentTime": "2026-05-06T07:08:09.123Z",
            "interval": 300,
            "status": "Pending",
            "statusInfo": {"customData": cd(), "reasonCode": "Busy", "additionalInfo": "later"}
        });
        check_action::<BootNotification>(&request_full, &response_full);
        let request_min = json!({
            "chargingStation": {"model": "M", "vendorName": "V"},
            "reason": "Unknown"
        });
        let response_min = json!({
            "currentTime": "2026-05-06T07:08:09.000Z",
            "interval": 0,
            "status": "Accepted"
        });
        check_action::<BootNotification>(&request_min, &response_min);
    }

    #[test]
    fn boot_notification_requires_reason_and_station() {
        assert!(
            serde_json::from_value::<BootNotificationRequest>(
                json!({"chargingStation": {"model": "M", "vendorName": "V"}})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<BootNotificationRequest>(json!({"reason": "PowerUp"}))
                .is_err()
        );
    }

    #[test]
    fn heartbeat() {
        check_action::<Heartbeat>(
            &json!({"customData": cd()}),
            &json!({"customData": cd(), "currentTime": "2026-05-06T07:08:09.007Z"}),
        );
        check_action::<Heartbeat>(
            &json!({}),
            &json!({"currentTime": "2026-05-06T07:08:09.000Z"}),
        );
    }

    #[test]
    fn status_notification() {
        let req = json!({
            "customData": cd(),
            "timestamp": "2026-05-06T07:08:09.500Z",
            "connectorStatus": "Occupied",
            "evseId": 1,
            "connectorId": 2
        });
        check_action::<StatusNotification>(&req, &json!({"customData": cd()}));
        check_action::<StatusNotification>(
            &json!({
                "timestamp": "2026-05-06T07:08:09.000Z",
                "connectorStatus": "Faulted",
                "evseId": 1,
                "connectorId": 1
            }),
            &json!({}),
        );
    }

    #[test]
    fn status_notification_requires_evse_and_connector() {
        assert!(
            serde_json::from_value::<StatusNotificationRequest>(json!({
                "timestamp": "2026-05-06T07:08:09.000Z",
                "connectorStatus": "Available"
            }))
            .is_err()
        );
    }
}
