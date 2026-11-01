//! MeterValues: periodic and clock-aligned samplers, the `MeterValues` command.
//!
//! TS reference: `meter-value-scheduler.ts`, `sendMeterValues` in `src/station.ts`.
//!
//! * `ConnectorSnapshot::meter_wh` is the energy source. A periodic tick adds
//!   `periodic_meter_delta_wh(interval)` once (TS parity); clock-aligned samples add nothing,
//!   which removes the TS double count. The sample itself is built by a transient
//!   `SimulatedMeter` at that energy.
//! * Periodic timers run per connector with a `Charging` transaction, **also while offline**:
//!   the meter keeps advancing, samples are dropped (nothing is queued). A transaction that
//!   is still local (`PendingStart`) is not sampled either (it has no CSMS id yet).
//! * The clock-aligned timer fires at the next multiple of `ClockAlignedDataInterval` and
//!   samples every connector, idle ones too (without `transactionId`).
//! * Timer-driven samples in flight are bounded ([`MAX_PENDING_SAMPLES`]): a silent CSMS and a
//!   tiny interval must not pile up calls. When the limit is hit the oldest sample is
//!   abandoned (its task is aborted; a queued CALL is withdrawn from the connection) and one
//!   warning per burst is logged; the next answered sample re-arms the warning.
//! * After every meter advance `MaxEnergyOnInvalidId` is checked.

use std::time::Duration;

use super::actor::{StationActor, TimerKey};
use super::calls::{Origin, Reply};
use super::command::Outcome;
use super::commands::call_failure;
use super::event::LogLevel;
use super::metering::{SimulatedMeter, clock_aligned_delay, periodic_meter_delta_wh};
use crate::error::StationError;
use crate::ids::ConnectorId;
use crate::ocpp::messages::{MeterValues, MeterValuesRequest};
use crate::ocpp::types::{ConnectorStatus, Measurand, ReadingContext};
use crate::station::config_registry::{METER_VALUES_ALIGNED_DATA, METER_VALUES_SAMPLED_DATA};

/// Most timer-driven `MeterValues` calls that may await an answer at the same time.
pub(crate) const MAX_PENDING_SAMPLES: usize = 16;

impl StationActor {
    fn is_sampling(&self, id: ConnectorId) -> bool {
        self.find_connector(id)
            .is_some_and(|c| !c.transaction.is_idle() && c.status == ConnectorStatus::Charging)
    }

    /// (Re)arms the periodic timers of charging connectors and the aligned timer. Timers that
    /// are already armed keep their phase; cancel them first to apply a new interval.
    pub(super) fn reschedule_metering(&mut self) {
        let interval = self.registry.meter_value_sample_interval_secs();
        let ids: Vec<ConnectorId> = self.state.connectors.iter().map(|c| c.id).collect();
        for id in ids {
            if interval > 0 && self.is_sampling(id) {
                self.timers.set_if_idle(
                    TimerKey::MeterPeriodic(id),
                    Duration::from_secs(u64::from(interval)),
                );
            } else {
                self.timers.cancel(&TimerKey::MeterPeriodic(id));
            }
        }
        let aligned = self.registry.clock_aligned_data_interval_secs();
        match clock_aligned_delay(self.clock.now_utc(), aligned).and_then(|d| d.to_std().ok()) {
            Some(delay) => {
                self.timers.set_if_idle(TimerKey::MeterAligned, delay);
            }
            None => {
                self.timers.cancel(&TimerKey::MeterAligned);
            }
        }
    }

    /// `MeterValueSampleInterval` / `ClockAlignedDataInterval` changed.
    pub(super) fn restart_metering(&mut self) {
        self.timers
            .cancel_where(|k| matches!(k, TimerKey::MeterPeriodic(_) | TimerKey::MeterAligned));
        self.reschedule_metering();
    }

    pub(super) fn on_meter_periodic(&mut self, id: ConnectorId) {
        if !self.is_sampling(id) {
            return;
        }
        let interval = self.registry.meter_value_sample_interval_secs();
        if interval == 0 {
            return;
        }
        self.timers.set(
            TimerKey::MeterPeriodic(id),
            Duration::from_secs(u64::from(interval)),
        );
        let measurands = self.registry.measurands(METER_VALUES_SAMPLED_DATA);
        self.sample_meter(
            id,
            ReadingContext::SamplePeriodic,
            periodic_meter_delta_wh(interval),
            &measurands,
            None,
        );
    }

