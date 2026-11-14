//! Sends the CSMS CALLs of a recording to the live stations.
//!
//! Every CALL goes out through [`CsmsHandle::call_session_timeout`] with the token of the
//! live connection, so a replacement connection can never receive it. The recorded
//! transaction id of a `RemoteStopTransaction` is never sent: the runner ties the recorded
//! `StartTransaction` to the live one (same station, connector and `idTag`, same order) with
//! the facts of the engine's [`Observer`], which was attached before the first connection.
//! The observer is drained during every wait, so the feed cannot overflow while the runner
//! sleeps or waits for an answer.

use std::collections::HashMap;
use std::time::Duration;

use kirby_csms_core::{ConnectedSession, CsmsHandle, Frame, Observation, Observer};
use tokio::time::{Instant, sleep_until, timeout_at};

use super::plan::{MAX_LIVE_STARTS, Plan, Step};
use crate::headless::sanitize;

struct LiveStart {
    connector: Option<u32>,
    id_tag: Option<String>,
    transaction_id: Option<i32>,
}

struct Live {
    token: ConnectedSession,
    booted: bool,
    starts: Vec<LiveStart>,
    /// Live transaction id to the position of its start; removed by a StopTransaction.
    active: HashMap<i32, usize>,
    /// A CALL was sent on this connection: losing it fails the run.
    used: bool,
}

struct Tracker<'a> {
    plan: &'a Plan,
    observer: &'a mut Observer,
    live: HashMap<String, Live>,
    /// CALLs answered so far.
    completed: usize,
}

