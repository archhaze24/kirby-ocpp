//! Configured calls and connection hooks.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use kirby_csms_core::{ConnectedSession, CsmsEvent, CsmsHandle};
use kirby_ocpp_core::config_file::{Block, BlockValues, Value};
use serde_json::{Value as Json, json};
use tokio::sync::broadcast;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

use crate::commands::{self, CommandLimits};
use crate::headless::Output;

pub const MAX_COMMANDS: usize = 256;
const MAX_TIMES: u64 = 1_000_000;

#[derive(Debug, Clone, PartialEq)]
pub enum Trigger {
    After(u64),
    Interval { ms: u64, times: Option<u64> },
    OnConnect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandSpec {
    pub id: String,
    pub station: String,
    pub protocol: Option<String>,
    pub action: String,
    pub payload: Json,
    pub timeout_ms: Option<u64>,
    pub trigger: Trigger,
}

fn number(block: &Block, key: &str, min: u64, max: u64) -> Result<Option<u64>, String> {
    block
        .integer(key)
        .map(|n| {
            u64::try_from(n)
                .ok()
                .filter(|n| (min..=max).contains(n))
                .ok_or_else(|| format!("`{key}` must be {min} to {max}"))
        })
        .transpose()
}

impl CommandSpec {
    pub fn from_block(block: &Block) -> Result<Self, String> {
        let id = block.text("id").unwrap_or_default();
        if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(
                "`id` must be 1 to 64 printable ASCII characters without spaces".to_owned(),
            );
        }
        let station = block.text("station").unwrap_or_default();
        commands::check_pattern(station)?;
        let action = block.text("action").unwrap_or_default();
        commands::check_call_action(action)?;
        let payload: Json = serde_json::from_str(block.text("payload").unwrap_or("{}"))
            .map_err(|_| "`payload` must be a JSON object".to_owned())?;
        if !payload.is_object() {
            return Err("`payload` must be a JSON object".to_owned());
        }
        let mut redacted = payload.clone();
        kirby_ocpp_core::ocpp::redact_payload(&mut redacted);
        if redacted != payload || has_authorization_value(&payload) {
            return Err("`payload` must not contain an AuthorizationKey value".to_owned());
        }
        let delay = number(block, "delay_ms", 0, commands::MAX_TIMEOUT_MS)?;
        let interval = number(block, "interval_ms", 1, commands::MAX_TIMEOUT_MS)?;
        let times = number(block, "times", 1, MAX_TIMES)?;
        let trigger = match block.text("trigger") {
            Some("after") if interval.is_none() && times.is_none() => {
                Trigger::After(delay.ok_or_else(|| "`after` requires `delay_ms`".to_owned())?)
            }
            Some("interval") if delay.is_none() => Trigger::Interval {
                ms: interval.ok_or_else(|| "`interval` requires `interval_ms`".to_owned())?,
                times,
            },
            Some("on_connect") if delay.is_none() && interval.is_none() && times.is_none() => {
                Trigger::OnConnect
            }
            Some("after" | "interval" | "on_connect") => {
                return Err("timing keys do not match `trigger`".to_owned());
            }
            _ => return Err("`trigger` must be after, interval or on_connect".to_owned()),
        };
        Ok(Self {
            id: id.to_owned(),
            station: station.to_owned(),
            protocol: block.text("protocol").map(str::to_owned),
            action: action.to_owned(),
            payload,
            timeout_ms: number(block, "timeout_ms", 1, commands::MAX_TIMEOUT_MS)?,
            trigger,
        })
    }

    pub fn render_values(&self) -> BlockValues<'static> {
        let (trigger, mut timing) = match self.trigger {
            Trigger::After(ms) => ("after", vec![("delay_ms", Value::Integer(to_integer(ms)))]),
            Trigger::Interval { ms, times } => {
                let mut values = vec![("interval_ms", Value::Integer(to_integer(ms)))];
                if let Some(times) = times {
                    values.push(("times", Value::Integer(to_integer(times))));
                }
                ("interval", values)
            }
            Trigger::OnConnect => ("on_connect", Vec::new()),
        };
        let mut values = vec![
            ("id", Value::Text(self.id.clone())),
            ("trigger", Value::Text(trigger.to_owned())),
            ("station", Value::Text(self.station.clone())),
            ("action", Value::Text(self.action.clone())),
            ("payload", Value::Text(self.payload.to_string())),
        ];
        values.append(&mut timing);
        if let Some(protocol) = &self.protocol {
            values.push(("protocol", Value::Text(protocol.clone())));
        }
        if let Some(ms) = self.timeout_ms {
            values.push(("timeout_ms", Value::Integer(to_integer(ms))));
        }
        values
    }

    fn trigger_name(&self) -> &'static str {
        match self.trigger {
            Trigger::After(_) => "after",
            Trigger::Interval { .. } => "interval",
            Trigger::OnConnect => "on_connect",
        }
    }
}

