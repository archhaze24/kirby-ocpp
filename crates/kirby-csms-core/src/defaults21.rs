//! Default replies of the central system to the OCPP 2.1 actions a station sends.
//!
//! The 24 actions OCPP 2.1 shares with 2.0.1 get the answers of [`crate::defaults201`]; this
//! module adds the ones for the actions 2.1 introduced.

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};

use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::v2::registry;

use crate::defaults::iso;
use crate::fault::Reply;

/// The longest `vatNumber` of `VatNumberValidation` (schema `maxLength`).
const MAX_VAT_NUMBER_CHARS: usize = 20;
/// How many certificates `GetCertificateChainStatus` asks about at most (schema `maxItems`).
const MAX_CHAIN_STATUS_ENTRIES: usize = 4;

/// Whether a station may send `action` as a CALL in OCPP 2.1: the 36 actions of its
/// charge-station side, `DataTransfer` included. `NotifyPeriodicEventStream` is a SEND
/// message, not a CALL, so it is not one.
pub(crate) fn is_station_action(action: &str) -> bool {
    registry::lookup_for(Protocol::V21, action)
        .is_some_and(|info| info.direction.station_may_send() && !info.send_only)
}

/// The default answer for `action` in OCPP 2.1. Actions a station may not send get `{}`.
pub(crate) fn default_reply(
    action: &str,
    payload: &Value,
    heartbeat_interval: u32,
    now: DateTime<Utc>,
) -> Reply {
    let body = match action {
        // The CSMS has no PKI backend: it answers what it was asked and reports `Failed`, as
        // `GetCertificateStatus` does.
        "GetCertificateChainStatus" => {
            json!({ "certificateStatus": chain_status(payload, now) })
        }
        "OpenPeriodicEventStream" | "PullDynamicScheduleUpdate" => json!({ "status": "Accepted" }),
        "VatNumberValidation" => vat_number(payload),
        // `BatterySwap`, `ClosePeriodicEventStream`, `NotifyDERAlarm`, `NotifyDERStartStop`,
        // `NotifyPriorityCharging`, `NotifySettlement` and `ReportDERControl` have an empty
        // response; the actions shared with 2.0.1 keep their answers.
        _ => {
            return crate::defaults201::default_reply(action, payload, heartbeat_interval, now);
        }
    };
    Reply::Result(body)
}

/// One `Failed` entry per certificate of the request (at most four), echoing its hash data
/// and source.
fn chain_status(payload: &Value, now: DateTime<Utc>) -> Vec<Value> {
    let next_update = iso(now + TimeDelta::days(1));
    payload
        .get("certificateStatusRequests")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_CHAIN_STATUS_ENTRIES)
        .filter_map(|request| {
            let hash_data = request
                .get("certificateHashData")
                .filter(|v| v.is_object())?;
            let source = match request.get("source").and_then(Value::as_str) {
                Some("CRL") => "CRL",
                _ => "OCSP",
            };
            Some(json!({
                "certificateHashData": hash_data,
                "source": source,
                "status": "Failed",
                "nextUpdate": next_update,
            }))
        })
        .collect()
}

