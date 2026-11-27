//! Hooks the actor loop and the modules call to reach each other.
//!
//! Each hook routes to the one module that implements it, so a module never needs to edit
//! another module's file.

use super::actor::{Station2Actor, TimerKey2};
use crate::ocpp::v2::types::WireEnum;
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
            TimerKey2::TxLimit(evse) => self.check_tx_limits(evse),
            TimerKey2::RunningCost(evse) => self.on_running_cost(evse),
            TimerKey2::DefaultTariffActivation => self.on_tariff_activation(),
            TimerKey2::EvConnectTimeout(evse) => self.on_ev_connect_timeout(evse),
            TimerKey2::ResetDelay => self.on_reset_timer(),
            TimerKey2::ReportChunk => self.on_report_chunk(),
            TimerKey2::ReservationExpiry => self.on_reservation_timer(),
            TimerKey2::ProfileReport => self.on_profile_report_timer(),
            TimerKey2::DynamicProfiles => self.on_dynamic_profiles_timer(),
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

    /// Refreshes the reservation, limit and list rows of the snapshot from the live state.
    fn refresh_evse_views(&mut self) {
        self.prune_default_tariffs();
        let views: Vec<_> = self
            .state
            .evses
            .iter()
            .map(|e| {
                let (offered, external) = self.sc_view(e.id);
                let tx = self.active_tx(e.id);
                let limit = tx.map(|tx| tx.limit).filter(|limit| !limit.is_empty());
                let reached = tx
                    .and_then(|tx| tx.limit_reached)
                    .map(|kind| kind.trigger().as_str().to_owned());
                let cost = tx.and_then(|tx| tx.total_cost);
                let cost_currency = tx.and_then(|tx| tx.cost_currency.clone());
                let tariff = tx
                    .and_then(|tx| tx.tariff.as_ref())
                    .map(|state| state.current().id.clone());
                let default = self.default_tariff(e.id).map(|tariff| tariff.id.clone());
                let priority = tx.is_some_and(|tx| tx.priority_charging);
                (
                    self.reservation_view(e.id),
                    offered,
                    external,
                    limit,
                    reached,
                    cost,
                    cost_currency,
                    tariff,
                    default,
                    priority,
                )
            })
            .collect();
        for (
            entry,
            (
                reservation,
                offered,
                external,
                limit,
                reached,
                cost,
                currency,
                tariff,
                default,
                priority,
            ),
        ) in self.state.evses.iter_mut().zip(views)
        {
            entry.reservation = reservation;
            entry.offered_w = offered;
            entry.external_limit = external;
            entry.transaction_limit = limit;
            entry.limit_reached = reached;
            entry.total_cost = cost;
            entry.cost_currency = currency;
            entry.tariff_id = tariff;
            entry.default_tariff_id = default;
            entry.priority_charging = priority;
        }
        self.state.local_list_version = self.local_list.version;
        self.state.local_list_entries = self.local_list_len();
        self.state.charging_profiles = self.sc_profile_count();
    }

    /// Refreshes the device-model rows of the snapshot. `publish` calls it before it compares
    /// the live model with the published one.
    pub(super) fn refresh_variables(&mut self) {
        self.refresh_variable_views();
        self.refresh_evse_views();
    }
}
