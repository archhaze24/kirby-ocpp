//! Default CSMS answers, ported from `scripts/smoke/harness.mjs`
//! (`responseFor`, `authorizeResponse`, `startTransactionResponse`).

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde_json::{Value, json};

use kirby_ocpp_core::ocpp::Protocol;

use crate::fault::Reply;

/// The OCPP 1.6 actions a charge point may send to a central system.
///
/// Includes the four Security Whitepaper actions (`SignCertificate`, `LogStatusNotification`,
/// `SecurityEventNotification`, `SignedFirmwareStatusNotification`).
pub const CHARGE_POINT_ACTIONS: &[&str] = crate::registry::ocpp16::STATION_ACTIONS;

/// The WebSocket subprotocol whose actions [`CHARGE_POINT_ACTIONS`] and
/// [`is_charge_point_action`] describe.
pub const ACTIONS_PROTOCOL: &str = Protocol::V16.token();

/// Whether `action` is one a charge point may initiate in OCPP 1.6.
pub fn is_charge_point_action(action: &str) -> bool {
    is_charge_point_action_for(Protocol::V16, action)
}

/// Whether `action` is one a charge point of `protocol` may initiate. `false` for every
/// action of a protocol without an action table (2.1 today).
pub fn is_charge_point_action_for(protocol: Protocol, action: &str) -> bool {
    match protocol {
        Protocol::V201 => crate::defaults201::is_station_action(action),
        _ => crate::registry::is_station_action(protocol.token(), action),
    }
}

/// The default answer for `action` in OCPP 1.6, used when no fault or responder applies.
///
/// `transaction_id` must be `Some` for `StartTransaction`. Actions that are not
/// charge-point actions get `{}`; callers that want `NotSupported` for them check
/// [`is_charge_point_action`] first.
pub fn default_reply(
    action: &str,
    payload: &Value,
    heartbeat_interval: u32,
    transaction_id: Option<i32>,
) -> Reply {
    default_reply_for(
        Protocol::V16,
        action,
        payload,
        heartbeat_interval,
        transaction_id,
    )
}

/// Like [`default_reply`] for `protocol`. OCPP 2.0.1 has its own answers and ignores
/// `transaction_id` (the station names its transactions); a protocol without default answers
/// (2.1 today) gets `{}` for every action.
pub fn default_reply_for(
    protocol: Protocol,
    action: &str,
    payload: &Value,
    heartbeat_interval: u32,
    transaction_id: Option<i32>,
) -> Reply {
    match protocol {
        Protocol::V16 => {}
        Protocol::V201 => {
            return crate::defaults201::default_reply(
                action,
                payload,
                heartbeat_interval,
                Utc::now(),
            );
        }
        _ => return Reply::Result(json!({})),
    }
    let now = Utc::now();
    let id_tag = payload
        .get("idTag")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let body = match action {
        "BootNotification" => json!({
            "status": "Accepted",
            "currentTime": iso(now),
            "interval": heartbeat_interval,
        }),
        "Heartbeat" => json!({ "currentTime": iso(now) }),
        "Authorize" => json!({ "idTagInfo": authorize_info(id_tag, now) }),
        "StartTransaction" => json!({
            "transactionId": transaction_id.unwrap_or_default(),
            "idTagInfo": start_info(id_tag, now),
        }),
        "DataTransfer" => json!({ "status": "Accepted", "data": "pong" }),
        // The CSMS never signs anything itself, but accepts the request (a test then
        // scripts the `CertificateSigned` that follows).
        "SignCertificate" => json!({ "status": "Accepted" }),
        // StopTransaction, MeterValues, StatusNotification, Diagnostics-, Firmware-, Log- and
        // SignedFirmwareStatusNotification, SecurityEventNotification and unknown actions: an
        // empty object.
        _ => json!({}),
    };
    Reply::Result(body)
}

