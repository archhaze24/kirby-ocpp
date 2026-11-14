use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ocpp::v2::types::{CustomData, StatusInfo, helpers, payload, wire_enum};

wire_enum! {
    /// Result of `DataTransfer` (`DataTransferStatusEnumType`).
    DataTransferStatus {
        Accepted = "Accepted",
        Rejected = "Rejected",
        UnknownMessageId = "UnknownMessageId",
        UnknownVendorId = "UnknownVendorId",
    }
}

payload! {
    /// `DataTransfer.req`.
    DataTransferRequest {
        /// Max 50 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub message_id: Option<String>,
        /// Vendor-defined JSON of any type. `None` is an absent property;
        /// `Some(Value::Null)` is an explicit JSON `null`.
        #[serde(default, deserialize_with = "helpers::any_type::deserialize", skip_serializing_if = "Option::is_none")]
        pub data: Option<Value>,
        /// Max 255 characters.
        pub vendor_id: String,
    }
}

payload! {
    /// `DataTransfer.conf`.
    DataTransferResponse {
        pub status: DataTransferStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
        /// Vendor-defined JSON of any type. `None` is an absent property;
        /// `Some(Value::Null)` is an explicit JSON `null`.
        #[serde(default, deserialize_with = "helpers::any_type::deserialize", skip_serializing_if = "Option::is_none")]
        pub data: Option<Value>,
    }
}

payload! {
    /// `SecurityEventNotification.req`.
    SecurityEventNotificationRequest {
        /// Max 50 characters.
        #[serde(rename = "type")]
        pub kind: String,
        #[serde(with = "helpers::ts")]
        pub timestamp: DateTime<Utc>,
        /// Max 255 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tech_info: Option<String>,
    }
}

payload! {
    /// `SecurityEventNotification.conf`: no fields besides `customData`.
    SecurityEventNotificationResponse {}
}

action! {
    /// `DataTransfer` (either direction).
    DataTransfer = "DataTransfer", DataTransferRequest => DataTransferResponse, OutboundAction, InboundAction
}
action! {
    /// `SecurityEventNotification` (station -> CSMS).
    SecurityEventNotification = "SecurityEventNotification", SecurityEventNotificationRequest => SecurityEventNotificationResponse, OutboundAction
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ocpp::action::{InboundAction, OutboundAction};
    use crate::ocpp::v2::test_support::{check_action, check_enum};

    fn cd() -> Value {
        json!({"vendorId": "acme", "x": {"y": [1, null, "z"]}})
    }

    fn status_info() -> Value {
        json!({"customData": cd(), "reasonCode": "Reason", "additionalInfo": "detail"})
    }

    #[test]
    fn enums_match_schema() {
        check_enum::<DataTransferStatus>("DataTransferResponse.json", "DataTransferStatusEnumType");
    }

    #[test]
    fn data_transfer() {
        check_action::<DataTransfer>(
            &json!({
                "customData": cd(),
                "messageId": "m",
                "data": {"a": [1, 2.5, null, {"b": "c"}]},
                "vendorId": "acme"
            }),
            &json!({
                "customData": cd(),
                "status": "UnknownMessageId",
                "statusInfo": status_info(),
                "data": ["any", 1]
            }),
        );
        check_action::<DataTransfer>(&json!({"vendorId": "v"}), &json!({"status": "Accepted"}));
    }

    #[test]
    fn data_transfer_any_type_scalars_survive() {
        for data in [
            json!("text"),
            json!(7),
            json!(1.5),
            json!(true),
            json!([]),
            json!({}),
        ] {
            check_action::<DataTransfer>(
                &json!({"vendorId": "v", "data": data}),
                &json!({"status": "Accepted", "data": data}),
            );
        }
    }

    #[test]
    fn data_transfer_distinguishes_null_from_absent() {
        let with_null: DataTransferRequest =
            serde_json::from_value(json!({"vendorId": "v", "data": null})).unwrap();
        assert_eq!(with_null.data, Some(Value::Null));
        assert_eq!(
            serde_json::to_value(&with_null).unwrap(),
            json!({"vendorId": "v", "data": null})
        );

        let absent: DataTransferRequest = serde_json::from_value(json!({"vendorId": "v"})).unwrap();
        assert_eq!(absent.data, None);
        assert_eq!(
            serde_json::to_value(&absent).unwrap(),
            json!({"vendorId": "v"})
        );

        let res_null: DataTransferResponse =
            serde_json::from_value(json!({"status": "Accepted", "data": null})).unwrap();
        assert_eq!(res_null.data, Some(Value::Null));
        assert_eq!(
            serde_json::to_value(&res_null).unwrap(),
            json!({"status": "Accepted", "data": null})
        );
        let res_absent: DataTransferResponse =
            serde_json::from_value(json!({"status": "Accepted"})).unwrap();
        assert_eq!(res_absent.data, None);
    }

    #[test]
    fn data_transfer_null_passes_the_schema() {
        check_action::<DataTransfer>(
            &json!({"vendorId": "v", "data": null}),
            &json!({"status": "Rejected", "data": null}),
        );
    }

    #[test]
    fn security_event_notification() {
        check_action::<SecurityEventNotification>(
            &json!({
                "customData": cd(),
                "type": "FirmwareUpdated",
                "timestamp": "2026-05-06T07:08:09.123Z",
                "techInfo": "v1.2"
            }),
            &json!({"customData": cd()}),
        );
        check_action::<SecurityEventNotification>(
            &json!({"type": "StartupOfTheDevice", "timestamp": "2026-05-06T07:08:09.000Z"}),
            &json!({}),
        );
    }

    #[test]
    fn data_transfer_is_bidirectional() {
        fn outbound<A: OutboundAction>() {}
        fn inbound<A: InboundAction>() {}
        outbound::<DataTransfer>();
        inbound::<DataTransfer>();
    }
}
