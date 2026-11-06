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
//!
//! Variables: an optional top-level `[vars]` table (strings, numbers, booleans; `--var`
//! overrides it) and `capture = { name = "/json/pointer" }` on `call` (from the CALLRESULT
//! payload) and `wait_call` (from the matched CALL payload). `${name}` is substituted at run
//! time in `station`, `expect_status` and the JSON of `payload`, `payload_contains`,
//! `expect_payload` and `respond`/`inject_fault` payloads. Action names (`call`) and typed
//! fields (`ms`, `times`, `timeout_ms`, `count`, `booted`) are never substituted. `call` also
//! takes `expect_payload = { ... }`: a JSON subset the CALLRESULT payload must contain.
//!
//! `wait_call` consumes the CALL it matches (each CALL satisfies at most one `wait_call`), but
//! waits may come in any order: a later wait can match a CALL that arrived before an earlier
//! wait's match. CALLs that arrive while other steps run are kept (the last 10 000).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use kirby_csms_core::{CsmsEvent, CsmsHandle, Direction, Frame, Reply};
use kirby_ocpp_core::scenario::vars::{self, Vars};
use kirby_ocpp_core::scenario::{ScenarioReport, StepReport, StepStatus};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Notify;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use tokio::time::timeout_at;

use crate::commands;

/// Longest accepted wait or sleep: one day.
const MAX_WAIT_MS: u64 = 86_400_000;
/// Inbound CALLs remembered for `wait_call`; the oldest are forgotten beyond this.
const SEEN_CAP: usize = 10_000;
/// Approximate bytes of remembered CALLs (names plus kept payloads); the oldest go first.
const SEEN_BYTES: usize = 16 * 1024 * 1024;

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
    /// Declared `[vars]`; `--var` overrides are added with [`Script::set_var`].
    pub vars: Vars,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub name: Option<String>,
    pub action: Action,
}

pub use crate::commands::FaultKind;

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
        /// Variable name -> JSON pointer into the matched CALL payload.
        #[serde(default)]
        capture: BTreeMap<String, String>,
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
        /// JSON subset the CALLRESULT payload must contain.
        #[serde(default)]
        expect_payload: Option<Value>,
        /// Variable name -> JSON pointer into the CALLRESULT payload.
        #[serde(default)]
        capture: BTreeMap<String, String>,
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
    vars: toml::Table,
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
        let mut script_vars = Vars::new();
        for (name, value) in raw.vars {
            let value = serde_json::to_value(&value)
                .map_err(|e| format!("invalid script: [vars] `{name}`: {e}"))?;
            vars::check_scalar(&value)
                .and_then(|()| script_vars.insert(&name, value))
                .map_err(|e| format!("invalid script: [vars] `{name}`: {e}"))?;
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
            vars: script_vars,
            steps,
        })
    }

    /// Sets (or replaces) a variable, e.g. from `--var`.
    ///
    /// # Errors
    /// The name is not valid.
    pub fn set_var(&mut self, name: &str, value: Value) -> Result<(), String> {
        self.vars.insert(name, value)
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
            call,
            timeout_ms,
            capture,
            ..
        } => {
            check_wait(*timeout_ms)?;
            if call.is_empty() {
                return bad("`call` must not be empty");
            }
            check_captures(capture)?;
        }
        Action::Call {
            station,
            call,
            timeout_ms,
            expect_status,
            expect_error,
            expect_payload,
            capture,
            ..
        } => {
            check_wait(*timeout_ms)?;
            check_captures(capture)?;
            if *expect_error && (expect_payload.is_some() || !capture.is_empty()) {
                return bad("`expect_error` excludes `expect_payload` and `capture`");
            }
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
            if let Err(message) = commands::check_fault(call, *kind, payload.as_ref(), *times) {
                return bad(&message);
            }
        }
    }
    Ok(())
}

