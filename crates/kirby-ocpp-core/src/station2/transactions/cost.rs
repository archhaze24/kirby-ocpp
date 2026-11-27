//! Local cost calculation from the usage recorded under each tariff.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{ActiveTx, EventExtra, Station2Actor, TimerKey2, TransactionEventKind, TriggerReason};
use crate::station2::device_model::TARIFF;
use crate::station2::tariff::{AuthorizationTariff, PRICE_FIELDS, Tariff};
use crate::station2::types::IdTokenRef;

const PERIOD_LIMIT: usize = 100;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Usage {
    energy_wh: f64,
    charging: Duration,
    idle: Duration,
}

impl Usage {
    fn add(&mut self, wh: f64, charging: Duration, idle: Duration) {
        self.energy_wh += wh;
        self.charging += charging;
        self.idle += idle;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Period {
    tariff: Tariff,
    start: DateTime<Utc>,
    usage: Usage,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
struct Money {
    excl: f64,
    incl: f64,
}

impl Money {
    fn add(&mut self, other: Self) {
        self.excl = (self.excl + other.excl).min(f64::MAX);
        self.incl = (self.incl + other.incl).min(f64::MAX);
    }

    fn round(self) -> Self {
        Self {
            excl: round(self.excl, 10_000.0),
            incl: round(self.incl, 10_000.0),
        }
    }

    fn scaled(self, ratio: f64) -> Self {
        Self {
            excl: self.excl * ratio,
            incl: self.incl * ratio,
        }
        .round()
    }

    fn wire(self) -> Value {
        json!({"exclTax": self.excl, "inclTax": self.incl})
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Totals {
    components: [Option<Money>; PRICE_FIELDS.len()],
    total: Money,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct StartFees {
    reserved: bool,
    reservation_seconds: Option<u64>,
}

/// Tariff and accumulated usage of a transaction, including earlier tariff periods.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransactionTariff {
    previous: Vec<Period>,
    current: Period,
    driver_token: Option<IdTokenRef>,
    changed_by_csms: bool,
    local_cost: bool,
    announce: bool,
    start_fees: Option<StartFees>,
    frozen: Option<Totals>,
}

fn round(value: f64, factor: f64) -> f64 {
    let scaled = value * factor;
    if scaled.is_finite() {
        scaled.round() / factor
    } else {
        value
    }
}

fn seconds_for_cost(seconds: u64) -> f64 {
    Duration::from_secs(seconds).as_secs_f64()
}

impl TransactionTariff {
    fn new(
        tariff: Tariff,
        start: DateTime<Utc>,
        local_cost: bool,
        driver_token: Option<IdTokenRef>,
    ) -> Self {
        Self {
            previous: Vec::new(),
            current: Period {
                tariff,
                start,
                usage: Usage::default(),
            },
            driver_token,
            changed_by_csms: false,
            local_cost,
            announce: true,
            start_fees: None,
            frozen: None,
        }
    }

    pub(in crate::station2) fn current(&self) -> &Tariff {
        &self.current.tariff
    }

    pub(in crate::station2) fn is_driver(&self) -> bool {
        self.changed_by_csms || self.driver_token.is_some()
    }

    pub(in crate::station2) fn driver_binding(&self) -> Option<(&IdTokenRef, &str)> {
        let first = self.previous.first().unwrap_or(&self.current);
        Some((self.driver_token.as_ref()?, &first.tariff.id))
    }

    pub(in crate::station2) fn periods_full(&self) -> bool {
        self.previous.len() + 1 >= PERIOD_LIMIT
    }

    fn change(&mut self, tariff: Tariff, now: DateTime<Utc>) {
        let period = Period {
            tariff,
            start: now,
            usage: Usage::default(),
        };
        self.previous
            .push(std::mem::replace(&mut self.current, period));
        self.changed_by_csms = true;
        self.announce = true;
    }

    fn periods(&self) -> impl Iterator<Item = &Period> {
        self.previous.iter().chain(std::iter::once(&self.current))
    }

    fn totals(&self) -> Totals {
        let mut components: [Option<Money>; PRICE_FIELDS.len()] = Default::default();
        for (index, period) in self.periods().enumerate() {
            let usage = &period.usage;
            let start_fees = self.start_fees.as_ref().filter(|_| index == 0);
            let volumes = [
                round(usage.energy_wh, 1000.0) / 1000.0,
                seconds_for_cost(usage.charging.as_secs()) / 60.0,
                seconds_for_cost(usage.idle.as_secs()) / 60.0,
                f64::from(start_fees.is_some()),
                seconds_for_cost(
                    start_fees
                        .and_then(|fees| fees.reservation_seconds)
                        .unwrap_or(0),
                ) / 60.0,
                f64::from(start_fees.is_some_and(|fees| fees.reserved)),
            ];
            for ((price, volume), component) in period
                .tariff
                .prices
                .iter()
                .zip(volumes)
                .zip(&mut components)
            {
                if let Some(price) = price {
                    let excl = (price.amount * volume).min(f64::MAX);
                    let incl = (excl * price.tax_factor).min(f64::MAX);
                    component
                        .get_or_insert_with(Money::default)
                        .add(Money { excl, incl });
                }
            }
        }
        let mut total = Money::default();
        for component in components.iter_mut().flatten() {
            *component = component.round();
            total.add(*component);
        }
        Totals {
            components,
            total: total.round(),
        }
    }

    fn evaluate(&mut self) -> f64 {
        if let Some(frozen) = &self.frozen {
            return frozen.total.incl;
        }
        let mut totals = self.totals();
        let max = self.current().max_cost;
        let cap_ratio = |limit: Option<f64>, total: f64| {
            let limit = limit.filter(|limit| total >= *limit)?;
            Some(if total > 0.0 { limit / total } else { 1.0 })
        };
        let excl = cap_ratio(max.excl_tax, totals.total.excl);
        let incl = cap_ratio(max.incl_tax, totals.total.incl);
        if let Some(ratio) = excl.into_iter().chain(incl).reduce(f64::min) {
            for amount in totals.components.iter_mut().flatten() {
                *amount = amount.scaled(ratio);
            }
            totals.total = totals.total.scaled(ratio);
            if excl.is_some_and(|excl| excl <= ratio) {
                totals.total.excl = max.excl_tax.unwrap_or(totals.total.excl);
            }
            if incl.is_some_and(|incl| incl <= ratio) {
                totals.total.incl = max.incl_tax.unwrap_or(totals.total.incl);
            }
            let total = totals.total.incl;
            self.frozen = Some(totals);
            return total;
        }
        totals.total.incl
    }

    fn total_cost(&self, ended: bool) -> Value {
        let totals = self.frozen.clone().unwrap_or_else(|| self.totals());
        let min = self.current().min_cost;
        let currency = &self.current().currency;
        if ended
            && (min.excl_tax.is_some_and(|bound| totals.total.excl < bound)
                || min.incl_tax.is_some_and(|bound| totals.total.incl < bound))
        {
            let money = Money {
                excl: min.excl_tax.or(min.incl_tax).unwrap_or_default(),
                incl: min.incl_tax.or(min.excl_tax).unwrap_or_default(),
            }
            .round()
            .wire();
            return json!({"currency": currency, "typeOfCost": "MinCost", "fixed": money, "total": money});
        }
        let kind = if self.frozen.is_some() {
            "MaxCost"
        } else {
            "NormalCost"
        };
        let mut cost = Map::from_iter([
            ("currency".to_owned(), json!(currency)),
            ("typeOfCost".to_owned(), json!(kind)),
            ("total".to_owned(), totals.total.wire()),
        ]);
        for (((_, _, name), amount), current) in PRICE_FIELDS
            .into_iter()
            .zip(totals.components)
            .zip(&self.current().prices)
        {
            if let Some(amount) = amount {
                let mut price = amount.wire();
                if let Some(rates) = current.as_ref().and_then(|price| price.tax_rates.as_ref()) {
                    price["taxRates"] = rates.clone();
                }
                cost.insert(name.to_owned(), price);
            }
        }
        Value::Object(cost)
    }

    fn details(&self, ended: bool) -> Value {
        let mut usage = Usage::default();
        for period in self.periods() {
            usage.add(
                period.usage.energy_wh,
                period.usage.charging,
                period.usage.idle,
            );
        }
        let mut total_usage = json!({
            "energy": round(usage.energy_wh, 1000.0),
            "chargingTime": usage.charging.as_secs(),
            "idleTime": usage.idle.as_secs(),
        });
        if let Some(seconds) = self
            .start_fees
            .as_ref()
            .and_then(|fees| fees.reservation_seconds)
        {
            total_usage["reservationTime"] = json!(seconds);
        }
        let mut details = json!({"totalCost": self.total_cost(ended), "totalUsage": total_usage});
        if ended {
            details["chargingPeriods"] = Value::Array(self.periods().map(Period::wire).collect());
        }
        details
    }

    fn event_fields(
        &mut self,
        ended: bool,
        running: bool,
        trigger: TriggerReason,
    ) -> (Option<&str>, Option<Value>) {
        let announce = std::mem::take(&mut self.announce);
        let tariff_id = announce.then_some(self.current().id.as_str());
        let details = (self.local_cost
            && (ended || (running && (announce || trigger == TriggerReason::RunningCost))))
            .then(|| self.details(ended));
        (tariff_id, details)
    }
}

impl Period {
    fn wire(&self) -> Value {
        let dimensions: Vec<_> = [
            ("Energy", json!(round(self.usage.energy_wh, 1000.0))),
            ("ChargingTime", json!(self.usage.charging.as_secs())),
            ("IdleTIme", json!(self.usage.idle.as_secs())),
        ]
        .into_iter()
        .zip(&self.tariff.prices)
        .filter_map(|((name, volume), price)| {
            price
                .as_ref()
                .map(|_| json!({"type": name, "volume": volume}))
        })
        .collect();
        let mut period = json!({"tariffId": self.tariff.id, "startPeriod": self.start});
        if !dimensions.is_empty() {
            period["dimensions"] = json!(dimensions);
        }
        period
    }
}

impl ActiveTx {
    pub(super) fn write_tariff_fields(
        &mut self,
        payload: &mut Value,
        ended: bool,
        running: bool,
        trigger: TriggerReason,
    ) {
        if let Some(state) = &mut self.tariff {
            let (tariff_id, details) = state.event_fields(ended, running, trigger);
            if let Some(id) = tariff_id {
                payload["transactionInfo"]["tariffId"] = json!(id);
            }
            if let Some(details) = details {
                payload["costDetails"] = details;
            }
        }
    }

    pub(in crate::station2) fn has_local_cost(&self) -> bool {
        self.tariff.as_ref().is_some_and(|tariff| tariff.local_cost)
    }

    pub(super) fn add_local_usage(&mut self, wh: f64, charging: Duration, idle: Duration) {
        if let Some(tariff) = self.tariff.as_mut().filter(|tariff| tariff.local_cost) {
            tariff.current.usage.add(wh, charging, idle);
        }
    }

    pub(super) fn refresh_local_cost(&mut self) {
        if let Some(tariff) = self.tariff.as_mut().filter(|tariff| tariff.local_cost) {
            self.total_cost = Some(tariff.evaluate());
            self.cost_currency = Some(tariff.current().currency.clone());
        }
    }
}

impl Station2Actor {
    pub(super) fn associate_tariff(&mut self, evse: u32, offered: AuthorizationTariff) {
        let Some(tx) = self.active_tx(evse).filter(|tx| tx.tariff.is_none()) else {
            return;
        };
        let token = tx.token.clone();
        let started_at = tx.started_at;
        let Some((tariff, driver_token)) =
            self.select_transaction_tariff(evse, token.as_ref(), offered)
        else {
            return;
        };
        let mut state = TransactionTariff::new(
            tariff,
            self.clock.now_utc(),
            self.local_cost_enabled(),
            driver_token,
        );
        let reservation = self.reservation_claim_on(evse);
        let reservation_seconds = reservation
            .filter(|_| state.current().prices.get(4).is_some_and(Option::is_some))
            .and_then(|reservation| reservation.received_at)
            .map(|received| u64::try_from((started_at - received).num_seconds()).unwrap_or(0));
        state.start_fees = Some(StartFees {
            reserved: reservation.is_some(),
            reservation_seconds,
        });
        if let Some(tx) = self.tx.active.get_mut(&evse) {
            tx.tariff = Some(state);
            tx.refresh_local_cost();
        }
        self.arm_running_cost(evse);
    }

    pub(in crate::station2) fn tx_change_tariff(&mut self, evse: u32, tariff: Tariff) {
        self.settle_tx_meters();
        let now = self.clock.now_utc();
        let enabled = self.local_cost_enabled();
        let Some(tx) = self.tx.active.get_mut(&evse) else {
            return;
        };
        if let Some(state) = &mut tx.tariff {
            state.change(tariff, now);
        } else {
            let mut state = TransactionTariff::new(tariff, now, enabled, None);
            state.changed_by_csms = true;
            tx.tariff = Some(state);
        }
        tx.refresh_local_cost();
        self.tx_event(
            evse,
            TransactionEventKind::Updated,
            TriggerReason::TariffChanged,
            EventExtra::default(),
        );
        self.arm_running_cost(evse);
    }

    pub(in crate::station2) fn rearm_running_cost(&mut self) {
        let evses: Vec<_> = self.tx.active.keys().copied().collect();
        for evse in evses {
            self.arm_running_cost(evse);
        }
    }

    pub(super) fn arm_running_cost(&mut self, evse: u32) {
        let key = TimerKey2::RunningCost(evse);
        if self.tariff_enabled("RunningCost")
            && self.active_tx(evse).is_some_and(ActiveTx::has_local_cost)
        {
            let interval = self.dm.uint(TARIFF, "Interval", Some("Cost"));
            self.timers
                .set(key, Duration::from_secs(u64::from(interval.max(1))));
        } else {
            self.timers.cancel(&key);
        }
    }

    pub(in crate::station2) fn on_running_cost(&mut self, evse: u32) {
        if self.tariff_enabled("RunningCost")
            && self.active_tx(evse).is_some_and(ActiveTx::has_local_cost)
        {
            self.tx_event(
                evse,
                TransactionEventKind::Updated,
                TriggerReason::RunningCost,
                EventExtra::default(),
            );
            self.arm_running_cost(evse);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(fields: &Value) -> TransactionTariff {
        let mut raw = fields.clone();
        raw["tariffId"] = json!("A");
        raw["currency"] = json!("USD");
        let mut state = TransactionTariff::new(
            Tariff::parse(raw, 10).unwrap(),
            DateTime::UNIX_EPOCH,
            true,
            None,
        );
        state.start_fees = Some(StartFees::default());
        state
    }

    #[test]
    fn cost_stacks_taxes_and_sums_rounded_components() {
        let mut state = state(&json!({
            "energy": {"prices":[{"priceKwh": 0.25}], "taxRates":[
                {"type":"a","tax":6}, {"type":"b","tax":4,"stack":0}, {"type":"c","tax":20,"stack":1}
            ]},
            "chargingTime": {"prices":[{"priceMinute": 0.5}]},
            "idleTime": {"prices":[{"priceMinute": 0.125}]},
            "fixedFee": {"prices":[{"priceFixed": 0.12345}]}
        }));
        state.current.usage.add(
            1_000.000_49,
            Duration::from_millis(60_999),
            Duration::from_millis(10_999),
        );
        let details = state.details(true);
        let total = &details["totalCost"];
        assert_eq!(
            details["totalUsage"],
            json!({"energy":1000.0,"chargingTime":60,"idleTime":10})
        );
        assert_eq!(total["energy"]["exclTax"], 0.25);
        assert_eq!(total["energy"]["inclTax"], 0.33);
        assert_eq!(total["chargingTime"]["exclTax"], 0.5);
        assert_eq!(total["idleTime"]["exclTax"], 0.0208);
        assert_eq!(total["fixed"]["exclTax"], 0.1235);
        assert_eq!(total["total"], json!({"exclTax":0.8943,"inclTax":0.9743}));
    }

    #[test]
    fn cost_maximum_freezes_money_and_minimum_applies_only_at_end() {
        let mut state = state(&json!({
            "energy": {"prices":[{"priceKwh": 1}]},
            "maxCost": {"inclTax": 0.5},
            "minCost": {"exclTax": 1.0}
        }));
        state
            .current
            .usage
            .add(550.0, Duration::ZERO, Duration::ZERO);
        state.evaluate();
        state
            .current
            .usage
            .add(500.0, Duration::ZERO, Duration::ZERO);
        assert_eq!(state.details(false)["totalCost"]["typeOfCost"], "MaxCost");
        assert_eq!(state.details(false)["totalCost"]["total"]["inclTax"], 0.5);
        assert_eq!(state.details(true)["totalUsage"]["energy"], 1050.0);
        assert_eq!(
            state.details(true)["totalCost"],
            json!({
                "currency":"USD", "typeOfCost":"MinCost",
                "fixed":{"exclTax":1.0,"inclTax":1.0}, "total":{"exclTax":1.0,"inclTax":1.0}
            })
        );
    }

    #[test]
    fn cost_change_keeps_period_usage_and_initial_fees() {
        let mut state = state(&json!({
            "energy":{"prices":[{"priceKwh":1}]}, "fixedFee":{"prices":[{"priceFixed":1}]}
        }));
        state
            .current
            .usage
            .add(500.0, Duration::ZERO, Duration::ZERO);
        let new = Tariff::parse(
            json!({
                "tariffId":"B", "currency":"USD", "energy":{"prices":[{"priceKwh":2}]},
                "fixedFee":{"prices":[{"priceFixed":20}]}
            }),
            10,
        )
        .unwrap();
        state.change(new, DateTime::UNIX_EPOCH + chrono::TimeDelta::seconds(1));
        state
            .current
            .usage
            .add(500.0, Duration::ZERO, Duration::ZERO);
        let details = state.details(true);
        assert_eq!(details["chargingPeriods"][0]["tariffId"], "A");
        assert_eq!(details["chargingPeriods"][1]["tariffId"], "B");
        assert_eq!(details["totalUsage"]["energy"], 1_000.0);
        assert_eq!(details["totalCost"]["energy"]["exclTax"], 1.5);
        assert_eq!(details["totalCost"]["fixed"]["exclTax"], 1.0);
    }
}
