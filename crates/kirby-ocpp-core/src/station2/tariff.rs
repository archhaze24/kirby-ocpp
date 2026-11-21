//! Installed tariffs, driver authorization, and tariff assignments.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::actor::{Station2Actor, TimerKey2};
use super::device_model::TARIFF;
use super::types::{IdTokenRef, Origin2};
use crate::error::CallError;
use crate::ocpp::Protocol;
use crate::ocpp::action::Action;
use crate::ocpp::frame::ErrorCode;
use crate::ocpp::responder::Responder;
use crate::ocpp::schema::SchemaRegistry;
use crate::ocpp::v2::messages::{
    ChangeTransactionTariff, ChangeTransactionTariffRequest, ChangeTransactionTariffResponse,
    ClearTariffs, ClearTariffsRequest, ClearTariffsResponse, ClearTariffsResult, EventData,
    EventNotificationKind, EventTrigger, GetTariffs, GetTariffsRequest, GetTariffsResponse,
    NotifyEvent, NotifyEventRequest, SetDefaultTariff, SetDefaultTariffRequest,
    SetDefaultTariffResponse, TariffAssignment, TariffChangeStatus, TariffClearStatus,
    TariffGetStatus, TariffKind, TariffSetStatus,
};
use crate::ocpp::v2::types::{Component, StatusInfo, Variable};
use crate::station::LogLevel;

/// Bounds the memory used by stored tariffs.
const MAX_TARIFF_BYTES: usize = 16 * 1024;
const FUTURE_LIMIT: usize = 4;
const DRIVER_LIMIT: usize = 32;
const CENTRAL_COST_LIMIT: usize = 1024;
const PROBLEM_OUTBOX_LIMIT: usize = 32;
static NEXT_EVENT_ID: AtomicU64 = AtomicU64::new(1);