fn check_captures(capture: &BTreeMap<String, String>) -> Result<(), String> {
    for (name, pointer) in capture {
        if !vars::is_valid_name(name) {
            return Err(format!("capture: `{name}` is not a valid variable name"));
        }
        vars::check_pointer(pointer).map_err(|e| format!("capture `{name}`: {e}"))?;
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

/// An inbound CALL seen by the pump.
#[derive(Debug)]
struct Seen {
    station: String,
    action: String,
    /// `Null` unless a `wait_call` for this action matches on the payload.
    payload: Value,
    /// A `wait_call` took it already.
    consumed: bool,
    /// Approximate memory of the entry.
    bytes: usize,
}

/// The remembered CALLs and their approximate size.
#[derive(Default)]
struct SeenStore {
    calls: VecDeque<Seen>,
    bytes: usize,
}

/// What the pump task collects while steps run.
struct Backlog {
    seen: Mutex<SeenStore>,
    /// Actions some `wait_call` waits for -> whether any such step matches on the payload.
    /// Other CALLs are not remembered at all.
    wanted: HashMap<String, bool>,
    max_bytes: usize,
    /// Events were lost to a slow receiver; `wait_call` may have missed a CALL.
    lagged: AtomicBool,
    /// The engine's event stream ended.
    closed: AtomicBool,
    /// Signalled after every event (the single waiter is the runner).
    wake: Notify,
}

/// Why `wait_call` gave up.
enum WaitLost {
    Timeout,
    Lagged,
}

/// Moves engine events into the [`Backlog`] continuously, also during long steps, so the
/// broadcast buffer cannot overflow while the runner is busy. Aborted on drop.
struct Pump {
    task: JoinHandle<()>,
}

impl Pump {
    fn spawn(mut events: broadcast::Receiver<CsmsEvent>, backlog: Arc<Backlog>) -> Self {
        let task = tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => backlog.record(event),
                    Err(RecvError::Lagged(_)) => backlog.lagged.store(true, Ordering::Relaxed),
                    Err(RecvError::Closed) => {
                        backlog.closed.store(true, Ordering::Relaxed);
                        backlog.wake.notify_one();
                        return;
                    }
                }
                backlog.wake.notify_one();
            }
        });
        Self { task }
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Backlog {
    fn new(script: &Script) -> Self {
        let mut wanted: HashMap<String, bool> = HashMap::new();
        for step in &script.steps {
            if let Action::WaitCall {
                call,
                payload_contains,
                capture,
                ..
            } = &step.action
            {
                *wanted.entry(call.clone()).or_default() |=
                    payload_contains.is_some() || !capture.is_empty();
            }
        }
        Self::with_wanted(wanted, SEEN_BYTES)
    }

    fn with_wanted(wanted: HashMap<String, bool>, max_bytes: usize) -> Self {
        Self {
            seen: Mutex::new(SeenStore::default()),
            wanted,
            max_bytes,
            lagged: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            wake: Notify::new(),
        }
    }

    /// Waits for a matching CALL and returns its payload (`Null` unless the action's payload
    /// is kept). Fails at once when events were lost and nothing matches:
    /// the CALL may be among the lost ones, so waiting out the timeout would only stall.
    async fn wait_call(
        &self,
        station: Option<&str>,
        call: &str,
        payload_contains: Option<&Value>,
        end: tokio::time::Instant,
    ) -> Result<Value, WaitLost> {
        loop {
            if let Some(payload) = self.take_call(station, call, payload_contains) {
                return Ok(payload);
            }
            if self.lagged.load(Ordering::Relaxed) {
                return Err(WaitLost::Lagged);
            }
            if self.next_event(end).await.is_err() {
                return Err(WaitLost::Timeout);
            }
        }
    }

    async fn next_event(&self, end: tokio::time::Instant) -> Result<(), ()> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(());
        }
        timeout_at(end, self.wake.notified()).await.map_err(|_| ())
    }

    fn record(&self, event: CsmsEvent) {
        if let CsmsEvent::Frame {
            station,
            direction: Direction::In,
            frame: Frame::Call {
                action, payload, ..
            },
        } = event
        {
            let Some(&needs_payload) = self.wanted.get(&action) else {
                return;
            };
            let payload = if needs_payload { payload } else { Value::Null };
            let bytes = station.len()
                + action.len()
                + 64
                + if needs_payload {
                    payload.to_string().len()
                } else {
                    0
                };
            let mut store = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
            store.bytes += bytes;
            store.calls.push_back(Seen {
                station,
                action,
                payload,
                consumed: false,
                bytes,
            });
            while store.calls.len() > 1
                && (store.calls.len() > SEEN_CAP || store.bytes > self.max_bytes)
            {
                if let Some(old) = store.calls.pop_front() {
                    store.bytes = store.bytes.saturating_sub(old.bytes);
                }
            }
        }
    }

    /// Consumes the oldest unconsumed CALL that fits and returns its kept payload.
    fn take_call(
        &self,
        station: Option<&str>,
        call: &str,
        contains: Option<&Value>,
    ) -> Option<Value> {
        let mut store = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let found = store.calls.iter_mut().find(|seen| {
            !seen.consumed
                && seen.action == call
                && station.is_none_or(|id| seen.station == id)
                && contains.is_none_or(|expected| json_subset(expected, &seen.payload))
        });
        found.map(|seen| {
            seen.consumed = true;
            seen.payload.clone()
        })
    }
}

