//! `Reset`: the simulated reboot and its plan.
//!
//! A plan moves through [`ResetPhase`]s, all driven by the `ResetDelay` timer so that the
//! `ResetResponse` is always queued before any work starts:
//!
//! 1. `Immediate` starts in `EndingTransactions`: every transaction ends with trigger
//!    `ResetCommand` and reason `ImmediateReset` (B12.FR.04). `OnIdle` starts in
//!    `WaitingForIdle` and continues once the last transaction ended on its own (B12.FR.03).
//! 2. The `Ended` events get until the call timeout to reach the CSMS; after that they stay
//!    queued and are sent after the reboot (B12.FR.05). Without a registered link there is
//!    nothing to wait for.
//! 3. `Rebooting`: the link is closed and opened again, and the next `BootNotification`
//!    carries `RemoteReset`. The link drops the plan when it sends that boot, or when the
//!    link is disconnected by command (the next boot is a power-up then).

use std::time::Duration;

use tokio::time::Instant;

use super::actor::{Station2Actor, TimerKey2};
use crate::ocpp::v2::messages::{BootReason, ResetKind, ResetStatus, TriggerReason};
use crate::ocpp::v2::types::StopReason;
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
}

/// Where a reset stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetPhase {
    /// `OnIdle`: transactions end normally first.
    WaitingForIdle,
    /// `Immediate`: transactions are being ended.
    EndingTransactions,
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
        if let Some(plan) = &self.reset {
            let mid_reboot = plan.phase == ResetPhase::Rebooting
                && self.state.registration.is_none()
                && self.link.wanted;
            if mid_reboot {
                return ResetStatus::Accepted;
            }
            // A finished or superseded plan is replaced below; one in progress is joined.
            if plan.phase == ResetPhase::EndingTransactions {
                return ResetStatus::Accepted;
            }
        }
        let busy = self.any_active_tx();
        let phase = match kind {
            ResetKind::Immediate => ResetPhase::EndingTransactions,
            ResetKind::OnIdle => ResetPhase::WaitingForIdle,
        };
        self.reset = Some(ResetPlan {
            kind,
            phase,
            reason: BootReason::RemoteReset,
        });
        self.auth.reset_deadline = None;
        self.auth.reset_ended = false;
        self.timers.set(TimerKey2::ResetDelay, Duration::ZERO);
        if kind == ResetKind::OnIdle && busy {
            ResetStatus::Scheduled
        } else {
            ResetStatus::Accepted
        }
    }

    /// The reset delay elapsed.
    pub(super) fn on_reset_timer(&mut self) {
        let Some(plan) = self.reset.clone() else {
            return;
        };
        match plan.phase {
            ResetPhase::WaitingForIdle => {
                if self.any_active_tx() {
                    // `reset_on_transaction_ended` wakes the plan up again.
                    return;
                }
                self.set_reset_phase(ResetPhase::EndingTransactions);
                self.wait_for_delivery();
            }
            ResetPhase::EndingTransactions => {
                if plan.kind == ResetKind::Immediate && !self.auth.reset_ended {
                    self.end_all_transactions();
                } else if plan.kind == ResetKind::OnIdle && self.any_active_tx() {
                    // A transaction started after the plan became ready: wait for it too.
                    self.set_reset_phase(ResetPhase::WaitingForIdle);
                    // The next delivery wait starts from its own deadline.
                    self.auth.reset_deadline = None;
                    return;
                }
                self.wait_for_delivery();
            }
            // The link ends this phase when the boot of the reboot goes out.
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
        self.auth.reset_ended = true;
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
        self.auth.reset_ended = false;
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
            phase: ResetPhase::EndingTransactions,
            reason: BootReason::RemoteReset,
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
