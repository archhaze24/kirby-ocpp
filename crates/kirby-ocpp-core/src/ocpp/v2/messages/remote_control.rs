use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ocpp::v2::types::{CustomData, Evse, IdToken, StatusInfo, payload, wire_enum};

wire_enum! {
    /// Kind of reset (`ResetEnumType`).
    ResetKind {
        Immediate = "Immediate",
        OnIdle = "OnIdle",
    }
}

wire_enum! {
    /// Result of `Reset` (`ResetStatusEnumType`).
    ResetStatus {
        Accepted = "Accepted",
        Rejected = "Rejected",
        Scheduled = "Scheduled",
    }
}

wire_enum! {
    /// Requested availability (`OperationalStatusEnumType`).
    OperationalStatus {
        Inoperative = "Inoperative",
        Operative = "Operative",
    }
}

wire_enum! {
    /// Result of `ChangeAvailability` (`ChangeAvailabilityStatusEnumType`).
    ChangeAvailabilityStatus {
        Accepted = "Accepted",
        Rejected = "Rejected",
        Scheduled = "Scheduled",
    }
}

wire_enum! {
    /// Result of `RequestStartTransaction` / `RequestStopTransaction`
    /// (`RequestStartStopStatusEnumType`).
    RequestStartStopStatus {
        Accepted = "Accepted",
        Rejected = "Rejected",
    }
}

wire_enum! {
    /// Result of `UnlockConnector` (`UnlockStatusEnumType`).
    UnlockStatus {
        Unlocked = "Unlocked",
        UnlockFailed = "UnlockFailed",
        OngoingAuthorizedTransaction = "OngoingAuthorizedTransaction",
        UnknownConnector = "UnknownConnector",
    }
}

wire_enum! {
    /// Message the station is asked to send (`MessageTriggerEnumType`).
    MessageTrigger {
        BootNotification = "BootNotification",
        LogStatusNotification = "LogStatusNotification",
        FirmwareStatusNotification = "FirmwareStatusNotification",
        Heartbeat = "Heartbeat",
        MeterValues = "MeterValues",
        SignChargingStationCertificate = "SignChargingStationCertificate",
        SignV2gCertificate = "SignV2GCertificate",
        StatusNotification = "StatusNotification",
        TransactionEvent = "TransactionEvent",
        SignCombinedCertificate = "SignCombinedCertificate",
        PublishFirmwareStatusNotification = "PublishFirmwareStatusNotification",
    }
}

wire_enum! {
    /// Result of `TriggerMessage` (`TriggerMessageStatusEnumType`).
    TriggerMessageStatus {
        Accepted = "Accepted",
        Rejected = "Rejected",
        NotImplemented = "NotImplemented",
    }
}

payload! {
    /// `Reset.req`.
    ResetRequest {
        #[serde(rename = "type")]
        pub kind: ResetKind,
        /// Absent resets the whole station.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub evse_id: Option<i32>,
    }
}