struct Runner<'a> {
    handle: &'a CsmsHandle,
    backlog: Arc<Backlog>,
    vars: Vars,
    _pump: Pump,
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
    let backlog = Arc::new(Backlog::new(script));
    let mut runner = Runner {
        handle,
        vars: script.vars.clone(),
        _pump: Pump::spawn(events, Arc::clone(&backlog)),
        backlog,
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
                let station = self.sub_opt(station.as_deref())?;
                loop {
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
                capture,
                timeout_ms,
            } => {
                let station = self.sub_opt(station.as_deref())?;
                let contains = payload_contains
                    .as_ref()
                    .map(|v| self.sub("payload_contains", v))
                    .transpose()?;
                let end = end_of(*timeout_ms);
                let payload = self
                    .backlog
                    .wait_call(station.as_deref(), call, contains.as_ref(), end)
                    .await
                    .map_err(|lost| match lost {
                        WaitLost::Timeout => {
                            format!("no matching {call} CALL within {timeout_ms} ms")
                        }
                        WaitLost::Lagged => format!(
                            "events were lost (the event stream lagged), so a {call} CALL may have been missed"
                        ),
                    })?;
                self.capture(capture, &payload)
            }
            Action::Call {
                station,
                call,
                payload,
                expect_status,
                expect_error,
                expect_payload,
                capture,
                timeout_ms,
            } => {
                let station = self.sub_str("station", station)?;
                let payload = self.sub("payload", payload)?;
                let expect_status = expect_status
                    .as_deref()
                    .map(|s| self.sub_str("expect_status", s))
                    .transpose()?;
                let expect_payload = expect_payload
                    .as_ref()
                    .map(|v| self.sub("expect_payload", v))
                    .transpose()?;
                let timeout = Duration::from_millis(*timeout_ms);
                let sent = self.handle.call_timeout(&station, call, payload, timeout);
                // The engine times the answer; this also bounds the time in the queue.
                let Ok(result) = tokio::time::timeout(timeout + Duration::from_secs(1), sent).await
                else {
                    return Err(format!("{call} was still queued after {timeout_ms} ms"));
                };
                let answer = check_answer(result, call, expect_status.as_deref(), *expect_error)?;
                if let Some(expected) = &expect_payload
                    && !json_subset(expected, &answer)
                {
                    return Err(format!(
                        "{call}: payload {answer} does not contain {expected}"
                    ));
                }
                self.capture(capture, &answer)
            }
            Action::Sleep { ms } => {
                tokio::time::sleep(Duration::from_millis(*ms)).await;
                Ok(())
            }
            Action::Respond { call, payload } => {
                let payload = self.sub("payload", payload)?;
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
                let payload = payload
                    .as_ref()
                    .map(|v| self.sub("payload", v))
                    .transpose()?;
                self.handle
                    .inject(commands::build_fault(call, *kind, payload, *times));
                Ok(())
            }
        }
    }

    fn sub(&self, field: &str, value: &Value) -> Result<Value, String> {
        vars::substitute(value, &self.vars).map_err(|e| format!("`{field}`: {e}"))
    }

    fn sub_str(&self, field: &str, text: &str) -> Result<String, String> {
        vars::substitute_str(text, &self.vars).map_err(|e| format!("`{field}`: {e}"))
    }

    fn sub_opt(&self, station: Option<&str>) -> Result<Option<String>, String> {
        station.map(|s| self.sub_str("station", s)).transpose()
    }

    /// Stores the values of `payload` at the pointers as variables.
    fn capture(
        &mut self,
        capture: &BTreeMap<String, String>,
        payload: &Value,
    ) -> Result<(), String> {
        for (name, pointer) in capture {
            let value =
                vars::capture(payload, pointer).map_err(|e| format!("capture `{name}`: {e}"))?;
            self.vars.insert(name, value)?;
        }
        Ok(())
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

    /// Waits until the pump saw another event. `Err` when `end` passed or the stream is gone.
    async fn next_event(&mut self, end: tokio::time::Instant) -> Result<(), ()> {
        self.backlog.next_event(end).await
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
) -> Result<Value, String> {
    match result {
        Ok(Frame::CallResult { payload, .. }) => {
            if expect_error {
                return Err(format!("{call}: expected a CALLERROR, got a CALLRESULT"));
            }
            match expect_status {
                None => Ok(payload),
                Some(expected) => {
                    let actual = payload.get("status").and_then(Value::as_str);
                    if actual == Some(expected) {
                        Ok(payload)
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
                Ok(Value::Null)
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
    use tokio::time::timeout;

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

    #[test]
    fn vars_capture_and_expect_payload_are_validated() {
        let head = "name = \"t\"\n";
        let script = Script::from_toml(&format!(
            "{head}[vars]\nn = 5\ns = \"x\"\nb = true\n[[steps]]\naction = \"wait_call\"\ncall = \"Authorize\"\ncapture = {{ tag = \"/idTag\", all = \"\" }}\n[[steps]]\naction = \"call\"\nstation = \"CP-${{s}}\"\ncall = \"X\"\nexpect_payload = {{ status = \"Accepted\" }}\ncapture = {{ st = \"/status\" }}\n"
        ))
        .unwrap();
        assert_eq!(script.vars.len(), 3);
        assert_eq!(script.vars.get("n"), Some(&serde_json::json!(5)));
        let step = |body: &str| err(&format!("{head}[[steps]]\n{body}"));
        assert!(
            err(&format!(
                "{head}[vars]\nbad-name = 1\n[[steps]]\naction = \"sleep\"\nms = 1"
            ))
            .contains("bad-name")
        );
        assert!(
            err(&format!(
                "{head}[vars]\nl = [1]\n[[steps]]\naction = \"sleep\"\nms = 1"
            ))
            .contains("`l`")
        );
        assert!(
            step("action = \"wait_call\"\ncall = \"X\"\ncapture = { \"a b\" = \"/x\" }")
                .contains("a b")
        );
        assert!(
            step("action = \"wait_call\"\ncall = \"X\"\ncapture = { a = \"x\" }")
                .contains("JSON pointer")
        );
        assert!(step("action = \"call\"\nstation = \"a\"\ncall = \"X\"\nexpect_error = true\nexpect_payload = {}").contains("exclud"));
        assert!(step("action = \"call\"\nstation = \"a\"\ncall = \"X\"\nexpect_error = true\ncapture = { a = \"/x\" }").contains("exclud"));
    }

    #[test]
    fn answers_return_the_payload() {
        let ok = Ok(Frame::call_result(
            "1",
            serde_json::json!({"status": "Accepted", "n": 1}),
        ));
        let payload = check_answer(ok, "X", Some("Accepted"), false).unwrap();
        assert_eq!(payload["n"], 1);
    }

    #[test]
    fn backlog_keeps_payload_for_capture_and_returns_it() {
        let script = Script::from_toml(
            "name = \"t\"\n[[steps]]\naction = \"wait_call\"\ncall = \"Authorize\"\ncapture = { tag = \"/idTag\" }\n",
        )
        .unwrap();
        let backlog = Backlog::new(&script);
        backlog.record(inbound(
            "CP-1",
            "Authorize",
            serde_json::json!({"idTag": "A"}),
        ));
        let got = backlog.take_call(None, "Authorize", None).unwrap();
        assert_eq!(got["idTag"], "A");
    }

    #[test]
    fn substitution_and_capture_failures_have_reasons() {
        let mut vars = Vars::new();
        vars.insert("n", serde_json::json!(5)).unwrap();
        let sub = vars::substitute(&serde_json::json!({"a": "${n}", "b": "x${n}"}), &vars).unwrap();
        assert_eq!(sub, serde_json::json!({"a": 5, "b": "x5"}));
        assert!(
            vars::substitute(&serde_json::json!("${nope}"), &vars)
                .unwrap_err()
                .contains("nope")
        );
        assert!(
            vars::capture(&serde_json::json!({"a": 1}), "/b")
                .unwrap_err()
                .contains("/b")
        );
    }

    fn inbound(station: &str, action: &str, payload: Value) -> CsmsEvent {
        CsmsEvent::Frame {
            station: station.to_owned(),
            direction: Direction::In,
            frame: Frame::call("1", action, payload),
        }
    }

    #[test]
    fn backlog_keeps_only_what_wait_call_needs() {
        let script = Script::from_toml(
            r#"
name = "t"
[[steps]]
action = "wait_call"
call = "Authorize"
payload_contains = { idTag = "A" }
[[steps]]
action = "wait_call"
call = "Heartbeat"
"#,
        )
        .unwrap();
        let backlog = Backlog::new(&script);
        let big = serde_json::json!({"idTag": "A", "pad": "x".repeat(50_000)});
        backlog.record(inbound("CP-1", "StatusNotification", big.clone()));
        backlog.record(inbound("CP-1", "Authorize", big));
        backlog.record(inbound(
            "CP-1",
            "Heartbeat",
            serde_json::json!({"pad": "y".repeat(50_000)}),
        ));
        {
            let store = backlog.seen.lock().unwrap();
            // The unwanted action is not kept; Heartbeat keeps no payload.
            assert_eq!(store.calls.len(), 2);
            assert_eq!(store.calls[1].payload, Value::Null);
            assert!(store.bytes < 200_000);
        }
        assert!(backlog.take_call(Some("CP-1"), "Heartbeat", None).is_some());
        assert!(backlog.take_call(Some("CP-1"), "Heartbeat", None).is_none());
        let wanted = serde_json::json!({"idTag": "A"});
        assert!(
            backlog
                .take_call(None, "Authorize", Some(&wanted))
                .is_some()
        );
    }

    #[test]
    fn backlog_byte_budget_forgets_the_oldest() {
        let wanted = HashMap::from([("Authorize".to_owned(), true)]);
        let backlog = Backlog::with_wanted(wanted, 150_000);
        for i in 0..20 {
            let pad = "z".repeat(50_000);
            backlog.record(inbound(
                &format!("CP-{i}"),
                "Authorize",
                serde_json::json!({"pad": pad}),
            ));
        }
        let store = backlog.seen.lock().unwrap();
        assert!(store.calls.len() <= 3 && !store.calls.is_empty());
        assert_eq!(store.calls.back().unwrap().station, "CP-19");
    }

    #[tokio::test]
    async fn wait_call_fails_at_once_after_lagged_events() {
        let wanted = HashMap::from([("Authorize".to_owned(), false)]);
        let backlog = Backlog::with_wanted(wanted, 1000);
        backlog.lagged.store(true, Ordering::Relaxed);
        let end = tokio::time::Instant::now() + Duration::from_secs(3600);
        let out = timeout(
            Duration::from_secs(5),
            backlog.wait_call(None, "Authorize", None, end),
        )
        .await
        .expect("must not wait out the step timeout");
        assert!(matches!(out, Err(WaitLost::Lagged)));
        // A CALL that did arrive still matches.
        backlog.record(inbound("CP-1", "Authorize", Value::Null));
        let out = backlog.wait_call(None, "Authorize", None, end).await;
        assert!(out.is_ok());
    }
}
