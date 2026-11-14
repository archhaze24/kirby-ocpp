//! Default replies of the central system to the OCPP 2.0.1 actions a station sends.

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};

use kirby_ocpp_core::ocpp::v2::registry;

use crate::defaults::iso;
use crate::fault::Reply;

/// Whether a station may send `action` in OCPP 2.0.1 (the 25 actions of its charge-station
/// side, `DataTransfer` included).
pub(crate) fn is_station_action(action: &str) -> bool {
    registry::direction(action).is_some_and(registry::ActionDirection::station_may_send)
}

/// The default answer for `action` in OCPP 2.0.1. Actions a station may not send get `{}`.
pub(crate) fn default_reply(
    action: &str,
    payload: &Value,
    heartbeat_interval: u32,
    now: DateTime<Utc>,
) -> Reply {
    let body = match action {
        "BootNotification" => json!({
            "currentTime": iso(now),
            "interval": i32::try_from(heartbeat_interval).unwrap_or(i32::MAX),
            "status": "Accepted",
        }),
        "Heartbeat" => json!({ "currentTime": iso(now) }),
        "Authorize" => json!({ "idTokenInfo": id_token_info(payload.get("idToken"), now) }),
        // `idTokenInfo` is required only when the request carried an `idToken`.
        "TransactionEvent" => match payload.get("idToken") {
            Some(token) => json!({ "idTokenInfo": id_token_info(Some(token), now) }),
            None => json!({}),
        },
        "DataTransfer" => json!({ "status": "Accepted", "data": "pong" }),
        // The CSMS never signs anything itself, but accepts the request (a test then
        // scripts the `CertificateSigned` that follows).
        "SignCertificate" => json!({ "status": "Accepted" }),
        // The CSMS has no ISO 15118 backend.
        "Get15118EVCertificate" => json!({ "status": "Failed", "exiResponse": "" }),
        "GetCertificateStatus" => json!({ "status": "Failed" }),
        "NotifyEVChargingNeeds" | "NotifyEVChargingSchedule" => json!({ "status": "Rejected" }),
        _ => json!({}),
    };
    Reply::Result(body)
}

