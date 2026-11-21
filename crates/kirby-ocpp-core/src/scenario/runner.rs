//! Running a [`Scenario`] against a station: a [`StationHandle`] (OCPP 1.6) or a
//! [`Station2Handle`](crate::station2::Station2Handle) (OCPP 2.0.1).
//!
//! The runner itself (frame bookkeeping, `expect_call`, variables, reports) is protocol-neutral.
//! What differs per protocol sits behind [`ScenarioTarget`].

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};
use tokio::time::{sleep, sleep_until, timeout};

use super::model::{Action, Expect, ReplyError, Step};
use super::vars::{self, Vars};
use super::{CancelToken, Scenario, ScenarioError};
use crate::error::StationError;
use crate::ocpp::Protocol;
use crate::station::ResponseRule;
use crate::station::response_rules::canonical_action;
use crate::station::scripted_reply::{ScriptedAnswer, ScriptedReply};
use crate::station::{Command, Event, FrameRecord, Outcome, StationHandle};

/// How one step ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "reason", rename_all = "snake_case")]
pub enum StepStatus {
    Passed,
    Failed(String),
    /// Not run because an earlier step failed or the run was cancelled.
    Skipped,
}

/// Result of one step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepReport {
    /// 0-based position in the scenario.
    pub index: usize,
    pub name: String,
    #[serde(flatten)]
    pub status: StepStatus,
    pub duration_ms: u64,
    /// Label, action and failure evidence (boxed to keep the type small).
    #[serde(flatten)]
    pub detail: Box<StepDetail>,
}

/// The machine-readable part of a [`StepReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepDetail {
    /// Stable label for machine-readable reports (see [`Step::stable_label`]).
    pub label: String,
    /// The step's action name (empty when the producer does not know it).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub action: String,
    /// Failed steps only: the resolved parameters (secrets redacted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// Failed assertions only: what was expected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<Value>,
    /// Failed assertions only: what was seen, clipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
}

impl StepReport {
    /// A report without failure evidence; the label is the name.
    #[must_use]
    pub fn new(index: usize, name: String, status: StepStatus, duration_ms: u64) -> Self {
        Self {
            index,
            detail: Box::new(StepDetail {
                label: name.clone(),
                action: String::new(),
                params: None,
                expected: None,
                actual: None,
            }),
            name,
            status,
            duration_ms,
        }
    }
}

/// Overall outcome of a run, in the vocabulary shared by every report level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Passed,
    Failed,
    /// The run produced no report (the station could not connect or boot).
    Error,
    Skipped,
}

impl RunStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Error => "error",
            Self::Skipped => "skipped",
        }
    }
}

/// Result of a whole run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScenarioReport {
    /// Report format version ([`super::SCHEMA_VERSION`]).
    pub schema: u32,
    pub ocpp_version: String,
    pub tool_version: String,
    /// Station id; `None` for runs without a station (CSMS scripts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub station: Option<String>,
    /// File path or `builtin:NAME` of the scenario.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// RFC 3339 UTC time the run started.
    pub started_at: String,
    pub scenario: String,
    pub status: RunStatus,
    /// Every step passed.
    pub passed: bool,
    /// The run was stopped through its [`CancelToken`].
    pub cancelled: bool,
    pub steps: Vec<StepReport>,
    pub duration_ms: u64,
}

impl ScenarioReport {
    /// A report with the current schema and OCPP version, no station or source, started now.
    #[must_use]
    pub fn new(
        scenario: String,
        passed: bool,
        cancelled: bool,
        steps: Vec<StepReport>,
        duration_ms: u64,
    ) -> Self {
        Self {
            schema: super::SCHEMA_VERSION,
            ocpp_version: super::OCPP_VERSION.to_owned(),
            tool_version: super::TOOL_VERSION.to_owned(),
            station: None,
            source: None,
            started_at: super::now_rfc3339(),
            scenario,
            status: if passed {
                RunStatus::Passed
            } else {
                RunStatus::Failed
            },
            passed,
            cancelled,
            steps,
            duration_ms,
        }
    }
}

/// Live progress, for UIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScenarioProgress {
    StepStarted { index: usize, name: String },
    StepFinished(StepReport),
}

/// Refuses to run two scenarios at once on one station. Clones share the guard.
#[derive(Debug, Clone)]
pub struct ScenarioRunner {
    handle: StationHandle,
    running: Arc<AtomicBool>,
}

struct Guard(Arc<AtomicBool>);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl ScenarioRunner {
    pub fn new(handle: StationHandle) -> Self {
        Self {
            handle,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Runs `scenario`.
    ///
    /// # Errors
    /// [`ScenarioError::AlreadyRunning`] if another run is in progress.
    pub async fn run(
        &self,
        scenario: &Scenario,
        cancel: &CancelToken,
        progress: Option<&mpsc::UnboundedSender<ScenarioProgress>>,
    ) -> Result<ScenarioReport, ScenarioError> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(ScenarioError::AlreadyRunning);
        }
        let _guard = Guard(self.running.clone());
        Ok(run_scenario(&self.handle, scenario, cancel, progress).await)
    }
}

/// Runs the steps in order and stops at the first failure (the rest is `Skipped`).
/// Cancelling `cancel` fails the step in flight with "cancelled".
///
/// `handle` is a [`StationHandle`] or a [`Station2Handle`](crate::station2::Station2Handle);
/// the report's `ocpp_version` is the station's.
///
/// This function has no re-entrancy guard; use [`ScenarioRunner`] for that.
pub async fn run_scenario<T: ScenarioTarget>(
    handle: &T,
    scenario: &Scenario,
    cancel: &CancelToken,
    progress: Option<&mpsc::UnboundedSender<ScenarioProgress>>,
) -> ScenarioReport {
    run_scenario_with_events(handle, scenario, cancel, progress, handle.subscribe()).await
}

