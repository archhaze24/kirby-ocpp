//! TOML scripts for the central system: sequential steps, fail-fast, run against the
//! stations that connect while the server is up.
//!
//! ```toml
//! name = "smoke"
//! description = "optional"
//!
//! [[steps]]
//! action = "wait_station"      # station = "CP-1" and/or count = 2; booted = true waits for the boot
//! station = "CP-1"
//! timeout_ms = 20000
//!
//! [[steps]]
//! action = "call"
//! station = "CP-1"
//! call = "ChangeConfiguration"
//! payload = { key = "HeartbeatInterval", value = "60" }
//! expect_status = "Accepted"   # or: expect_error = true
//! ```
//!
//! Other steps: `wait_call {station?, call, payload_contains?, timeout_ms}`, `sleep {ms}`,
//! `respond {call, payload}`, `clear_respond {call}`,
//! `inject_fault {call, kind = "call_error"|"silence"|"close"|"invalid_payload", payload?, times}`.
//! Every step takes an optional `name`.

use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};

use kirby_csms_core::{CsmsEvent, CsmsHandle, Direction, Fault, Frame, Reply};
use kirby_ocpp_core::scenario::{ScenarioReport, StepReport, StepStatus};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::time::timeout_at;

/// Longest accepted wait or sleep: one day.
const MAX_WAIT_MS: u64 = 86_400_000;
/// Inbound CALLs remembered for `wait_call`; the oldest are forgotten beyond this.
const SEEN_CAP: usize = 10_000;

fn default_timeout_ms() -> u64 {
    5_000
}

fn default_wait_station_ms() -> u64 {
    30_000
}

fn default_times() -> usize {
    1
}

fn default_payload() -> Value {
    Value::Object(serde_json::Map::new())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Script {
    pub name: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub name: Option<String>,
    pub action: Action,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultKind {
    CallError,
    Silence,
    Close,
    InvalidPayload,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Wait until `station` is connected and/or at least `count` stations are (default: 1).
    WaitStation {
        #[serde(default)]
        station: Option<String>,
        #[serde(default)]
        count: Option<usize>,
        /// Also wait for the BootNotification to be tracked (the socket is open before).
        #[serde(default)]
        booted: bool,
        #[serde(default = "default_wait_station_ms")]
        timeout_ms: u64,
    },
    /// Wait for an inbound CALL not yet matched by an earlier `wait_call`.
    WaitCall {
        #[serde(default)]
        station: Option<String>,
        call: String,
        #[serde(default)]
        payload_contains: Option<Value>,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
    },
    /// Send a CALL and check its answer.
    Call {
        station: String,
        call: String,
        #[serde(default = "default_payload")]
        payload: Value,
        #[serde(default)]
        expect_status: Option<String>,
        #[serde(default)]
        expect_error: bool,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
    },
    Sleep {
        ms: u64,
    },
    /// Answer every inbound `call` with this CALLRESULT payload.
    Respond {
        call: String,
        payload: Value,
    },
    ClearRespond {
        call: String,
    },
    InjectFault {
        call: String,
        kind: FaultKind,
        #[serde(default)]
        payload: Option<Value>,
        #[serde(default = "default_times")]
        times: usize,
    },
}

#[derive(Debug, Deserialize)]
struct RawScript {
    name: String,
    #[serde(default)]
    steps: Vec<toml::Table>,
}

impl Script {
    /// Parses and validates a script.
    ///
    /// # Errors
    /// A message naming the offending step for unknown actions, missing or unknown fields and
    /// bad values.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let raw: RawScript =
            toml::from_str(text).map_err(|e| format!("invalid script: {}", e.message()))?;
        if raw.name.trim().is_empty() {
            return Err("invalid script: `name` must not be empty".to_owned());
        }
        if raw.steps.is_empty() {
            return Err("invalid script: a script needs at least one `[[steps]]`".to_owned());
        }
        let steps = raw
            .steps
            .into_iter()
            .enumerate()
            .map(|(i, table)| {
                parse_step(table).map_err(|e| format!("invalid script: step {}: {e}", i + 1))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            name: raw.name,
            steps,
        })
    }

    /// Reads and parses a script file.
    ///
    /// # Errors
    /// The file cannot be read, or see [`Script::from_toml`].
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read script file {}: {e}", path.display()))?;
        Self::from_toml(&text)
    }
}

