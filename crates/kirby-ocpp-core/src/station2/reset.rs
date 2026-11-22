//! `Reset`: the simulated reboot and its plan.
//!
//! A plan moves through [`ResetPhase`]s, all driven by the `ResetDelay` timer so that the
//! `ResetResponse` is always queued before any work starts:
//!
//! 1. `Immediate` ends every transaction with trigger `ResetCommand` and reason
//!    `ImmediateReset` (B12.FR.04). `OnIdle` waits for the last transaction to end
//!    (B12.FR.03). `ImmediateAndResume` suspends energy transfer (B13.FR.04/20).
//! 2. The transaction events get until the call timeout to reach the CSMS; after that they stay
//!    queued and are sent after the reboot (B12.FR.05). Without a registered link there is
//!    nothing to wait for.
//! 3. `Rebooting`: the link is closed and opened again, and the next `BootNotification`
//!    carries `RemoteReset`, or `ScheduledReset` when the reset was answered `Scheduled`
//!    (TC_B_21_CS, TC_B_22_CS). `ImmediateAndResume` keeps its plan until the boot is
//!    accepted and the transactions resume. The station reports `ResetOrReboot` after an
//!    accepted boot (A04.FR.01).

use std::collections::BTreeMap;
use std::time::Duration;

use tokio::time::Instant;

use super::actor::{Station2Actor, TimerKey2};
use super::types::TxId2;
use crate::ocpp::Protocol;
use crate::ocpp::v2::messages::{BootReason, ResetKind, ResetStatus, TriggerReason};
use crate::ocpp::v2::types::{ChargingState, StopReason};
use crate::station::LogLevel;

/// How often a reset checks whether its events have left the queue.
const DELIVERY_POLL: Duration = Duration::from_millis(50);

/// A reset that is waiting for its turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetPlan {
    pub kind: ResetKind,
    pub phase: ResetPhase,
    /// The reason the next `BootNotification` carries.
    pub reason: BootReason,
    /// The transaction identity and charging state to restore for each affected EVSE.
    pub(super) charging_before_reset: BTreeMap<u32, (TxId2, ChargingState)>,
}

impl ResetPlan {
    pub(super) fn resumes_transaction(&self, evse: u32, id: &TxId2, prepared: bool) -> bool {
        self.kind == ResetKind::ImmediateAndResume
            && (!prepared
                || self
                    .charging_before_reset
                    .get(&evse)
                    .is_some_and(|(planned_id, _)| planned_id == id))
    }
}

/// Where a reset stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetPhase {
    /// `OnIdle`: transactions end normally first.
    WaitingForIdle,
    /// Prepare the transactions and wait for their events to reach the CSMS.
    PreparingTransactions,
    /// The link is closed and the station boots again.
    Rebooting,
}

impl Station2Actor {
    /// Starts a reset and returns the status for the `ResetResponse`. Only registers the plan
    /// and arms the timer; the work happens when the timer fires, after the response.
    pub(super) fn start_reset(&mut self, kind: ResetKind, evse: Option<u32>) -> ResetStatus {
        // B11.FR.09: resetting a single EVSE is not supported (`EVSE.AllowReset` is false).
        if evse.is_some() {
            return ResetStatus::Rejected;
        }
        if kind == ResetKind::ImmediateAndResume
            && (self.protocol() != Protocol::V21 || self.tx_resumption_timeout() == 0)
        {
            return ResetStatus::Rejected;
        }
        if let Some(plan) = &self.reset {
            let mid_reboot = plan.phase == ResetPhase::Rebooting
                && (plan.kind == ResetKind::ImmediateAndResume
                    || (self.state.registration.is_none() && self.link.wanted));
            if mid_reboot {
                return ResetStatus::Accepted;
            }
            // A finished or superseded plan is replaced below; one in progress is joined.
            if plan.phase == ResetPhase::PreparingTransactions {
                return ResetStatus::Accepted;
            }
        }
        let busy = self.any_active_tx();
        let kind = if kind == ResetKind::ImmediateAndResume && !busy {
            ResetKind::Immediate
        } else {
            kind
        };
        let phase = match kind {
            ResetKind::OnIdle => ResetPhase::WaitingForIdle,
            ResetKind::Immediate | ResetKind::ImmediateAndResume => {
                ResetPhase::PreparingTransactions
            }
        };
        // A reset answered `Scheduled` runs later on its own: its boot is a `ScheduledReset`.
        let scheduled = kind == ResetKind::OnIdle && busy;
        self.reset = Some(ResetPlan {
            kind,
            phase,
            reason: if scheduled {
                BootReason::ScheduledReset
            } else {
                BootReason::RemoteReset
            },
            charging_before_reset: BTreeMap::new(),
        });
        self.auth.reset_deadline = None;
        self.auth.reset_prepared = false;
        self.timers.set(TimerKey2::ResetDelay, Duration::ZERO);
        if scheduled {
            ResetStatus::Scheduled
        } else {
            ResetStatus::Accepted
        }
    }

