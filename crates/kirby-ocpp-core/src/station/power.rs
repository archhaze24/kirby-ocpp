//! The actor side of the power model: integrates the energy of charging connectors.
//!
//! The pure parts live in [`super::metering`] (readings) and [`super::smart_charging`]
//! (limit and energy over a time span). Here the actor keeps, per connector, when it last
//! looked and the energy not yet added to `meter_wh`.
//!
//! * [`StationActor::settle_meters`] runs at the start of every actor loop iteration, before
//!   anything mutates the state, and once more after the handler, before the snapshot is
//!   published (it is idempotent): the second call records the status the handler left, so
//!   the next span is not integrated with a stale `Charging` flag. The state does not change
//!   between iterations, so the status and the profiles it sees held for the whole elapsed time. Elapsed time is
//!   measured with the monotonic clock; the wall-clock start of the span is only used to
//!   find the active schedule periods, which are integrated piecewise.
//! * [`StationActor::flush_meters`] moves the accumulated energy into the snapshot's
//!   `meter_wh` (whole Wh, the remainder is carried). Everything that reads `meter_wh` for
//!   the wire (samples, Start/StopTransaction, persistence) flushes first; the loop itself
//!   does not touch the snapshot, so an idle wake-up never produces a state change.

use chrono::{DateTime, Utc};
use tokio::time::Instant;

use super::actor::StationActor;
use super::metering::{MeterReading, PowerModel};
use super::smart_charging::{PowerRequest, energy_wh, limit_w_at};
use super::transaction::TransactionState;
use crate::ids::{ConnectorId, TxId};
use crate::ocpp::types::ConnectorStatus;

/// Per-connector integration state.
#[derive(Debug, Clone)]
pub(crate) struct MeterRuntime {
    last: Instant,
    last_utc: DateTime<Utc>,
    /// Status was `Charging` at `last`.
    charging: bool,
    /// Energy integrated since the last flush, in Wh.
    pending_wh: f64,
    /// Sub-Wh remainder carried between flushes (below 1).
    carry_wh: f64,
    /// Register (Wh) at the previous sample: base of the interval measurand.
    sample_base_wh: f64,
}

fn transaction_context(state: &TransactionState) -> (Option<TxId>, Option<DateTime<Utc>>) {
    match state {
        TransactionState::Active(a) => (Some(a.tx_id), Some(a.started_at)),
        TransactionState::PendingStart(p) => (Some(p.tx_id), Some(p.started_at)),
        _ => (None, None),
    }
}

impl StationActor {
    pub(super) fn power_model(&self) -> PowerModel {
        PowerModel::from_config(&self.config)
    }

    fn charging_connectors(&self) -> u32 {
        let count = self
            .state
            .connectors
            .iter()
            .filter(|c| c.status == ConnectorStatus::Charging)
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    fn power_request(&self, id: ConnectorId) -> PowerRequest {
        let (active_tx, tx_started_at) = self
            .find_connector(id)
            .map_or((None, None), |c| transaction_context(&c.transaction));
        PowerRequest {
            connector: id,
            active_tx,
            tx_started_at,
            model: self.power_model(),
            charging_connectors: self.charging_connectors(),
        }
    }

    /// Integrates the energy of every connector up to now (see the module docs).
    pub(super) fn settle_meters(&mut self) {
        let now = Instant::now();
        let now_utc = self.clock.now_utc();
        let ids: Vec<(ConnectorId, bool)> = self
            .state
            .connectors
            .iter()
            .map(|c| (c.id, c.status == ConnectorStatus::Charging))
            .collect();
        for (id, charging) in ids {
            let register = self.find_connector(id).map_or(0.0, |c| wh_f64(c.meter_wh));
            let Some(runtime) = self.meters.get(&id).cloned() else {
                self.meters.insert(
                    id,
                    MeterRuntime {
                        last: now,
                        last_utc: now_utc,
                        charging,
                        pending_wh: 0.0,
                        carry_wh: 0.0,
                        sample_base_wh: register,
                    },
                );
                continue;
            };
            let mut runtime = runtime;
            let elapsed = now.saturating_duration_since(runtime.last);
            if runtime.charging && !elapsed.is_zero() {
                let from = runtime.last_utc;
                let to = chrono::Duration::from_std(elapsed)
                    .ok()
                    .and_then(|d| from.checked_add_signed(d))
                    .unwrap_or(from);
                let request = self.power_request(id);
                runtime.pending_wh += energy_wh(self.profiles.profiles(), &request, from, to);
            }
            if charging && !runtime.charging {
                runtime.sample_base_wh = register + runtime.carry_wh + runtime.pending_wh;
            }
            runtime.last = now;
            runtime.last_utc = now_utc;
            runtime.charging = charging;
            self.meters.insert(id, runtime);
        }
    }

    /// Settles, then adds the integrated energy to the connectors' `meter_wh`.
    pub(super) fn flush_meters(&mut self) {
        self.settle_meters();
        let mut changed = false;
        for connector in &mut self.state.connectors {
            let Some(runtime) = self.meters.get_mut(&connector.id) else {
                continue;
            };
            if runtime.pending_wh <= 0.0 {
                continue;
            }
            let total = runtime.carry_wh + runtime.pending_wh;
            let whole = total.floor();
            runtime.pending_wh = 0.0;
            runtime.carry_wh = total - whole;
            if whole >= 1.0 {
                connector.meter_wh = connector.meter_wh.saturating_add(floor_u64(whole));
                changed = true;
            }
        }
        if changed {
            self.mark_dirty();
        }
    }

    /// The reading of connector `id` (`0` = the whole station, sums over connectors) after
    /// a flush. With `advance_interval` the interval base moves to the current register.
    pub(super) fn meter_reading(
        &mut self,
        id: ConnectorId,
        advance_interval: bool,
    ) -> MeterReading {
        self.flush_meters();
        let now = self.clock.now_utc();
        let ids: Vec<ConnectorId> = if id.is_station() {
            self.state.connectors.iter().map(|c| c.id).collect()
        } else {
            vec![id]
        };
        let model = self.power_model();
        let mut reading = MeterReading {
            energy_wh: 0.0,
            charging: false,
            power_w: 0.0,
            offered_w: 0.0,
            interval_wh: 0.0,
        };
        for connector_id in ids {
            let Some(connector) = self.find_connector(connector_id) else {
                continue;
            };
            let register = wh_f64(connector.meter_wh);
            let charging = connector.status == ConnectorStatus::Charging;
            let request = self.power_request(connector_id);
            let offered = model.effective_w(limit_w_at(self.profiles.profiles(), &request, now));
            let base = self
                .meters
                .get(&connector_id)
                .map_or(register, |r| r.sample_base_wh);
            reading.energy_wh += register;
            reading.charging |= charging;
            reading.offered_w += offered;
            if charging {
                reading.power_w += offered;
            }
            reading.interval_wh += (register - base).max(0.0);
            if advance_interval && let Some(runtime) = self.meters.get_mut(&connector_id) {
                runtime.sample_base_wh = register;
            }
        }
        reading
    }
}

/// Wh counters stay far below 2^53.
#[allow(
    clippy::cast_precision_loss,
    reason = "Wh counters stay far below 2^53"
)]
fn wh_f64(wh: u64) -> f64 {
    wh as f64
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is a non-negative whole number of Wh far below u64::MAX"
)]
fn floor_u64(value: f64) -> u64 {
    value as u64
}