fn parse_step(mut table: toml::Table) -> Result<Step, String> {
    let name = match table.remove("name") {
        None => None,
        Some(toml::Value::String(s)) => Some(s),
        Some(_) => return Err("`name` must be a string".to_owned()),
    };
    let tag = match table.get("action") {
        None => return Err("missing field `action`".to_owned()),
        Some(toml::Value::String(s)) => s.clone(),
        Some(_) => return Err("`action` must be a string".to_owned()),
    };
    let action = toml::Value::Table(table)
        .try_into::<Action>()
        .map_err(|e| format!("action `{tag}`: {}", e.message()))?;
    validate(&action)?;
    Ok(Step { name, action })
}

fn validate(action: &Action) -> Result<(), String> {
    let bad = |msg: &str| Err(msg.to_owned());
    let check_wait = |ms: u64| match ms {
        0 => bad("`timeout_ms` must be positive"),
        ms if ms > MAX_WAIT_MS => bad("`timeout_ms` must be at most 86400000 (one day)"),
        _ => Ok(()),
    };
    match action {
        Action::WaitStation {
            station,
            count,
            timeout_ms,
            ..
        } => {
            check_wait(*timeout_ms)?;
            if *count == Some(0) {
                return bad("`count` must be 1 or more");
            }
            if station.as_deref() == Some("") {
                return bad("`station` must not be empty");
            }
        }
        Action::WaitCall {
            call, timeout_ms, ..
        } => {
            check_wait(*timeout_ms)?;
            if call.is_empty() {
                return bad("`call` must not be empty");
            }
        }
        Action::Call {
            station,
            call,
            timeout_ms,
            expect_status,
            expect_error,
            ..
        } => {
            check_wait(*timeout_ms)?;
            if station.is_empty() || call.is_empty() {
                return bad("`station` and `call` must not be empty");
            }
            if *expect_error && expect_status.is_some() {
                return bad("`expect_status` and `expect_error` exclude each other");
            }
        }
        Action::Sleep { ms } if *ms > MAX_WAIT_MS => {
            return bad("`ms` must be at most 86400000 (one day)");
        }
        Action::Respond { call, .. } | Action::ClearRespond { call } if call.is_empty() => {
            return bad("`call` must not be empty");
        }
        Action::Sleep { .. } | Action::Respond { .. } | Action::ClearRespond { .. } => {}
        Action::InjectFault {
            call,
            kind,
            payload,
            times,
        } => {
            if call.is_empty() {
                return bad("`call` must not be empty");
            }
            if *times == 0 {
                return bad("`times` must be 1 or more");
            }
            if *kind == FaultKind::InvalidPayload && payload.is_none() {
                return bad("kind `invalid_payload` needs a `payload`");
            }
        }
    }
    Ok(())
}

impl Step {
    /// The display name: the given one, or a description of the action.
    pub fn title(&self) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        match &self.action {
            Action::WaitStation { station, count, .. } => match (station, count) {
                (Some(station), Some(count)) => format!("wait_station {station} (>= {count})"),
                (Some(station), None) => format!("wait_station {station}"),
                (None, count) => format!("wait_station (>= {})", count.unwrap_or(1)),
            },
            Action::WaitCall { station, call, .. } => match station {
                Some(station) => format!("wait_call {station} {call}"),
                None => format!("wait_call {call}"),
            },
            Action::Call { station, call, .. } => format!("call {station} {call}"),
            Action::Sleep { ms } => format!("sleep {ms} ms"),
            Action::Respond { call, .. } => format!("respond {call}"),
            Action::ClearRespond { call } => format!("clear_respond {call}"),
            Action::InjectFault { call, kind, .. } => {
                format!("inject_fault {call} {kind:?}")
            }
        }
    }
}