/// [`run_scenario`] with variables: `overrides` (for example from `--var`) replace the
/// scenario's `[vars]`, and `index` is the value of the built-in `${index}` (the station's
/// place in a fleet, 1 for a single station). `${station_id}` is the station's id.
///
/// Names in `overrides` that are built-ins are ignored (the built-ins always win).
pub async fn run_scenario_with_vars<T: ScenarioTarget>(
    handle: &T,
    scenario: &Scenario,
    cancel: &CancelToken,
    progress: Option<&mpsc::UnboundedSender<ScenarioProgress>>,
    overrides: &Vars,
    index: u64,
) -> ScenarioReport {
    run_scenario_subscribed(
        handle,
        scenario,
        cancel,
        progress,
        overrides,
        index,
        handle.subscribe(),
    )
    .await
}

/// [`run_scenario_with_vars`] over an event subscription the caller took earlier, for example
/// before connecting: `expect_call` steps then also see CALLs that arrived while the caller
/// waited for the boot, before the scenario started. The receiver's queue is bounded (the
/// station's broadcast capacity); events lost to lag are reported like any other.
pub async fn run_scenario_subscribed<T: ScenarioTarget>(
    handle: &T,
    scenario: &Scenario,
    cancel: &CancelToken,
    progress: Option<&mpsc::UnboundedSender<ScenarioProgress>>,
    overrides: &Vars,
    index: u64,
    events: broadcast::Receiver<Event>,
) -> ScenarioReport {
    run_inner(
        handle,
        scenario,
        cancel,
        progress,
        events,
        initial_vars(handle, scenario, overrides, index),
    )
    .await
}

fn initial_vars<T: ScenarioTarget>(
    handle: &T,
    scenario: &Scenario,
    overrides: &Vars,
    index: u64,
) -> Vars {
    let mut vars = scenario.vars.clone();
    for (name, value) in overrides.iter() {
        let _ = match overrides.written_text(name) {
            Some(text) => vars.insert_with_text(name, value.clone(), text.to_owned()),
            None => vars.insert(name, value.clone()),
        };
    }
    let _ = vars.insert("station_id", handle.station_id().into());
    let _ = vars.insert("index", index.into());
    vars
}

/// [`run_scenario`] with an explicit event subscription (tests use a tiny channel to force lag).
#[doc(hidden)]
pub async fn run_scenario_with_events<T: ScenarioTarget>(
    handle: &T,
    scenario: &Scenario,
    cancel: &CancelToken,
    progress: Option<&mpsc::UnboundedSender<ScenarioProgress>>,
    events: broadcast::Receiver<Event>,
) -> ScenarioReport {
    let vars = initial_vars(handle, scenario, &Vars::new(), 1);
    run_inner(handle, scenario, cancel, progress, events, vars).await
}

async fn run_inner<T: ScenarioTarget>(
    handle: &T,
    scenario: &Scenario,
    cancel: &CancelToken,
    progress: Option<&mpsc::UnboundedSender<ScenarioProgress>>,
    events: broadcast::Receiver<Event>,
    vars: Vars,
) -> ScenarioReport {
    let started = Instant::now();
    let started_at = super::now_rfc3339();
    let mut ctx = Ctx {
        evidence: Evidence::default(),
        handle,
        events,
        lagged: 0,
        loss_at: None,
        wanted: wanted_calls(scenario),
        frames: VecDeque::new(),
        frame_bytes: 0,
        cancel,
        vars,
        index: 0,
    };
    let flat = scenario.expanded_steps();
    let mut steps = Vec::with_capacity(flat.len());
    let mut failed = false;
    // A step was cut short or skipped by the cancel token (not merely: it fired at the end).
    let mut cut_short = false;
    for (index, flat_step) in flat.iter().enumerate() {
        let name = flat_step.name.clone();
        let label = flat_step.label.clone();
        let action = flat_step.step.action.name().to_owned();
        if failed || cancel.is_cancelled() {
            let status = if failed {
                StepStatus::Skipped
            } else {
                failed = true;
                cut_short = true;
                StepStatus::Failed("cancelled".to_owned())
            };
            let mut report = StepReport::new(index, name, status, 0);
            report.detail.label = label;
            report.detail.action = action;
            if let Some(tx) = progress {
                let _ = tx.send(ScenarioProgress::StepFinished(report.clone()));
            }
            steps.push(report);
            continue;
        }
        if let Some(tx) = progress {
            let _ = tx.send(ScenarioProgress::StepStarted {
                index,
                name: name.clone(),
            });
        }
        let step_started = Instant::now();
        ctx.evidence = Evidence::default();
        ctx.index = index as u64;
        for (counter, iteration) in &flat_step.counters {
            // Counter names are valid and never built-ins (checked when the scenario loads).
            let _ = ctx.vars.insert(counter, (iteration + 1).into());
            let _ = ctx.vars.insert(&format!("{counter}0"), (*iteration).into());
        }
        let result = tokio::select! {
            result = ctx.run_step(flat_step.step) => result,
            () = cancel.cancelled() => Err("cancelled".to_owned()),
        };
        let status = match result {
            Ok(()) => StepStatus::Passed,
            Err(reason) => {
                failed = true;
                cut_short |= cancel.is_cancelled();
                StepStatus::Failed(reason)
            }
        };
        let evidence = std::mem::take(&mut ctx.evidence);
        let mut report = StepReport::new(index, name, status, millis(step_started.elapsed()));
        report.detail.label = label;
        report.detail.action = action;
        if matches!(report.status, StepStatus::Failed(_)) {
            report.detail.params = evidence.params.or_else(|| step_params(flat_step.step));
            report.detail.expected = evidence.expected;
            report.detail.actual = evidence.actual;
        }
        if let Some(tx) = progress {
            let _ = tx.send(ScenarioProgress::StepFinished(report.clone()));
        }
        steps.push(report);
    }
    if flat
        .iter()
        .any(|f| matches!(f.step.action, Action::Reply { .. }))
    {
        finish_replies(handle, &flat, &mut steps, failed).await;
    }
    let mut report = ScenarioReport::new(
        scenario.name.clone(),
        steps.iter().all(|s| s.status == StepStatus::Passed),
        cut_short,
        steps,
        millis(started.elapsed()),
    );
    report.started_at = started_at;
    report.ocpp_version = String::from(handle.protocol().ocpp_version());
    report.station = Some(handle.station_id());
    report.source.clone_from(&scenario.source);
    report
}

