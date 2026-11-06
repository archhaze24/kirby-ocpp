//! Command execution shared by the stdin JSON-lines handler and the HTTP API, so that result
//! shapes, limits and validation cannot drift apart.

use std::sync::Arc;
use std::time::Duration;

use kirby_csms_core::{CsmsError, CsmsHandle, Fault, Frame};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::headless::station_summary;

/// Longest `timeout_ms` accepted: one day.
pub const MAX_TIMEOUT_MS: u64 = 86_400_000;

/// Most commands in flight at once, stdin and API together.
pub const MAX_IN_FLIGHT: usize = 256;

/// The in-flight slots shared by every command source.
pub fn new_slots() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(MAX_IN_FLIGHT))
}

/// Every known station as a JSON array of summaries (see [`station_summary`]).
pub fn stations(handle: &CsmsHandle) -> Value {
    let list: Vec<Value> = handle
        .stations()
        .iter()
        .map(|info| station_summary(info, handle.state(&info.id).as_ref()))
        .collect();
    Value::Array(list)
}

/// Validates an optional `timeout_ms` value.
pub fn parse_timeout(value: Option<&Value>) -> Result<Option<Duration>, &'static str> {
    match value {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|ms| (1..=MAX_TIMEOUT_MS).contains(ms))
            .map(|ms| Some(Duration::from_millis(ms)))
            .ok_or("`timeout_ms` must be 1..=86400000"),
    }
}

/// A `result` object with `ok: false` and no CALL outcome.
pub fn failure(station: Option<&str>, action: Option<&str>, error: &str) -> Value {
    json!({
        "type": "result", "station": station, "action": action, "ok": false, "error": error
    })
}

/// The outcome of [`call`].
#[derive(Debug)]
pub struct CallAnswer {
    /// The `result` object.
    pub value: Value,
    /// The station is unknown or not connected (nothing was sent).
    pub missing_station: bool,
}

/// Sends one CALL and builds the `result` object. `timeout` defaults to the server's
/// call timeout.
pub async fn call(
    handle: &CsmsHandle,
    station: &str,
    action: &str,
    payload: Value,
    timeout: Option<Duration>,
) -> CallAnswer {
    let result = match timeout {
        Some(timeout) => handle.call_timeout(station, action, payload, timeout).await,
        None => handle.call(station, action, payload).await,
    };
    let (value, missing_station) = match result {
        Ok(Frame::CallResult { payload, .. }) => (
            json!({"type": "result", "station": station, "action": action, "ok": true, "result": payload}),
            false,
        ),
        Ok(Frame::CallError {
            code,
            description,
            details,
            ..
        }) => (
            json!({
                "type": "result", "station": station, "action": action, "ok": false,
                "error": format!("CALLERROR {}: {description}", code.as_str()),
                "code": code.as_str(), "details": details,
            }),
            false,
        ),
        Ok(Frame::Call { .. }) => (
            failure(Some(station), Some(action), "unexpected CALL as an answer"),
            false,
        ),
        Err(error) => (
            failure(Some(station), Some(action), &error.to_string()),
            matches!(
                error,
                CsmsError::UnknownStation { .. } | CsmsError::NotConnected { .. }
            ),
        ),
    };
    CallAnswer {
        value,
        missing_station,
    }
}

/// What an injected fault does instead of the normal answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultKind {
    CallError,
    Silence,
    Close,
    InvalidPayload,
}

/// Largest `times` of one fault.
pub const MAX_FAULT_TIMES: usize = 1_000_000;

/// Checks the fields of an `inject_fault` request (script step and API alike).
pub fn check_fault(
    action: &str,
    kind: FaultKind,
    payload: Option<&Value>,
    times: usize,
) -> Result<(), String> {
    if action.is_empty() {
        return Err("`call` must not be empty".to_owned());
    }
    if times == 0 {
        return Err("`times` must be 1 or more".to_owned());
    }
    if times > MAX_FAULT_TIMES {
        return Err(format!("`times` must be at most {MAX_FAULT_TIMES}"));
    }
    if kind == FaultKind::InvalidPayload && payload.is_none() {
        return Err("kind `invalid_payload` needs a `payload`".to_owned());
    }
    Ok(())
}

/// Builds the fault; `payload` is only used by `invalid_payload`.
pub fn build_fault(action: &str, kind: FaultKind, payload: Option<Value>, times: usize) -> Fault {
    let fault = match kind {
        FaultKind::CallError => Fault::call_error(action),
        FaultKind::Silence => Fault::silence(action),
        FaultKind::Close => Fault::close(action),
        FaultKind::InvalidPayload => Fault::invalid_payload(action, payload.unwrap_or_default()),
    };
    fault.times(times)
}