/// An inbound CALL seen by the runner.
#[derive(Debug)]
struct Seen {
    station: String,
    action: String,
    payload: Value,
}

struct Runner<'a> {
    handle: &'a CsmsHandle,
    events: broadcast::Receiver<CsmsEvent>,
    seen: VecDeque<Seen>,
    /// Index into `seen` of the first CALL `wait_call` may still match.
    cursor: usize,
    /// Events were lost to a slow receiver; `wait_call` may have missed a CALL.
    lagged: bool,
}

/// Runs the script against the live server. `deadline` bounds the whole run: the step that is
/// running when it passes fails and the rest are skipped.
///
/// `events` should come from a `subscribe()` made before any station can connect, so no CALL
/// is missed by `wait_call`.
pub async fn run(
    script: &Script,
    handle: &CsmsHandle,
    events: broadcast::Receiver<CsmsEvent>,
    deadline: Instant,
) -> ScenarioReport {
    let started = Instant::now();
    let mut runner = Runner {
        handle,
        events,
        seen: VecDeque::new(),
        cursor: 0,
        lagged: false,
    };
    let mut steps = Vec::with_capacity(script.steps.len());
    let mut failed = false;
    for (index, step) in script.steps.iter().enumerate() {
        let name = step.title();
        let began = Instant::now();
        let status = if failed {
            StepStatus::Skipped
        } else {
            let outcome = timeout_at(
                tokio::time::Instant::from_std(deadline),
                runner.execute(&step.action),
            )
            .await;
            match outcome {
                Ok(Ok(())) => StepStatus::Passed,
                Ok(Err(reason)) => StepStatus::Failed(reason),
                Err(_) => StepStatus::Failed("the script --timeout was reached".to_owned()),
            }
        };
        failed |= matches!(status, StepStatus::Failed(_));
        steps.push(StepReport {
            index,
            name,
            status,
            duration_ms: millis(began.elapsed()),
        });
    }
    ScenarioReport {
        scenario: script.name.clone(),
        passed: !failed,
        cancelled: false,
        steps,
        duration_ms: millis(started.elapsed()),
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl Runner<'_> {
    #[allow(clippy::too_many_lines, reason = "one arm per script action")]
    async fn execute(&mut self, action: &Action) -> Result<(), String> {
        match action {
            Action::WaitStation {
                station,
                count,
                booted,
                timeout_ms,
            } => {
                let booted = *booted;
                let end = end_of(*timeout_ms);
                let need = count.unwrap_or(1);
                let count = *count;
                loop {
                    self.drain();
                    if self.stations_ready(station.as_deref(), count, booted) {
                        return Ok(());
                    }
                    if self.next_event(end).await.is_err() {
                        return Err(match station {
                            Some(station) => format!(
                                "station {station} did not connect within {timeout_ms} ms ({} connected)",
                                self.connected(booted)
                            ),
                            None => format!(
                                "fewer than {need} stations connected within {timeout_ms} ms ({} connected)",
                                self.connected(booted)
                            ),
                        });
                    }
                }
            }
            Action::WaitCall {
                station,
                call,
                payload_contains,
                timeout_ms,
            } => {
                let end = end_of(*timeout_ms);
                loop {
                    self.drain();
                    if self.take_call(station.as_deref(), call, payload_contains.as_ref()) {
                        return Ok(());
                    }
                    if self.next_event(end).await.is_err() {
                        let lost = if self.lagged {
                            " (events were dropped, a CALL may have been missed)"
                        } else {
                            ""
                        };
                        return Err(format!(
                            "no matching {call} CALL within {timeout_ms} ms{lost}"
                        ));
                    }
                }
            }
            Action::Call {
                station,
                call,
                payload,
                expect_status,
                expect_error,
                timeout_ms,
            } => {
                let timeout = Duration::from_millis(*timeout_ms);
                let sent = self
                    .handle
                    .call_timeout(station, call, payload.clone(), timeout);
                // The engine times the answer; this also bounds the time in the queue.
                let Ok(result) = tokio::time::timeout(timeout + Duration::from_secs(1), sent).await
                else {
                    return Err(format!("{call} was still queued after {timeout_ms} ms"));
                };
                check_answer(result, call, expect_status.as_deref(), *expect_error)
            }
            Action::Sleep { ms } => {
                tokio::time::sleep(Duration::from_millis(*ms)).await;
                Ok(())
            }
            Action::Respond { call, payload } => {
                let payload = payload.clone();
                self.handle
                    .respond(call, move |_| Reply::Result(payload.clone()));
                Ok(())
            }
            Action::ClearRespond { call } => {
                self.handle.clear_responder(call);
                Ok(())
            }
            Action::InjectFault {
                call,
                kind,
                payload,
                times,
            } => {
                let fault = match (kind, payload) {
                    (FaultKind::CallError, _) => Fault::call_error(call),
                    (FaultKind::Silence, _) => Fault::silence(call),
                    (FaultKind::Close, _) => Fault::close(call),
                    (FaultKind::InvalidPayload, payload) => {
                        Fault::invalid_payload(call, payload.clone().unwrap_or_default())
                    }
                };
                self.handle.inject(fault.times(*times));
                Ok(())
            }
        }
    }

    /// Connected stations (with `booted`: only those whose BootNotification was tracked).
    fn connected(&self, booted: bool) -> usize {
        self.handle
            .stations()
            .iter()
            .filter(|s| s.connected && self.is_ready(&s.id, booted))
            .count()
    }

    fn is_ready(&self, id: &str, booted: bool) -> bool {
        !booted
            || self
                .handle
                .state(id)
                .is_some_and(|state| state.boot.is_some())
    }

    fn stations_ready(&self, station: Option<&str>, count: Option<usize>, booted: bool) -> bool {
        let named_ok = station.is_none_or(|id| {
            self.handle
                .station(id)
                .is_some_and(|info| info.connected && self.is_ready(id, booted))
        });
        let count_ok = match (station, count) {
            (Some(_), None) => true,
            (_, count) => self.connected(booted) >= count.unwrap_or(1),
        };
        named_ok && count_ok
    }

    /// Moves every queued event into the `seen` list.
    fn drain(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(event) => self.record(event),
                Err(TryRecvError::Lagged(_)) => self.lagged = true,
                Err(TryRecvError::Empty | TryRecvError::Closed) => return,
            }
        }
    }

    /// Waits for one more event. `Err` when `end` passed or the event stream is gone.
    async fn next_event(&mut self, end: tokio::time::Instant) -> Result<(), ()> {
        match timeout_at(end, self.events.recv()).await {
            Ok(Ok(event)) => {
                self.record(event);
                Ok(())
            }
            Ok(Err(RecvError::Lagged(_))) => {
                self.lagged = true;
                Ok(())
            }
            Ok(Err(RecvError::Closed)) | Err(_) => Err(()),
        }
    }

    fn record(&mut self, event: CsmsEvent) {
        if let CsmsEvent::Frame {
            station,
            direction: Direction::In,
            frame: Frame::Call {
                action, payload, ..
            },
        } = event
        {
            if self.seen.len() >= SEEN_CAP {
                self.seen.pop_front();
                self.cursor = self.cursor.saturating_sub(1);
            }
            self.seen.push_back(Seen {
                station,
                action,
                payload,
            });
        }
    }

    /// Finds the first unmatched CALL that fits and moves the cursor behind it.
    fn take_call(&mut self, station: Option<&str>, call: &str, contains: Option<&Value>) -> bool {
        let found = self
            .seen
            .iter()
            .enumerate()
            .skip(self.cursor)
            .find(|(_, seen)| {
                seen.action == call
                    && station.is_none_or(|id| seen.station == id)
                    && contains.is_none_or(|expected| json_subset(expected, &seen.payload))
            });
        match found {
            Some((index, _)) => {
                self.cursor = index + 1;
                true
            }
            None => false,
        }
    }
}