/// Disarms the scripted replies the scenario left behind. Unless the scenario failed already
/// (then an unused reply is a consequence, not a cause), a `reply` step with `must_fire = true`
/// whose reply was not used up fails in the final report.
async fn finish_replies<T: ScenarioTarget>(
    handle: &T,
    flat: &[super::model::FlatStep<'_>],
    steps: &mut [StepReport],
    failed: bool,
) {
    let Some(payload) = handle.clear_replies().await else {
        return;
    };
    if failed {
        return;
    }
    let Some(unused) = payload.get("unused").and_then(Value::as_array) else {
        return;
    };
    for entry in unused {
        let Some(tag) = entry.get("tag").and_then(Value::as_u64) else {
            continue;
        };
        let Ok(index) = usize::try_from(tag) else {
            continue;
        };
        let (Some(flat_step), Some(report)) = (flat.get(index), steps.get_mut(index)) else {
            continue;
        };
        let Action::Reply {
            call,
            times,
            must_fire: true,
            ..
        } = &flat_step.step.action
        else {
            continue;
        };
        let left = entry.get("remaining").and_then(Value::as_u64).unwrap_or(0);
        let used = u64::from(*times).saturating_sub(left);
        report.status = StepStatus::Failed(format!(
            "reply: the scripted {} reply was used {used} of {times} time(s) before the scenario ended",
            vars::clip_text(call)
        ));
        report.detail.params = step_params(flat_step.step);
    }
}

/// What a failed step can tell about itself beyond the reason text.
#[derive(Debug, Default)]
pub struct Evidence {
    pub(crate) params: Option<Value>,
    pub(crate) expected: Option<Value>,
    pub(crate) actual: Option<String>,
}

/// The step's parameters as JSON, without the `action` tag, secrets redacted.
fn step_params(step: &Step) -> Option<Value> {
    let mut value = step.action.to_json().ok()?;
    let fields = value.as_object_mut()?;
    fields.remove("action");
    if let Some(expect) = &step.expect
        && let Ok(expect) = serde_json::to_value(expect)
    {
        fields.insert("expect".to_owned(), expect);
    }
    super::redact_params(&mut value);
    Some(bound_params(value))
}

/// Characters of one string parameter kept in failure evidence.
const PARAM_CLIP_CHARS: usize = 1024;

/// Serialized size above which the evidence holds a marker instead of the parameters.
const PARAMS_MAX_BYTES: usize = 16 * 1024;

/// Clips long string values; an object that is still larger than [`PARAMS_MAX_BYTES`]
/// becomes `{"truncated": true, "bytes": N}`.
fn bound_params(mut value: Value) -> Value {
    fn clip(value: &mut Value) {
        match value {
            Value::String(text) if text.chars().nth(PARAM_CLIP_CHARS).is_some() => {
                *text = vars::clip_chars(text, PARAM_CLIP_CHARS);
            }
            Value::Array(items) => items.iter_mut().for_each(clip),
            Value::Object(map) => map.values_mut().for_each(clip),
            _ => {}
        }
    }
    clip(&mut value);
    let bytes = value.to_string().len();
    if bytes > PARAMS_MAX_BYTES {
        return serde_json::json!({"truncated": true, "bytes": bytes});
    }
    value
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// CALL frames kept for `expect_call` at most (the oldest unmatched ones go first).
const FRAME_CAP: usize = 10_000;
/// Approximate bytes of kept frames at most.
const FRAME_BYTES: usize = 16 * 1024 * 1024;

/// `wanted` key for an `expect_call` whose call name is a placeholder.
const ANY_CALL: &str = "*";

/// The action names some `expect_call` waits for -> whether any of them matches on the payload.
fn wanted_calls(scenario: &Scenario) -> HashMap<String, bool> {
    let mut wanted: HashMap<String, bool> = HashMap::new();
    for flat in scenario.expanded_steps() {
        let step = flat.step;
        if let Action::ExpectCall { call, matches, .. } = &step.action {
            // A call name with a placeholder is only known when the step runs: keep every CALL.
            let key = if call.contains("${") { ANY_CALL } else { call };
            *wanted.entry(key.to_owned()).or_default() |=
                matches.is_some() || !step.capture.is_empty();
        }
    }
    wanted
}

struct Ctx<'a, T: ScenarioTarget = StationHandle> {
    evidence: Evidence,
    /// The station under test.
    handle: &'a T,
    events: broadcast::Receiver<Event>,
    /// Events lost since the last report: the subscription dropped them because the runner
    /// fell behind, or `frames` overflowed. Frames may be among them, so an `expect_call`
    /// that cannot be answered from the frames before the loss fails instead of waiting for
    /// a CALL that may be gone. A failed `expect_call` reports the loss and clears it.
    lagged: u64,
    /// Where the earliest loss sits: lost frames would come after `frames[..loss_at]`.
    loss_at: Option<usize>,
    /// Actions some `expect_call` waits for (-> whether a payload match is needed). Other
    /// frames are not kept.
    wanted: HashMap<String, bool>,
    /// CALL frames that a later `expect_call` can still match: everything up to the last
    /// match is dropped. Frames of steps without a payload match keep no payload.
    frames: VecDeque<(FrameRecord, usize)>,
    frame_bytes: usize,
    cancel: &'a CancelToken,
    vars: Vars,
    /// Index of the running step in the expanded scenario (tags scripted replies).
    index: u64,
}

impl<T: ScenarioTarget> Ctx<'_, T> {
    fn record(&mut self, event: Event) {
        let Event::Frame(mut record) = event else {
            return;
        };
        if record.frame.get(0).and_then(Value::as_u64) != Some(2) {
            return;
        }
        let Some(call) = record.frame.get(2).and_then(Value::as_str) else {
            return;
        };
        let (specific, any) = (self.wanted.get(call), self.wanted.get(ANY_CALL));
        if specific.is_none() && any.is_none() {
            return;
        }
        let needs_payload = specific.copied().unwrap_or(false) | any.copied().unwrap_or(false);
        let bytes = if needs_payload {
            record.frame.to_string().len()
        } else {
            if let Some(items) = record.frame.as_array_mut() {
                items.truncate(3);
            }
            128
        };
        self.frame_bytes += bytes;
        self.frames.push_back((record, bytes));
        while self.frames.len() > 1
            && (self.frames.len() > FRAME_CAP || self.frame_bytes > FRAME_BYTES)
        {
            self.drop_front(1);
            // The dropped frame could have been the one a later `expect_call` wants.
            self.lagged = self.lagged.saturating_add(1);
            self.loss_at = Some(0);
        }
    }

    /// Removes the `n` oldest frames.
    fn drop_front(&mut self, n: usize) {
        for _ in 0..n {
            let Some((_, size)) = self.frames.pop_front() else {
                break;
            };
            self.frame_bytes = self.frame_bytes.saturating_sub(size);
        }
        self.loss_at = self.loss_at.map(|at| at.saturating_sub(n));
    }

    fn note_lost(&mut self, missed: u64) {
        self.lagged = self.lagged.saturating_add(missed);
        self.loss_at.get_or_insert(self.frames.len());
    }

    fn pump(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(event) => self.record(event),
                Err(broadcast::error::TryRecvError::Lagged(missed)) => self.note_lost(missed),
                Err(_) => break,
            }
        }
    }

    /// Awaits `work` while recording station events, so a long wait (sleep, condition, command)
    /// cannot let the subscription overflow and lose frames a later `expect_call` needs.
    async fn with_events<R>(&mut self, work: impl Future<Output = R>) -> R {
        tokio::pin!(work);
        let mut open = true;
        loop {
            tokio::select! {
                out = &mut work => return out,
                event = self.events.recv(), if open => match event {
                    Ok(event) => self.record(event),
                    Err(broadcast::error::RecvError::Lagged(missed)) => self.note_lost(missed),
                    // `expect_call` reports a stopped station; other steps do not read events.
                    Err(broadcast::error::RecvError::Closed) => open = false,
                },
            }
        }
    }

    async fn run_step(&mut self, step: &Step) -> Result<(), String> {
        self.pump();
        self.evidence.params = step_params(step);
        let step = &substitute_step(step, &self.vars)?;
        self.evidence.params = step_params(step);
        let handle = self.handle;
        let answer = match &step.action {
            Action::Sleep { ms } => {
                self.with_events(sleep(Duration::from_millis(*ms))).await;
                None
            }
            _ if step.action.is_wait_for() => {
                if let Err(failure) = self.with_events(handle.wait_for(step)).await {
                    self.evidence.expected = failure.expected;
                    return Err(failure.reason);
                }
                None
            }
            Action::ExpectCall {
                call,
                direction,
                matches,
                timeout_ms,
            } => Some(
                self.expect_call(call, *direction, matches.as_ref(), *timeout_ms)
                    .await?,
            ),
            _ => {
                // The target maps the step to a command and checks its outcome. Evidence it
                // collects (expected, actual) joins the step's own parameters.
                let mut evidence = Evidence::default();
                let answer = self
                    .with_events(handle.run_command_step(step, self.index, &mut evidence))
                    .await;
                self.evidence.expected = evidence.expected;
                self.evidence.actual = evidence.actual;
                answer?
            }
        };
        for (name, pointer) in &step.capture {
            let answer = answer
                .as_ref()
                .ok_or_else(|| format!("capture `{name}`: the step has no answer to read"))?;
            let value =
                vars::capture(answer, pointer).map_err(|e| format!("capture `{name}`: {e}"))?;
            self.vars
                .insert(name, value)
                .map_err(|e| format!("capture `{name}`: {e}"))?;
        }
        Ok(())
    }

    async fn expect_call(
        &mut self,
        call: &str,
        direction: Option<crate::station::Direction>,
        matches: Option<&Value>,
        timeout_ms: u64,
    ) -> Result<Value, String> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            self.pump();
            let found = self.frames.iter().position(|(record, _)| {
                record.frame.get(2).and_then(Value::as_str) == Some(call)
                    && direction.is_none_or(|d| d == record.direction)
                    && matches
                        .is_none_or(|m| json_subset(m, record.frame.get(3).unwrap_or(&Value::Null)))
            });
            // A match before the earliest loss is the first one there is; behind a loss an
            // earlier matching frame may be gone.
            if let Some(index) = found
                && self.loss_at.is_none_or(|at| index < at)
            {
                let payload = self.frames[index]
                    .0
                    .frame
                    .get(3)
                    .cloned()
                    .unwrap_or(Value::Null);
                self.drop_front(index + 1);
                return Ok(payload);
            }
            if self.loss_at.is_some() {
                let lost = std::mem::take(&mut self.lagged);
                self.loss_at = None;
                return Err(format!(
                    "the scenario runner fell behind and {lost} station events were dropped; \
                     CALL frames may have been missed, so `expect_call` cannot be trusted"
                ));
            }
            tokio::select! {
                event = self.events.recv() => match event {
                    Ok(event) => self.record(event),
                    Err(broadcast::error::RecvError::Lagged(missed)) => self.note_lost(missed),
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err("station stopped".to_owned());
                    }
                },
                () = sleep_until(deadline) => {
                    let mut expected = serde_json::json!({"call": call});
                    if let Some(direction) = direction {
                        expected["direction"] = serde_json::to_value(direction).unwrap_or_default();
                    }
                    if let Some(matches) = matches {
                        expected["match"] = super::redacted(matches);
                    }
                    self.evidence.expected = Some(expected);
                    // The latest frame of that action that did not match, else the latest
                    // CALL of any kept action. A frame kept without its payload shows its
                    // action and direction instead.
                    let seen = self
                        .frames
                        .iter()
                        .rev()
                        .find(|(record, _)| record.frame.get(2).and_then(Value::as_str) == Some(call))
                        .or_else(|| self.frames.back());
                    self.evidence.actual = seen.map(|(record, _)| {
                        let same = record.frame.get(2).and_then(Value::as_str) == Some(call);
                        match record.frame.get(3) {
                            Some(payload) if same => vars::clip_json(&super::redacted(payload)),
                            _ => vars::clip_json(&serde_json::json!({
                                "call": record.frame.get(2).and_then(Value::as_str),
                                "direction": record.direction,
                            })),
                        }
                    });
                    return Err(format!("timed out after {timeout_ms} ms waiting for a {call} CALL"));
                }
                () = self.cancel.cancelled() => return Err("cancelled".to_owned()),
            }
        }
    }
}

