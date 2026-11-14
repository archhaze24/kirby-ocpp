use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ocpp::v2::types::{
    CustomData, Evse, IdToken, IdTokenInfo, MessageContent, MeterValue, Transaction, helpers,
    payload, wire_enum,
};

wire_enum! {
    /// Position of a `TransactionEvent` in its transaction
    /// (`TransactionEventEnumType`).
    TransactionEventKind {
        Ended = "Ended",
        Started = "Started",
        Updated = "Updated",
    }
}

wire_enum! {
    /// What caused a `TransactionEvent` (`TriggerReasonEnumType`).
    TriggerReason {
        Authorized = "Authorized",
        CablePluggedIn = "CablePluggedIn",
        ChargingRateChanged = "ChargingRateChanged",
        ChargingStateChanged = "ChargingStateChanged",
        Deauthorized = "Deauthorized",
        EnergyLimitReached = "EnergyLimitReached",
        EvCommunicationLost = "EVCommunicationLost",
        EvConnectTimeout = "EVConnectTimeout",
        MeterValueClock = "MeterValueClock",
        MeterValuePeriodic = "MeterValuePeriodic",
        TimeLimitReached = "TimeLimitReached",
        Trigger = "Trigger",
        UnlockCommand = "UnlockCommand",
        StopAuthorized = "StopAuthorized",
        EvDeparted = "EVDeparted",
        EvDetected = "EVDetected",
        RemoteStop = "RemoteStop",
        RemoteStart = "RemoteStart",
        AbnormalCondition = "AbnormalCondition",
        SignedDataReceived = "SignedDataReceived",
        ResetCommand = "ResetCommand",
    }
}

payload! {
    /// `TransactionEvent.req`.
    ///
    /// `evse`, `id_token` and the other optional fields are often sent only
    /// when they change; absent means "unchanged", not "cleared".
    TransactionEventRequest {
        pub event_type: TransactionEventKind,
        /// Absent or non-empty.
        #[serde(default, skip_serializing_if = "helpers::omit_empty")]
        pub meter_value: Option<Vec<MeterValue>>,
        #[serde(with = "helpers::ts")]
        pub timestamp: DateTime<Utc>,
        pub trigger_reason: TriggerReason,
        /// Sequence number within the transaction.
        pub seq_no: i32,
        /// Set when the event was queued while offline. Not defaulted when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub offline: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub number_of_phases_used: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cable_max_current: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reservation_id: Option<i32>,
        pub transaction_info: Transaction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub evse: Option<Evse>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub id_token: Option<IdToken>,
    }
}

payload! {
    /// `TransactionEvent.conf`.
    TransactionEventResponse {
        /// Running or final cost in the currency configured on the station.
        /// Decoded as `f64`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub total_cost: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub charging_priority: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub id_token_info: Option<IdTokenInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub updated_personal_message: Option<MessageContent>,
    }
}

payload! {
    /// `MeterValues.req`.
    MeterValuesRequest {
        /// EVSE the readings belong to; 0 is the whole station.
        pub evse_id: i32,
        /// At least one entry.
        pub meter_value: Vec<MeterValue>,
    }
}

payload! {
    /// `MeterValues.conf`: no fields besides `customData`.
    MeterValuesResponse {}
}

payload! {
    /// `GetTransactionStatus.req`.
    GetTransactionStatusRequest {
        /// Max 36 characters. Absent asks about queued messages overall.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub transaction_id: Option<String>,
    }
}

payload! {
    /// `GetTransactionStatus.conf`.
    GetTransactionStatusResponse {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub ongoing_indicator: Option<bool>,
        pub messages_in_queue: bool,
    }
}

