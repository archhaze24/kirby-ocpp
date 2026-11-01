//! `StatusNotification` discipline.
//!
//! TS reference: `src/station/status-scheduler.ts`, `sendStatusNotification` in
//! `src/station.ts`. Rules:
//!
//! * What is compared and remembered is the whole reported tuple ([`StatusKey`]:
//!   status, errorCode, info, vendorId, vendorErrorCode). The same status is re-sent
//!   when only the error code or info changed (the TS scheduler suppressed that).
//! * `MinimumStatusDuration > 0` debounces: a change arms a per-connector timer; every
//!   further change restarts it. When it fires and the state differs from what was last
//!   sent, one notification goes out. If the state went back to the last sent value in
//!   between, nothing is sent. The wire `timestamp` is the time of the **change**, not
//!   of the (delayed) send. `MinimumStatusDuration = 0` sends at once and flushes what is
//!   pending.
//! * Not connected, or not registered (`BootNotification` not `Accepted`): nothing is
//!   sent and nothing is queued; after `Accepted` every connector, including connector 0,
//!   is re-sent with force.
//! * Forced sends (boot, `TriggerMessage`, the `StatusNotification` command) ignore the
//!   debounce and the last-sent value.
//!
//! [`StatusTracker`] is the pure part (unit tested below); the `impl StationActor` block
//! wires it to timers and calls.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};

use super::actor::{StationActor, TimerKey};
use super::calls::{Origin, Reply};
use super::command::Outcome;
use super::commands::call_failure;
use crate::error::StationError;
use crate::ids::ConnectorId;
use crate::ocpp::messages::{StatusNotification, StatusNotificationRequest};
use crate::ocpp::types::{ChargePointErrorCode, ConnectorStatus};
use crate::station::event::LogLevel;
use crate::station::snapshot::ConnectorSnapshot;

/// The reportable state of a connector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusKey {
    pub status: ConnectorStatus,
    pub error_code: ChargePointErrorCode,
    pub info: Option<String>,
    pub vendor_id: Option<String>,
    pub vendor_error_code: Option<String>,
}

impl StatusKey {
    pub fn of(connector: &ConnectorSnapshot) -> Self {
        Self {
            status: connector.status,
            error_code: connector.error_code,
            info: connector.info.clone(),
            vendor_id: connector.vendor_id.clone(),
            vendor_error_code: connector.vendor_error_code.clone(),
        }
    }
}

/// What to do about a changed connector state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// Nothing to do; a running debounce timer keeps running.
    Nothing,
    /// The state equals what was last sent: cancel a pending debounce, send nothing.
    Cancel,
    /// Send now, with this timestamp.
    Send(DateTime<Utc>),
    /// (Re)start the debounce timer.
    Defer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    key: StatusKey,
    changed_at: DateTime<Utc>,
}

/// Last sent state and pending debounce of one connector.
#[derive(Debug, Default)]
pub struct StatusTracker {
    last_sent: Option<StatusKey>,
    pending: Option<Pending>,
}

impl StatusTracker {
    /// The connector state is now `current`; decide what to do.
    pub fn plan_change(
        &mut self,
        current: &StatusKey,
        now: DateTime<Utc>,
        minimum: Duration,
    ) -> Plan {
        if self.last_sent.as_ref() == Some(current) {
            self.pending = None;
            return Plan::Cancel;
        }
        if minimum.is_zero() {
            self.pending = None;
            return Plan::Send(now);
        }
        if self.pending.as_ref().is_some_and(|p| &p.key == current) {
            return Plan::Nothing;
        }
        self.pending = Some(Pending {
            key: current.clone(),
            changed_at: now,
        });
        Plan::Defer
    }

    /// The debounce timer fired (or the debounce is being flushed): the timestamp to send
    /// with, if the state still differs from what was last sent.
    pub fn take_due(&mut self, current: &StatusKey) -> Option<DateTime<Utc>> {
        let pending = self.pending.take()?;
        (self.last_sent.as_ref() != Some(current)).then_some(pending.changed_at)
    }

    /// A forced send supersedes any pending debounce.
    pub fn forced(&mut self) {
        self.pending = None;
    }

    pub fn mark_sent(&mut self, key: StatusKey) {
        self.last_sent = Some(key);
    }