pub(super) mod sealed {
    /// Keeps [`ScenarioTarget`](super::ScenarioTarget) implementable inside this crate only.
    pub trait Sealed {}
}

/// Why a `wait_for` step failed.
#[derive(Debug)]
pub struct WaitFailure {
    /// The failure reason for the step report.
    pub(crate) reason: String,
    /// What the step waited for (failure evidence), when the failure is a timeout.
    pub(crate) expected: Option<Value>,
}

/// The station side of a scenario run: what differs between OCPP versions.
///
/// Sealed: [`StationHandle`] (OCPP 1.6) and [`Station2Handle`](crate::station2::Station2Handle)
/// (OCPP 2.x) are the targets. Use this trait as a bound when calling [`run_scenario`] or
/// its variants for either station type. Frame bookkeeping, `expect_call`, `sleep`,
/// variables and reports are the same for both.
pub trait ScenarioTarget: sealed::Sealed + Sync {
    /// The protocol the station speaks; the report's `ocpp_version` follows it.
    fn protocol(&self) -> Protocol;

    /// The station's id, for the report and the `${station_id}` variable.
    fn station_id(&self) -> String;

    /// A new subscription to the station's events.
    fn subscribe(&self) -> broadcast::Receiver<Event>;

    /// Waits until the station state satisfies the condition of the `wait_for` step (within the
    /// step's `timeout_ms`).
    fn wait_for(&self, step: &Step) -> impl Future<Output = Result<(), WaitFailure>> + Send;