pub(super) const PRICE_FIELDS: [(&str, &str, &str); 6] = [
    ("energy", "priceKwh", "energy"),
    ("chargingTime", "priceMinute", "chargingTime"),
    ("idleTime", "priceMinute", "idleTime"),
    ("fixedFee", "priceFixed", "fixed"),
    ("reservationTime", "priceMinute", "reservationTime"),
    ("reservationFixed", "priceFixed", "reservationFixed"),
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct Price {
    pub amount: f64,
    pub tax_factor: f64,
    pub tax_rates: Option<Value>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct CostBound {
    pub excl_tax: Option<f64>,
    pub incl_tax: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct Tariff {
    raw: Value,
    pub id: String,
    pub currency: String,
    valid_from: Option<DateTime<Utc>>,
    pub prices: [Option<Price>; PRICE_FIELDS.len()],
    pub min_cost: CostBound,
    pub max_cost: CostBound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InvalidTariff {
    InvalidValue,
    TooManyElements,
    ConditionNotSupported,
}

impl InvalidTariff {
    fn reason(self) -> &'static str {
        match self {
            Self::InvalidValue => "InvalidValue",
            Self::TooManyElements => "TooManyElements",
            Self::ConditionNotSupported => "ConditionNotSupported",
        }
    }
}

fn number(value: &Value) -> Result<f64, InvalidTariff> {
    value
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0)
        .ok_or(InvalidTariff::InvalidValue)
}

pub(super) fn tariff_size_ok(raw: &Value) -> bool {
    serde_json::to_vec(raw).is_ok_and(|bytes| bytes.len() <= MAX_TARIFF_BYTES)
}

fn tax_stack(rate: &Value) -> i64 {
    let Some(stack) = rate.get("stack") else {
        return 0;
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "JSON Schema accepts integral floats; values outside i64 saturate"
    )]
    stack
        .as_i64()
        .or_else(|| stack.as_f64().map(|value| value as i64))
        .unwrap_or_default()
}

fn tax_rates(field: &Value) -> Result<f64, InvalidTariff> {
    let Some(rates) = field.get("taxRates").and_then(Value::as_array) else {
        return Ok(1.0);
    };
    let mut rates = rates
        .iter()
        .map(|rate| {
            let tax = number(rate.get("tax").unwrap_or(&Value::Null))?;
            Ok((tax_stack(rate), tax / 100.0))
        })
        .collect::<Result<Vec<_>, _>>()?;
    rates.sort_by_key(|(stack, _)| *stack);
    let mut factor = 1.0_f64;
    for rates in rates.chunk_by(|(a, _), (b, _)| a == b) {
        factor *= 1.0 + rates.iter().map(|(_, tax)| tax).sum::<f64>();
    }
    Ok(factor.min(f64::MAX))
}

fn cost_bound(field: &Value) -> Result<CostBound, InvalidTariff> {
    tax_rates(field)?;
    Ok(CostBound {
        excl_tax: field.get("exclTax").map(number).transpose()?,
        incl_tax: field.get("inclTax").map(number).transpose()?,
    })
}

impl Tariff {
    pub(super) fn parse(raw: Value, max_elements: usize) -> Result<Self, InvalidTariff> {
        if !tariff_size_ok(&raw) {
            return Err(InvalidTariff::TooManyElements);
        }
        let invalid = InvalidTariff::InvalidValue;
        let id = raw.get("tariffId").and_then(Value::as_str).ok_or(invalid)?;
        let currency = raw.get("currency").and_then(Value::as_str).ok_or(invalid)?;
        let valid_from = raw
            .get("validFrom")
            .map(|value| serde_json::from_value(value.clone()).map_err(|_| invalid))
            .transpose()?;
        let mut prices: [Option<Price>; PRICE_FIELDS.len()] = Default::default();
        let mut too_many = false;
        let mut conditional = false;
        for ((field, price_key, _), applied) in PRICE_FIELDS.into_iter().zip(&mut prices) {
            let Some(value) = raw.get(field) else {
                continue;
            };
            let elements = value
                .get("prices")
                .and_then(Value::as_array)
                .ok_or(invalid)?;
            let tax_factor = tax_rates(value)?;
            too_many |= elements.len() > max_elements;
            for element in elements {
                let amount = number(element.get(price_key).unwrap_or(&Value::Null))?;
                conditional |= element.get("conditions").is_some();
                applied.get_or_insert_with(|| Price {
                    amount,
                    tax_factor,
                    tax_rates: value.get("taxRates").cloned(),
                });
            }
        }
        let min_cost = cost_bound(raw.get("minCost").unwrap_or(&Value::Null))?;
        let max_cost = cost_bound(raw.get("maxCost").unwrap_or(&Value::Null))?;
        if prices.iter().all(Option::is_none) {
            return Err(invalid);
        }
        if too_many {
            return Err(InvalidTariff::TooManyElements);
        }
        if conditional {
            return Err(InvalidTariff::ConditionNotSupported);
        }
        Ok(Self {
            id: id.to_owned(),
            currency: currency.to_owned(),
            raw,
            valid_from,
            prices,
            min_cost,
            max_cost,
        })
    }
}

#[derive(Debug, Default)]
pub(super) struct TariffState {
    defaults: BTreeMap<u32, Vec<Tariff>>,
    /// Oldest authorization first.
    drivers: VecDeque<(IdTokenRef, Tariff)>,
    /// Passed to the authorization that received it when every cache slot is in use.
    unplaced_driver: Option<(IdTokenRef, Tariff)>,
    central_cost: VecDeque<IdTokenRef>,
    pub problem: bool,
    problem_outbox: VecDeque<NotifyEventRequest>,
}

fn effective(entries: &[Tariff], now: DateTime<Utc>) -> Option<&Tariff> {
    entries
        .iter()
        .filter(|entry| entry.valid_from.is_none_or(|at| at <= now))
        .max_by_key(|entry| entry.valid_from)
}

impl TariffState {
    fn prune(&mut self, now: DateTime<Utc>) -> bool {
        let mut changed = false;
        for entries in self.defaults.values_mut() {
            if let Some(id) = effective(entries, now).map(|entry| entry.id.clone()) {
                let before = entries.len();
                entries
                    .retain(|entry| entry.id == id || entry.valid_from.is_some_and(|at| at > now));
                changed |= before != entries.len();
            }
        }
        changed
    }

    fn install(&mut self, evse: u32, tariff: Tariff, now: DateTime<Utc>) {
        let entries = self.defaults.entry(evse).or_default();
        if tariff.valid_from.is_none() {
            entries.retain(|entry| entry.valid_from.is_some_and(|at| at > now));
        }
        entries.push(tariff);
    }
}

impl Station2Actor {
    pub(super) fn local_tariffs_enabled(&self) -> bool {
        self.protocol() == Protocol::V21 && self.tariff_enabled("Tariff")
    }

    pub(super) fn local_cost_enabled(&self) -> bool {
        self.local_tariffs_enabled() && self.tariff_enabled("Cost")
    }

    pub(super) fn tariff_enabled(&self, instance: &str) -> bool {
        self.dm.station_text(TARIFF, "Enabled", Some(instance)) == "true"
    }

    fn parse_tariff(&self, raw: Value) -> Result<Tariff, InvalidTariff> {
        Tariff::parse(
            raw,
            self.dm.uint(TARIFF, "MaxElements", Some("Tariff")) as usize,
        )
    }

    pub(super) fn default_tariff(&self, evse: u32) -> Option<&Tariff> {
        effective(self.tariffs.defaults.get(&evse)?, self.clock.now_utc())
    }

    pub(super) fn prune_default_tariffs(&mut self) {
        if self.tariffs.prune(self.clock.now_utc()) {
            self.mark_dirty();
        }
    }

    pub(super) fn on_tariff_activation(&mut self) {
        self.prune_default_tariffs();
        self.arm_tariff_activation();
    }

    fn arm_tariff_activation(&mut self) {
        let now = self.clock.now_utc();
        let next = self
            .tariffs
            .defaults
            .values()
            .flatten()
            .filter_map(|entry| (entry.valid_from? - now).to_std().ok())
            .filter(|delay| !delay.is_zero())
            .min();
        if let Some(delay) = next {
            self.timers.set(TimerKey2::DefaultTariffActivation, delay);
        } else {
            self.timers.cancel(&TimerKey2::DefaultTariffActivation);
        }
    }

    pub(super) fn persist_default_tariffs(&mut self) -> Vec<Value> {
        self.tariffs.prune(self.clock.now_utc());
        self.tariffs
            .defaults
            .iter()
            .flat_map(|(evse, entries)| {
                entries.iter().map(move |entry| {
                    json!({"evseId": evse, "tariff": entry.raw, "validFrom": entry.valid_from})
                })
            })
            .collect()
    }

    pub(super) fn restore_default_tariffs(&mut self, entries: Vec<Value>) {
        if self.protocol() != Protocol::V21 {
            return;
        }
        let now = self.clock.now_utc();
        for entry in entries {
            if let Some((evse, tariff)) = self.read_default_tariff(entry)
                && !self.future_tariffs_full(evse, &tariff, now)
            {
                self.tariffs.install(evse, tariff, now);
                self.tariffs.prune(now);
            } else {
                self.log(
                    LogLevel::Warn,
                    "invalid stored default tariff dropped".to_owned(),
                );
            }
        }
        self.arm_tariff_activation();
    }

    fn read_default_tariff(&self, mut entry: Value) -> Option<(u32, Tariff)> {
        let evse = u32::try_from(entry.get("evseId")?.as_u64()?).ok()?;
        self.evse(evse)?;
        let raw = entry.get_mut("tariff")?.take();
        SchemaRegistry::for_protocol(Protocol::V21)
            .ok()?
            .validate_request(
                SetDefaultTariff::NAME,
                &json!({"evseId": evse, "tariff": raw}),
            )
            .ok()?;
        let tariff = self.parse_tariff(raw).ok()?;
        let valid_from: Option<DateTime<Utc>> =
            serde_json::from_value(entry.get_mut("validFrom")?.take()).ok()?;
        if tariff.valid_from != valid_from {
            return None;
        }
        Some((evse, tariff))
    }

    fn selected_evses(&self, evse: u32) -> impl Iterator<Item = u32> + '_ {
        self.evses
            .iter()
            .map(|evse| evse.id)
            .filter(move |id| evse == 0 || evse == *id)
    }

    fn future_tariffs_full(&self, evse: u32, tariff: &Tariff, now: DateTime<Utc>) -> bool {
        tariff.valid_from.is_some_and(|at| at > now)
            && self
                .tariffs
                .defaults
                .get(&evse)
                .into_iter()
                .flatten()
                .filter(|entry| entry.valid_from.is_some_and(|at| at > now))
                .count()
                >= FUTURE_LIMIT
    }

    fn tariff_id_exists(&self, id: &str) -> bool {
        self.tariffs
            .defaults
            .values()
            .flatten()
            .any(|entry| entry.id == id)
            || self
                .tariffs
                .drivers
                .iter()
                .any(|(_, tariff)| tariff.id == id)
            || self.evses.iter().any(|evse| {
                self.active_tx(evse.id)
                    .and_then(|tx| tx.tariff.as_ref())
                    .is_some_and(|state| state.current().id == id)
            })
    }

    fn driver_in_use(&self, token: &IdTokenRef, tariff_id: &str) -> bool {
        self.evses.iter().any(|evse| {
            self.active_tx(evse.id)
                .and_then(|tx| tx.tariff.as_ref())
                .and_then(|state| state.driver_binding())
                == Some((token, tariff_id))
        })
    }

    pub(super) fn forget_driver_tariff(&mut self, token: &IdTokenRef) {
        self.tariffs.central_cost.retain(|entry| entry != token);
        self.take_unplaced_driver(token);
        if let Some(index) =
            self.tariffs.drivers.iter().position(|(entry, tariff)| {
                entry == token && !self.driver_in_use(entry, &tariff.id)
            })
        {
            self.tariffs.drivers.remove(index);
        }
    }

    pub(super) fn take_unplaced_driver(&mut self, token: &IdTokenRef) -> Option<Tariff> {
        if self
            .tariffs
            .unplaced_driver
            .as_ref()
            .is_some_and(|(entry, _)| entry == token)
        {
            self.tariffs
                .unplaced_driver
                .take()
                .map(|(_, tariff)| tariff)
        } else {
            None
        }
    }

    pub(super) fn remove_ended_driver_tariff(&mut self, token: &IdTokenRef) {
        if !self.evses.iter().any(|evse| {
            self.active_tx(evse.id).is_some_and(|tx| {
                tx.token.as_ref() == Some(token)
                    || tx
                        .tariff
                        .as_ref()
                        .and_then(|state| state.driver_binding())
                        .is_some_and(|(bound, _)| bound == token)
            })
        }) {
            self.tariffs.drivers.retain(|(entry, _)| entry != token);
        }
    }

    /// Returns false when an invalid tariff deauthorizes the token.
    pub(super) fn apply_driver_tariff(&mut self, token: &IdTokenRef, raw: Option<Value>) -> bool {
        if !self.local_tariffs_enabled() {
            return true;
        }
        let Some(raw) = raw else {
            return true;
        };
        self.tariffs.unplaced_driver = None;
        self.tariffs.central_cost.retain(|entry| entry != token);
        match self.parse_tariff(raw) {
            Ok(tariff) => {
                self.tariffs.drivers.retain(|(entry, _)| entry != token);
                if self.tariffs.drivers.len() >= DRIVER_LIMIT
                    && let Some(index) = self
                        .tariffs
                        .drivers
                        .iter()
                        .position(|(entry, tariff)| !self.driver_in_use(entry, &tariff.id))
                {
                    self.tariffs.drivers.remove(index);
                }
                if self.tariffs.drivers.len() < DRIVER_LIMIT {
                    self.tariffs.drivers.push_back((token.clone(), tariff));
                } else {
                    self.tariffs.unplaced_driver = Some((token.clone(), tariff));
                }
                self.set_tariff_problem(None);
                true
            }
            Err(error) => {
                self.tariffs.drivers.retain(|(entry, _)| entry != token);
                self.set_tariff_problem(Some(error.reason()));
                match self
                    .dm
                    .station_text(TARIFF, "HandleFailedTariff", Some("Tariff"))
                    .as_str()
                {
                    "UseDefaultTariff" => true,
                    "CentralCost" => {
                        if self.tariffs.central_cost.len() >= CENTRAL_COST_LIMIT {
                            self.tariffs.central_cost.pop_front();
                        }
                        self.tariffs.central_cost.push_back(token.clone());
                        true
                    }
                    _ => false,
                }
            }
        }
    }

    pub(super) fn select_transaction_tariff(
        &mut self,
        evse: u32,
        token: Option<&IdTokenRef>,
        offered: Option<Tariff>,
    ) -> Option<(Tariff, Option<IdTokenRef>)> {
        let central_cost = token
            .and_then(|token| {
                self.tariffs
                    .central_cost
                    .iter()
                    .position(|entry| entry == token)
            })
            .and_then(|index| self.tariffs.central_cost.remove(index))
            .is_some();
        if central_cost || !self.local_tariffs_enabled() {
            return None;
        }
        if let Some(token) = token {
            if let Some(tariff) = offered {
                return Some((tariff, Some(token.clone())));
            }
            if let Some((_, tariff)) = self
                .tariffs
                .drivers
                .iter()
                .find(|(entry, _)| entry == token)
            {
                return Some((tariff.clone(), Some(token.clone())));
            }
        }
        self.prune_default_tariffs();
        self.default_tariff(evse)
            .cloned()
            .map(|tariff| (tariff, None))
    }

    pub(super) fn set_tariff_problem(&mut self, reason: Option<&str>) {
        let problem = reason.is_some();
        if self.tariffs.problem == problem {
            return;
        }
        self.tariffs.problem = problem;
        let event_id = NEXT_EVENT_ID.fetch_add(1, Ordering::Relaxed);
        let now = self.clock.now_utc();
        let request = NotifyEventRequest {
            generated_at: now,
            seq_no: 0,
            tbc: None,
            event_data: vec![EventData {
                event_id,
                timestamp: now,
                trigger: EventTrigger::Alerting,
                actual_value: problem.to_string(),
                event_notification_type: EventNotificationKind::HardWiredNotification,
                component: Component {
                    name: TARIFF.to_owned(),
                    evse: None,
                    instance: None,
                    custom_data: None,
                },
                variable: Variable {
                    name: "Problem".to_owned(),
                    instance: None,
                    custom_data: None,
                },
                cause: None,
                tech_code: reason.map(str::to_owned),
                tech_info: None,
                cleared: None,
                transaction_id: None,
                variable_monitoring_id: None,
                severity: None,
                custom_data: None,
            }],
            custom_data: None,
        };
        self.enqueue_tariff_event(request);
        self.flush_tariff_events();
    }

    fn enqueue_tariff_event(&mut self, request: NotifyEventRequest) {
        let outbox = &mut self.tariffs.problem_outbox;
        if outbox.len() >= PROBLEM_OUTBOX_LIMIT {
            outbox.pop_front();
        }
        outbox.push_back(request);
    }

    pub(super) fn flush_tariff_events(&mut self) {
        if self.link.conn.is_none() || self.may_send(NotifyEvent::NAME, Origin2::Station).is_err() {
            return;
        }
        let mut outbox = std::mem::take(&mut self.tariffs.problem_outbox);
        outbox
            .make_contiguous()
            .sort_by_key(|request| request.event_data.first().map(|event| event.event_id));
        for request in outbox {
            let retry = request.clone();
            self.call::<NotifyEvent>(Origin2::Station, &request, move |actor, result| {
                if matches!(
                    result,
                    Err(CallError::Closed { .. }
                        | CallError::NotConnected
                        | CallError::NotRegistered { .. })
                ) {
                    actor.enqueue_tariff_event(retry);
                }
            });
        }
    }
}

