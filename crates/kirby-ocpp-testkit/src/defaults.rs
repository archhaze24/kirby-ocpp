//! Default CSMS answers, ported from `scripts/smoke/harness.mjs`
//! (`responseFor`, `authorizeResponse`, `startTransactionResponse`).

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde_json::{Value, json};

use crate::fault::Reply;

/// The default answer for `action`, used when no fault or responder applies.
///
/// `transaction_id` must be `Some` for `StartTransaction`. Unknown actions get `{}`.
pub(crate) fn reply(
    action: &str,
    payload: &Value,
    heartbeat_interval: u32,
    transaction_id: Option<i32>,
) -> Reply {
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
        // StopTransaction, MeterValues, StatusNotification, Diagnostics- and
        // FirmwareStatusNotification and unknown actions: an empty object.
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
fn iso(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Millis, true)
}