impl Tracker<'_> {
    /// Whether the plan has no CALL left for `station`; its connection may end then.
    fn finished(&self, station: &str) -> bool {
        self.plan
            .steps
            .iter()
            .skip(self.completed)
            .all(|step| step.station != station)
    }

    /// Takes the next observation and applies it.
    async fn pump(&mut self) -> Result<(), String> {
        let observation = self
            .observer
            .recv()
            .await
            .map_err(|error| format!("cannot follow the stations: {error}"))?;
        self.absorb(observation)
    }

    fn absorb(&mut self, observation: Observation) -> Result<(), String> {
        match observation {
            Observation::Connected(token) => {
                let station = token.station().to_owned();
                if !self.plan.stations.contains_key(&station) {
                    return Ok(());
                }
                if self.live.get(&station).is_some_and(|live| live.used) && !self.finished(&station)
                {
                    return Err(format!(
                        "station {} was replaced during the replay",
                        sanitize(&station)
                    ));
                }
                self.live.insert(
                    station,
                    Live {
                        token,
                        booted: false,
                        starts: Vec::new(),
                        active: HashMap::new(),
                        used: false,
                    },
                );
            }
            Observation::Ended { station, session } => {
                let Some(live) = self.live.get(&station) else {
                    return Ok(());
                };
                if live.token.id() != session {
                    return Ok(());
                }
                if live.used && !self.finished(&station) {
                    return Err(format!(
                        "the connection of station {} ended during the replay",
                        sanitize(&station)
                    ));
                }
                self.live.remove(&station);
            }
            Observation::Boot {
                station,
                session,
                accepted,
            } => {
                if let Some(live) = self.current(&station, session)
                    && accepted
                {
                    live.booted = true;
                }
            }
            Observation::Start {
                station,
                session,
                connector,
                id_tag,
                transaction_id,
            } => {
                if let Some(live) = self.current(&station, session) {
                    if live.starts.len() >= MAX_LIVE_STARTS {
                        return Err(format!(
                            "station {} started more than {MAX_LIVE_STARTS} transactions",
                            sanitize(&station)
                        ));
                    }
                    if let Some(id) = transaction_id
                        && live.active.insert(id, live.starts.len()).is_some()
                    {
                        return Err(format!(
                            "station {} was given the transaction id {id} while it is still active",
                            sanitize(&station)
                        ));
                    }
                    live.starts.push(LiveStart {
                        connector,
                        id_tag,
                        transaction_id,
                    });
                }
            }
            Observation::Stop {
                station,
                session,
                transaction_id,
            } => {
                if let (Some(live), Some(id)) = (self.current(&station, session), transaction_id) {
                    live.active.remove(&id);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn current(&mut self, station: &str, session: kirby_csms_core::SessionId) -> Option<&mut Live> {
        self.live
            .get_mut(station)
            .filter(|live| live.token.id() == session)
    }

    /// Applies observations until `ready` holds, at most until `limit`.
    async fn wait_for(
        &mut self,
        limit: Option<Instant>,
        what: &str,
        mut ready: impl FnMut(&Self) -> Result<bool, String>,
    ) -> Result<(), String> {
        loop {
            if ready(self)? {
                return Ok(());
            }
            match limit {
                Some(limit) => timeout_at(limit, self.pump())
                    .await
                    .map_err(|_| format!("timed out waiting for {what}"))??,
                None => self.pump().await?,
            }
        }
    }

    /// Sleeps until `at` while applying observations.
    async fn sleep_until(&mut self, at: Instant) -> Result<(), String> {
        loop {
            tokio::select! {
                () = sleep_until(at) => return Ok(()),
                result = self.pump() => result?,
            }
        }
    }

    /// The live transaction id for the recorded start `index` of `step`'s station, after
    /// the identity of every start up to it was checked.
    fn live_transaction(&self, step: &Step, index: usize) -> Result<i32, String> {
        let station = sanitize(&step.station);
        let live = self
            .live
            .get(&step.station)
            .ok_or_else(|| format!("station {station} is not connected"))?;
        let recorded = &self.plan.stations[&step.station].starts;
        for (position, wanted) in recorded.iter().enumerate().take(index + 1) {
            let start = live.starts.get(position).ok_or_else(|| {
                format!(
                    "station {station} has not made StartTransaction number {}",
                    position + 1
                )
            })?;
            if start.connector != Some(wanted.connector)
                || start.id_tag.as_deref() != Some(wanted.id_tag.as_str())
            {
                return Err(format!(
                    "StartTransaction number {} of station {station} differs from the recording (connector or idTag)",
                    position + 1
                ));
            }
        }
        let id = live.starts[index].transaction_id.ok_or_else(|| {
            format!(
                "StartTransaction number {} of station {station} was not answered with a transaction id",
                index + 1
            )
        })?;
        if live.active.get(&id) != Some(&index) {
            return Err(format!(
                "the transaction of StartTransaction number {} of station {station} is no longer active",
                index + 1
            ));
        }
        Ok(id)
    }
}

/// Runs the plan; `sent` counts the CALLs that were answered as recorded.
pub(super) async fn run(
    plan: &Plan,
    handle: &CsmsHandle,
    observer: &mut Observer,
    call_timeout: Duration,
    sent: &mut usize,
) -> Result<(), String> {
    let mut tracker = Tracker {
        plan,
        observer,
        live: HashMap::new(),
        completed: 0,
    };
    // Every station that gets a CALL must be connected before the clock starts.
    tracker
        .wait_for(None, "the stations to connect", |tracker| {
            Ok(tracker
                .plan
                .stations
                .keys()
                .all(|station| tracker.live.contains_key(station)))
        })
        .await?;
    let first_ms = plan.steps.first().map_or(0, |step| step.elapsed_ms);
    let started = Instant::now();
    for (number, step) in plan.steps.iter().enumerate() {
        let due = started + Duration::from_millis(step.elapsed_ms - first_ms);
        tracker.sleep_until(due).await?;
        let limit = Instant::now() + call_timeout;
        let name = sanitize(&step.action);
        let station = sanitize(&step.station);
        tracker
            .wait_for(
                Some(limit),
                &format!("station {station} before {name}"),
                |t| {
                    let Some(live) = t.live.get(&step.station) else {
                        return Ok(false);
                    };
                    Ok((!step.needs_boot || live.booted)
                        && step.start.is_none_or(|index| live.starts.len() > index))
                },
            )
            .await?;
        let mut payload = step.payload.clone();
        if let Some(index) = step.start {
            let id = tracker.live_transaction(step, index)?;
            payload["transactionId"] = id.into();
        }
        let Some(live) = tracker.live.get_mut(&step.station) else {
            return Err(format!("station {station} is not connected"));
        };
        live.used = true;
        let token = live.token.clone();
        let mut call = std::pin::pin!(handle.call_session_timeout(
            &token,
            &step.action,
            payload,
            call_timeout
        ));
        let answer = loop {
            tokio::select! {
                answer = &mut call => break answer,
                result = tracker.pump() => result?,
            }
        };
        match answer {
            Ok(Frame::CallResult { .. }) if !step.expect_error => {}
            Ok(Frame::CallError { .. }) if step.expect_error => {}
            Ok(Frame::CallResult { .. }) => {
                return Err(format!(
                    "CALL {} ({name} to {station}): the recording has a CALLERROR, the station answered with a result",
                    number + 1
                ));
            }
            Ok(Frame::CallError { code, .. }) => {
                return Err(format!(
                    "CALL {} ({name} to {station}): the recording has a result, the station answered with the CALLERROR {}",
                    number + 1,
                    sanitize(&format!("{code:?}"))
                ));
            }
            Ok(_) => {
                return Err(format!(
                    "CALL {} ({name} to {station}): unexpected answer",
                    number + 1
                ));
            }
            Err(error) => {
                return Err(format!(
                    "CALL {} ({name} to {station}): {}",
                    number + 1,
                    sanitize(&error.to_string())
                ));
            }
        }
        *sent += 1;
        tracker.completed = number + 1;
    }
    Ok(())
}
