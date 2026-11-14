//! Command execution shared by the stdin JSON-lines handler and the HTTP API, so that result
//! shapes, limits and validation cannot drift apart.

use std::sync::Arc;
use std::time::Duration;

use chrono::SecondsFormat;
use futures_util::{StreamExt, stream::FuturesUnordered};
use kirby_csms_core::registry;
use kirby_csms_core::{
    CsmsError, CsmsHandle, Direction, Fault, FaultInfo, Frame, FrameEntry, Reply, RuleInfo, Scope,
};
use kirby_ocpp_core::ocpp::redact_payload;
use kirby_ocpp_core::text::is_unsafe_char;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::headless::{frame_json, station_summary};

/// Longest `timeout_ms` accepted: one day.
pub const MAX_TIMEOUT_MS: u64 = 86_400_000;

/// Most commands in flight at once, stdin and API together.
pub const MAX_IN_FLIGHT: usize = 256;

/// The in-flight slots shared by every command source.
pub fn new_slots() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(MAX_IN_FLIGHT))
}

/// Most stations selected by one command.
pub const MAX_BULK_STATIONS: usize = 64;
/// Payload bytes held by all command sources together.
pub const PAYLOAD_BUDGET: usize = 32 * 1024 * 1024;

/// Limits shared by HTTP, stdin and configured commands.
#[derive(Debug, Clone)]
pub struct CommandLimits {
    pub slots: Arc<Semaphore>,
    pub payload: Arc<Semaphore>,
}

pub fn new_limits() -> CommandLimits {
    CommandLimits {
        slots: new_slots(),
        payload: Arc::new(Semaphore::new(PAYLOAD_BUDGET)),
    }
}

/// Reservations last until the complete command finishes or is abandoned.
#[derive(Debug)]
pub struct CommandPermit {
    _slots: OwnedSemaphorePermit,
    _payload: OwnedSemaphorePermit,
}

impl CommandLimits {
    pub fn try_reserve(&self, count: usize, bytes: usize) -> Result<CommandPermit, &'static str> {
        let count = u32::try_from(count).map_err(|_| "too many commands in flight")?;
        let slots = self
            .slots
            .clone()
            .try_acquire_many_owned(count)
            .map_err(|_| "too many commands in flight")?;
        let bytes =
            u32::try_from(bytes).map_err(|_| "too much request data held by commands in flight")?;
        let payload = self
            .payload
            .clone()
            .try_acquire_many_owned(bytes)
            .map_err(|_| "too much request data held by commands in flight (at most 32 MiB)")?;
        Ok(CommandPermit {
            _slots: slots,
            _payload: payload,
        })
    }
}

pub fn check_pattern(pattern: &str) -> Result<(), String> {
    check_station_id(pattern)
}

pub fn check_call_action(action: &str) -> Result<(), String> {
    if action.is_empty() || action.len() > 128 || action.chars().any(is_unsafe_char) {
        return Err(
            "`action` must be 1 to 128 bytes without control or bidirectional characters"
                .to_owned(),
        );
    }
    Ok(())
}

/// Matches Unicode characters with only `*` and `?` as metacharacters.
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    let text: Vec<char> = text.chars().collect();
    let mut previous = vec![false; text.len() + 1];
    previous[0] = true;
    for part in pattern.chars() {
        let mut current = vec![false; text.len() + 1];
        current[0] = part == '*' && previous[0];
        for (index, character) in text.iter().enumerate() {
            current[index + 1] = match part {
                '*' => previous[index + 1] || current[index],
                '?' => previous[index],
                literal => literal == *character && previous[index],
            };
        }
        previous = current;
    }
    previous.last().copied().unwrap_or(false)
}

pub fn select_stations(
    handle: &CsmsHandle,
    pattern: &str,
    protocol: Option<&str>,
) -> Result<Vec<kirby_csms_core::StationInfo>, String> {
    check_pattern(pattern)?;
    if let Some(protocol) = protocol {
        check_protocol(protocol)?;
    }
    let mut selected = Vec::new();
    for station in handle.stations() {
        if glob_matches(pattern, &station.id)
            && protocol.is_none_or(|protocol| station.subprotocol.as_deref() == Some(protocol))
        {
            if selected.len() == MAX_BULK_STATIONS {
                return Err("at most 64 stations may match one command".to_owned());
            }
            selected.push(station);
        }
    }
    selected.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(selected)
}

/// Runs a bounded selection without detached tasks; dropping it abandons every call.
pub async fn call_many(
    handle: &CsmsHandle,
    stations: &[kirby_csms_core::StationInfo],
    action: &str,
    payload: &Value,
    timeout: Option<Duration>,
) -> Vec<Value> {
    let mut pending: FuturesUnordered<_> = stations
        .iter()
        .map(|station| async move {
            let answer = call(handle, &station.id, action, payload.clone(), timeout).await;
            (station.id.clone(), bulk_answer(station, answer))
        })
        .collect();
    let mut answers = Vec::with_capacity(stations.len());
    while let Some(answer) = pending.next().await {
        answers.push(answer);
    }
    answers.sort_by(|left, right| left.0.cmp(&right.0));
    answers.into_iter().map(|(_, value)| value).collect()
}