    /// Maps a command step (any step except `sleep`, `wait_for` and `expect_call`) to the
    /// station's command, sends it and checks the step's `expect`. `tag` is the step's index in
    /// the expanded scenario. Returns the answer a `capture` may read, if the step has one.
    fn run_command_step(
        &self,
        step: &Step,
        tag: u64,
        evidence: &mut Evidence,
    ) -> impl Future<Output = Result<Option<Value>, String>> + Send;

    /// Disarms every scripted reply. The answer lists the ones that were not used up:
    /// `{"unused":[{"tag":..,"remaining":..}]}`; `None` when the station gave none.
    fn clear_replies(&self) -> impl Future<Output = Option<Value>> + Send;
}

impl sealed::Sealed for StationHandle {}

impl ScenarioTarget for StationHandle {
    fn protocol(&self) -> Protocol {
        Protocol::V16
    }

    fn station_id(&self) -> String {
        self.snapshot().charge_point_id
    }

    fn subscribe(&self) -> broadcast::Receiver<Event> {
        StationHandle::subscribe(self)
    }

    async fn wait_for(&self, step: &Step) -> Result<(), WaitFailure> {
        let Action::WaitFor {
            condition,
            timeout_ms,
        } = &step.action
        else {
            return Err(WaitFailure {
                reason: format!("`{}` is not a wait_for step", step.action.name()),
                expected: None,
            });
        };
        let mut rx = self.watch_snapshot();
        match timeout(
            Duration::from_millis(*timeout_ms),
            rx.wait_for(|s| condition.holds(s)),
        )
        .await
        {
            Ok(Ok(snapshot)) => {
                drop(snapshot);
                Ok(())
            }
            Ok(Err(_)) => Err(WaitFailure {
                reason: "station stopped".to_owned(),
                expected: None,
            }),
            Err(_) => Err(WaitFailure {
                reason: format!("timed out after {timeout_ms} ms waiting for {condition:?}"),
                expected: serde_json::to_value(condition).ok(),
            }),
        }
    }

    async fn run_command_step(
        &self,
        step: &Step,
        tag: u64,
        evidence: &mut Evidence,
    ) -> Result<Option<Value>, String> {
        match &step.action {
            Action::Reply {
                call,
                result,
                error,
                times,
                delay_ms,
                ..
            } => {
                let command = reply_command(
                    tag,
                    call,
                    result.as_ref(),
                    error.as_ref(),
                    *times,
                    *delay_ms,
                )?;
                self.send(command)
                    .await
                    .map_err(|error| format!("reply: {error}"))?;
                Ok(None)
            }
            action => {
                let command = to_command(action)?;
                let result = self.send(command).await;
                check(step, action, result, evidence)
            }
        }
    }

    async fn clear_replies(&self) -> Option<Value> {
        let cleared = timeout(
            Duration::from_secs(2),
            self.send(Command::ClearScriptedReplies),
        )
        .await;
        match cleared {
            Ok(Ok(Outcome::Response { payload })) => Some(payload),
            _ => None,
        }
    }
}

/// The command that arms a `reply` step (variables already substituted); `tag` is the step's
/// index in the expanded scenario.
fn reply_command(
    tag: u64,
    call: &str,
    result: Option<&Value>,
    error: Option<&ReplyError>,
    times: u32,
    delay_ms: u64,
) -> Result<Command, String> {
    let action = canonical_action(call).map_err(|e| format!("reply: {e}"))?;
    let reply = scripted_reply(
        tag,
        action,
        result,
        error,
        times,
        delay_ms,
        super::model::schema_registry(),
    )?;
    Ok(Command::ArmScriptedReply { reply })
}

/// Builds the scripted reply of a `reply` step for the canonical `action`: the answer is
/// checked against the response schema of `registry` (the station's protocol) and the error
/// code against the CALLERROR codes. The text of a failure starts with `reply:`.
pub(super) fn scripted_reply(
    tag: u64,
    action: &str,
    result: Option<&Value>,
    error: Option<&ReplyError>,
    times: u32,
    delay_ms: u64,
    registry: Option<&crate::ocpp::schema::SchemaRegistry>,
) -> Result<ScriptedReply, String> {
    let answer = match (result, error) {
        (Some(payload), None) => {
            if !payload.is_object() {
                return Err("reply: `result` must be a table".to_owned());
            }
            if let Some(registry) = registry
                && let Err(violation) = registry.validate_response(action, payload)
            {
                return Err(format!(
                    "reply: `result` is not a valid {action} response: {violation}"
                ));
            }
            ScriptedAnswer::Result {
                payload: payload.clone(),
            }
        }
        (None, Some(error)) => ScriptedAnswer::Error {
            code: super::model::standard_error_code(&error.code).ok_or_else(|| {
                format!(
                    "reply: unknown error code `{}`",
                    vars::clip_text(&error.code)
                )
            })?,
            description: error.description.clone(),
            details: error.details.clone(),
        },
        _ => return Err("reply: needs exactly one of `result` and `error`".to_owned()),
    };
    Ok(ScriptedReply {
        tag,
        action: action.to_owned(),
        answer,
        remaining: times,
        delay_ms,
    })
}

