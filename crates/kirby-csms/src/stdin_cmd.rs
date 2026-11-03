//! JSON-line commands on stdin.
//!
//! * `{"station":"CP-1","action":"Reset","payload":{"type":"Soft"},"timeout_ms":5000,"id":"my-tag"}`
//!   sends the CALL (payload defaults to `{}`, `timeout_ms` to `--call-timeout`, `id` is optional
//!   and echoed back) and prints `{"type":"result","station":..,"action":..,"ok":true,"result":{..}}`
//!   or `{"type":"result",..,"ok":false,"error":"..."}`.
//! * `{"cmd":"stations"}` prints `{"type":"stations","stations":[..]}`.
//!
//! Every line gets exactly one answer line; a bad line never stops the processing.

use std::io::BufRead;
use std::sync::Arc;
use std::time::Duration;

use kirby_csms_core::{CsmsHandle, Frame};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::headless::{Output, station_summary};

/// Longest `timeout_ms` accepted: one day.
const MAX_TIMEOUT_MS: u64 = 86_400_000;

/// Starts reading stdin on a plain thread (a blocked tokio stdin read would hold up the
/// runtime shutdown) and answers every line in its own task, so one slow command does not
/// delay the others. End of input just ends the reader; the server keeps running.
pub fn spawn(handle: CsmsHandle, out: Arc<Output>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if line.trim().is_empty() {
                continue;
            }
            let handle = handle.clone();
            let out = Arc::clone(&out);
            tokio::spawn(async move {
                let answer = handle_line(&handle, &line).await;
                out.json_line(&answer);
            });
        }
    });
}

/// Executes one input line and returns the line to print.
pub async fn handle_line(handle: &CsmsHandle, line: &str) -> Value {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(error) => return failure(None, None, None, &format!("invalid JSON: {error}")),
    };
    let Some(object) = value.as_object() else {
        return failure(None, None, None, "a command must be a JSON object");
    };
    let tag = object.get("id").cloned();
    if let Some(cmd) = object.get("cmd") {
        return match cmd.as_str() {
            Some("stations") => stations(handle, tag),
            _ => failure(
                tag,
                None,
                None,
                &format!("unknown cmd {cmd}; supported: \"stations\""),
            ),
        };
    }
    let text = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let (Some(station), Some(action)) = (text("station"), text("action")) else {
        return failure(
            tag,
            text("station"),
            text("action"),
            "needs string fields `station` and `action` (or `cmd`)",
        );
    };
    let payload = object.get("payload").cloned().unwrap_or_else(|| json!({}));
    let timeout = match object.get("timeout_ms") {
        None => None,
        Some(value) => match value
            .as_u64()
            .filter(|ms| (1..=MAX_TIMEOUT_MS).contains(ms))
        {
            Some(ms) => Some(Duration::from_millis(ms)),
            None => {
                return failure(
                    tag,
                    Some(station),
                    Some(action),
                    "`timeout_ms` must be 1..=86400000",
                );
            }
        },
    };
    let result = match timeout {
        Some(timeout) => handle.call_timeout(station, action, payload, timeout).await,
        None => handle.call(station, action, payload).await,
    };
    let mut answer = match result {
        Ok(Frame::CallResult { payload, .. }) => {
            json!({"type": "result", "station": station, "action": action, "ok": true, "result": payload})
        }
        Ok(Frame::CallError {
            code,
            description,
            details,
            ..
        }) => json!({
            "type": "result", "station": station, "action": action, "ok": false,
            "error": format!("CALLERROR {}: {description}", code.as_str()),
            "code": code.as_str(), "details": details,
        }),
        Ok(Frame::Call { .. }) => {
            return failure(
                tag,
                Some(station),
                Some(action),
                "unexpected CALL as an answer",
            );
        }
        Err(error) => return failure(tag, Some(station), Some(action), &error.to_string()),
    };
    add_tag(&mut answer, tag);
    answer
}

fn stations(handle: &CsmsHandle, tag: Option<Value>) -> Value {
    let list: Vec<Value> = handle
        .stations()
        .iter()
        .map(|info| station_summary(info, handle.state(&info.id).as_ref()))
        .collect();
    let mut answer = json!({"type": "stations", "stations": list});
    add_tag(&mut answer, tag);
    answer
}

fn failure(tag: Option<Value>, station: Option<&str>, action: Option<&str>, error: &str) -> Value {
    let mut answer = json!({
        "type": "result", "station": station, "action": action, "ok": false, "error": error
    });
    add_tag(&mut answer, tag);
    answer
}

fn add_tag(answer: &mut Value, tag: Option<Value>) {
    if let (Some(tag), Some(object)) = (tag, answer.as_object_mut()) {
        object.insert("id".to_owned(), tag);
    }
}