fn status_info(reason: &str) -> StatusInfo {
    StatusInfo {
        reason_code: reason.to_owned(),
        additional_info: None,
        custom_data: None,
    }
}

fn known_tariff_evse(actor: &Station2Actor, id: u64) -> Option<u32> {
    u32::try_from(id)
        .ok()
        .filter(|&id| id == 0 || actor.evse(id).is_some())
}

pub(super) fn set_default_tariff(
    actor: &mut Station2Actor,
    responder: Responder<SetDefaultTariff>,
    request: SetDefaultTariffRequest,
) {
    use TariffSetStatus as Status;
    if !actor.local_tariffs_enabled() {
        actor.respond_error(
            responder,
            ErrorCode::NotSupported,
            "local tariffs are disabled",
        );
        return;
    }
    let (status, reason) =
        install_default(actor, request).map_or_else(|error| error, |()| (Status::Accepted, None));
    actor.respond(
        responder,
        SetDefaultTariffResponse {
            status,
            status_info: reason.map(status_info),
            custom_data: None,
        },
    );
    if status == Status::Accepted {
        actor.flush_now();
    }
}

fn install_default(
    actor: &mut Station2Actor,
    request: SetDefaultTariffRequest,
) -> Result<(), (TariffSetStatus, Option<&'static str>)> {
    use TariffSetStatus as Status;
    let Some(evse) = known_tariff_evse(actor, request.evse_id) else {
        return Err((Status::Rejected, Some("UnknownEVSE")));
    };
    let tariff = actor
        .parse_tariff(request.tariff)
        .map_err(|error| match error {
            InvalidTariff::InvalidValue => (Status::Rejected, Some(error.reason())),
            InvalidTariff::TooManyElements => (Status::TooManyElements, None),
            InvalidTariff::ConditionNotSupported => (Status::ConditionNotSupported, None),
        })?;
    actor.prune_default_tariffs();
    if actor.tariff_id_exists(&tariff.id) {
        return Err((Status::DuplicateTariffId, None));
    }
    let now = actor.clock.now_utc();
    let evses: Vec<_> = actor.selected_evses(evse).collect();
    if evses
        .iter()
        .any(|id| actor.future_tariffs_full(*id, &tariff, now))
    {
        return Err((Status::Rejected, Some("OutOfMemory")));
    }
    for id in evses {
        actor.tariffs.install(id, tariff.clone(), now);
    }
    actor.on_tariff_activation();
    Ok(())
}