fn to_command(action: &Action) -> Result<Command, String> {
    Ok(match action.clone() {
        Action::Connect {} => Command::Connect,
        Action::Disconnect {} => Command::Disconnect,
        Action::Reconnect {} => Command::Reconnect,
        Action::Boot {} => Command::Boot,
        Action::Heartbeat {} => Command::Heartbeat,
        Action::Plug { connector } => Command::PlugIn { connector },
        Action::Unplug { connector } => Command::Unplug { connector },
        Action::Authorize { id_tag } => Command::Authorize { id_tag },
        Action::StartTx { connector, id_tag } => Command::StartTransaction { connector, id_tag },
        Action::StopTx {
            connector,
            reason,
            id_tag,
        } => Command::StopTransaction {
            connector,
            reason,
            id_tag,
        },
        Action::Meter {
            connector,
            wh_delta,
        } => Command::MeterValues {
            connector,
            wh_delta,
            measurands: None,
        },
        Action::Status { connector, status } => Command::SetConnectorStatus { connector, status },
        Action::Fault {
            connector,
            error_code,
            info,
        } => Command::Fault {
            connector,
            error_code,
            info,
            vendor_error_code: None,
        },
        Action::ClearFault { connector } => Command::ClearFault { connector },
        Action::SendRaw {
            call,
            payload,
            validate,
        } => Command::SendRaw {
            action: call,
            payload,
            validate,
        },
        Action::DataTransfer {
            vendor_id,
            message_id,
            data,
        } => Command::DataTransfer {
            vendor_id,
            message_id,
            data,
        },
        Action::RequestCertificate {} => Command::RequestCertificate,
        Action::SecurityEvent {
            event_type,
            tech_info,
        } => Command::SendSecurityEvent {
            event_type,
            tech_info,
        },
        Action::SetRule { rule } => Command::SetResponseRule {
            rule: rule
                .parse::<ResponseRule>()
                .map_err(|e| format!("set_rule: {e}"))?,
        },
        Action::ClearRules { call } => Command::ClearResponseRules {
            action: call.clone(),
        },
        Action::Sleep { .. }
        | Action::WaitFor { .. }
        | Action::ExpectCall { .. }
        | Action::Reply { .. }
        | Action::Repeat { .. } => {
            return Err("not a command".to_owned());
        }
        Action::V201(action) => {
            return Err(format!(
                "`{}` is an OCPP 2.0.1 step; this station speaks OCPP 1.6",
                action.name()
            ));
        }
    })
}