fn bounded_payload(value: Option<Value>) -> Value {
    let Some(value) = value else {
        return Value::Null;
    };
    let bytes = value.to_string().len();
    if bytes > 64 * 1024 {
        json!({"truncated":true,"originalBytes":bytes})
    } else {
        value
    }
}

#[cfg(test)]
mod bulk_tests {
    use super::*;

    #[test]
    fn glob_matches_whole_unicode_ids_without_regex_features() {
        for (pattern, text, matches) in [
            ("CP-?", "CP-я", true),
            ("CP-?", "CP-💡", true),
            ("CP-?", "CP-aa", false),
            ("*a*b", "aaab", true),
            ("*a*b", "aaabc", false),
            ("[ab]", "a", false),
            ("CP-*", "cp-a", false),
            ("*", "", true),
            ("**?*", "x", true),
            ("?", "", false),
        ] {
            assert_eq!(glob_matches(pattern, text), matches, "{pattern} {text}");
        }
    }

    #[test]
    fn batch_admission_rolls_back_slots_when_bytes_do_not_fit() {
        let limits = CommandLimits {
            slots: Arc::new(Semaphore::new(3)),
            payload: Arc::new(Semaphore::new(10)),
        };
        let held = limits.try_reserve(1, 7).unwrap();
        assert!(limits.try_reserve(2, 4).is_err());
        assert_eq!(limits.slots.available_permits(), 2);
        assert_eq!(limits.payload.available_permits(), 3);
        assert!(limits.try_reserve(3, 0).is_err());
        drop(held);
        assert_eq!(limits.slots.available_permits(), 3);
        assert_eq!(limits.payload.available_permits(), 10);
    }

    #[test]
    fn large_payloads_are_bounded_and_small_payloads_are_preserved() {
        let value = json!({"data":"x".repeat(70_000)});
        let bytes = value.to_string().len();
        assert_eq!(
            bounded_payload(Some(value)),
            json!({"truncated":true,"originalBytes":bytes})
        );
        assert_eq!(
            bounded_payload(Some(json!({"status":"Accepted"}))),
            json!({"status":"Accepted"})
        );
    }

    #[test]
    fn bulk_outcomes_bound_untrusted_error_codes() {
        let station = kirby_csms_core::StationInfo {
            id: "CP-1".to_owned(),
            connected: true,
            connected_at: None,
            disconnected_at: None,
            remote_addr: None,
            subprotocol: Some("ocpp1.6".to_owned()),
            received_calls: 0,
            received_by_action: std::collections::BTreeMap::new(),
        };
        let answer = CallAnswer {
            outcome: Outcome::CallError,
            value: json!({"ok":false, "code":"x".repeat(80_000),
                "error":"untrusted error", "details":{}}),
        };
        let result = bulk_answer(&station, answer);
        assert_eq!(result["outcome"], "call_error");
        assert!(result["code"].as_str().unwrap().len() <= 128);
        assert!(result.to_string().len() < 1024);
    }
}

fn bulk_answer(station: &kirby_csms_core::StationInfo, mut answer: CallAnswer) -> Value {
    let (outcome, error_code) = match &answer.outcome {
        Outcome::Result => ("result", None),
        Outcome::CallError => ("call_error", Some("call_error")),
        Outcome::Timeout => ("timeout", Some("timeout")),
        Outcome::Failed(CsmsError::UnknownStation { .. }) => ("error", Some("unknown_station")),
        Outcome::Failed(CsmsError::NotConnected { .. } | CsmsError::ConnectionClosed { .. }) => {
            ("error", Some("station_not_connected"))
        }
        Outcome::Failed(CsmsError::QueueFull { .. }) => ("error", Some("too_many_requests")),
        Outcome::Failed(_) => ("error", Some("internal")),
    };
    let message: String = answer
        .value
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .take(1024)
        .collect();
    json!({
        "station": station.id, "protocol": station.subprotocol,
        "outcome": outcome, "ok": error_code.is_none(),
        "result": bounded_payload(answer.value.get_mut("result").map(Value::take)),
        "error": error_code.map(|code| json!({"code":code,"message":message})),
        "code": answer.value.get("code").and_then(Value::as_str)
            .map(|code| code.chars().take(128).collect::<String>()),
        "details": bounded_payload(answer.value.get_mut("details").map(Value::take)),
    })
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
        Ok(Frame::CallResult { mut payload, .. }) => {
            if !handle.show_secrets() {
                redact_payload(&mut payload);
            }
            (
                json!({"type": "result", "station": station, "action": action, "ok": true, "result": payload}),
                Outcome::Result,
            )
        }
        Ok(Frame::CallError {
            code,
            description,
            mut details,
            ..
        }) => {
            if !handle.show_secrets() {
                redact_payload(&mut details);
            }
            (
                json!({
                    "type": "result", "station": station, "action": action, "ok": false,
                    "error": format!("CALLERROR {}: {description}", code.as_str()),
                    "code": code.as_str(), "details": details,
                }),
                Outcome::CallError,
            )
        }
        Ok(_) => (
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
