//! Hooks the actor loop and the modules call to reach each other.
//!
//! Each hook routes to the one module that implements it, so a module never needs to edit
//! another module's file.

use super::actor::{Station2Actor, TimerKey2};
use crate::store::StoreError;

impl Station2Actor {
    /// A timer fired.
    pub(super) fn on_timer(&mut self, key: TimerKey2) {
        match key {
            TimerKey2::Heartbeat => self.on_heartbeat_timer(),
            TimerKey2::Reconnect => self.on_reconnect_timer(),
            TimerKey2::BootRetry => self.on_boot_retry_timer(),
            TimerKey2::MeterPeriodic(evse) => self.on_meter_periodic(evse),
            TimerKey2::MeterAligned => self.on_meter_aligned(),
            TimerKey2::TxEndedSample(evse) => self.on_tx_ended_sample(evse),
            TimerKey2::TxEndedAligned(evse) => self.on_tx_ended_aligned(evse),
            TimerKey2::TxRetry => self.on_tx_retry_timer(),
            TimerKey2::EvConnectTimeout(evse) => self.on_ev_connect_timeout(evse),
            TimerKey2::ResetDelay => self.on_reset_timer(),
            TimerKey2::ReportChunk => self.on_report_chunk(),
            TimerKey2::ReservationExpiry => self.on_reservation_timer(),
            TimerKey2::ProfileReport => self.on_profile_report_timer(),
            TimerKey2::PersistFlush => self.on_persist_flush(),
        }
    }

    /// A state file write finished.
    pub(super) fn on_saved(&mut self, result: Result<(), StoreError>) {
        self.on_store_saved(result);
    }

    /// Brings the energy of the simulated meters up to the current time. The loop calls it at
    /// the start of every iteration and again before publishing.
    pub(super) fn settle_meters(&mut self) {
        self.settle_tx_meters();
    }

    /// Refreshes the device-model rows of the snapshot. `publish` calls it before it compares
    /// the live model with the published one.
    pub(super) fn refresh_variables(&mut self) {
        self.refresh_variable_views();
    }
}