fn to_integer(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn has_authorization_value(value: &Json) -> bool {
    match value {
        Json::Object(fields) => {
            fields
                .get("key")
                .and_then(Json::as_str)
                .is_some_and(|key| key.eq_ignore_ascii_case("AuthorizationKey"))
                && fields.contains_key("value")
                || fields.values().any(has_authorization_value)
        }
        Json::Array(items) => items.iter().any(has_authorization_value),
        _ => false,
    }
}

pub struct Scheduler(JoinHandle<()>);

impl Scheduler {
    pub fn spawn(
        handle: CsmsHandle,
        specs: Vec<CommandSpec>,
        limits: CommandLimits,
        out: Arc<Output>,
        timeout: Duration,
    ) -> Self {
        // Subscribe before the snapshot, so a connection racing startup is seen by either path.
        let events = handle.subscribe();
        Self(tokio::spawn(run(
            handle,
            Arc::new(specs),
            limits,
            out,
            timeout,
            events,
        )))
    }

    pub async fn finish(self) {
        self.0.abort();
        let _ = self.0.await;
    }
}

struct Timer {
    due: Option<Instant>,
    attempts: u64,
}

fn next_due(trigger: &Trigger, attempts: u64, now: Instant) -> Option<Instant> {
    match trigger {
        Trigger::After(ms) if attempts == 0 => Some(now + Duration::from_millis(*ms)),
        Trigger::Interval { ms, times } if times.is_none_or(|limit| attempts < limit) => {
            Some(now + Duration::from_millis(*ms))
        }
        _ => None,
    }
}

fn summary(out: &Output, spec: &CommandSpec, status: &str, selected: usize, succeeded: usize) {
    out.event(
        &json!({"type":"automation", "id":spec.id, "trigger":spec.trigger_name(), "status":status,
        "selected":selected, "succeeded":succeeded}),
        &format!("command {}: {status} ({succeeded}/{selected})", spec.id),
    );
}

async fn dispatch(
    handle: &CsmsHandle,
    spec: &CommandSpec,
    limits: &CommandLimits,
    out: &Output,
    timeout: Duration,
    session: Option<ConnectedSession>,
) {
    let stations = if session.is_some() {
        Vec::new()
    } else {
        let Ok(stations) =
            commands::select_stations(handle, &spec.station, spec.protocol.as_deref())
        else {
            summary(out, spec, "rejected", 0, 0);
            return;
        };
        stations
    };
    let count = if session.is_some() { 1 } else { stations.len() };
    let Some(bytes) = spec.payload.to_string().len().checked_mul(count) else {
        summary(out, spec, "rejected", count, 0);
        return;
    };
    let Ok(_permit) = limits.try_reserve(count, bytes) else {
        summary(out, spec, "rejected", count, 0);
        return;
    };
    let timeout = spec.timeout_ms.map_or(timeout, Duration::from_millis);
    let succeeded = if let Some(session) = session {
        let result = handle
            .call_session_timeout(&session, &spec.action, spec.payload.clone(), timeout)
            .await;
        if matches!(
            result,
            Err(kirby_csms_core::CsmsError::NotConnected { .. }
                | kirby_csms_core::CsmsError::UnknownStation { .. })
        ) {
            summary(out, spec, "skipped", count, 0);
            return;
        }
        usize::from(matches!(
            result,
            Ok(kirby_csms_core::Frame::CallResult { .. })
        ))
    } else {
        commands::call_many(
            handle,
            &stations,
            &spec.action,
            &spec.payload,
            Some(timeout),
        )
        .await
        .iter()
        .filter(|answer| answer.get("outcome").and_then(Json::as_str) == Some("result"))
        .count()
    };
    summary(
        out,
        spec,
        if succeeded == count {
            "completed"
        } else {
            "failed"
        },
        count,
        succeeded,
    );
}

async fn run(
    handle: CsmsHandle,
    specs: Arc<Vec<CommandSpec>>,
    limits: CommandLimits,
    out: Arc<Output>,
    timeout: Duration,
    mut events: broadcast::Receiver<CsmsEvent>,
) {
    let mut timers: Vec<_> = specs
        .iter()
        .map(|spec| Timer {
            due: next_due(&spec.trigger, 0, Instant::now()),
            attempts: 0,
        })
        .collect();
    let mut seen = HashMap::new();
    let mut tasks = JoinSet::new();
    let has_hooks = specs.iter().any(|spec| spec.trigger == Trigger::OnConnect);
    if has_hooks {
        reconcile(
            &handle, &specs, &limits, &out, timeout, &mut seen, &mut tasks,
        );
    }
    loop {
        let due = timers.iter().filter_map(|timer| timer.due).min();
        tokio::select! {
            event = events.recv(), if has_hooks => match event {
                Ok(CsmsEvent::Connected { .. } | CsmsEvent::Disconnected { .. } | CsmsEvent::Forgotten { .. })
                | Err(broadcast::error::RecvError::Lagged(_)) => reconcile(&handle, &specs, &limits, &out, timeout, &mut seen, &mut tasks),
                Err(broadcast::error::RecvError::Closed) => return,
                Ok(_) => {}
            },
            result = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Ok(Some(index))) = result
                    && let (Some(timer), Some(spec)) = (timers.get_mut(index), specs.get(index)) {
                    timer.due = next_due(&spec.trigger, timer.attempts, Instant::now());
                }
            }
            () = async { if let Some(due) = due { tokio::time::sleep_until(due).await; } else { std::future::pending().await } } => {
                for (index, timer) in timers.iter_mut().enumerate() {
                    if timer.due.is_some_and(|due| due <= Instant::now()) {
                        timer.due = None;
                        timer.attempts += 1;
                        if let Some(spec) = specs.get(index) {
                            if tasks.len() >= MAX_COMMANDS {
                                summary(&out, spec, "rejected", 0, 0);
                                timer.due = next_due(&spec.trigger, timer.attempts, Instant::now());
                            } else {
                                let (handle, specs, limits, out) = (handle.clone(), specs.clone(), limits.clone(), out.clone());
                                tasks.spawn(async move { if let Some(spec) = specs.get(index) {
                                    dispatch(&handle, spec, &limits, &out, timeout, None).await;
                                } Some(index) });
                            }
                        }
                    }
                }
            }
        }
    }
}