    /// The reset delay elapsed.
    pub(super) fn on_reset_timer(&mut self) {
        let Some(plan) = &self.reset else {
            return;
        };
        match plan.phase {
            ResetPhase::WaitingForIdle => {
                if self.any_active_tx() {
                    // `reset_on_transaction_ended` wakes the plan up again.
                    return;
                }
                self.set_reset_phase(ResetPhase::PreparingTransactions);
                self.wait_for_delivery();
            }
            ResetPhase::PreparingTransactions => {
                match plan.kind {
                    ResetKind::Immediate if !self.auth.reset_prepared => {
                        self.end_all_transactions();
                    }
                    ResetKind::ImmediateAndResume if !self.auth.reset_prepared => {
                        self.suspend_for_reset();
                    }
                    ResetKind::OnIdle if self.any_active_tx() => {
                        // A transaction started after the plan became ready: wait for it too.
                        self.set_reset_phase(ResetPhase::WaitingForIdle);
                        // The next delivery wait starts from its own deadline.
                        self.auth.reset_deadline = None;
                        return;
                    }
                    _ => {}
                }
                self.wait_for_delivery();
            }
            // The link completes the reboot.
            ResetPhase::Rebooting => {}
        }
    }

    /// A transaction ended on `evse`; an `OnIdle` reset may proceed now. Never reboots
    /// itself: the transaction engine calls this while it is still ending transactions.
    pub(super) fn reset_on_transaction_ended(&mut self, _evse: u32) {
        let waiting = self
            .reset
            .as_ref()
            .is_some_and(|plan| plan.phase == ResetPhase::WaitingForIdle);
        if waiting && !self.any_active_tx() {
            self.timers.set(TimerKey2::ResetDelay, Duration::ZERO);
        }
    }

    fn set_reset_phase(&mut self, phase: ResetPhase) {
        if let Some(plan) = &mut self.reset {
            plan.phase = phase;
        }
    }

    fn any_active_tx(&self) -> bool {
        self.evses.iter().any(|e| self.active_tx(e.id).is_some())
    }

    /// Ends every running transaction for an `Immediate` reset (B12.FR.04).
    fn end_all_transactions(&mut self) {
        self.auth.reset_prepared = true;
        let ids: Vec<u32> = self
            .evses
            .iter()
            .map(|e| e.id)
            .filter(|id| self.active_tx(*id).is_some())
            .collect();
        for id in ids {
            if let Err(error) =
                self.end_transaction(id, StopReason::ImmediateReset, TriggerReason::ResetCommand)
            {
                self.log(
                    LogLevel::Warn,
                    format!("reset: the transaction on EVSE {id} could not be ended: {error}"),
                );
            }
        }
    }

    /// B13: suspends energy and reports the reset without ending the transactions.
    fn suspend_for_reset(&mut self) {
        self.auth.reset_prepared = true;
        let mut previous = BTreeMap::new();
        for evse in self.active_tx_evses() {
            let Some(tx) = self.active_tx(evse) else {
                continue;
            };
            let state = tx.charging_state;
            let resume = self
                .reset
                .as_ref()
                .and_then(|plan| plan.charging_before_reset.get(&evse))
                .filter(|(id, _)| *id == tx.id)
                .map_or(state, |(_, resume)| *resume);
            previous.insert(evse, (tx.id.clone(), resume));
            let suspended = if state == ChargingState::Charging {
                ChargingState::SuspendedEvse
            } else {
                state
            };
            self.tx_reset_update(evse, suspended, TriggerReason::ResetCommand);
        }
        if let Some(plan) = &mut self.reset {
            plan.charging_before_reset = previous;
        }
    }

