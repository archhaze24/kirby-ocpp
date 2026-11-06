//! Running a [`Scenario`] against a [`StationHandle`].

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};
use tokio::time::{sleep, sleep_until, timeout};

use super::model::{Action, Expect, Step};
use super::{CancelToken, Scenario, ScenarioError};
use crate::error::StationError;
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
}

/// Result of a whole run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScenarioReport {
    pub scenario: String,
    /// Every step passed.
    pub passed: bool,
    /// The run was stopped through its [`CancelToken`].
    pub cancelled: bool,
    pub steps: Vec<StepReport>,
    pub duration_ms: u64,
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
/// This function has no re-entrancy guard; use [`ScenarioRunner`] for that.
pub async fn run_scenario(
    handle: &StationHandle,
    scenario: &Scenario,
    cancel: &CancelToken,
    progress: Option<&mpsc::UnboundedSender<ScenarioProgress>>,
) -> ScenarioReport {
    run_scenario_with_events(handle, scenario, cancel, progress, handle.subscribe()).await
}

/// [`run_scenario`] with an explicit event subscription (tests use a tiny channel to force lag).
#[doc(hidden)]
pub async fn run_scenario_with_events(
    handle: &StationHandle,
    scenario: &Scenario,
    cancel: &CancelToken,
    progress: Option<&mpsc::UnboundedSender<ScenarioProgress>>,
    events: broadcast::Receiver<Event>,
) -> ScenarioReport {
    let started = Instant::now();
    let mut ctx = Ctx {
        handle,
        events,
        lagged: 0,
        loss_at: None,
        wanted: wanted_calls(scenario),
        frames: VecDeque::new(),
        frame_bytes: 0,
        cancel,
    };
    let mut steps = Vec::with_capacity(scenario.steps.len());
    let mut failed = false;
    // A step was cut short or skipped by the cancel token (not merely: it fired at the end).
    let mut cut_short = false;
    for (index, step) in scenario.steps.iter().enumerate() {
        let name = step.label();
        if failed || cancel.is_cancelled() {
            let status = if failed {
                StepStatus::Skipped
            } else {
                failed = true;
                cut_short = true;
                StepStatus::Failed("cancelled".to_owned())
            };
            let report = StepReport {
                index,
                name,
                status,
                duration_ms: 0,
            };
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
        let result = tokio::select! {
            result = ctx.run_step(step) => result,
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
        let report = StepReport {
            index,
            name,
            status,
            duration_ms: millis(step_started.elapsed()),
        };
        if let Some(tx) = progress {
            let _ = tx.send(ScenarioProgress::StepFinished(report.clone()));
        }
        steps.push(report);
    }
    ScenarioReport {
        scenario: scenario.name.clone(),
        passed: steps.iter().all(|s| s.status == StepStatus::Passed),
        cancelled: cut_short,
        steps,
        duration_ms: millis(started.elapsed()),
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// CALL frames kept for `expect_call` at most (the oldest unmatched ones go first).
const FRAME_CAP: usize = 10_000;
/// Approximate bytes of kept frames at most.
const FRAME_BYTES: usize = 16 * 1024 * 1024;

/// The action names some `expect_call` waits for -> whether any of them matches on the payload.
fn wanted_calls(scenario: &Scenario) -> HashMap<String, bool> {
    let mut wanted: HashMap<String, bool> = HashMap::new();
    for step in &scenario.steps {
        if let Action::ExpectCall { call, matches, .. } = &step.action {
            *wanted.entry(call.clone()).or_default() |= matches.is_some();
        }
    }
    wanted
}

struct Ctx<'a> {
    handle: &'a StationHandle,
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
}

impl Ctx<'_> {
    fn record(&mut self, event: Event) {
        let Event::Frame(mut record) = event else {
            return;
        };
        if record.frame.get(0).and_then(Value::as_u64) != Some(2) {
            return;
        }
        let Some(&needs_payload) = record
            .frame
            .get(2)
            .and_then(Value::as_str)
            .and_then(|call| self.wanted.get(call))
        else {
            return;
        };
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

    async fn run_step(&mut self, step: &Step) -> Result<(), String> {
        self.pump();
        match &step.action {
            Action::Sleep { ms } => {
                sleep(Duration::from_millis(*ms)).await;
                Ok(())
            }
            Action::WaitFor {
                condition,
                timeout_ms,
            } => {
                let mut rx = self.handle.watch_snapshot();
                match timeout(
                    Duration::from_millis(*timeout_ms),
                    rx.wait_for(|s| condition.holds(s)),
                )
                .await
                {
                    Ok(Ok(_)) => Ok(()),
                    Ok(Err(_)) => Err("station stopped".to_owned()),
                    Err(_) => Err(format!(
                        "timed out after {timeout_ms} ms waiting for {condition:?}"
                    )),
                }
            }
            Action::ExpectCall {
                call,
                direction,
                matches,
                timeout_ms,
            } => {
                self.expect_call(call, *direction, matches.as_ref(), *timeout_ms)
                    .await
            }
            action => {
                let command = to_command(action).ok_or("not a command")?;
                let result = self.handle.send(command).await;
                check(step, action, result)
            }
        }
    }

    async fn expect_call(
        &mut self,
        call: &str,
        direction: Option<crate::station::Direction>,
        matches: Option<&Value>,
        timeout_ms: u64,
    ) -> Result<(), String> {
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
                self.drop_front(index + 1);
                return Ok(());
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
                    return Err(format!("timed out after {timeout_ms} ms waiting for a {call} CALL"));
                }
                () = self.cancel.cancelled() => return Err("cancelled".to_owned()),
            }
        }
    }
}

fn to_command(action: &Action) -> Option<Command> {
    Some(match action.clone() {
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
        Action::SetRule { rule } => Command::SetResponseRule { rule: rule.clone() },
        Action::ClearRules { call } => Command::ClearResponseRules {
            action: call.clone(),
        },
        Action::Sleep { .. } | Action::WaitFor { .. } | Action::ExpectCall { .. } => return None,
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
) -> Result<(), String> {
    let default = Expect::default();
    let expect = step.expect.as_ref().unwrap_or(&default);
    let outcome = match result {
        Err(error) if expect.error => {
            let _ = error;
            return Ok(());
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
                return Err(format!(
                    "{}: expected status {wanted}, got {actual}",
                    action.name()
                ));
            }
            None => {
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
                return Err(format!(
                    "{}: response {payload} does not contain {wanted}",
                    action.name()
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
    Ok(())
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
                handle: &handle,
                events: handle.subscribe(),
                lagged: 0,
                loss_at: None,
                wanted: HashMap::from([("Heartbeat".to_owned(), false)]),
                frames: VecDeque::new(),
                frame_bytes: 0,
                cancel: &cancel,
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