    /// The send of `key` failed: forget it, so the next change is sent again.
    pub fn mark_failed(&mut self, key: &StatusKey) {
        if self.last_sent.as_ref() == Some(key) {
            self.last_sent = None;
        }
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Forget everything (the connection is gone).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Trackers by connector id (0 = charge point).
pub type Trackers = BTreeMap<ConnectorId, StatusTracker>;

impl StationActor {
    fn status_key(&self, id: ConnectorId) -> Option<StatusKey> {
        self.find_connector(id).map(StatusKey::of)
    }

    /// The connector's reportable state may have changed: notify according to the
    /// discipline described in the module docs. Never blocks, never fails.
    pub(super) fn status_changed(&mut self, id: ConnectorId) {
        if !self.can_send_status(Origin::Station) {
            return;
        }
        let Some(key) = self.status_key(id) else {
            return;
        };
        let now = self.clock.now_utc();
        let minimum = self.registry.minimum_status_duration();
        match self
            .trackers
            .entry(id)
            .or_default()
            .plan_change(&key, now, minimum)
        {
            Plan::Nothing => {}
            Plan::Cancel => {
                self.timers.cancel(&TimerKey::StatusDebounce(id));
            }
            Plan::Send(at) => {
                self.timers.cancel(&TimerKey::StatusDebounce(id));
                self.dispatch_status(id, key, at, Origin::Station, None);
            }
            Plan::Defer => self.timers.set(TimerKey::StatusDebounce(id), minimum),
        }
    }

    /// The debounce timer of `id` fired.
    pub(super) fn status_timer_fired(&mut self, id: ConnectorId) {
        let Some(key) = self.status_key(id) else {
            return;
        };
        if !self.can_send_status(Origin::Station) {
            self.trackers.entry(id).or_default().forced();
            return;
        }
        if let Some(at) = self.trackers.entry(id).or_default().take_due(&key) {
            self.dispatch_status(id, key, at, Origin::Station, None);
        }
    }

    /// Sends the current state of `id` regardless of debounce and last-sent value.
    /// `reply` (a user command) is answered when the CALL resolves, or at once with
    /// the reason it could not be sent.
    pub(super) fn send_status_forced(
        &mut self,
        id: ConnectorId,
        origin: Origin,
        reply: Option<Reply>,
    ) {
        let Some(key) = self.status_key(id) else {
            if let Some(reply) = reply {
                let _ = reply.send(Err(StationError::UnknownConnector(id)));
            }
            return;
        };
        self.timers.cancel(&TimerKey::StatusDebounce(id));
        self.trackers.entry(id).or_default().forced();
        let now = self.clock.now_utc();
        self.dispatch_status(id, key, now, origin, reply);
    }

    /// After `BootNotification` was accepted: connector 0 first, then every connector.
    pub(super) fn resend_all_statuses(&mut self) {
        let ids: Vec<ConnectorId> = std::iter::once(ConnectorId::STATION)
            .chain(self.state.connectors.iter().map(|c| c.id))
            .collect();
        for id in ids {
            self.send_status_forced(id, Origin::Station, None);
        }
    }

    /// `MinimumStatusDuration` became 0: send what was waiting for its timer.
    pub(super) fn flush_pending_statuses(&mut self) {
        let waiting: Vec<ConnectorId> = self
            .trackers
            .iter()
            .filter(|(_, tracker)| tracker.has_pending())
            .map(|(id, _)| *id)
            .collect();
        for id in waiting {
            self.timers.cancel(&TimerKey::StatusDebounce(id));
            self.status_timer_fired(id);
        }
    }

    /// The connection is gone: drop debounce state, there is nothing to send to.
    pub(super) fn reset_status_tracking(&mut self) {
        self.timers
            .cancel_where(|key| matches!(key, TimerKey::StatusDebounce(_)));
        self.trackers.values_mut().for_each(StatusTracker::reset);
    }

    /// Whether a `StatusNotification` of `origin` may go out now.
    pub(super) fn can_send_status(&self, origin: Origin) -> bool {
        self.link.conn.is_some() && self.may_send("StatusNotification", origin).is_ok()
    }

    fn dispatch_status(
        &mut self,
        id: ConnectorId,
        key: StatusKey,
        timestamp: DateTime<Utc>,
        origin: Origin,
        reply: Option<Reply>,
    ) {
        let request = StatusNotificationRequest {
            connector_id: id,
            error_code: key.error_code,
            info: key.info.clone(),
            status: key.status,
            timestamp: Some(timestamp),
            vendor_id: key.vendor_id.clone(),
            vendor_error_code: key.vendor_error_code.clone(),
        };
        self.trackers.entry(id).or_default().mark_sent(key.clone());
        self.call::<StatusNotification>(origin, &request, move |actor, result| match result {
            Ok(_) => {
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(Outcome::Done));
                }
            }
            Err(error) => {
                actor.trackers.entry(id).or_default().mark_failed(&key);
                actor.log(
                    LogLevel::Warn,
                    format!("StatusNotification for connector {id} failed: {error}"),
                );
                if let Some(reply) = reply {
                    let _ = reply.send(Err(call_failure(error)));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    const MIN: Duration = Duration::from_secs(5);

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    fn key(status: ConnectorStatus) -> StatusKey {
        StatusKey {
            status,
            error_code: ChargePointErrorCode::NoError,
            info: None,
            vendor_id: None,
            vendor_error_code: None,
        }
    }

    #[test]
    fn without_a_minimum_duration_changes_are_sent_at_once() {
        let mut tracker = StatusTracker::default();
        let preparing = key(ConnectorStatus::Preparing);
        assert_eq!(
            tracker.plan_change(&preparing, t(1), Duration::ZERO),
            Plan::Send(t(1))
        );
        tracker.mark_sent(preparing.clone());
        // Unchanged state: nothing to say.
        assert_eq!(
            tracker.plan_change(&preparing, t(2), Duration::ZERO),
            Plan::Cancel
        );
    }

    #[test]
    fn same_status_with_a_new_error_code_is_a_change() {
        let mut tracker = StatusTracker::default();
        let mut faulted = key(ConnectorStatus::Faulted);
        faulted.error_code = ChargePointErrorCode::OtherError;
        tracker.mark_sent(faulted.clone());
        faulted.error_code = ChargePointErrorCode::GroundFailure;
        assert_eq!(
            tracker.plan_change(&faulted, t(1), Duration::ZERO),
            Plan::Send(t(1))
        );
        let mut with_info = faulted.clone();
        tracker.mark_sent(faulted);
        with_info.info = Some("door open".into());
        assert_eq!(
            tracker.plan_change(&with_info, t(2), Duration::ZERO),
            Plan::Send(t(2))
        );
    }

    #[test]
    fn debounce_defers_and_the_timestamp_is_the_change_time() {
        let mut tracker = StatusTracker::default();
        let preparing = key(ConnectorStatus::Preparing);
        assert_eq!(tracker.plan_change(&preparing, t(10), MIN), Plan::Defer);
        // Same state again: the running timer is left alone.
        assert_eq!(tracker.plan_change(&preparing, t(12), MIN), Plan::Nothing);
        assert!(tracker.has_pending());
        // The timer fires at t(15): timestamp is t(10), when the change happened.
        assert_eq!(tracker.take_due(&preparing), Some(t(10)));
        assert!(!tracker.has_pending());
    }

    #[test]
    fn a_newer_change_restarts_the_debounce_with_its_own_time() {
        let mut tracker = StatusTracker::default();
        assert_eq!(
            tracker.plan_change(&key(ConnectorStatus::Preparing), t(0), MIN),
            Plan::Defer
        );
        let charging = key(ConnectorStatus::Charging);
        assert_eq!(tracker.plan_change(&charging, t(3), MIN), Plan::Defer);
        assert_eq!(tracker.take_due(&charging), Some(t(3)));
    }

    #[test]
    fn reverting_within_the_window_sends_nothing() {
        let mut tracker = StatusTracker::default();
        let available = key(ConnectorStatus::Available);
        tracker.mark_sent(available.clone());
        assert_eq!(
            tracker.plan_change(&key(ConnectorStatus::Preparing), t(0), MIN),
            Plan::Defer
        );
        assert_eq!(tracker.plan_change(&available, t(1), MIN), Plan::Cancel);
        assert!(!tracker.has_pending());
        assert_eq!(tracker.take_due(&available), None);
    }

    #[test]
    fn a_due_state_equal_to_the_last_sent_is_skipped() {
        let mut tracker = StatusTracker::default();
        let available = key(ConnectorStatus::Available);
        tracker.plan_change(&key(ConnectorStatus::Preparing), t(0), MIN);
        tracker.mark_sent(available.clone());
        assert_eq!(tracker.take_due(&available), None);
    }

    #[test]
    fn a_failed_send_is_retried_by_the_next_change() {
        let mut tracker = StatusTracker::default();
        let preparing = key(ConnectorStatus::Preparing);
        tracker.mark_sent(preparing.clone());
        tracker.mark_failed(&preparing);
        assert_eq!(
            tracker.plan_change(&preparing, t(1), Duration::ZERO),
            Plan::Send(t(1))
        );
        // A failure of an older key does not clobber a newer one.
        tracker.mark_sent(key(ConnectorStatus::Charging));
        tracker.mark_failed(&preparing);
        assert_eq!(
            tracker.plan_change(&key(ConnectorStatus::Charging), t(2), Duration::ZERO),
            Plan::Cancel
        );
    }

    #[test]
    fn forced_and_reset_clear_the_pending_state() {
        let mut tracker = StatusTracker::default();
        tracker.plan_change(&key(ConnectorStatus::Preparing), t(0), MIN);
        tracker.forced();
        assert!(!tracker.has_pending());
        tracker.mark_sent(key(ConnectorStatus::Preparing));
        tracker.reset();
        assert_eq!(
            tracker.plan_change(&key(ConnectorStatus::Preparing), t(1), Duration::ZERO),
            Plan::Send(t(1))
        );
    }
}