    pub(super) fn on_meter_aligned(&mut self) {
        let aligned = self.registry.clock_aligned_data_interval_secs();
        if let Some(delay) =
            clock_aligned_delay(self.clock.now_utc(), aligned).and_then(|d| d.to_std().ok())
        {
            self.timers.set(TimerKey::MeterAligned, delay);
        }
        let measurands = self.registry.measurands(METER_VALUES_ALIGNED_DATA);
        let ids: Vec<ConnectorId> = self.state.connectors.iter().map(|c| c.id).collect();
        for id in ids {
            self.sample_meter(id, ReadingContext::SampleClock, 0, &measurands, None);
        }
    }

    /// Advances the meter of `id` by `delta_wh` and sends one sample, if it can be sent.
    pub(super) fn sample_meter(
        &mut self,
        id: ConnectorId,
        context: ReadingContext,
        delta_wh: u64,
        measurands: &[Measurand],
        reply: Option<Reply>,
    ) {
        let now = self.clock.now_utc();
        let (energy_wh, status, tx) = {
            let Some(connector) = self.find_connector_mut(id) else {
                if let Some(reply) = reply {
                    let _ = reply.send(Err(StationError::UnknownConnector(id)));
                }
                return;
            };
            connector.meter_wh = connector.meter_wh.saturating_add(delta_wh);
            (connector.meter_wh, connector.status, connector.tx_id())
        };
        if delta_wh > 0 {
            self.mark_dirty();
        }
        let sendable = self.can_talk() && !tx.is_some_and(crate::ids::TxId::is_local);
        if !sendable {
            self.log(
                LogLevel::Info,
                format!("MeterValues for connector {id} not sent (offline or local transaction)"),
            );
            if let Some(reply) = reply {
                let _ = reply.send(Ok(Outcome::Done));
            }
            self.check_energy_limit(id);
            return;
        }
        #[allow(
            clippy::cast_precision_loss,
            reason = "Wh counters stay far below 2^53"
        )]
        let mut meter = SimulatedMeter::with_energy_wh(energy_wh as f64);
        let sample = meter.sample(now, status, measurands, context);
        let request = MeterValuesRequest {
            connector_id: id,
            transaction_id: tx.and_then(crate::ids::TxId::wire),
            meter_value: vec![sample],
        };
        let timer_driven = reply.is_none();
        if timer_driven {
            self.make_room_for_sample();
        }
        let token = self.next_call_token;
        self.call::<MeterValues>(Origin::Station, &request, move |actor, result| {
            if result.is_ok() {
                actor.sample_overflow_warned = false;
            }
            if let Some(reply) = reply {
                let _ = reply.send(result.map(|_| Outcome::Done).map_err(call_failure));
            }
            actor.check_energy_limit(id);
        });
        if timer_driven && self.calls.contains_key(&token) {
            self.sample_calls.push_back(token);
        }
    }

    /// Abandons the oldest in-flight timer-driven samples until one more fits.
    fn make_room_for_sample(&mut self) {
        self.sample_calls.retain(|t| self.calls.contains_key(t));
        while self.sample_calls.len() >= MAX_PENDING_SAMPLES {
            let Some(call) = self
                .sample_calls
                .pop_front()
                .and_then(|token| self.calls.remove(&token))
            else {
                continue;
            };
            call.task.abort();
            if !self.sample_overflow_warned {
                self.sample_overflow_warned = true;
                self.log(
                    LogLevel::Warn,
                    format!(
                        "CSMS is not answering MeterValues; dropping the oldest samples \
                         (keeping at most {MAX_PENDING_SAMPLES} in flight)"
                    ),
                );
            }
        }
    }

    /// The `MeterValues` command.
    pub(super) fn meter_values_command(
        &mut self,
        id: ConnectorId,
        wh_delta: u64,
        measurands: Option<Vec<Measurand>>,
        reply: Reply,
    ) {
        if self.connector(id).is_err() {
            let _ = reply.send(Err(StationError::UnknownConnector(id)));
            return;
        }
        let measurands = measurands
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| self.registry.measurands(METER_VALUES_SAMPLED_DATA));
        self.sample_meter(
            id,
            ReadingContext::SamplePeriodic,
            wh_delta,
            &measurands,
            Some(reply),
        );
    }
}
