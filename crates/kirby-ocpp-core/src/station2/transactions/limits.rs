//! OCPP 2.1 transaction limits and CSMS cost updates.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{ActiveTx, EventExtra};
use crate::ocpp::Protocol;
use crate::ocpp::v2::messages::{TransactionEventKind, TriggerReason};
use crate::ocpp::v2::types::{ChargingState, StopReason};
use crate::station::LogLevel;
use crate::station2::actor::{Station2Actor, TimerKey2};
use crate::station2::command::Outcome2;
use crate::station2::types::StartStopPoint;

/// Energy, duration and cost limits of an OCPP 2.1 transaction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct TransactionLimit2 {
    pub max_energy_wh: Option<f64>,
    pub max_time_s: Option<i64>,
    pub max_cost: Option<f64>,
}

impl TransactionLimit2 {
    pub(in crate::station2) fn is_empty(self) -> bool {
        self == Self::default()
    }

    fn from_wire(value: &Value) -> Self {
        Self {
            max_energy_wh: value.get("maxEnergy").and_then(Value::as_f64),
            max_time_s: value.get("maxTime").and_then(time_seconds),
            max_cost: value.get("maxCost").and_then(Value::as_f64),
        }
    }

    pub(super) fn to_wire(self) -> Value {
        Value::Object(Map::from_iter(
            [
                ("maxEnergy", self.max_energy_wh.map(Value::from)),
                ("maxTime", self.max_time_s.map(Value::from)),
                ("maxCost", self.max_cost.map(Value::from)),
            ]
            .into_iter()
            .filter_map(|(key, value)| Some((key.to_owned(), value?))),
        ))
    }

    fn with(self, update: Self) -> Self {
        Self {
            max_energy_wh: update.max_energy_wh.or(self.max_energy_wh),
            max_time_s: update.max_time_s.or(self.max_time_s),
            max_cost: update.max_cost.or(self.max_cost),
        }
    }

    fn exceeds(self, cap: Self) -> bool {
        fn exceeds<T: PartialOrd>(value: Option<T>, cap: Option<T>) -> bool {
            value.zip(cap).is_some_and(|(value, cap)| value > cap)
        }
        exceeds(self.max_energy_wh, cap.max_energy_wh)
            || exceeds(self.max_time_s, cap.max_time_s)
            || exceeds(self.max_cost, cap.max_cost)
    }
}

/// JSON Schema also accepts integral decimal values such as 60.0 as integers.
fn time_seconds(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        let seconds = value.as_f64()?;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the schema requires an integer; values outside i64 saturate"
        )]
        let seconds = seconds as i64;
        Some(seconds)
    })
}

/// The limit that suspended energy transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LimitKind {
    Energy,
    Time,
    Cost,
}

impl LimitKind {
    pub(in crate::station2) fn trigger(self) -> TriggerReason {
        match self {
            Self::Energy => TriggerReason::EnergyLimitReached,
            Self::Time => TriggerReason::TimeLimitReached,
            Self::Cost => TriggerReason::CostLimitReached,
        }
    }

    fn stop_reason(self) -> StopReason {
        match self {
            Self::Energy => StopReason::EnergyLimitReached,
            Self::Time => StopReason::TimeLimitReached,
            Self::Cost => StopReason::LocalOutOfCredit,
        }
    }
}

impl ActiveTx {
    pub(in crate::station2) fn time_limit_reached(&self, now: DateTime<Utc>) -> bool {
        self.limit
            .max_time_s
            .is_some_and(|max| max <= 0 || (now - self.started_at).num_seconds() >= max)
    }

    fn reached_limit(&self, now: DateTime<Utc>) -> Option<LimitKind> {
        if self.time_limit_reached(now) {
            Some(LimitKind::Time)
        } else if self
            .limit
            .max_energy_wh
            .is_some_and(|max| max <= 0.0 || self.energy_wh >= max)
        {
            Some(LimitKind::Energy)
        } else if self
            .limit
            .max_cost
            .is_some_and(|max| max <= 0.0 || self.total_cost.is_some_and(|cost| cost >= max))
        {
            Some(LimitKind::Cost)
        } else {
            None
        }
    }
}