fn reconcile(
    handle: &CsmsHandle,
    specs: &Arc<Vec<CommandSpec>>,
    limits: &CommandLimits,
    out: &Arc<Output>,
    timeout: Duration,
    seen: &mut HashMap<String, ConnectedSession>,
    tasks: &mut JoinSet<Option<usize>>,
) {
    let sessions: HashMap<_, _> = handle
        .connected_sessions()
        .into_iter()
        .map(|session| (session.station().to_owned(), session))
        .collect();
    seen.retain(|id, token| sessions.get(id) == Some(token));
    for session in sessions.into_values() {
        if seen.get(session.station()) == Some(&session) {
            continue;
        }
        seen.insert(session.station().to_owned(), session.clone());
        for (index, spec) in specs.iter().enumerate() {
            if spec.trigger != Trigger::OnConnect
                || !commands::glob_matches(&spec.station, session.station())
                || spec
                    .protocol
                    .as_deref()
                    .is_some_and(|protocol| Some(protocol) != session.protocol())
            {
                continue;
            }
            if tasks.len() >= MAX_COMMANDS {
                summary(out, spec, "rejected", 1, 0);
                continue;
            }
            let (handle, specs, limits, out, session) = (
                handle.clone(),
                specs.clone(),
                limits.clone(),
                out.clone(),
                session.clone(),
            );
            tasks.spawn(async move {
                if let Some(spec) = specs.get(index) {
                    dispatch(&handle, spec, &limits, &out, timeout, Some(session)).await;
                }
                None
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Trigger, next_due};
    use std::time::Duration;
    use tokio::time::Instant;

    #[test]
    fn intervals_start_after_completion_and_stop_after_the_attempt_limit() {
        let completed = Instant::now() + Duration::from_secs(100);
        let trigger = Trigger::Interval {
            ms: 20,
            times: Some(2),
        };
        assert_eq!(
            next_due(&trigger, 1, completed),
            Some(completed + Duration::from_millis(20))
        );
        assert_eq!(next_due(&trigger, 2, completed), None);
        assert_eq!(next_due(&Trigger::After(0), 0, completed), Some(completed));
        assert_eq!(next_due(&Trigger::After(0), 1, completed), None);
        assert_eq!(next_due(&Trigger::OnConnect, 0, completed), None);
    }
}