/// Accepts every VAT number and echoes it (and the EVSE) back.
fn vat_number(payload: &Value) -> Value {
    let vat: String = payload
        .get("vatNumber")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .take(MAX_VAT_NUMBER_CHARS)
        .collect();
    let mut body = json!({ "vatNumber": vat, "status": "Accepted" });
    if let Some(evse) = payload.get("evseId").and_then(Value::as_u64) {
        body["evseId"] = json!(evse);
    }
    body
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::TimeZone;
    use kirby_ocpp_core::ocpp::schema::SchemaRegistry;

    use super::*;
    use crate::defaults201::tests::sample_request as sample_request_v201;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 14, 12, 0, 0).unwrap()
    }

    fn body(action: &str, payload: &Value) -> Value {
        match default_reply(action, payload, 300, now()) {
            Reply::Result(body) => body,
            other => panic!("expected a result, got {other:?}"),
        }
    }

    /// A minimal valid request of an action 2.1 introduced; the others use the 2.0.1 samples.
    fn sample_request(action: &str) -> Value {
        let ts = "2026-11-14T12:00:00.000Z";
        match action {
            "BatterySwap" => json!({
                "eventType": "BatteryIn",
                "requestId": 1,
                "idToken": { "idToken": "TAG1", "type": "ISO14443" },
                "batteryData": [{ "evseId": 1, "serialNumber": "S1", "soC": 50, "soH": 90 }],
            }),
            "ClosePeriodicEventStream" => json!({ "id": 1 }),
            "GetCertificateChainStatus" => json!({ "certificateStatusRequests": [{
                "source": "OCSP",
                "urls": ["https://ocsp.example.com"],
                "certificateHashData": {
                    "hashAlgorithm": "SHA256",
                    "issuerNameHash": "a",
                    "issuerKeyHash": "b",
                    "serialNumber": "1",
                },
            }] }),
            "NotifyDERAlarm" => json!({ "controlType": "FreqDroop", "timestamp": ts }),
            "NotifyDERStartStop" => json!({
                "controlId": "C1",
                "started": true,
                "timestamp": ts,
            }),
            "NotifyPriorityCharging" => json!({ "transactionId": "T1", "activated": true }),
            "NotifySettlement" => json!({
                "pspRef": "PSP1",
                "status": "Settled",
                "settlementAmount": 12.5,
                "settlementTime": ts,
            }),
            "OpenPeriodicEventStream" => json!({ "constantStreamData": {
                "id": 1,
                "variableMonitoringId": 1,
                "params": { "interval": 10 },
            } }),
            "PullDynamicScheduleUpdate" => json!({ "chargingProfileId": 1 }),
            "ReportDERControl" => json!({ "requestId": 1 }),
            "VatNumberValidation" => json!({ "vatNumber": "NL123456789B01", "evseId": 1 }),
            _ => sample_request_v201(action),
        }
    }

    fn station_actions() -> Vec<&'static str> {
        registry::actions_for(Protocol::V21)
            .iter()
            .filter(|info| info.direction.station_may_send() && !info.send_only)
            .map(|info| info.name)
            .collect()
    }

    #[test]
    fn every_station_action_gets_a_schema_valid_reply() {
        let schemas = SchemaRegistry::for_protocol(Protocol::V21).unwrap();
        let actions = station_actions();
        assert_eq!(actions.len(), 36);
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
    fn the_new_actions_are_the_ones_beyond_2_0_1() {
        let new: BTreeSet<&str> = station_actions()
            .into_iter()
            .filter(|name| !crate::defaults201::is_station_action(name))
            .collect();
        assert_eq!(
            new,
            BTreeSet::from([
                "BatterySwap",
                "ClosePeriodicEventStream",
                "GetCertificateChainStatus",
                "NotifyDERAlarm",
                "NotifyDERStartStop",
                "NotifyPriorityCharging",
                "NotifySettlement",
                "OpenPeriodicEventStream",
                "PullDynamicScheduleUpdate",
                "ReportDERControl",
                "VatNumberValidation",
            ])
        );
    }

    #[test]
    fn only_station_calls_are_station_actions() {
        assert!(!is_station_action("Reset"));
        assert!(!is_station_action("RequestBatterySwap"));
        assert!(!is_station_action("Foo"));
        assert!(!is_station_action("StartTransaction"));
        // A SEND message is not a CALL.
        assert!(!is_station_action("NotifyPeriodicEventStream"));
    }

    #[test]
    fn shared_actions_keep_their_2_0_1_answers() {
        let boot = body("BootNotification", &json!({}));
        assert_eq!(boot["currentTime"], "2026-11-14T12:00:00.000Z");
        assert_eq!(boot["interval"], 300);
        assert_eq!(boot["status"], "Accepted");
        let request = json!({ "idToken": { "idToken": "BADTAG", "type": "Local" } });
        assert_eq!(
            body("Authorize", &request)["idTokenInfo"],
            json!({ "status": "Blocked" })
        );
    }

    #[test]
    fn chain_status_answers_each_certificate() {
        let reply = body(
            "GetCertificateChainStatus",
            &sample_request("GetCertificateChainStatus"),
        );
        let entry = &reply["certificateStatus"][0];
        assert_eq!(entry["status"], "Failed");
        assert_eq!(entry["source"], "OCSP");
        assert_eq!(entry["certificateHashData"]["serialNumber"], "1");
        assert_eq!(entry["nextUpdate"], "2026-11-15T12:00:00.000Z");
    }

    #[test]
    fn vat_number_is_echoed_and_accepted() {
        let reply = body(
            "VatNumberValidation",
            &json!({ "vatNumber": "NL1", "evseId": 2 }),
        );
        assert_eq!(
            reply,
            json!({ "vatNumber": "NL1", "status": "Accepted", "evseId": 2 })
        );
        let long = "x".repeat(100);
        let reply = body("VatNumberValidation", &json!({ "vatNumber": long }));
        assert_eq!(reply["vatNumber"].as_str().map(str::len), Some(20));
        assert!(reply.get("evseId").is_none());
    }

    #[test]
    fn hostile_payloads_do_not_panic() {
        for action in [
            "GetCertificateChainStatus",
            "VatNumberValidation",
            "Authorize",
        ] {
            for payload in [
                json!(null),
                json!([1]),
                json!({ "vatNumber": 5, "evseId": "x" }),
                json!({ "certificateStatusRequests": 5 }),
                json!({ "certificateStatusRequests": [null, 1, { "certificateHashData": 3 }] }),
            ] {
                let _ = body(action, &payload);
            }
        }
        let many = json!({ "certificateStatusRequests":
            (0..50).map(|_| json!({ "certificateHashData": {} })).collect::<Vec<_>>() });
        let reply = body("GetCertificateChainStatus", &many);
        assert_eq!(reply["certificateStatus"].as_array().map(Vec::len), Some(4));
    }
}
