//! `Reset`: the simulated reboot and its plan.

#![allow(
    dead_code,
    clippy::unused_self,
    reason = "placeholder module; remove this allow when the module is implemented"
)]

use super::actor::Station2Actor;
use crate::ocpp::v2::messages::{BootReason, ResetKind, ResetStatus};

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
    /// Starts a reset and returns the status for the `ResetResponse`.
    pub(super) fn start_reset(&mut self, _kind: ResetKind, _evse: Option<u32>) -> ResetStatus {
        ResetStatus::Rejected
    }

    /// The reset delay elapsed.
    pub(super) fn on_reset_timer(&mut self) {}

    /// A transaction ended on `evse`; an `OnIdle` reset may proceed now.
    pub(super) fn reset_on_transaction_ended(&mut self, _evse: u32) {}
}
