//! MeterValues: periodic and clock-aligned samplers, the `MeterValues` command.
//!
//! TS reference: `meter-value-scheduler.ts`, `sendMeterValues` in `src/station.ts`.
//!
//! * `ConnectorSnapshot::meter_wh` is the energy register. It follows the power model
//!   (`power.rs`: energy is the integral of the charging power); sampling only flushes the
//!   integrated energy and reads the numbers, so periodic and clock-aligned samples cannot
//!   double count. The `MeterValues` command additionally adds its `wh_delta` to the register.
//! * Periodic timers run per connector with a running transaction (`MeterValueSampleInterval`
//!   applies for the whole transaction, also while it is suspended: the energy values just do
//!   not advance), **also while offline**. A sample of a transaction that cannot be reported yet
//!   (offline, or still on a local id) goes to the bounded, persisted sample queue
//!   (`offline.rs`) and is sent after the start of its transaction was synchronised, before
//!   its stop; idle-connector samples are dropped.
//! * The clock-aligned timer fires at the next multiple of `ClockAlignedDataInterval` and
//!   samples every connector, idle ones too (without `transactionId`).
//! * Timer-driven samples in flight are bounded (at least [`MAX_PENDING_SAMPLES`], and never
//!   fewer than the number of connectors, so one clock-aligned burst fits): a silent CSMS and a
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
use super::metering::{clock_aligned_delay, meter_value};
use super::offline::MAX_QUEUED_SAMPLES;
use super::transaction::TransactionState;
use crate::error::{CallError, StationError};
use crate::ids::ConnectorId;
use crate::ocpp::messages::{MeterValues, MeterValuesRequest};
use crate::ocpp::types::{Measurand, ReadingContext};
use crate::station::config_registry::{METER_VALUES_ALIGNED_DATA, METER_VALUES_SAMPLED_DATA};

/// Most timer-driven `MeterValues` calls that may await an answer at the same time.
pub(crate) const MAX_PENDING_SAMPLES: usize = 16;

impl StationActor {
    fn is_sampling(&self, id: ConnectorId) -> bool {
        self.find_connector(id).is_some_and(|c| {
            matches!(
                c.transaction,
                TransactionState::PendingStart(_) | TransactionState::Active(_)
            )
        })
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
        self.sample_meter(id, ReadingContext::SamplePeriodic, 0, &measurands, None);
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
        self.flush_meters();
        let tx = {
            let Some(connector) = self.find_connector_mut(id) else {
                if let Some(reply) = reply {
                    let _ = reply.send(Err(StationError::UnknownConnector(id)));
                }
                return;
            };
            connector.meter_wh = connector.meter_wh.saturating_add(delta_wh);
            connector.tx_id()
        };
        if delta_wh > 0 {
            self.mark_dirty();
        }
        let sendable = self.can_talk() && !tx.is_some_and(crate::ids::TxId::is_local);
        if !sendable {
            // Transaction-related samples wait for their turn: a real id while offline, a local
            // id while the start sits in the offline queue (an online start still in flight has
            // no entry; its samples are dropped).
            let queued_for = tx.filter(|t| {
                if t.is_local() {
                    self.queue.entries().iter().any(|e| e.tx_id == *t)
                } else {
                    !self.can_talk()
                }
            });
            if let Some(tx) = queued_for {
                let reading = self.meter_reading(id, true);
                let sample = meter_value(&self.power_model(), &reading, now, measurands, context);
                if self.samples.push(id, tx, sample).is_some() {
                    self.log(
                        LogLevel::Warn,
                        format!(
                            "offline MeterValues queue is full ({MAX_QUEUED_SAMPLES} samples): dropped the oldest sample"
                        ),
                    );
                }
                self.mark_dirty();
                self.log(
                    LogLevel::Info,
                    format!("MeterValues for connector {id} queued until the transaction is synchronised"),
                );
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(Outcome::Done));
                }
                self.check_energy_limit(id);
                return;
            }
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
        let reading = self.meter_reading(id, true);
        let sample = meter_value(&self.power_model(), &reading, now, measurands, context);
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

    /// Sends the queued samples whose transaction has a real id (all of them, or those of
    /// `only`). The connection serializes CALLs, so they reach the CSMS in the order taken and
    /// ahead of anything sent afterwards, e.g. the `StopTransaction` of the same transaction.
    /// A sample stays queued (and persisted) until its CALL ended; a lost link keeps it.
    pub(super) fn flush_offline_samples(&mut self, only: Option<(ConnectorId, crate::ids::TxId)>) {
        if !self.can_talk() {
            return;
        }
        for queued in self.samples.take_ready(only) {
            let Some(wire) = queued.tx_id.wire() else {
                continue;
            };
            let request = MeterValuesRequest {
                connector_id: queued.connector,
                transaction_id: Some(wire),
                meter_value: vec![queued.sample.clone()],
            };
            let sample_id = queued.id;
            self.call::<MeterValues>(Origin::Station, &request, move |actor, result| {
                let keep = matches!(
                    result,
                    Err(CallError::NotConnected
                        | CallError::Closed { .. }
                        | CallError::NotRegistered { .. })
                );
                if let (false, Err(error)) = (keep, &result) {
                    actor.log(
                        LogLevel::Warn,
                        format!("queued MeterValues dropped: {error}"),
                    );
                }
                actor.samples.finish(sample_id, keep);
                actor.mark_dirty();
            });
        }
    }

    /// Abandons the oldest in-flight timer-driven samples until one more fits.
    ///
    /// Only timer-driven samples live in `sample_calls` (commands and offline flushes are
    /// never abandoned), so aborting a token here cannot cut a command's reply short.
    fn make_room_for_sample(&mut self) {
        self.sample_calls.retain(|t| self.calls.contains_key(t));
        let limit = MAX_PENDING_SAMPLES.max(self.state.connectors.len());
        while self.sample_calls.len() >= limit {
            let Some(call) = self
                .sample_calls
                .pop_front()
                .and_then(|token| self.calls.remove(&token))
            else {
                continue;
            };
            debug_assert_eq!(call.action, "MeterValues");
            call.task.abort();
            if !self.sample_overflow_warned {
                self.sample_overflow_warned = true;
                self.log(
                    LogLevel::Warn,
                    format!(
                        "CSMS is not answering MeterValues; dropping the oldest samples \
                         (keeping at most {limit} in flight)"
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