fn end_of(timeout_ms: u64) -> tokio::time::Instant {
    tokio::time::Instant::now() + Duration::from_millis(timeout_ms)
}

fn check_answer(
    result: Result<Frame, kirby_csms_core::CsmsError>,
    call: &str,
    expect_status: Option<&str>,
    expect_error: bool,
) -> Result<(), String> {
    match result {
        Ok(Frame::CallResult { payload, .. }) => {
            if expect_error {
                return Err(format!("{call}: expected a CALLERROR, got a CALLRESULT"));
            }
            match expect_status {
                None => Ok(()),
                Some(expected) => {
                    let actual = payload.get("status").and_then(Value::as_str);
                    if actual == Some(expected) {
                        Ok(())
                    } else {
                        Err(format!(
                            "{call}: expected status {expected}, got {}",
                            actual.map_or_else(|| format!("payload {payload}"), str::to_owned)
                        ))
                    }
                }
            }
        }
        Ok(Frame::CallError {
            code, description, ..
        }) => {
            if expect_error {
                Ok(())
            } else {
                Err(format!(
                    "{call}: CALLERROR {}: {description}",
                    code.as_str()
                ))
            }
        }
        Ok(Frame::Call { .. }) => Err(format!("{call}: unexpected CALL as an answer")),
        Err(error) => Err(error.to_string()),
    }
}