/// Magic idTags for `Authorize`.
fn authorize_info(id_tag: &str, now: DateTime<Utc>) -> Value {
    match id_tag {
        "BADTAG" => json!({ "status": "Blocked" }),
        "EXPIREDAUTH" => json!({ "status": "Accepted", "expiryDate": expired(now) }),
        "PARENTAUTH" => json!({ "status": "Accepted", "parentIdTag": "PARENTTAG" }),
        "CONCURRENTAUTH" => json!({ "status": "ConcurrentTx" }),
        _ => json!({ "status": "Accepted" }),
    }
}

/// Magic idTags for `StartTransaction`.
fn start_info(id_tag: &str, now: DateTime<Utc>) -> Value {
    match id_tag {
        "BLOCKSTART" => json!({ "status": "Blocked" }),
        "EXPIREDSTART" => json!({ "status": "Accepted", "expiryDate": expired(now) }),
        "CONCURRENTSTART" => json!({ "status": "ConcurrentTx" }),
        _ => json!({ "status": "Accepted" }),
    }
}

/// A timestamp one minute in the past, so the idTag is already expired.
fn expired(now: DateTime<Utc>) -> String {
    iso(now - TimeDelta::seconds(60))
}

/// JavaScript `Date.prototype.toISOString` format (millisecond precision, `Z`).
pub(crate) fn iso(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_id_tags_map_to_statuses() {
        let Reply::Result(body) = default_reply("Authorize", &json!({"idTag": "BADTAG"}), 5, None)
        else {
            panic!("expected a result");
        };
        assert_eq!(body["idTagInfo"]["status"], "Blocked");
        let Reply::Result(body) = default_reply(
            "StartTransaction",
            &json!({"idTag": "CONCURRENTSTART"}),
            5,
            Some(9),
        ) else {
            panic!("expected a result");
        };
        assert_eq!(body["transactionId"], 9);
        assert_eq!(body["idTagInfo"]["status"], "ConcurrentTx");
    }

    #[test]
    fn security_actions_get_canned_answers() {
        for action in [
            "SignCertificate",
            "LogStatusNotification",
            "SecurityEventNotification",
            "SignedFirmwareStatusNotification",
        ] {
            assert!(is_charge_point_action(action));
            let Reply::Result(body) = default_reply(action, &json!({}), 5, None) else {
                panic!("expected a result");
            };
            if action == "SignCertificate" {
                assert_eq!(body, json!({"status": "Accepted"}));
            } else {
                assert_eq!(body, json!({}));
            }
        }
    }

    #[test]
    fn charge_point_actions_are_recognised() {
        assert!(is_charge_point_action("Heartbeat"));
        assert!(!is_charge_point_action("Reset"));
    }

    #[test]
    fn protocol_variants_delegate_for_1_6_and_know_nothing_else() {
        assert_eq!(ACTIONS_PROTOCOL, "ocpp1.6");
        assert!(is_charge_point_action_for(Protocol::V16, "Heartbeat"));
        assert!(is_charge_point_action_for(Protocol::V201, "Heartbeat"));
        assert!(is_charge_point_action_for(
            Protocol::V201,
            "TransactionEvent"
        ));
        assert!(!is_charge_point_action_for(
            Protocol::V201,
            "StartTransaction"
        ));
        assert!(!is_charge_point_action_for(Protocol::V201, "Reset"));
        assert!(!is_charge_point_action_for(Protocol::V21, "Heartbeat"));
        let Reply::Result(body) =
            default_reply_for(Protocol::V16, "Heartbeat", &json!({}), 5, None)
        else {
            panic!("expected a result");
        };
        assert!(body["currentTime"].is_string());
        let Reply::Result(body) =
            default_reply_for(Protocol::V201, "Heartbeat", &json!({}), 5, Some(7))
        else {
            panic!("expected a result");
        };
        assert!(body["currentTime"].is_string());
        let Reply::Result(body) =
            default_reply_for(Protocol::V21, "Heartbeat", &json!({}), 5, None)
        else {
            panic!("expected a result");
        };
        assert_eq!(body, json!({}));
    }
}
