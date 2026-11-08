//! Command execution shared by the stdin JSON-lines handler and the HTTP API, so that result
//! shapes, limits and validation cannot drift apart.

use std::sync::Arc;
use std::time::Duration;

use chrono::SecondsFormat;
use kirby_csms_core::registry;
use kirby_csms_core::{
    CsmsError, CsmsHandle, Direction, Fault, FaultInfo, Frame, FrameEntry, Reply, RuleInfo, Scope,
};
use kirby_ocpp_core::text::is_unsafe_char;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::headless::{frame_json, station_summary};

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

/// What became of a [`call`].
#[derive(Debug)]
pub enum Outcome {
    /// The station answered with a CALLRESULT.
    Result,
    /// The station answered with a CALLERROR.
    CallError,
    /// The station did not answer in time.
    Timeout,
    /// No usable answer and the cause is not a protocol-level outcome: the station is unknown
    /// or not connected, the queue is full, the connection closed, or an internal failure.
    Failed(CsmsError),
}

/// The answer of [`call`].
#[derive(Debug)]
pub struct CallAnswer {
    /// The shared `result` object (the stdin framing).
    pub value: Value,
    /// What happened.
    pub outcome: Outcome,
}

impl CallAnswer {
    /// The body of an API answer for a call that reached the station (or timed out): the
    /// shared `result` object without `type`, `station` and `action` (the request names
    /// them), plus `outcome`. `None` when the call failed before any protocol outcome.
    pub fn api_body(&self) -> Option<Value> {
        let outcome = match self.outcome {
            Outcome::Result => "result",
            Outcome::CallError => "call_error",
            Outcome::Timeout => "timeout",
            Outcome::Failed(_) => return None,
        };
        let mut body = self.value.clone();
        let object = body.as_object_mut()?;
        for key in ["type", "station", "action"] {
            object.remove(key);
        }
        object.insert("outcome".to_owned(), outcome.into());
        Some(body)
    }
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
    let (value, outcome) = match result {
        Ok(Frame::CallResult { payload, .. }) => (
            json!({"type": "result", "station": station, "action": action, "ok": true, "result": payload}),
            Outcome::Result,
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
            Outcome::CallError,
        ),
        Ok(Frame::Call { .. }) => (
            failure(Some(station), Some(action), "unexpected CALL as an answer"),
            Outcome::Failed(CsmsError::InvalidResponse {
                station: station.to_owned(),
                action: action.to_owned(),
                reason: "unexpected CALL as an answer".to_owned(),
            }),
        ),
        Err(error) => {
            let value = failure(Some(station), Some(action), &error.to_string());
            let outcome = if matches!(error, CsmsError::Timeout { .. }) {
                Outcome::Timeout
            } else {
                Outcome::Failed(error)
            };
            (value, outcome)
        }
    };
    CallAnswer { value, outcome }
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

/// Longest action name of a fault, in bytes.
pub const MAX_FAULT_ACTION_LEN: usize = 128;

/// Checks the fields of an `inject_fault` request (script step and API alike). The action
/// name may be any text without control characters, so faults can target actions the
/// server does not know.
pub fn check_fault(
    action: &str,
    kind: FaultKind,
    payload: Option<&Value>,
    times: usize,
) -> Result<(), String> {
    if action.is_empty() {
        return Err("`call` must not be empty".to_owned());
    }
    if action.len() > MAX_FAULT_ACTION_LEN {
        return Err(format!(
            "the action name must be at most {MAX_FAULT_ACTION_LEN} bytes"
        ));
    }
    if action.chars().any(is_unsafe_char) {
        return Err("the action name must not contain control characters".to_owned());
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

use kirby_csms_core::MAX_STATION_ID_LEN;

/// Checks a station id used in a scope or a command: 1 to 128 bytes, no control or bidi
/// characters.
pub fn check_station_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("the station id must not be empty".to_owned());
    }
    if id.len() > MAX_STATION_ID_LEN {
        return Err(format!(
            "the station id must be at most {MAX_STATION_ID_LEN} bytes"
        ));
    }
    if id.chars().any(is_unsafe_char) {
        return Err(
            "the station id must not contain control or bidirectional characters".to_owned(),
        );
    }
    Ok(())
}

/// Checks that `protocol` has an action table in the registry.
pub fn check_protocol(protocol: &str) -> Result<(), String> {
    if registry::protocol_actions(protocol).is_some() {
        return Ok(());
    }
    let known: Vec<&str> = registry::protocols().collect();
    Err(format!(
        "unsupported protocol; supported: {}",
        known.join(", ")
    ))
}

/// Builds and checks the [`Scope`] of a rule or fault from an optional station id and an
/// optional protocol.
pub fn scope_of(station: Option<&str>, protocol: Option<&str>) -> Result<Scope, String> {
    if let Some(station) = station {
        check_station_id(station)?;
    }
    if let Some(protocol) = protocol {
        check_protocol(protocol)?;
    }
    Ok(Scope {
        station: station.map(str::to_owned),
        protocol: protocol.map(str::to_owned),
    })
}

/// Whether `action` is one a station sends: in `protocol` when given, else in any protocol
/// with an action table.
pub fn is_rule_action(action: &str, protocol: Option<&str>) -> bool {
    match protocol {
        Some(protocol) => registry::is_station_action(protocol, action),
        None => registry::protocols().any(|protocol| registry::is_station_action(protocol, action)),
    }
}

/// Sets the fixed CALLRESULT payload of `action` in `scope`. The global scope (no station,
/// no protocol) is the engine-wide responder; any other scope is a capped scoped rule.
pub fn set_rule(
    handle: &CsmsHandle,
    scope: &Scope,
    action: &str,
    payload: Value,
) -> Result<(), String> {
    if *scope == Scope::global() {
        handle.respond(action, move |_| Reply::Result(payload.clone()));
        Ok(())
    } else {
        handle.respond_scoped(scope, action, payload)
    }
}

/// Removes the rule of `action` in `scope`; returns whether there was one.
pub fn clear_rule(handle: &CsmsHandle, scope: &Scope, action: &str) -> bool {
    if *scope == Scope::global() {
        handle.clear_responder(action)
    } else {
        handle.clear_scoped_rule(scope.station.as_deref(), action)
    }
}

/// A scoped rule as JSON: `station` (null for a global rule with a protocol), `action`,
/// `protocol` (null when not limited), `payload`.
pub fn rule_json(rule: &RuleInfo) -> Value {
    json!({
        "station": rule.scope.station,
        "action": rule.action,
        "protocol": rule.scope.protocol,
        "payload": rule.payload,
    })
}

/// A stored fault as JSON: `id`, `scope` (`global` or `station`), `station`, `protocol`,
/// `action`, `kind`, `remaining`.
pub fn fault_json(fault: &FaultInfo) -> Value {
    json!({
        "id": fault.id,
        "scope": if fault.scope.station.is_some() { "station" } else { "global" },
        "station": fault.scope.station,
        "protocol": fault.scope.protocol,
        "action": fault.action,
        "kind": fault.kind,
        "remaining": fault.remaining,
    })
}

/// The stored faults as JSON objects, of one station (`Some`) or of every scope (`None`).
pub fn faults_list(handle: &CsmsHandle, station: Option<&str>) -> Vec<Value> {
    handle
        .faults()
        .iter()
        .filter(|fault| station.is_none_or(|id| fault.scope.station.as_deref() == Some(id)))
        .map(fault_json)
        .collect()
}

/// The frames of a station history as JSON objects shaped like the `--json` frame lines.
pub fn history_frames(station: &str, entries: &[FrameEntry]) -> Vec<Value> {
    entries
        .iter()
        .map(|entry| {
            let mut value = frame_json(&entry.frame);
            if let Some(object) = value.as_object_mut() {
                object.insert("type".to_owned(), "frame".into());
                object.insert(
                    "ts".to_owned(),
                    entry.ts.to_rfc3339_opts(SecondsFormat::Millis, true).into(),
                );
                object.insert("station".to_owned(), station.into());
                object.insert(
                    "direction".to_owned(),
                    match entry.direction {
                        Direction::In => "in",
                        Direction::Out => "out",
                    }
                    .into(),
                );
            }
            value
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_fault_bounds_times_and_action_names() {
        let ok = |action: &str, times: usize| check_fault(action, FaultKind::Silence, None, times);
        assert!(ok("Heartbeat", 1).is_ok());
        assert!(ok("NoSuchAction", MAX_FAULT_TIMES).is_ok());
        assert!(ok("Heartbeat", 0).is_err());
        assert!(ok("Heartbeat", MAX_FAULT_TIMES + 1).is_err());
        assert!(ok("", 1).is_err());
        assert!(ok("Heart\nbeat", 1).is_err());
        assert!(ok("Heart\u{1b}[31mbeat", 1).is_err());
        assert!(ok("Heart\u{202e}beat", 1).is_err());
        assert!(ok(&"a".repeat(MAX_FAULT_ACTION_LEN), 1).is_ok());
        assert!(ok(&"a".repeat(MAX_FAULT_ACTION_LEN + 1), 1).is_err());
    }
}