impl Station2Actor {
    pub(in crate::station2) fn set_transaction_limit(
        &mut self,
        evse: u32,
        limit: TransactionLimit2,
    ) -> Outcome2 {
        let reject = |reason: &str| Outcome2::Rejected {
            reason: reason.to_owned(),
        };
        if self.protocol() != Protocol::V21 {
            return reject("transaction limits require OCPP 2.1");
        }
        let Some(tx) = self.active_tx(evse) else {
            return reject("no ongoing transaction");
        };
        if limit.is_empty() {
            return reject("no limit fields supplied");
        }
        if [limit.max_energy_wh, limit.max_cost]
            .into_iter()
            .flatten()
            .any(|value| !value.is_finite())
        {
            return reject("transaction limits must be finite");
        }
        if limit.exceeds(tx.csms_limit) {
            return reject("the limit exceeds the limit set by the CSMS");
        }
        self.tx_limit_set(evse, limit);
        Outcome2::Done
    }

    /// Applies the cost and limits from the answer to an event of an ongoing transaction.
    pub(in crate::station2) fn on_tx_event_limits(&mut self, evse: u32, id: &str, answer: &Value) {
        let v21 = self.protocol() == Protocol::V21;
        let Some(tx) = self.tx.active.get_mut(&evse).filter(|tx| tx.id.0 == id) else {
            return;
        };
        let mut cost = answer.get("totalCost").and_then(Value::as_f64);
        let ignored_cost = cost.is_some() && tx.has_local_cost();
        if ignored_cost {
            cost = None;
        }
        if let Some(cost) = cost {
            tx.total_cost = Some(cost);
        }
        let incoming = v21
            .then(|| {
                answer
                    .get("transactionLimit")
                    .map(TransactionLimit2::from_wire)
            })
            .flatten()
            .unwrap_or_default();
        let limit = tx.limit.with(incoming);
        let csms_limit = tx.csms_limit.with(incoming);
        let changed = tx.limit != limit;
        let dirty = cost.is_some() || tx.csms_limit != csms_limit;
        tx.csms_limit = csms_limit;
        if ignored_cost {
            self.log(
                LogLevel::Info,
                "TransactionEventResponse cost ignored during local cost calculation".to_owned(),
            );
        }
        if changed {
            self.tx_limit_set(evse, incoming);
        } else {
            if dirty {
                self.mark_dirty();
            }
            if v21 && (cost.is_some() || !incoming.is_empty()) {
                self.check_tx_limits(evse);
            }
        }
    }

    /// Records the latest cost for an ongoing transaction, ignoring unknown and finished ids.
    pub(in crate::station2) fn on_cost_updated(&mut self, id: &str, cost: f64) {
        let Some(tx) = self.tx.active.values_mut().find(|tx| tx.id.0 == id) else {
            return;
        };
        if tx.has_local_cost() {
            self.log(
                LogLevel::Info,
                "CostUpdated ignored during local cost calculation".to_owned(),
            );
            return;
        }
        tx.total_cost = Some(cost);
        let evse = tx.evse;
        self.mark_dirty();
        if self.protocol() == Protocol::V21 {
            self.check_tx_limits(evse);
        }
    }

    fn tx_limit_set(&mut self, evse: u32, fields: TransactionLimit2) {
        self.settle_tx_meters();
        let ready = self.connector_ready(evse);
        let now = self.clock.now_utc();
        let Some(tx) = self.tx.active.get_mut(&evse) else {
            return;
        };
        let limit = tx.limit.with(fields);
        let changed = tx.limit != limit;
        if tx.limit.max_time_s != limit.max_time_s {
            self.timers.cancel(&TimerKey2::TxLimit(evse));
        }
        tx.limit = limit;
        let may_resume = changed && tx.limit_reached.is_some() && tx.reached_limit(now).is_none();
        let charging_state = if may_resume {
            tx.limit_reached = None;
            let reset = self
                .reset
                .as_mut()
                .filter(|plan| plan.resumes_transaction(evse, &tx.id, self.auth.reset_prepared));
            let resumed = if tx.authorized && tx.ev_connected && (reset.is_some() || ready) {
                ChargingState::Charging
            } else if tx.ev_connected {
                ChargingState::EvConnected
            } else {
                ChargingState::Idle
            };
            if let Some(plan) = reset {
                plan.charging_before_reset
                    .insert(evse, (tx.id.clone(), resumed));
                None
            } else {
                tx.charging_state = resumed;
                Some(resumed)
            }
        } else {
            None
        };
        self.tx_event(
            evse,
            TransactionEventKind::Updated,
            TriggerReason::LimitSet,
            EventExtra {
                charging_state,
                transaction_limit: Some(fields),
                ..EventExtra::default()
            },
        );
        self.evaluate_tx_limits(evse, changed, false);
    }