#[derive(Default)]
struct DriverAssignment {
    tokens: BTreeSet<String>,
    evses: Vec<u32>,
}

fn assignments(actor: &Station2Actor, evse: u32) -> Vec<TariffAssignment> {
    let mut defaults = BTreeMap::new();
    let mut drivers = BTreeMap::<&str, DriverAssignment>::new();
    for id in actor.selected_evses(evse) {
        for entry in actor.tariffs.defaults.get(&id).into_iter().flatten() {
            let (_, ids) = defaults
                .entry(&entry.id)
                .or_insert_with(|| (entry.valid_from, Vec::new()));
            ids.push(id);
        }
        if let Some(state) = actor.active_tx(id).and_then(|tx| tx.tariff.as_ref())
            && state.is_driver()
        {
            let driver = drivers.entry(&state.current().id).or_default();
            driver.evses.push(id);
        }
    }
    for (token, tariff) in &actor.tariffs.drivers {
        if evse == 0 || drivers.contains_key(tariff.id.as_str()) {
            drivers
                .entry(&tariff.id)
                .or_default()
                .tokens
                .insert(token.id_token.clone());
        }
    }
    defaults
        .into_iter()
        .map(|(id, (valid_from, evses))| TariffAssignment {
            tariff_id: id.clone(),
            tariff_kind: TariffKind::DefaultTariff,
            evse_ids: Some(evses),
            valid_from,
            id_tokens: None,
            custom_data: None,
        })
        .chain(drivers.into_iter().map(|(id, driver)| TariffAssignment {
            tariff_id: id.to_owned(),
            tariff_kind: TariffKind::DriverTariff,
            evse_ids: (!driver.evses.is_empty()).then_some(driver.evses),
            valid_from: None,
            id_tokens: (!driver.tokens.is_empty()).then(|| driver.tokens.into_iter().collect()),
            custom_data: None,
        }))
        .collect()
}