/// The `status` an outcome carries, for `expect.status`.
fn outcome_status(outcome: &Outcome) -> Option<String> {
    match outcome {
        Outcome::Authorized(decision) => Some(decision.id_tag_info.status.to_string()),
        Outcome::TransactionStarted { .. } => Some("Accepted".to_owned()),
        Outcome::TransactionRejected { status } => Some(status.to_string()),
        Outcome::Response { payload } => payload
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

fn check(
    step: &Step,
    action: &Action,
    result: Result<Outcome, StationError>,
    evidence: &mut Evidence,
) -> Result<Option<Value>, String> {
    let default = Expect::default();
    let expect = step.expect.as_ref().unwrap_or(&default);
    let outcome = match result {
        Err(error) if expect.error => {
            let _ = error;
            return Ok(None);
        }
        Err(error) => return Err(format!("{}: {error}", action.name())),
        Ok(_) if expect.error => {
            return Err(format!(
                "{}: expected an error, but it succeeded",
                action.name()
            ));
        }
        Ok(outcome) => outcome,
    };
    let implicit = matches!(action, Action::Authorize { .. } | Action::StartTx { .. });
    let wanted = match (&expect.status, implicit) {
        (Some(status), _) => Some(status.as_str()),
        (None, true) => Some("Accepted"),
        (None, false) => None,
    };
    if let Some(wanted) = wanted {
        match outcome_status(&outcome) {
            Some(actual) if actual == wanted => {}
            Some(actual) => {
                evidence.expected = Some(Value::String(wanted.to_owned()));
                evidence.actual = Some(vars::clip_text(&actual));
                return Err(format!(
                    "{}: expected status {wanted}, got {}",
                    action.name(),
                    vars::clip_text(&actual)
                ));
            }
            None => {
                evidence.expected = Some(Value::String(wanted.to_owned()));
                return Err(format!(
                    "{}: expected status {wanted}, but the outcome carries none",
                    action.name()
                ));
            }
        }
    }
    if let Some(wanted) = &expect.payload {
        match &outcome {
            Outcome::Response { payload } if json_subset(wanted, payload) => {}
            Outcome::Response { payload } => {
                let (wanted, payload) = (super::redacted(wanted), super::redacted(payload));
                evidence.expected = Some(wanted.clone());
                evidence.actual = Some(vars::clip_json(&payload));
                return Err(format!(
                    "{}: response {} does not contain {}",
                    action.name(),
                    vars::clip_json(&payload),
                    vars::clip_json(&wanted)
                ));
            }
            _ => {
                return Err(format!(
                    "{}: expected a response payload, the outcome has none",
                    action.name()
                ));
            }
        }
    }
    Ok(match outcome {
        Outcome::Response { payload } => Some(payload),
        _ => None,
    })
}

/// Applies the variables to the step's strings: the JSON of `payload` / `match` (typed
/// substitution), the other string fields (text), and `expect`. Fields with another type
/// (`connector`, `reason`, `wh_delta`, `ms`, `timeout_ms`, ...) are never substituted.
fn substitute_step(step: &Step, vars: &Vars) -> Result<Step, String> {
    let mut action = step.action.to_json().map_err(|e| e.to_string())?;
    let mut out = step.clone();
    if action.to_string().contains('$')
        && let Value::Object(fields) = &mut action
    {
        // `data` is free-form JSON in 2.0.1 (a string in 1.6).
        let typed: &[&str] = match step.action.grammar() {
            Protocol::V201 => &["payload", "match", "result", "error", "data"],
            _ => &["payload", "match", "result", "error"],
        };
        for (key, value) in fields.iter_mut() {
            if typed.contains(&key.as_str()) {
                *value = vars::substitute(value, vars).map_err(|e| format!("{key}: {e}"))?;
            } else if let Value::String(text) = value {
                *text = vars::substitute_str(text, vars).map_err(|e| format!("{key}: {e}"))?;
            }
        }
        out.action = Action::from_json(action, step.action.grammar())
            .map_err(|e| format!("{} after substituting variables: {e}", step.action.name()))?;
    }
    if let Some(expect) = &mut out.expect {
        if let Some(status) = &mut expect.status {
            *status =
                vars::substitute_str(status, vars).map_err(|e| format!("expect.status: {e}"))?;
        }
        if let Some(payload) = &mut expect.payload {
            *payload =
                vars::substitute(payload, vars).map_err(|e| format!("expect.payload: {e}"))?;
        }
    }
    Ok(out)
}

/// `true` if every part of `expected` is present in `actual` (objects are matched by key,
/// arrays element-wise, everything else by equality).
pub(crate) fn json_subset(expected: &Value, actual: &Value) -> bool {
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
    use serde_json::json;

    use super::*;

    #[test]
    fn subset_matching() {
        assert!(json_subset(&json!({"a": 1}), &json!({"a": 1, "b": 2})));
        assert!(!json_subset(&json!({"a": 2}), &json!({"a": 1})));
        assert!(!json_subset(&json!({"c": 1}), &json!({"a": 1})));
        assert!(json_subset(
            &json!({"a": {"b": 1}}),
            &json!({"a": {"b": 1, "c": 2}})
        ));
        assert!(json_subset(
            &json!([1, {"x": 1}]),
            &json!([1, {"x": 1, "y": 2}])
        ));
        assert!(!json_subset(&json!([1]), &json!([1, 2])));
    }

    fn call_event(action: &str, id: &str) -> Event {
        Event::Frame(FrameRecord {
            ts: chrono::Utc::now(),
            direction: crate::station::Direction::In,
            msg_id: Some(id.to_owned()),
            action: Some(action.to_owned()),
            frame: json!([2, id, action, {"pad": "x".repeat(1000)}]),
        })
    }

    /// A context over an idle station that wants `Heartbeat` frames.
    fn with_ctx<R>(f: impl AsyncFnOnce(&mut Ctx<'_>) -> R) -> R {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let clock = crate::clock::ManualClock::new(chrono::Utc::now());
            let config =
                crate::StationConfig::new("ws://localhost:9000/ocpp").without_persistence();
            let (handle, join) = crate::Station::spawn(config, Arc::new(clock));
            let cancel = CancelToken::new();
            let mut ctx = Ctx {
                evidence: Evidence::default(),
                handle: &handle,
                events: handle.subscribe(),
                lagged: 0,
                loss_at: None,
                wanted: HashMap::from([("Heartbeat".to_owned(), false)]),
                frames: VecDeque::new(),
                frame_bytes: 0,
                cancel: &cancel,
                vars: Vars::new(),
                index: 0,
            };
            let out = f(&mut ctx).await;
            drop(ctx);
            let _ = handle.send(Command::Shutdown).await;
            let _ = join.await;
            out
        })
    }

    #[test]
    fn only_wanted_frames_are_kept_and_matched_ones_are_dropped() {
        with_ctx(async |ctx| {
            ctx.record(call_event("Heartbeat", "1"));
            ctx.record(call_event("StatusNotification", "2"));
            ctx.record(call_event("Heartbeat", "3"));
            assert_eq!(ctx.frames.len(), 2);
            // No payload match is wanted, so no payload is kept.
            assert_eq!(ctx.frames[0].0.frame.as_array().map(Vec::len), Some(3));
            ctx.expect_call("Heartbeat", None, None, 50).await.unwrap();
            assert_eq!(ctx.frames.len(), 1);
            ctx.expect_call("Heartbeat", None, None, 50).await.unwrap();
            assert!(ctx.frames.is_empty() && ctx.frame_bytes == 0);
        });
    }

    #[test]
    fn a_timeout_names_what_was_seen_when_no_payload_was_kept() {
        with_ctx(async |ctx| {
            // Nothing received: no `actual`.
            ctx.expect_call("Heartbeat", None, None, 20)
                .await
                .unwrap_err();
            assert_eq!(ctx.evidence.actual, None);

            // A kept frame of the wanted action (wrong direction) holds no payload: its
            // action and direction are shown.
            ctx.record(call_event("Heartbeat", "1"));
            ctx.expect_call("Heartbeat", Some(crate::station::Direction::Out), None, 20)
                .await
                .unwrap_err();
            let actual = ctx.evidence.actual.take().unwrap();
            assert_eq!(actual, r#"{"call":"Heartbeat","direction":"in"}"#);

            // Only other actions were seen: the latest one is named.
            ctx.wanted.insert("StatusNotification".to_owned(), false);
            ctx.frames.clear();
            ctx.frame_bytes = 0;
            ctx.record(call_event("StatusNotification", "2"));
            ctx.expect_call("Heartbeat", None, None, 20)
                .await
                .unwrap_err();
            let actual = ctx.evidence.actual.take().unwrap();
            assert!(actual.contains("StatusNotification"), "{actual}");
        });
    }

    #[test]
    fn a_loss_only_fails_what_it_can_affect_and_is_reported_once() {
        with_ctx(async |ctx| {
            ctx.record(call_event("Heartbeat", "1"));
            ctx.note_lost(3);
            // The frame before the loss is still the first match.
            ctx.expect_call("Heartbeat", None, None, 50).await.unwrap();
            // Anything after the loss could have been preceded by a lost match.
            ctx.record(call_event("Heartbeat", "2"));
            let reason = ctx
                .expect_call("Heartbeat", None, None, 50)
                .await
                .unwrap_err();
            assert!(
                reason.contains("fell behind") && reason.contains("3 station events"),
                "{reason}"
            );
            // The report cleared the loss: later expectations work again.
            ctx.record(call_event("Heartbeat", "3"));
            ctx.expect_call("Heartbeat", None, None, 50).await.unwrap();
        });
    }

    #[test]
    fn a_pending_command_does_not_let_the_subscription_overflow() {
        with_ctx(async |ctx| {
            let (tx, rx) = broadcast::channel(2);
            ctx.events = rx;
            let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
            let work = async {
                tx.send(call_event("Heartbeat", "1")).unwrap();
                // Stands in for a command in flight: the queue is read meanwhile.
                tokio::time::timeout(Duration::from_secs(5), async {
                    while !tx.is_empty() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("queue drained");
                for _ in 0..5 {
                    tx.send(Event::StateChanged).unwrap();
                }
                drop(done_tx);
                let _ = done_rx.await;
            };
            ctx.with_events(work).await;
            assert_eq!(ctx.frames.len(), 1);
            ctx.expect_call("Heartbeat", None, None, 50).await.unwrap();
        });
    }

    // The OCPP 2.0.1 seam: a hand-built 2.0.1 step goes through variable substitution, label
    // and parameter rendering exactly like a 1.6 step.

    fn v201_step(action: crate::scenario::Action2) -> Step {
        Step {
            name: None,
            expect: None,
            capture: std::collections::BTreeMap::new(),
            action: Action::V201(Box::new(action)),
        }
    }

    fn tag_vars() -> Vars {
        let mut vars = Vars::new();
        vars.insert("tag", json!("KIRBY-1")).unwrap();
        vars.insert("count", json!(3)).unwrap();
        vars
    }

    #[test]
    fn a_v201_step_substitutes_variables_and_renders_label_and_params() {
        use crate::ocpp::v2::types::IdTokenType;
        use crate::scenario::Action2;

        let step = v201_step(Action2::StartTx {
            evse: 1,
            id_token: "${tag}".to_owned(),
            token_type: IdTokenType::Iso14443,
        });
        assert_eq!(step.action.name(), "start_tx");
        assert_eq!(step.action.grammar(), Protocol::V201);
        assert_eq!(step.stable_label(), "start_tx e1 ${tag}");

        let resolved = substitute_step(&step, &tag_vars()).unwrap();
        assert_eq!(
            resolved.action,
            Action::V201(Box::new(Action2::StartTx {
                evse: 1,
                id_token: "KIRBY-1".to_owned(),
                token_type: IdTokenType::Iso14443,
            }))
        );
        assert_eq!(resolved.stable_label(), "start_tx e1 KIRBY-1");
        assert_eq!(
            step_params(&resolved),
            Some(json!({"evse": 1, "id_token": "KIRBY-1", "type": "ISO14443"}))
        );
    }

    #[test]
    fn v201_data_is_substituted_as_json_and_neutral_steps_keep_their_form() {
        use crate::scenario::Action2;

        let step = v201_step(Action2::DataTransfer {
            vendor_id: "v-${tag}".to_owned(),
            message_id: None,
            data: Some(json!({"n": "${count}", "s": "x-${tag}"})),
        });
        let resolved = substitute_step(&step, &tag_vars()).unwrap();
        assert_eq!(
            resolved.action,
            Action::V201(Box::new(Action2::DataTransfer {
                vendor_id: "v-KIRBY-1".to_owned(),
                message_id: None,
                data: Some(json!({"n": 3, "s": "x-KIRBY-1"})),
            }))
        );

        // A protocol-neutral step substitutes as a 1.6 `Action`, in a 2.0.1 scenario too.
        let neutral = Step {
            name: None,
            expect: None,
            capture: std::collections::BTreeMap::new(),
            action: Action::SendRaw {
                call: "Heartbeat".to_owned(),
                payload: json!({"k": "${tag}"}),
                validate: false,
            },
        };
        let resolved = substitute_step(&neutral, &tag_vars()).unwrap();
        assert_eq!(
            step_params(&resolved),
            Some(json!({"call": "Heartbeat", "payload": {"k": "KIRBY-1"}, "validate": false}))
        );
    }

    #[test]
    fn v201_steps_round_trip_through_json_and_work_inside_repeat() {
        use crate::scenario::Action2;

        let action = Action::V201(Box::new(Action2::Plug { evse: 2 }));
        let json = action.to_json().unwrap();
        assert_eq!(json, json!({"action": "plug", "evse": 2}));
        assert_eq!(Action::from_json(json, Protocol::V201).unwrap(), action);
        // The same JSON is not a 1.6 step.
        assert!(Action::from_json(json!({"action": "plug", "evse": 2}), Protocol::V16).is_err());
        // 2.1 reads the 2.x grammar.
        assert_eq!(
            Action::from_json(json!({"action": "plug", "evse": 2}), Protocol::V21).unwrap(),
            action
        );
        assert!(Action::from_json(json!({"action": "plug"}), Protocol::V21).is_err());

        let looped = Scenario {
            name: "t".to_owned(),
            description: String::new(),
            vars: Vars::new(),
            steps: vec![Step {
                name: None,
                expect: None,
                capture: std::collections::BTreeMap::new(),
                action: Action::Repeat {
                    count: 2,
                    var: None,
                    steps: vec![v201_step(Action2::StartTx {
                        evse: 1,
                        id_token: "${tag}".to_owned(),
                        token_type: crate::ocpp::v2::types::IdTokenType::Iso14443,
                    })],
                },
            }],
            tags: Vec::new(),
            source: None,
            ocpp_version: Some(Protocol::V201),
        };
        assert_eq!(looped.protocol(), Protocol::V201);
        assert!(looped.uses_variable("tag"));
        assert!(!looped.uses_variable("other"));
        let flat = looped.expanded_steps();
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[1].label, "start_tx e1 ${tag} [2]");
    }

    #[test]
    fn the_frame_cap_drops_the_oldest_and_counts_a_loss() {
        with_ctx(async |ctx| {
            for i in 0..FRAME_CAP + 5 {
                ctx.record(call_event("Heartbeat", &i.to_string()));
            }
            assert_eq!(ctx.frames.len(), FRAME_CAP);
            assert_eq!(ctx.lagged, 5);
            let reason = ctx
                .expect_call("Heartbeat", None, None, 50)
                .await
                .unwrap_err();
            assert!(reason.contains("fell behind"), "{reason}");
        });
    }
}