    pub(super) fn arm_tx_limit(&mut self, evse: u32) {
        let timer = TimerKey2::TxLimit(evse);
        if self.protocol() == Protocol::V21
            && let Some(tx) = self.active_tx(evse)
        {
            let time = tx
                .limit
                .max_time_s
                .filter(|_| tx.limit_reached != Some(LimitKind::Time));
            let check_meter = tx.limit_reached.is_none()
                && ((tx.charging_state == ChargingState::Charging
                    && tx.limit.max_energy_wh.is_some())
                    || (tx.has_local_cost() && tx.limit.max_cost.is_some()));
            if time.is_some() || check_meter {
                let tick = Duration::from_secs(1);
                let delay = time.map_or(tick, |max| {
                    let elapsed = (self.clock.now_utc() - tx.started_at)
                        .to_std()
                        .unwrap_or_default();
                    let duration = Duration::from_secs(u64::try_from(max).unwrap_or_default());
                    tick.min(duration.saturating_sub(elapsed))
                });
                self.timers.set_if_idle(timer, delay);
                return;
            }
        }
        self.timers.cancel(&timer);
    }

    pub(in crate::station2) fn check_tx_limits(&mut self, evse: u32) {
        self.evaluate_tx_limits(evse, false, false);
    }

    pub(super) fn evaluate_tx_limits(&mut self, evse: u32, limit_changed: bool, quiet: bool) {
        if !quiet {
            self.settle_tx_meters();
        }
        let now = self.clock.now_utc();
        let Some(tx) = self.tx.active.get_mut(&evse) else {
            return;
        };
        let previous = tx.limit_reached;
        let Some(reached) = tx.reached_limit(now) else {
            return;
        };
        let new_time_limit = reached == LimitKind::Time && previous != Some(LimitKind::Time);
        if previous.is_some() && !limit_changed && !new_time_limit {
            return;
        }
        self.tx.meters.entry(evse).or_default().charging = false;
        tx.limit_reached = Some(reached);
        let reset_suspended = self.auth.reset_prepared
            && self
                .reset
                .as_ref()
                .is_some_and(|plan| plan.resumes_transaction(evse, &tx.id, true));
        if !reset_suspended {
            tx.charging_state = ChargingState::SuspendedEvse;
        }
        let charging_state = Some(tx.charging_state);
        if self
            .tx_stop_points()
            .contains(&StartStopPoint::EnergyTransfer)
        {
            if quiet {
                if let Err(error) =
                    self.end_core(evse, reached.stop_reason(), reached.trigger(), None, true)
                {
                    self.log(
                        LogLevel::Warn,
                        format!("restored transaction not ended: {error}"),
                    );
                }
            } else {
                self.finish(evse, reached.stop_reason(), reached.trigger(), None);
            }
        } else if previous.is_some() && !new_time_limit {
            self.mark_dirty();
        } else {
            let kind = TransactionEventKind::Updated;
            let trigger = reached.trigger();
            let extra = EventExtra {
                charging_state,
                ..EventExtra::default()
            };
            if quiet {
                if let Some(event) = self.build_event(evse, kind, trigger, extra) {
                    self.queue_enqueue(event);
                }
            } else {
                self.tx_event(evse, kind, trigger, extra);
            }
        }
    }
}