action! {
    /// `TransactionEvent` (station -> CSMS).
    TransactionEvent = "TransactionEvent", TransactionEventRequest => TransactionEventResponse, OutboundAction
}
action! {
    /// `MeterValues` (station -> CSMS).
    MeterValues = "MeterValues", MeterValuesRequest => MeterValuesResponse, OutboundAction
}
action! {
    /// `GetTransactionStatus` (CSMS -> station).
    GetTransactionStatus = "GetTransactionStatus", GetTransactionStatusRequest => GetTransactionStatusResponse, InboundAction
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::ocpp::v2::test_support::{check_action, check_enum};

    fn cd() -> Value {
        json!({"vendorId": "acme", "x": [{"y": 1.5}, null]})
    }

    fn meter_value() -> Value {
        json!({
            "customData": cd(),
            "timestamp": "2026-05-06T07:08:09.123Z",
            "sampledValue": [
                {
                    "customData": cd(),
                    "value": 1234.5,
                    "context": "Sample.Periodic",
                    "measurand": "Energy.Active.Import.Register",
                    "phase": "L1-N",
                    "location": "Outlet",
                    "signedMeterValue": {
                        "customData": cd(),
                        "signedMeterData": "AAEC",
                        "signingMethod": "ECDSA",
                        "encodingMethod": "OCMF",
                        "publicKey": "key"
                    },
                    "unitOfMeasure": {"customData": cd(), "unit": "kWh", "multiplier": -3}
                },
                {"value": 7}
            ]
        })
    }

    fn id_token() -> Value {
        json!({
            "customData": cd(),
            "additionalInfo": [{"customData": cd(), "additionalIdToken": "e", "type": "k"}],
            "idToken": "TOKEN",
            "type": "ISO15693"
        })
    }

    #[test]
    fn enums_match_schema() {
        check_enum::<TransactionEventKind>(
            "TransactionEventRequest.json",
            "TransactionEventEnumType",
        );
        check_enum::<TriggerReason>("TransactionEventRequest.json", "TriggerReasonEnumType");
    }

    #[test]
    fn transaction_event() {
        let req = json!({
            "customData": cd(),
            "eventType": "Updated",
            "meterValue": [meter_value()],
            "timestamp": "2026-05-06T07:08:09.001Z",
            "triggerReason": "ChargingStateChanged",
            "seqNo": 3,
            "offline": false,
            "numberOfPhasesUsed": 3,
            "cableMaxCurrent": 32,
            "reservationId": 7,
            "transactionInfo": {
                "customData": cd(),
                "transactionId": "tx-0123456789",
                "chargingState": "SuspendedEVSE",
                "timeSpentCharging": 60,
                "stoppedReason": "SOCLimitReached",
                "remoteStartId": 11
            },
            "evse": {"customData": cd(), "id": 1, "connectorId": 1},
            "idToken": id_token()
        });
        let res = json!({
            "customData": cd(),
            "totalCost": 12.5,
            "chargingPriority": 2,
            "idTokenInfo": {"status": "Accepted", "evseId": [1]},
            "updatedPersonalMessage": {"format": "ASCII", "content": "Thanks"}
        });
        check_action::<TransactionEvent>(&req, &res);
        check_action::<TransactionEvent>(
            &json!({
                "eventType": "Started",
                "timestamp": "2026-05-06T07:08:09.000Z",
                "triggerReason": "Authorized",
                "seqNo": 0,
                "transactionInfo": {"transactionId": "t"}
            }),
            &json!({}),
        );
    }

    #[test]
    fn transaction_event_keeps_offline_true_and_omits_it_when_absent() {
        let base = json!({
            "eventType": "Ended",
            "timestamp": "2026-05-06T07:08:09.000Z",
            "triggerReason": "EVDeparted",
            "seqNo": 9,
            "transactionInfo": {"transactionId": "t"}
        });
        let absent: TransactionEventRequest = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(absent.offline, None);
        assert!(
            serde_json::to_value(&absent)
                .unwrap()
                .get("offline")
                .is_none()
        );

        let mut with_offline = base;
        with_offline["offline"] = json!(true);
        check_action::<TransactionEvent>(&with_offline, &json!({}));
    }

    #[test]
    fn transaction_id_stays_a_string() {
        let parsed: TransactionEventRequest = serde_json::from_value(json!({
            "eventType": "Started",
            "timestamp": "2026-05-06T07:08:09.000Z",
            "triggerReason": "Authorized",
            "seqNo": 0,
            "transactionInfo": {"transactionId": "42"}
        }))
        .unwrap();
        assert_eq!(parsed.transaction_info.transaction_id, "42");
        assert_eq!(
            serde_json::to_value(&parsed).unwrap()["transactionInfo"]["transactionId"],
            json!("42")
        );
    }

    #[test]
    fn meter_values() {
        check_action::<MeterValues>(
            &json!({"customData": cd(), "evseId": 2, "meterValue": [meter_value()]}),
            &json!({"customData": cd()}),
        );
        check_action::<MeterValues>(
            &json!({
                "evseId": 0,
                "meterValue": [{
                    "timestamp": "2026-05-06T07:08:09.000Z",
                    "sampledValue": [{"value": 0}]
                }]
            }),
            &json!({}),
        );
    }

    #[test]
    fn meter_value_number_spelling_survives() {
        let wire = json!({
            "evseId": 1,
            "meterValue": [{
                "timestamp": "2026-05-06T07:08:09.000Z",
                "sampledValue": [{"value": 100}, {"value": 100.25}, {"value": -4}]
            }]
        });
        let typed: MeterValuesRequest = serde_json::from_value(wire.clone()).unwrap();
        assert!(typed.meter_value[0].sampled_value[0].value.is_i64());
        assert!(typed.meter_value[0].sampled_value[1].value.is_f64());
        assert_eq!(serde_json::to_value(&typed).unwrap(), wire);
    }

    #[test]
    fn get_transaction_status() {
        check_action::<GetTransactionStatus>(
            &json!({"customData": cd(), "transactionId": "tx"}),
            &json!({"customData": cd(), "ongoingIndicator": true, "messagesInQueue": false}),
        );
        check_action::<GetTransactionStatus>(&json!({}), &json!({"messagesInQueue": true}));
    }
}