#[allow(clippy::needless_pass_by_value, reason = "inbound handler signature")]
pub(super) fn get_tariffs(
    actor: &mut Station2Actor,
    responder: Responder<GetTariffs>,
    request: GetTariffsRequest,
) {
    if !actor.local_tariffs_enabled() {
        actor.respond_error(
            responder,
            ErrorCode::NotSupported,
            "local tariffs are disabled",
        );
        return;
    }
    actor.prune_default_tariffs();
    let evse = known_tariff_evse(actor, request.evse_id);
    let (status, reason, tariff_assignments) = if let Some(evse) = evse {
        let entries = assignments(actor, evse);
        if entries.is_empty() {
            (TariffGetStatus::NoTariff, None, None)
        } else {
            (TariffGetStatus::Accepted, None, Some(entries))
        }
    } else {
        (TariffGetStatus::Rejected, Some("UnknownEVSE"), None)
    };
    actor.respond(
        responder,
        GetTariffsResponse {
            status,
            status_info: reason.map(status_info),
            tariff_assignments,
            custom_data: None,
        },
    );
}

pub(super) fn clear_tariffs(
    actor: &mut Station2Actor,
    responder: Responder<ClearTariffs>,
    request: ClearTariffsRequest,
) {
    use TariffClearStatus as Status;
    if !actor.local_tariffs_enabled() {
        actor.respond_error(
            responder,
            ErrorCode::NotSupported,
            "local tariffs are disabled",
        );
        return;
    }
    let result = |status, tariff_id, reason: Option<&str>| ClearTariffsResult {
        status,
        tariff_id,
        status_info: reason.map(status_info),
        custom_data: None,
    };
    actor.prune_default_tariffs();
    let evse = known_tariff_evse(actor, request.evse_id.unwrap_or(0));
    let mut removed = BTreeSet::new();
    let clear_tariffs_result = if let Some(evse) = evse {
        let requested = request
            .tariff_ids
            .map(|ids| ids.into_iter().collect::<BTreeSet<_>>());
        for (id, entries) in &mut actor.tariffs.defaults {
            if evse == 0 || evse == *id {
                entries.retain(|entry| {
                    if requested.as_ref().is_none_or(|ids| ids.contains(&entry.id)) {
                        removed.insert(entry.id.clone());
                        false
                    } else {
                        true
                    }
                });
            }
        }
        if removed.is_empty() {
            vec![result(Status::NoTariff, None, None)]
        } else {
            requested
                .unwrap_or_else(|| removed.clone())
                .into_iter()
                .map(|id| {
                    let status = if removed.contains(&id) {
                        Status::Accepted
                    } else {
                        Status::NoTariff
                    };
                    result(status, Some(id), None)
                })
                .collect()
        }
    } else {
        vec![result(Status::Rejected, None, Some("UnknownEVSE"))]
    };
    actor.respond(
        responder,
        ClearTariffsResponse {
            clear_tariffs_result,
            custom_data: None,
        },
    );
    if !removed.is_empty() {
        actor.arm_tariff_activation();
        actor.flush_now();
    }
}