/// Magic id tokens for `Authorize` and `TransactionEvent`.
fn id_token_info(token: Option<&Value>, now: DateTime<Utc>) -> Value {
    let text = token
        .and_then(|token| token.get("idToken"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = token
        .and_then(|token| token.get("type"))
        .and_then(Value::as_str);
    if kind == Some("NoAuthorization") {
        return json!({ "status": "Accepted" });
    }
    match text {
        "BADTAG" | "BLOCKSTART" => json!({ "status": "Blocked" }),
        "EXPIREDAUTH" | "EXPIREDSTART" => json!({
            "status": "Expired",
            "cacheExpiryDateTime": iso(now - TimeDelta::seconds(60)),
        }),
        "CONCURRENTAUTH" | "CONCURRENTSTART" => json!({ "status": "ConcurrentTx" }),
        "PARENTAUTH" => json!({
            "status": "Accepted",
            "groupIdToken": { "idToken": "PARENTTAG", "type": "Central" },
        }),
        _ => json!({ "status": "Accepted" }),
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use kirby_ocpp_core::ocpp::Protocol;
    use kirby_ocpp_core::ocpp::schema::SchemaRegistry;

    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 14, 12, 0, 0).unwrap()
    }

    fn body(action: &str, payload: &Value) -> Value {
        match default_reply(action, payload, 300, now()) {
            Reply::Result(body) => body,
            other => panic!("expected a result, got {other:?}"),
        }
    }

    #[allow(clippy::too_many_lines)] // one flat table of minimal requests
    fn sample_request(action: &str) -> Value {
        let token = json!({ "idToken": "TAG1", "type": "ISO14443" });
        let schedule = json!({
            "id": 1,
            "chargingRateUnit": "W",
            "chargingSchedulePeriod": [{ "startPeriod": 0, "limit": 1000 }],
        });
        let ts = "2026-11-14T12:00:00.000Z";
        match action {
            "Authorize" => json!({ "idToken": token }),
            "BootNotification" => json!({
                "reason": "PowerUp",
                "chargingStation": { "model": "M", "vendorName": "V" },
            }),
            "ClearedChargingLimit" => json!({ "chargingLimitSource": "EMS" }),
            "DataTransfer" => json!({ "vendorId": "V" }),
            "FirmwareStatusNotification"
            | "LogStatusNotification"
            | "PublishFirmwareStatusNotification" => json!({ "status": "Idle" }),
            "Get15118EVCertificate" => json!({
                "iso15118SchemaVersion": "urn:iso:15118:2:2013:MsgDef",
                "action": "Install",
                "exiRequest": "AAAA",
            }),
            "GetCertificateStatus" => json!({ "ocspRequestData": {
                "hashAlgorithm": "SHA256",
                "issuerNameHash": "a",
                "issuerKeyHash": "b",
                "serialNumber": "1",
                "responderURL": "https://ocsp.example.com",
            } }),
            "MeterValues" => json!({
                "evseId": 1,
                "meterValue": [{ "timestamp": ts, "sampledValue": [{ "value": 1.5 }] }],
            }),
            "NotifyChargingLimit" => json!({ "chargingLimit": { "chargingLimitSource": "EMS" } }),
            "NotifyCustomerInformation" => json!({
                "data": "d",
                "seqNo": 0,
                "generatedAt": ts,
                "requestId": 1,
            }),
            "NotifyDisplayMessages" => json!({ "requestId": 1 }),
            "NotifyEVChargingNeeds" => json!({
                "evseId": 1,
                "chargingNeeds": { "requestedEnergyTransfer": "DC" },
            }),
            "NotifyEVChargingSchedule" => json!({
                "timeBase": ts,
                "evseId": 1,
                "chargingSchedule": schedule,
            }),
            "NotifyEvent" => json!({
                "generatedAt": ts,
                "seqNo": 0,
                "eventData": [{
                    "eventId": 1,
                    "timestamp": ts,
                    "trigger": "Alerting",
                    "actualValue": "x",
                    "eventNotificationType": "HardWiredNotification",
                    "component": { "name": "C" },
                    "variable": { "name": "V" },
                }],
            }),
            "NotifyMonitoringReport" | "NotifyReport" => json!({
                "requestId": 1,
                "generatedAt": ts,
                "seqNo": 0,
            }),
            "ReportChargingProfiles" => json!({
                "requestId": 1,
                "chargingLimitSource": "EMS",
                "evseId": 1,
                "chargingProfile": [{
                    "id": 1,
                    "stackLevel": 0,
                    "chargingProfilePurpose": "TxProfile",
                    "chargingProfileKind": "Absolute",
                    "chargingSchedule": [schedule],
                }],
            }),
            "ReservationStatusUpdate" => json!({
                "reservationId": 1,
                "reservationUpdateStatus": "Expired",
            }),
            "SecurityEventNotification" => json!({ "type": "FirmwareUpdated", "timestamp": ts }),
            "SignCertificate" => json!({ "csr": "csr" }),
            "StatusNotification" => json!({
                "timestamp": ts,
                "connectorStatus": "Available",
                "evseId": 1,
                "connectorId": 1,
            }),
            "TransactionEvent" => json!({
                "eventType": "Started",
                "timestamp": ts,
                "triggerReason": "Authorized",
                "seqNo": 0,
                "transactionInfo": { "transactionId": "T1" },
            }),
            _ => json!({}),
        }
    }

    fn station_actions() -> Vec<&'static str> {
        registry::iter()
            .filter(|info| info.direction.station_may_send())
            .map(|info| info.name)
            .collect()
    }

    #[test]
    fn every_station_action_gets_a_schema_valid_reply() {
        let schemas = SchemaRegistry::for_protocol(Protocol::V201).unwrap();
        let actions = station_actions();
        assert_eq!(actions.len(), 25);
        for action in actions {
            assert!(is_station_action(action), "{action}");
            let request = sample_request(action);
            schemas
                .validate_request(action, &request)
                .unwrap_or_else(|error| panic!("{action} request: {error}"));
            let reply = body(action, &request);
            schemas
                .validate_response(action, &reply)
                .unwrap_or_else(|error| panic!("{action} reply {reply}: {error}"));
        }
    }

    #[test]
    fn csms_only_actions_are_not_station_actions() {
        assert!(!is_station_action("Reset"));
        assert!(!is_station_action("Foo"));
        assert!(!is_station_action("StartTransaction"));
    }

    #[test]
    fn boot_and_heartbeat_carry_the_injected_time() {
        let boot = body("BootNotification", &json!({}));
        assert_eq!(boot["currentTime"], "2026-11-14T12:00:00.000Z");
        assert_eq!(boot["interval"], 300);
        assert_eq!(boot["status"], "Accepted");
        let beat = body("Heartbeat", &json!({}));
        assert_eq!(beat, json!({ "currentTime": "2026-11-14T12:00:00.000Z" }));
    }

    #[test]
    fn interval_is_clamped_to_i32() {
        let Reply::Result(boot) = default_reply("BootNotification", &json!({}), u32::MAX, now())
        else {
            panic!("expected a result");
        };
        assert_eq!(boot["interval"], i32::MAX);
    }

    #[test]
    fn magic_tokens_map_to_statuses() {
        let status = |action: &str, token: &str| {
            let request = json!({ "idToken": { "idToken": token, "type": "Local" } });
            body(action, &request)["idTokenInfo"].clone()
        };
        for action in ["Authorize", "TransactionEvent"] {
            for token in ["BADTAG", "BLOCKSTART"] {
                assert_eq!(status(action, token), json!({ "status": "Blocked" }));
            }
            for token in ["CONCURRENTAUTH", "CONCURRENTSTART"] {
                assert_eq!(status(action, token), json!({ "status": "ConcurrentTx" }));
            }
            for token in ["EXPIREDAUTH", "EXPIREDSTART"] {
                assert_eq!(
                    status(action, token),
                    json!({
                        "status": "Expired",
                        "cacheExpiryDateTime": "2026-11-14T11:59:00.000Z",
                    })
                );
            }
            assert_eq!(
                status(action, "PARENTAUTH"),
                json!({
                    "status": "Accepted",
                    "groupIdToken": { "idToken": "PARENTTAG", "type": "Central" },
                })
            );
            assert_eq!(status(action, "ANYTHING"), json!({ "status": "Accepted" }));
        }
    }

    #[test]
    fn no_authorization_is_always_accepted() {
        let request = json!({ "idToken": { "idToken": "BADTAG", "type": "NoAuthorization" } });
        assert_eq!(
            body("Authorize", &request)["idTokenInfo"],
            json!({ "status": "Accepted" })
        );
    }

    #[test]
    fn transaction_event_omits_id_token_info_without_a_token() {
        assert_eq!(body("TransactionEvent", &json!({})), json!({}));
        assert_eq!(
            body("TransactionEvent", &sample_request("TransactionEvent")),
            json!({})
        );
        let mut request = sample_request("TransactionEvent");
        request["idToken"] = json!({ "idToken": "TAG1", "type": "ISO14443" });
        assert_eq!(
            body("TransactionEvent", &request)["idTokenInfo"]["status"],
            "Accepted"
        );
    }

    #[test]
    fn hostile_payloads_do_not_panic() {
        for payload in [
            json!(null),
            json!([1]),
            json!({ "idToken": 5 }),
            json!({ "idToken": { "idToken": [] } }),
        ] {
            assert_eq!(
                body("Authorize", &payload)["idTokenInfo"],
                json!({ "status": "Accepted" })
            );
        }
    }
}