    /// B13: resumes the transactions once the CSMS accepts the reboot's boot.
    pub(super) fn finish_reset_resume(&mut self) {
        if !self.reset.as_ref().is_some_and(|plan| {
            plan.phase == ResetPhase::Rebooting && plan.kind == ResetKind::ImmediateAndResume
        }) {
            return;
        }
        let Some(plan) = self.reset.take() else {
            return;
        };
        for (evse, (id, state)) in plan.charging_before_reset {
            if self
                .active_tx(evse)
                .is_some_and(|tx| tx.id == id && tx.time_limit_reached(self.clock.now_utc()))
            {
                self.check_tx_limits(evse);
            }
            if let Some(tx) = self.active_tx(evse).filter(|tx| tx.id == id) {
                let state = self.resumed_charging_state(tx, state);
                self.tx_reset_update(evse, state, TriggerReason::TxResumed);
            }
        }
        self.raise_security_event("ResetOrReboot");
        self.log(LogLevel::Info, "reset finished".to_owned());
    }

    /// Reboots once the queued events were delivered, the delivery deadline passed, or
    /// nothing can be delivered at all.
    fn wait_for_delivery(&mut self) {
        let timeout = self.message_timeout();
        let deadline = *self
            .auth
            .reset_deadline
            .get_or_insert_with(|| Instant::now() + timeout);
        let can_deliver = self.link.conn.is_some() && self.registration_accepted();
        if can_deliver && self.queue_has_messages_for(None) {
            if Instant::now() < deadline {
                self.timers.set(TimerKey2::ResetDelay, DELIVERY_POLL);
                return;
            }
            self.log(
                LogLevel::Warn,
                "reset: events are still queued after the call timeout; they follow the reboot"
                    .to_owned(),
            );
        }
        self.reboot();
    }

    /// The simulated reboot: persisted state stays, the link and everything volatile restarts.
    fn reboot(&mut self) {
        self.set_reset_phase(ResetPhase::Rebooting);
        self.auth.reset_deadline = None;
        self.auth.reset_prepared = false;
        self.log(LogLevel::Info, "rebooting".to_owned());
        self.flush_now();
        self.reconnect_for_reset();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::{broadcast, mpsc, oneshot};

    use super::*;
    use crate::clock::ManualClock;
    use crate::config::StationConfig;
    use crate::ocpp::Protocol;
    use crate::ocpp::v2::types::IdTokenType;
    use crate::station2::types::IdTokenRef;

    fn actor() -> Station2Actor {
        let mut config = StationConfig::new("ws://127.0.0.1:1")
            .with_connectors(2)
            .without_persistence();
        config.protocol = Protocol::V201;
        let (_commands, receiver) = mpsc::channel(4);
        let (events, _) = broadcast::channel(64);
        Station2Actor::new(
            config,
            Arc::new(ManualClock::new(chrono::Utc::now())),
            receiver,
            events,
            None,
        )
        .0
    }

    #[tokio::test]
    async fn an_on_idle_plan_that_falls_back_forgets_the_delivery_deadline() {
        let mut a = actor();
        let (reply, _answer) = oneshot::channel();
        a.cmd_plug_in(1, reply);
        a.on_authorized(
            1,
            IdTokenRef {
                id_token: "TAG".to_owned(),
                kind: IdTokenType::Local,
            },
            None,
            TriggerReason::Authorized,
        )
        .expect("authorized");
        a.reset = Some(ResetPlan {
            kind: ResetKind::OnIdle,
            phase: ResetPhase::PreparingTransactions,
            reason: BootReason::RemoteReset,
            charging_before_reset: BTreeMap::new(),
        });
        // The deadline of an earlier delivery wait has passed long ago.
        a.auth.reset_deadline = Some(Instant::now());
        a.on_reset_timer();
        assert_eq!(
            a.reset.as_ref().map(|plan| plan.phase),
            Some(ResetPhase::WaitingForIdle)
        );
        assert!(a.auth.reset_deadline.is_none(), "a stale deadline is kept");
    }
}