/// `true` if every part of `expected` is present in `actual` (objects are matched by key,
/// arrays element-wise, everything else by equality). Same rule as the station scenarios.
fn json_subset(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Object(e), Value::Object(a)) => e
            .iter()
            .all(|(key, value)| a.get(key).is_some_and(|v| json_subset(value, v))),
        (Value::Array(e), Value::Array(a)) => {
            e.len() == a.len() && e.iter().zip(a).all(|(e, a)| json_subset(e, a))
        }
        _ => expected == actual,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(text: &str) -> String {
        Script::from_toml(text).unwrap_err()
    }

    #[test]
    fn parses_every_step() {
        let script = Script::from_toml(
            r#"
name = "t"
[[steps]]
action = "wait_station"
count = 2
[[steps]]
action = "wait_call"
call = "Authorize"
payload_contains = { idTag = "A" }
[[steps]]
name = "cfg"
action = "call"
station = "CP-1"
call = "ChangeConfiguration"
payload = { key = "K", value = "1" }
expect_status = "Accepted"
[[steps]]
action = "sleep"
ms = 10
[[steps]]
action = "respond"
call = "Authorize"
payload = { idTagInfo = { status = "Blocked" } }
[[steps]]
action = "clear_respond"
call = "Authorize"
[[steps]]
action = "inject_fault"
call = "Heartbeat"
kind = "close"
times = 2
"#,
        )
        .unwrap();
        assert_eq!(script.steps.len(), 7);
        assert_eq!(script.steps[2].title(), "cfg");
        assert_eq!(script.steps[0].title(), "wait_station (>= 2)");
    }

    #[test]
    fn errors_name_the_step() {
        let head = "name = \"t\"\n[[steps]]\naction = \"sleep\"\nms = 1\n[[steps]]\n";
        assert!(err(&format!("{head}action = \"nope\"")).contains("step 2"));
        assert!(err(&format!("{head}action = \"call\"\nstation = \"a\"")).contains("step 2"));
        assert!(err(&format!("{head}action = \"sleep\"\nms = 1\nextra = 1")).contains("step 2"));
        assert!(
            err(&format!(
                "{head}action = \"wait_call\"\ncall = \"X\"\ntimeout_ms = 0"
            ))
            .contains("positive")
        );
        assert!(
            err(&format!(
                "{head}action = \"inject_fault\"\ncall = \"X\"\nkind = \"invalid_payload\""
            ))
            .contains("payload")
        );
        assert!(err("name = \"t\"").contains("at least one"));
    }

    #[test]
    fn answers_are_checked() {
        let ok = |status: &str| {
            Ok(Frame::call_result(
                "1",
                serde_json::json!({"status": status}),
            ))
        };
        assert!(check_answer(ok("Accepted"), "X", Some("Accepted"), false).is_ok());
        assert!(check_answer(ok("Rejected"), "X", Some("Accepted"), false).is_err());
        assert!(check_answer(ok("Accepted"), "X", None, true).is_err());
    }
}