pub(super) fn change_transaction_tariff(
    actor: &mut Station2Actor,
    responder: Responder<ChangeTransactionTariff>,
    request: ChangeTransactionTariffRequest,
) {
    use TariffChangeStatus as Status;
    if !actor.local_tariffs_enabled() {
        actor.respond_error(
            responder,
            ErrorCode::NotSupported,
            "local tariffs are disabled",
        );
        return;
    }
    let reply = |status, reason: Option<&str>| ChangeTransactionTariffResponse {
        status,
        status_info: reason.map(status_info),
        custom_data: None,
    };
    let Some(evse) = actor.evses.iter().map(|e| e.id).find(|id| {
        actor
            .active_tx(*id)
            .is_some_and(|tx| tx.id.0 == request.transaction_id)
    }) else {
        actor.respond(responder, reply(Status::TxNotFound, None));
        return;
    };
    let tariff = match actor.parse_tariff(request.tariff) {
        Ok(tariff) => tariff,
        Err(error) => {
            let (status, reason) = match error {
                InvalidTariff::InvalidValue => (Status::Rejected, Some(error.reason())),
                InvalidTariff::TooManyElements => (Status::TooManyElements, None),
                InvalidTariff::ConditionNotSupported => (Status::ConditionNotSupported, None),
            };
            actor.respond(responder, reply(status, reason));
            return;
        }
    };
    if let Some(state) = actor.active_tx(evse).and_then(|tx| tx.tariff.as_ref()) {
        if state.current().currency != tariff.currency {
            actor.respond(responder, reply(Status::NoCurrencyChange, None));
            return;
        }
        if state.periods_full() {
            actor.respond(responder, reply(Status::Rejected, Some("OutOfMemory")));
            return;
        }
    }
    actor.respond(responder, reply(Status::Accepted, None));
    actor.tx_change_tariff(evse, tariff);
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;

    fn priced(id: &str, at: Option<DateTime<Utc>>) -> Value {
        let mut raw =
            json!({"tariffId": id, "currency": "USD", "energy": {"prices": [{"priceKwh": 1}]}});
        if let Some(at) = at {
            raw["validFrom"] = json!(at);
        }
        raw
    }

    #[test]
    fn tariff_review_tax_stacks_group_negative_zero_with_default() {
        let field = json!({"taxRates":[{"type":"A","tax":10,"stack":-0.0},{"type":"B","tax":20}]});
        let factor = tax_rates(&field).unwrap();
        assert!((factor * 100.0 - 130.0).abs() < 1e-10, "factor: {factor}");
    }

    #[test]
    fn tariff_effective_dates_and_undated_replacement() {
        let now = DateTime::UNIX_EPOCH + TimeDelta::hours(1);
        let later = now + TimeDelta::seconds(2);
        let latest = later + TimeDelta::seconds(2);
        let mut state = TariffState::default();
        for (id, at) in [
            ("base", None),
            ("latest", Some(latest)),
            ("later", Some(later)),
            ("past", Some(now - TimeDelta::seconds(1))),
        ] {
            state.install(1, Tariff::parse(priced(id, at), 10).unwrap(), now);
        }
        assert_eq!(effective(&state.defaults[&1], now).unwrap().id, "past");
        assert!(state.prune(now));
        assert_eq!(state.defaults[&1].len(), 3);
        state.install(
            1,
            Tariff::parse(priced("immediate", None), 10).unwrap(),
            now,
        );
        assert_eq!(effective(&state.defaults[&1], now).unwrap().id, "immediate");
        assert_eq!(effective(&state.defaults[&1], later).unwrap().id, "later");
        assert!(state.prune(later));
        assert_eq!(state.defaults[&1].len(), 2);
        assert_eq!(effective(&state.defaults[&1], latest).unwrap().id, "latest");
        assert!(state.prune(latest));
        assert_eq!(state.defaults[&1].len(), 1);
    }

    #[test]
    fn tariff_validation_checks_all_prices_then_counts_then_conditions() {
        let mut raw = priced("test", None);
        raw["energy"]["prices"] = json!([
            {"priceKwh": 0, "conditions": {}},
            {"priceKwh": -1},
        ]);
        assert_eq!(
            Tariff::parse(raw.clone(), 1),
            Err(InvalidTariff::InvalidValue)
        );
        raw["energy"]["prices"][1]["priceKwh"] = json!(1);
        assert_eq!(
            Tariff::parse(raw.clone(), 1),
            Err(InvalidTariff::TooManyElements)
        );
        assert_eq!(
            Tariff::parse(raw.clone(), 2),
            Err(InvalidTariff::ConditionNotSupported)
        );
        raw["energy"]["prices"][0] = json!({"priceKwh": 2});
        let tariff = Tariff::parse(raw, 2).unwrap();
        assert_eq!(
            tariff.prices[0].as_ref().unwrap().amount.to_bits(),
            2.0_f64.to_bits()
        );
    }
}