payload! {
    /// `Reset.conf`.
    ResetResponse {
        pub status: ResetStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

payload! {
    /// `ChangeAvailability.req`.
    ChangeAvailabilityRequest {
        /// Absent addresses the whole station.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub evse: Option<Evse>,
        pub operational_status: OperationalStatus,
    }
}

payload! {
    /// `ChangeAvailability.conf`.
    ChangeAvailabilityResponse {
        pub status: ChangeAvailabilityStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

payload! {
    /// `RequestStartTransaction.req`.
    RequestStartTransactionRequest {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub evse_id: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub group_id_token: Option<IdToken>,
        pub id_token: IdToken,
        /// Echoed back by the station in `TransactionEvent.transactionInfo.remoteStartId`.
        pub remote_start_id: i32,
        /// `ChargingProfileType` kept as raw JSON (profile, schedules, sales
        /// tariff); the schema validates its shape.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub charging_profile: Option<Value>,
    }
}

payload! {
    /// `RequestStartTransaction.conf`.
    RequestStartTransactionResponse {
        pub status: RequestStartStopStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
        /// Max 36 characters; set when the transaction already started.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub transaction_id: Option<String>,
    }
}

payload! {
    /// `RequestStopTransaction.req`.
    RequestStopTransactionRequest {
        /// Max 36 characters.
        pub transaction_id: String,
    }
}

payload! {
    /// `RequestStopTransaction.conf`.
    RequestStopTransactionResponse {
        pub status: RequestStartStopStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

payload! {
    /// `UnlockConnector.req`.
    UnlockConnectorRequest {
        pub evse_id: i32,
        pub connector_id: i32,
    }
}

payload! {
    /// `UnlockConnector.conf`.
    UnlockConnectorResponse {
        pub status: UnlockStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

payload! {
    /// `TriggerMessage.req`.
    TriggerMessageRequest {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub evse: Option<Evse>,
        pub requested_message: MessageTrigger,
    }
}

payload! {
    /// `TriggerMessage.conf`.
    TriggerMessageResponse {
        pub status: TriggerMessageStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

action! {
    /// `Reset` (CSMS -> station).
    Reset = "Reset", ResetRequest => ResetResponse, InboundAction
}
action! {
    /// `ChangeAvailability` (CSMS -> station).
    ChangeAvailability = "ChangeAvailability", ChangeAvailabilityRequest => ChangeAvailabilityResponse, InboundAction
}
action! {
    /// `RequestStartTransaction` (CSMS -> station).
    RequestStartTransaction = "RequestStartTransaction", RequestStartTransactionRequest => RequestStartTransactionResponse, InboundAction
}
action! {
    /// `RequestStopTransaction` (CSMS -> station).
    RequestStopTransaction = "RequestStopTransaction", RequestStopTransactionRequest => RequestStopTransactionResponse, InboundAction
}
action! {
    /// `UnlockConnector` (CSMS -> station).
    UnlockConnector = "UnlockConnector", UnlockConnectorRequest => UnlockConnectorResponse, InboundAction
}
action! {
    /// `TriggerMessage` (CSMS -> station).
    TriggerMessage = "TriggerMessage", TriggerMessageRequest => TriggerMessageResponse, InboundAction
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ocpp::v2::test_support::{check_action, check_enum};

    fn cd() -> Value {
        json!({"vendorId": "acme", "x": {"y": [1, null, "z"]}})
    }

    fn status_info() -> Value {
        json!({"customData": cd(), "reasonCode": "Reason", "additionalInfo": "detail"})
    }

    fn id_token() -> Value {
        json!({
            "customData": cd(),
            "additionalInfo": [{"customData": cd(), "additionalIdToken": "e", "type": "k"}],
            "idToken": "TOKEN",
            "type": "KeyCode"
        })
    }

    #[test]
    fn enums_match_schema() {
        check_enum::<ResetKind>("ResetRequest.json", "ResetEnumType");
        check_enum::<ResetStatus>("ResetResponse.json", "ResetStatusEnumType");
        check_enum::<OperationalStatus>(
            "ChangeAvailabilityRequest.json",
            "OperationalStatusEnumType",
        );
        check_enum::<ChangeAvailabilityStatus>(
            "ChangeAvailabilityResponse.json",
            "ChangeAvailabilityStatusEnumType",
        );
        check_enum::<RequestStartStopStatus>(
            "RequestStartTransactionResponse.json",
            "RequestStartStopStatusEnumType",
        );
        check_enum::<UnlockStatus>("UnlockConnectorResponse.json", "UnlockStatusEnumType");
        check_enum::<MessageTrigger>("TriggerMessageRequest.json", "MessageTriggerEnumType");
        check_enum::<TriggerMessageStatus>(
            "TriggerMessageResponse.json",
            "TriggerMessageStatusEnumType",
        );
    }

    #[test]
    fn reset() {
        check_action::<Reset>(
            &json!({"customData": cd(), "type": "OnIdle", "evseId": 1}),
            &json!({"customData": cd(), "status": "Scheduled", "statusInfo": status_info()}),
        );
        check_action::<Reset>(
            &json!({"type": "Immediate"}),
            &json!({"status": "Accepted"}),
        );
    }

    #[test]
    fn change_availability() {
        check_action::<ChangeAvailability>(
            &json!({
                "customData": cd(),
                "evse": {"customData": cd(), "id": 2, "connectorId": 1},
                "operationalStatus": "Inoperative"
            }),
            &json!({"customData": cd(), "status": "Scheduled", "statusInfo": status_info()}),
        );
        check_action::<ChangeAvailability>(
            &json!({"operationalStatus": "Operative"}),
            &json!({"status": "Rejected"}),
        );
    }

    #[test]
    fn request_start_transaction() {
        let profile = json!({
            "customData": cd(),
            "id": 1,
            "stackLevel": 0,
            "chargingProfilePurpose": "TxProfile",
            "chargingProfileKind": "Absolute",
            "recurrencyKind": "Daily",
            "validFrom": "2026-05-06T07:08:09.000Z",
            "validTo": "2026-05-07T07:08:09.000Z",
            "chargingSchedule": [{
                "id": 1,
                "startSchedule": "2026-05-06T07:08:09.000Z",
                "duration": 3600,
                "chargingRateUnit": "A",
                "chargingSchedulePeriod": [
                    {"startPeriod": 0, "limit": 16.5, "numberPhases": 3, "phaseToUse": 1}
                ],
                "minChargingRate": 6.5,
                "salesTariff": {
                    "id": 1,
                    "salesTariffDescription": "tariff",
                    "numEPriceLevels": 1,
                    "salesTariffEntry": [{
                        "relativeTimeInterval": {"start": 0, "duration": 60},
                        "ePriceLevel": 1,
                        "consumptionCost": [{
                            "startValue": 0.5,
                            "cost": [{
                                "costKind": "CarbonDioxideEmission",
                                "amount": 5,
                                "amountMultiplier": -1
                            }]
                        }]
                    }]
                }
            }],
            "transactionId": "tx-1"
        });
        check_action::<RequestStartTransaction>(
            &json!({
                "customData": cd(),
                "evseId": 1,
                "groupIdToken": id_token(),
                "idToken": id_token(),
                "remoteStartId": 77,
                "chargingProfile": profile
            }),
            &json!({
                "customData": cd(),
                "status": "Accepted",
                "statusInfo": status_info(),
                "transactionId": "tx-1"
            }),
        );
        check_action::<RequestStartTransaction>(
            &json!({"idToken": {"idToken": "T", "type": "Central"}, "remoteStartId": 1}),
            &json!({"status": "Rejected"}),
        );
    }

    #[test]
    fn request_stop_transaction() {
        check_action::<RequestStopTransaction>(
            &json!({"customData": cd(), "transactionId": "tx-1"}),
            &json!({"customData": cd(), "status": "Rejected", "statusInfo": status_info()}),
        );
        check_action::<RequestStopTransaction>(
            &json!({"transactionId": "t"}),
            &json!({"status": "Accepted"}),
        );
    }

    #[test]
    fn unlock_connector() {
        check_action::<UnlockConnector>(
            &json!({"customData": cd(), "evseId": 1, "connectorId": 1}),
            &json!({"customData": cd(), "status": "UnknownConnector", "statusInfo": status_info()}),
        );
        check_action::<UnlockConnector>(
            &json!({"evseId": 1, "connectorId": 2}),
            &json!({"status": "Unlocked"}),
        );
    }

    #[test]
    fn trigger_message() {
        check_action::<TriggerMessage>(
            &json!({
                "customData": cd(),
                "evse": {"customData": cd(), "id": 1, "connectorId": 1},
                "requestedMessage": "SignV2GCertificate"
            }),
            &json!({"customData": cd(), "status": "NotImplemented", "statusInfo": status_info()}),
        );
        check_action::<TriggerMessage>(
            &json!({"requestedMessage": "Heartbeat"}),
            &json!({"status": "Accepted"}),
        );
    }
}
