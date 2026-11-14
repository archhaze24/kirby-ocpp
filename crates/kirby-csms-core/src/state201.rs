//! Per-station state folded from OCPP 2.0.1 messages.
//!
//! [`StationState::apply_v2`] folds the CALLs the engine answered into the shared fields of
//! [`StationState`] (boot, heartbeat, security events) and into [`StationState2`], which
//! holds what only 2.0.1 has: EVSEs with their connectors, transactions keyed by the
//! station-made transaction id, and meter readings.
//!
//! The fold never trusts the station:
//!
//! * a payload that does not decode changes nothing;
//! * every stored string is cut to its schema maximum, because inbound validation can be
//!   switched off and the state leaves through the API as it is;
//! * every collection is bounded, and device-model values (`NotifyReport`) are counted,
//!   never stored.
//!
//! Stations queue events while offline and replay them late, twice or out of order, so
//! the transaction fold is idempotent: a repeated `seqNo` folds nothing, optional fields
//! that a message omits keep their value, meter readings never move back in time, and a
//! stopped transaction is never revived.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::types::RegistrationStatus as LegacyRegistrationStatus;
use kirby_ocpp_core::ocpp::v2::messages::{
    BootNotificationRequest, BootNotificationResponse, BootReason, ConnectorStatus,
    MeterValuesRequest, NotifyReportRequest, RegistrationStatus, SecurityEventNotificationRequest,
    StatusNotificationRequest, TransactionEventKind, TransactionEventRequest,
    TransactionEventResponse, TriggerReason,
};
use kirby_ocpp_core::ocpp::v2::types::{
    AuthorizationStatus, ChargingState, IdTokenType, Measurand, MeterValue, ReadingContext,
    SampledValue, StopReason, WireEnum,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::{
    ActiveTransaction, BootInfo, MAX_ACTIVE_TRANSACTIONS, MAX_CONNECTORS, MAX_SECURITY_EVENTS,
    SecurityEventRecord, StationState, TransactionId, TransactionRow, TransactionStatus,
};

/// Most EVSEs (0 is the station) with a tracked meter reading.
pub const MAX_METER_EVSES: usize = 1024;
/// Most disjoint `seqNo` ranges remembered per transaction.
pub const MAX_SEQ_RANGES: usize = 32;

/// Schema limits of the strings the fold stores (characters).
const MAX_TRANSACTION_ID_LEN: usize = 36;
const MAX_ID_TOKEN_LEN: usize = 36;
const MAX_VENDOR_NAME_LEN: usize = 50;
const MAX_MODEL_LEN: usize = 20;
const MAX_SERIAL_NUMBER_LEN: usize = 25;
const MAX_FIRMWARE_VERSION_LEN: usize = 50;
const MAX_MODEM_ID_LEN: usize = 20;
const MAX_EVENT_TYPE_LEN: usize = 50;
const MAX_TECH_INFO_LEN: usize = 255;

/// The OCPP 2.0.1 part of a station's state. See the module docs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StationState2 {
    /// The protocol token the data was folded under (`ocpp2.0.1`).
    pub protocol: String,
    /// The `reason` of the last `BootNotification`.
    pub boot_reason: Option<BootReason>,
    /// EVSE number (1 and up) to its connectors.
    pub evses: BTreeMap<u32, EvseState>,
    /// Transactions, oldest first: every active one plus at most
    /// `transaction_history_cap` stopped ones.
    pub transactions: Vec<Tx2Record>,
    /// Latest readings from `MeterValues` per EVSE (0 is the station), outside transactions.
    pub meters: BTreeMap<u32, MeterReading>,
    pub counters: Counters2,
    /// The last `NotifyReport`; its content is not kept.
    pub last_report: Option<ReportInfo>,
}

impl StationState2 {
    pub(crate) fn new(protocol: Protocol) -> Self {
        Self {
            protocol: protocol.token().to_owned(),
            boot_reason: None,
            evses: BTreeMap::new(),
            transactions: Vec::new(),
            meters: BTreeMap::new(),
            counters: Counters2::default(),
            last_report: None,
        }
    }

    /// `"<evse>/<connector>"` to status name.
    pub(crate) fn connector_statuses(&self) -> impl Iterator<Item = (String, String)> + '_ {
        self.evses.iter().flat_map(|(evse, state)| {
            state.connectors.iter().map(move |(connector, state)| {
                (
                    format!("{evse}/{connector}"),
                    state.status.as_str().to_owned(),
                )
            })
        })
    }

    pub(crate) fn active_transactions(&self) -> impl Iterator<Item = ActiveTransaction> + '_ {
        self.transactions
            .iter()
            .filter(|tx| tx.status == TransactionStatus::Active)
            .map(|tx| ActiveTransaction {
                id: TransactionId::Text(tx.id.clone()),
                connector: tx.connector,
                evse: tx.evse,
            })
    }

    pub(crate) fn active_count(&self) -> usize {
        self.transactions
            .iter()
            .filter(|tx| tx.status == TransactionStatus::Active)
            .count()
    }

    pub(crate) fn rows(&self) -> impl Iterator<Item = TransactionRow> + '_ {
        self.transactions.iter().map(Tx2Record::row)
    }

    fn connector_count(&self) -> usize {
        self.evses.values().map(|evse| evse.connectors.len()).sum()
    }

    /// Stores a connector status unless it is older than the stored one (a station that
    /// replays queued reports after an offline period) or the connector cap is reached.
    fn set_connector(
        &mut self,
        evse: u32,
        connector: u32,
        status: ConnectorStatus,
        timestamp: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> bool {
        match self
            .evses
            .get(&evse)
            .and_then(|state| state.connectors.get(&connector))
        {
            Some(stored) if timestamp < stored.timestamp => return false,
            None if self.connector_count() >= MAX_CONNECTORS => return false,
            _ => {}
        }
        self.evses.entry(evse).or_default().connectors.insert(
            connector,
            Connector2State {
                status,
                timestamp,
                received_at: now,
            },
        );
        true
    }

    /// Folds one `TransactionEvent`. `answer` is our decoded response, if it decoded.
    fn fold_transaction_event(
        &mut self,
        req: TransactionEventRequest,
        answer: Option<&TransactionEventResponse>,
        history_cap: usize,
    ) -> bool {
        let id = req.transaction_info.transaction_id.clone();
        let known = self.transactions.iter().position(|tx| tx.id == id);
        let created = known.is_none();
        let index = known.unwrap_or_else(|| {
            self.transactions.push(Tx2Record::new(id, &req));
            self.transactions.len() - 1
        });
        let Some(tx) = self.transactions.get_mut(index) else {
            return false;
        };
        let highest = tx.seq.highest();
        if !tx.seq.record(req.seq_no) {
            self.counters.duplicate_events += 1;
            return false;
        }
        // Only an event at or past the highest folded `seqNo` of a running transaction may
        // replace what is stored; any other fills what is still unknown.
        let latest = tx.status == TransactionStatus::Active
            && highest.is_none_or(|highest| req.seq_no >= highest);
        let was_active = tx.status == TransactionStatus::Active;
        tx.fold_event(req, answer, latest);
        let stopped = was_active && tx.status == TransactionStatus::Stopped;
        self.counters.transaction_events += 1;
        if created || stopped {
            self.trim_transactions(history_cap);
        }
        true
    }

    /// Drops the oldest active transactions beyond [`MAX_ACTIVE_TRANSACTIONS`] and the
    /// oldest stopped ones beyond `history_cap`.
    fn trim_transactions(&mut self, history_cap: usize) {
        let active = self.active_count();
        let stopped = self.transactions.len() - active;
        let mut drop_active = active.saturating_sub(MAX_ACTIVE_TRANSACTIONS);
        let mut drop_stopped = stopped.saturating_sub(history_cap);
        self.transactions.retain(|tx| {
            let pending = match tx.status {
                TransactionStatus::Active => &mut drop_active,
                TransactionStatus::Stopped => &mut drop_stopped,
            };
            if *pending > 0 {
                *pending -= 1;
                false
            } else {
                true
            }
        });
    }
}

/// One EVSE's connectors.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EvseState {
    /// Connector number (1 and up) to its last reported status.
    pub connectors: BTreeMap<u32, Connector2State>,
}

/// One connector's last reported status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Connector2State {
    pub status: ConnectorStatus,
    /// The timestamp the station put in the report.
    pub timestamp: DateTime<Utc>,
    /// When we received it (our clock).
    pub received_at: DateTime<Utc>,
}

/// Latest readings of one EVSE from `MeterValues`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MeterReading {
    /// Latest `Energy.Active.Import.Register`, Wh.
    pub energy_wh: Option<f64>,
    /// Latest `Power.Active.Import`, W.
    pub power_w: Option<f64>,
    /// The station's timestamp of the newest reading folded.
    pub last_meter_at: Option<DateTime<Utc>>,
}

/// Message counters of a 2.0.1 station.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Counters2 {
    /// `TransactionEvent`s folded (not duplicates, not rejected).
    pub transaction_events: u64,
    /// `TransactionEvent`s whose `seqNo` was already folded for the transaction.
    pub duplicate_events: u64,
    /// `TransactionEvent`s with an empty or over-long `transactionId`.
    pub rejected_events: u64,
    /// `MeterValues` accepted.
    pub meter_values: u64,
    /// `NotifyReport`s accepted.
    pub notify_reports: u64,
}

/// The last `NotifyReport`, without its content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportInfo {
    pub request_id: i32,
    pub seq_no: i32,
    /// Whether more parts follow.
    pub tbc: bool,
    /// Our clock.
    pub at: DateTime<Utc>,
}

/// One transaction, from its first `TransactionEvent` on.
///
/// Optional fields merge: a message that omits one leaves it as it is.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tx2Record {
    /// The station-made id, at most 36 characters.
    pub id: String,
    pub evse: Option<u32>,
    pub connector: Option<u32>,
    /// At most 36 characters.
    pub id_token: Option<String>,
    pub id_token_type: Option<IdTokenType>,
    /// The `idTokenInfo.status` we answered for the token.
    pub id_token_status: Option<AuthorizationStatus>,
    pub status: TransactionStatus,
    pub charging_state: Option<ChargingState>,
    pub remote_start_id: Option<i32>,
    pub reservation_id: Option<i32>,
    /// The trigger of the event with the highest `seqNo`.
    pub trigger_reason: TriggerReason,
    /// The earliest event timestamp seen; never absent.
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
    /// `None` while active, or when the station sent none (allowed for `Local`).
    pub stopped_reason: Option<StopReason>,
    /// Seconds.
    pub time_spent_charging: Option<i32>,
    /// Latest `Energy.Active.Import.Register`, Wh.
    pub energy_wh: Option<f64>,
    /// Latest `Power.Active.Import`, W.
    pub power_w: Option<f64>,
    /// Register at the `Transaction.Begin` sample, Wh rounded.
    pub meter_start_wh: Option<i64>,
    /// Register at the `Transaction.End` sample, Wh rounded.
    pub meter_stop_wh: Option<i64>,
    /// The station's timestamp of the newest reading folded.
    pub last_meter_at: Option<DateTime<Utc>>,
    pub seq: SeqTracker,
}

impl Tx2Record {
    fn new(id: String, req: &TransactionEventRequest) -> Self {
        Self {
            id,
            evse: None,
            connector: None,
            id_token: None,
            id_token_type: None,
            id_token_status: None,
            status: TransactionStatus::Active,
            charging_state: None,
            remote_start_id: None,
            reservation_id: None,
            trigger_reason: req.trigger_reason,
            started_at: req.timestamp,
            stopped_at: None,
            stopped_reason: None,
            time_spent_charging: None,
            energy_wh: None,
            power_w: None,
            meter_start_wh: None,
            meter_stop_wh: None,
            last_meter_at: None,
            seq: SeqTracker::default(),
        }
    }

    fn row(&self) -> TransactionRow {
        TransactionRow {
            id: TransactionId::Text(self.id.clone()),
            connector: self.connector,
            id_tag: self.id_token.clone(),
            id_tag_status: self
                .id_token_status
                .map(|status| status.as_str().to_owned()),
            meter_start: self.meter_start_wh,
            started_at: self.started_at,
            status: self.status,
            meter_stop: self.meter_stop_wh,
            stopped_at: self.stopped_at,
            stop_reason: self.stopped_reason.map(|reason| reason.as_str().to_owned()),
            energy_wh: self.energy_wh,
            power_w: self.power_w,
            last_meter_at: self.last_meter_at,
            evse: self.evse,
        }
    }

    /// Merges one event into the record. `latest` tells whether the event may replace
    /// stored values (see the caller).
    fn fold_event(
        &mut self,
        req: TransactionEventRequest,
        answer: Option<&TransactionEventResponse>,
        latest: bool,
    ) {
        self.started_at = self.started_at.min(req.timestamp);
        if latest {
            self.trigger_reason = req.trigger_reason;
        }
        let info = req.transaction_info;
        merge(&mut self.charging_state, info.charging_state, latest);
        merge(&mut self.remote_start_id, info.remote_start_id, latest);
        merge(
            &mut self.time_spent_charging,
            info.time_spent_charging,
            latest,
        );
        merge(&mut self.reservation_id, req.reservation_id, latest);
        if let Some(evse) = req.evse
            && let Some(evse_id) = positive(evse.id)
        {
            merge(&mut self.evse, Some(evse_id), latest);
            merge(
                &mut self.connector,
                evse.connector_id.and_then(positive),
                latest,
            );
        }
        if let Some(token) = req.id_token
            && (latest || self.id_token.is_none())
        {
            let token_id = truncate(token.id_token, MAX_ID_TOKEN_LEN);
            if self.id_token.as_ref() != Some(&token_id) {
                self.id_token_status = None;
            }
            self.id_token = Some(token_id);
            self.id_token_type = Some(token.kind);
            let status = answer
                .and_then(|answer| answer.id_token_info.as_ref())
                .map(|info| info.status);
            merge(&mut self.id_token_status, status, true);
        }
        for meter in req.meter_value.iter().flatten() {
            self.fold_meter(meter);
        }
        if req.event_type == TransactionEventKind::Ended {
            if self.status == TransactionStatus::Active {
                self.status = TransactionStatus::Stopped;
                self.stopped_at = Some(req.timestamp);
            }
            merge(&mut self.stopped_reason, info.stopped_reason, false);
        }
    }

    fn fold_meter(&mut self, meter: &MeterValue) {
        let readings = Readings::from_meter(meter);
        if self.meter_start_wh.is_none() {
            self.meter_start_wh = readings.start_wh;
        }
        merge(&mut self.meter_stop_wh, readings.stop_wh, true);
        if self
            .last_meter_at
            .is_some_and(|last| meter.timestamp < last)
        {
            return;
        }
        if readings.energy_wh.is_none() && readings.power_w.is_none() {
            return;
        }
        merge(&mut self.energy_wh, readings.energy_wh, true);
        merge(&mut self.power_w, readings.power_w, true);
        self.last_meter_at = Some(meter.timestamp);
    }
}

/// The `seqNo`s of one transaction that were folded, as a bounded list of ranges.
///
/// Beyond [`MAX_SEQ_RANGES`] disjoint ranges the lowest one is forgotten. Ranges are never
/// merged across a gap: that would mark unseen numbers as seen, and a late event inside the
/// gap would be dropped as a duplicate. Folding a forgotten old `seqNo` again is harmless:
/// meter readings are guarded by their timestamps and a stopped transaction stays stopped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SeqTracker {
    /// Ascending, disjoint, non-adjacent `(first, last)` pairs.
    ranges: Vec<(i32, i32)>,
    /// Events whose `seqNo` was already folded.
    pub duplicates: u32,
    /// Events folded with a `seqNo` below the highest one seen before.
    pub out_of_order: u32,
}

impl SeqTracker {
    /// Records `seq`. Returns `false`, and counts a duplicate, if it was already recorded.
    pub(crate) fn record(&mut self, seq: i32) -> bool {
        let pos = self.ranges.partition_point(|&(_, last)| last < seq);
        if self.ranges.get(pos).is_some_and(|&(first, _)| first <= seq) {
            self.duplicates = self.duplicates.saturating_add(1);
            return false;
        }
        if self.highest().is_some_and(|highest| seq < highest) {
            self.out_of_order = self.out_of_order.saturating_add(1);
        }
        let prev = pos.checked_sub(1);
        let after_prev = prev
            .and_then(|index| self.ranges.get(index))
            .is_some_and(|&(_, last)| i64::from(last) + 1 == i64::from(seq));
        let before_next = self
            .ranges
            .get(pos)
            .is_some_and(|&(first, _)| i64::from(first) - 1 == i64::from(seq));
        match (after_prev, before_next) {
            (true, true) => {
                let (_, last) = self.ranges.remove(pos);
                if let Some(range) = prev.and_then(|index| self.ranges.get_mut(index)) {
                    range.1 = last;
                }
            }
            (true, false) => {
                if let Some(range) = prev.and_then(|index| self.ranges.get_mut(index)) {
                    range.1 = seq;
                }
            }
            (false, true) => {
                if let Some(range) = self.ranges.get_mut(pos) {
                    range.0 = seq;
                }
            }
            (false, false) => {
                self.ranges.insert(pos, (seq, seq));
                if self.ranges.len() > MAX_SEQ_RANGES {
                    self.ranges.remove(0);
                }
            }
        }
        true
    }

    /// The highest `seqNo` recorded.
    pub fn highest(&self) -> Option<i32> {
        self.ranges.last().map(|&(_, last)| last)
    }

    /// How many numbers between the first and the last recorded one were never seen.
    pub fn missing(&self) -> u64 {
        self.ranges
            .windows(2)
            .map(|pair| match pair {
                [(_, last), (first, _)] => {
                    u64::try_from(i64::from(*first) - i64::from(*last) - 1).unwrap_or(0)
                }
                _ => 0,
            })
            .sum()
    }
}

/// Energy and power readings, and the register at the transaction edges, of one
/// `MeterValue`. Later samples win.
#[derive(Debug, Default)]
struct Readings {
    energy_wh: Option<f64>,
    power_w: Option<f64>,
    start_wh: Option<i64>,
    stop_wh: Option<i64>,
}

impl Readings {
    fn from_meter(meter: &MeterValue) -> Self {
        let mut readings = Self::default();
        for sample in &meter.sampled_value {
            match reading(sample) {
                Some(Reading::Energy(wh)) => {
                    readings.energy_wh = Some(wh);
                    match sample.context {
                        Some(ReadingContext::TransactionBegin) => {
                            readings.start_wh = rounded(wh);
                        }
                        Some(ReadingContext::TransactionEnd) => readings.stop_wh = rounded(wh),
                        _ => {}
                    }
                }
                Some(Reading::Power(w)) => readings.power_w = Some(w),
                None => {}
            }
        }
        readings
    }
}

enum Reading {
    Energy(f64),
    Power(f64),
}

/// A sample as Wh or W: `value * 10^multiplier`, kilo units x1000. Phase samples, other
/// measurands, other units and non-finite results are skipped.
fn reading(sample: &SampledValue) -> Option<Reading> {
    if sample.phase.is_some() {
        return None;
    }
    let unit = sample.unit_of_measure.as_ref();
    let name = unit.and_then(|unit| unit.unit.as_deref());
    let raw = scale(
        sample.value.as_f64()?,
        unit.and_then(|unit| unit.multiplier).unwrap_or(0),
    );
    let reading = match sample
        .measurand
        .unwrap_or(Measurand::EnergyActiveImportRegister)
    {
        Measurand::EnergyActiveImportRegister => Reading::Energy(match name.unwrap_or("Wh") {
            "Wh" => raw,
            "kWh" => raw * 1000.0,
            _ => return None,
        }),
        Measurand::PowerActiveImport => Reading::Power(match name.unwrap_or("W") {
            "W" => raw,
            "kW" => raw * 1000.0,
            _ => return None,
        }),
        _ => return None,
    };
    let value = match reading {
        Reading::Energy(value) | Reading::Power(value) => value,
    };
    value.is_finite().then_some(reading)
}

/// `value * 10^multiplier`; divides for a negative multiplier to keep decimals exact.
fn scale(value: f64, multiplier: i32) -> f64 {
    if multiplier >= 0 {
        value * 10f64.powi(multiplier)
    } else {
        multiplier
            .checked_neg()
            .map_or(f64::NAN, |exponent| value / 10f64.powi(exponent))
    }
}

/// `value` as a whole number, `None` outside the `i64` range.
#[allow(clippy::cast_possible_truncation)]
fn rounded(value: f64) -> Option<i64> {
    (value.abs() < 9.0e18).then(|| value.round() as i64)
}

/// Sets `slot` from `value` when `value` is present and either the event is `latest` or
/// the slot is still empty. An absent `value` never clears the slot.
fn merge<T>(slot: &mut Option<T>, value: Option<T>, latest: bool) {
    if let Some(value) = value
        && (latest || slot.is_none())
    {
        *slot = Some(value);
    }
}

/// An id of 1 or more; `0` and negative ids are not addressable EVSEs or connectors.
fn positive(id: i32) -> Option<u32> {
    u32::try_from(id).ok().filter(|&id| id >= 1)
}

/// Cuts `text` to at most `max` characters.
fn truncate(text: String, max: usize) -> String {
    if text.chars().count() <= max {
        text
    } else {
        text.chars().take(max).collect()
    }
}

fn legacy_registration(status: RegistrationStatus) -> LegacyRegistrationStatus {
    match status {
        RegistrationStatus::Accepted => LegacyRegistrationStatus::Accepted,
        RegistrationStatus::Pending => LegacyRegistrationStatus::Pending,
        RegistrationStatus::Rejected => LegacyRegistrationStatus::Rejected,
    }
}

/// A `NotifyEvent` entry that reports a connector state
/// (`Connector.AvailabilityState`, trigger `Delta`): EVSE, connector, status, event time.
fn connector_event(event: &Value) -> Option<(u32, u32, ConnectorStatus, DateTime<Utc>)> {
    let component = event.get("component")?;
    if component.get("name")?.as_str()? != "Connector"
        || event.get("variable")?.get("name")?.as_str()? != "AvailabilityState"
        || event.get("trigger")?.as_str()? != "Delta"
    {
        return None;
    }
    let location = component.get("evse")?;
    let evse = positive(i32::try_from(location.get("id")?.as_i64()?).ok()?)?;
    let connector = positive(i32::try_from(location.get("connectorId")?.as_i64()?).ok()?)?;
    let value = event.get("actualValue")?.as_str()?;
    let status = ConnectorStatus::ALL
        .iter()
        .find(|status| status.as_str() == value)
        .copied()?;
    let timestamp = DateTime::parse_from_rfc3339(event.get("timestamp")?.as_str()?)
        .ok()?
        .with_timezone(&Utc);
    Some((evse, connector, status, timestamp))
}

impl StationState {
    /// Folds one answered CALL of a station on OCPP 2.0.1 into the state. `response` is the
    /// CALLRESULT payload we sent. Returns whether the state changed.
    ///
    /// Updates that only bump a counter (a rejected or repeated `TransactionEvent`) and
    /// heartbeats do not count as a change. `protocol` other than 2.0.1 folds nothing.
    pub(crate) fn apply_v2(
        &mut self,
        protocol: Protocol,
        action: &str,
        request: &Value,
        response: &Value,
        now: DateTime<Utc>,
        history_cap: usize,
    ) -> bool {
        if protocol != Protocol::V201 {
            return false;
        }
        match action {
            "BootNotification" => self.v2_boot(request, response, now),
            "Heartbeat" => {
                // Recorded silently, as in 1.6: heartbeats come constantly.
                self.last_heartbeat_at = Some(now);
                false
            }
            "StatusNotification" => self.v2_status(request, now),
            "NotifyEvent" => self.v2_notify_event(request, now),
            "TransactionEvent" => self.v2_transaction_event(request, response, history_cap),
            "MeterValues" => self.v2_meter_values(request),
            "SecurityEventNotification" => self.v2_security_event(request, now),
            "NotifyReport" => self.v2_notify_report(request, now),
            _ => false,
        }
    }

    fn v2_mut(&mut self) -> &mut StationState2 {
        self.v2
            .get_or_insert_with(|| Box::new(StationState2::new(Protocol::V201)))
    }

    fn v2_boot(&mut self, request: &Value, response: &Value, now: DateTime<Utc>) -> bool {
        let Ok(req) = BootNotificationRequest::deserialize(request) else {
            return false;
        };
        let res = BootNotificationResponse::deserialize(response).ok();
        let station = req.charging_station;
        let text = |value: Option<String>, max: usize| value.map(|text| truncate(text, max));
        let (iccid, imsi) = station
            .modem
            .map_or((None, None), |modem| (modem.iccid, modem.imsi));
        self.boot = Some(BootInfo {
            vendor: truncate(station.vendor_name, MAX_VENDOR_NAME_LEN),
            model: truncate(station.model, MAX_MODEL_LEN),
            charge_point_serial_number: text(station.serial_number, MAX_SERIAL_NUMBER_LEN),
            charge_box_serial_number: None,
            firmware_version: text(station.firmware_version, MAX_FIRMWARE_VERSION_LEN),
            iccid: text(iccid, MAX_MODEM_ID_LEN),
            imsi: text(imsi, MAX_MODEM_ID_LEN),
            meter_type: None,
            meter_serial_number: None,
            booted_at: now,
            registration: res.as_ref().map(|res| legacy_registration(res.status)),
            interval: res.map(|res| res.interval),
        });
        self.v2_mut().boot_reason = Some(req.reason);
        true
    }

    fn v2_status(&mut self, request: &Value, now: DateTime<Utc>) -> bool {
        let Ok(req) = StatusNotificationRequest::deserialize(request) else {
            return false;
        };
        let (Some(evse), Some(connector)) = (positive(req.evse_id), positive(req.connector_id))
        else {
            return false;
        };
        self.v2_mut()
            .set_connector(evse, connector, req.connector_status, req.timestamp, now)
    }

    /// A station may report a connector's state as a `NotifyEvent` on
    /// `Connector.AvailabilityState` instead of a `StatusNotification`. Only that event is
    /// folded; nothing else of the message is kept.
    fn v2_notify_event(&mut self, request: &Value, now: DateTime<Utc>) -> bool {
        let Some(events) = request.get("eventData").and_then(Value::as_array) else {
            return false;
        };
        let mut changed = false;
        for (evse, connector, status, timestamp) in events.iter().filter_map(connector_event) {
            changed |= self
                .v2_mut()
                .set_connector(evse, connector, status, timestamp, now);
        }
        changed
    }

    fn v2_transaction_event(
        &mut self,
        request: &Value,
        response: &Value,
        history_cap: usize,
    ) -> bool {
        let Ok(req) = TransactionEventRequest::deserialize(request) else {
            return false;
        };
        let id_len = req.transaction_info.transaction_id.chars().count();
        if id_len == 0 || id_len > MAX_TRANSACTION_ID_LEN {
            self.v2_mut().counters.rejected_events += 1;
            return false;
        }
        let answer = TransactionEventResponse::deserialize(response).ok();
        self.v2_mut()
            .fold_transaction_event(req, answer.as_ref(), history_cap)
    }

    /// `MeterValues` outside a transaction: latest energy and power per EVSE.
    fn v2_meter_values(&mut self, request: &Value) -> bool {
        let Ok(req) = MeterValuesRequest::deserialize(request) else {
            return false;
        };
        let Ok(evse) = u32::try_from(req.evse_id) else {
            return false;
        };
        let v2 = self.v2_mut();
        v2.counters.meter_values += 1;
        let mut changed = false;
        for meter in &req.meter_value {
            let readings = Readings::from_meter(meter);
            if readings.energy_wh.is_none() && readings.power_w.is_none() {
                continue;
            }
            if !v2.meters.contains_key(&evse) && v2.meters.len() >= MAX_METER_EVSES {
                break;
            }
            let stored = v2.meters.entry(evse).or_default();
            if stored
                .last_meter_at
                .is_some_and(|last| meter.timestamp < last)
            {
                continue;
            }
            merge(&mut stored.energy_wh, readings.energy_wh, true);
            merge(&mut stored.power_w, readings.power_w, true);
            stored.last_meter_at = Some(meter.timestamp);
            changed = true;
        }
        changed
    }

    fn v2_security_event(&mut self, request: &Value, now: DateTime<Utc>) -> bool {
        let Ok(req) = SecurityEventNotificationRequest::deserialize(request) else {
            return false;
        };
        self.security_events.push_back(SecurityEventRecord {
            event_type: truncate(req.kind, MAX_EVENT_TYPE_LEN),
            timestamp: req.timestamp,
            tech_info: req.tech_info.map(|text| truncate(text, MAX_TECH_INFO_LEN)),
            received_at: now,
        });
        while self.security_events.len() > MAX_SECURITY_EVENTS {
            self.security_events.pop_front();
        }
        self.security_events_total += 1;
        true
    }

    /// Counts the report. Device-model values are never stored: the state leaves through
    /// the API unredacted.
    fn v2_notify_report(&mut self, request: &Value, now: DateTime<Utc>) -> bool {
        let Ok(req) = NotifyReportRequest::deserialize(request) else {
            return false;
        };
        let v2 = self.v2_mut();
        v2.counters.notify_reports += 1;
        v2.last_report = Some(ReportInfo {
            request_id: req.request_id,
            seq_no: req.seq_no,
            tbc: req.tbc.unwrap_or(false),
            at: now,
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::state::TransactionRecord;

    /// The clock the fold sees.
    fn now() -> DateTime<Utc> {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }

    /// An event timestamp `sec` seconds into the test day.
    fn at(sec: u32) -> String {
        format!(
            "2026-01-01T{:02}:{:02}:{:02}.000Z",
            sec / 3600,
            sec / 60 % 60,
            sec % 60
        )
    }

    fn time(sec: u32) -> DateTime<Utc> {
        at(sec).parse().unwrap()
    }

    fn fold_with(
        state: &mut StationState,
        action: &str,
        request: &Value,
        response: &Value,
        history_cap: usize,
    ) -> bool {
        state.apply_v2(
            Protocol::V201,
            action,
            request,
            response,
            now(),
            history_cap,
        )
    }

    fn fold(state: &mut StationState, action: &str, request: &Value) -> bool {
        fold_with(state, action, request, &json!({}), 100)
    }

    /// Recursively overlays `patch` on `target`.
    fn overlay(target: &mut Value, patch: &Value) {
        match (target, patch) {
            (Value::Object(target), Value::Object(patch)) => {
                for (key, value) in patch {
                    overlay(target.entry(key.clone()).or_insert(Value::Null), value);
                }
            }
            (target, patch) => *target = patch.clone(),
        }
    }

    /// A `TransactionEvent` of transaction `tx-1`; `patch` overlays extra fields.
    fn event(kind: &str, seq: i32, sec: u32, patch: &Value) -> Value {
        event_for("tx-1", kind, seq, sec, patch)
    }

    fn event_for(id: &str, kind: &str, seq: i32, sec: u32, patch: &Value) -> Value {
        let mut value = json!({
            "eventType": kind,
            "timestamp": at(sec),
            "triggerReason": "Authorized",
            "seqNo": seq,
            "transactionInfo": {"transactionId": id}
        });
        overlay(&mut value, patch);
        value
    }

    fn send(state: &mut StationState, request: &Value) -> bool {
        fold(state, "TransactionEvent", request)
    }

    fn energy(sec: u32, value: &Value) -> Value {
        json!({"meterValue": [{"timestamp": at(sec), "sampledValue": [{"value": value}]}]})
    }

    fn first_tx(state: &StationState) -> &Tx2Record {
        state.v2.as_ref().unwrap().transactions.first().unwrap()
    }

    fn v2(state: &StationState) -> &StationState2 {
        state.v2.as_ref().unwrap()
    }

    fn assert_close(actual: Option<f64>, expected: f64) {
        let actual = actual.unwrap();
        assert!(
            (actual - expected).abs() < 1e-6,
            "{actual} is not {expected}"
        );
    }

    // ---- 1.6 stays as it was ----

    /// A 1.6 station folded through the 1.6 code, with fixed clocks.
    fn legacy_state() -> StationState {
        let now = now();
        let mut state = StationState::default();
        let boot = json!({"chargePointVendor": "Acme", "chargePointModel": "C1",
            "chargePointSerialNumber": "SN-1", "firmwareVersion": "1.0"});
        let boot_res = json!({"status": "Accepted", "currentTime": "2026-01-01T00:00:00.000Z",
            "interval": 300});
        state.apply("BootNotification", &boot, &boot_res, now, 5);
        let status = json!({"connectorId": 1, "errorCode": "NoError", "status": "Charging",
            "timestamp": "2026-01-01T00:00:01.000Z"});
        state.apply("StatusNotification", &status, &json!({}), now, 5);
        for (id, tag) in [(7, "TAG-A"), (8, "TAG-B")] {
            let start = json!({"connectorId": 1, "idTag": tag, "meterStart": 10,
                "timestamp": "2026-01-01T00:00:02.000Z"});
            let res = json!({"transactionId": id, "idTagInfo": {"status": "Accepted"}});
            state.apply("StartTransaction", &start, &res, now, 5);
        }
        let stop = json!({"transactionId": 8, "meterStop": 99,
            "timestamp": "2026-01-01T01:00:00.000Z", "reason": "Remote"});
        state.apply("StopTransaction", &stop, &json!({}), now, 5);
        let meter = json!({"connectorId": 1, "transactionId": 7, "meterValue": [{
            "timestamp": "2026-01-01T00:10:00.000Z",
            "sampledValue": [{"value": "1.5", "unit": "kWh"},
                {"value": "7000", "measurand": "Power.Active.Import", "unit": "W"}]}]});
        state.apply("MeterValues", &meter, &json!({}), now, 5);
        let sec = json!({"type": "SettingSystemTime", "timestamp": "2026-01-01T00:00:03.000Z",
            "techInfo": "x"});
        state.apply("SecurityEventNotification", &sec, &json!({}), now, 5);
        state.apply("Heartbeat", &json!({}), &json!({}), now, 5);
        state
    }

    #[test]
    fn legacy_state_json_is_unchanged() {
        // Captured from the 1.6 code before the 2.0.1 side state existed.
        const GOLDEN: &str = r#"{"boot":{"vendor":"Acme","model":"C1","charge_point_serial_number":"SN-1","charge_box_serial_number":null,"firmware_version":"1.0","iccid":null,"imsi":null,"meter_type":null,"meter_serial_number":null,"booted_at":"2026-01-01T00:00:00Z","registration":"Accepted","interval":300},"connectors":{"1":{"status":"Charging","error_code":"NoError","info":null,"vendor_id":null,"vendor_error_code":null,"timestamp":"2026-01-01T00:00:01Z","received_at":"2026-01-01T00:00:00Z"}},"transactions":[{"id":7,"connector":1,"id_tag":"TAG-A","id_tag_status":"Accepted","meter_start":10,"started_at":"2026-01-01T00:00:02Z","status":"active","meter_stop":null,"stopped_at":null,"stop_reason":null,"energy_wh":1500.0,"power_w":7000.0,"last_meter_at":"2026-01-01T00:10:00Z"},{"id":8,"connector":1,"id_tag":"TAG-B","id_tag_status":"Accepted","meter_start":10,"started_at":"2026-01-01T00:00:02Z","status":"stopped","meter_stop":99,"stopped_at":"2026-01-01T01:00:00Z","stop_reason":"Remote","energy_wh":null,"power_w":null,"last_meter_at":null}],"last_heartbeat_at":"2026-01-01T00:00:00Z","diagnostics_status":null,"firmware_status":null,"failed_calls":0,"unknown_stops":0,"security_events":[{"event_type":"SettingSystemTime","timestamp":"2026-01-01T00:00:03Z","tech_info":"x","received_at":"2026-01-01T00:00:00Z"}],"security_events_total":1,"log_status":null,"signed_firmware_status":null,"certificate_request":null,"installed_certificates":{}}"#;
        let state = legacy_state();
        assert!(state.v2.is_none());
        assert_eq!(serde_json::to_string(&state).unwrap(), GOLDEN);
    }

    #[test]
    fn legacy_row_json_is_the_old_record_json_plus_evse() {
        const OLD_ACTIVE: &str = r#"{"id":7,"connector":1,"id_tag":"TAG-A","id_tag_status":"Accepted","meter_start":10,"started_at":"2026-01-01T00:00:02Z","status":"active","meter_stop":null,"stopped_at":null,"stop_reason":null,"energy_wh":1500.0,"power_w":7000.0,"last_meter_at":"2026-01-01T00:10:00Z"}"#;
        const OLD_STOPPED: &str = r#"{"id":8,"connector":1,"id_tag":"TAG-B","id_tag_status":"Accepted","meter_start":10,"started_at":"2026-01-01T00:00:02Z","status":"stopped","meter_stop":99,"stopped_at":"2026-01-01T01:00:00Z","stop_reason":"Remote","energy_wh":null,"power_w":null,"last_meter_at":null}"#;
        let state = legacy_state();
        for (record, old) in state.transactions.iter().zip([OLD_ACTIVE, OLD_STOPPED]) {
            assert_eq!(serde_json::to_string(record).unwrap(), old);
            let row = serde_json::to_string(&TransactionRow::from(record)).unwrap();
            let expected = format!("{},\"evse\":null}}", old.strip_suffix('}').unwrap());
            assert_eq!(row, expected);
        }
        let rows = state.transaction_rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, TransactionId::Number(7));
    }

    #[test]
    fn legacy_accessors_map_the_1_6_vectors() {
        let state = legacy_state();
        assert_eq!(state.vendor_model(), Some(("Acme", "C1")));
        assert_eq!(
            state.connector_statuses(),
            BTreeMap::from([("1".to_owned(), "Charging".to_owned())])
        );
        assert_eq!(
            state.active_transactions(),
            [ActiveTransaction {
                id: TransactionId::Number(7),
                connector: Some(1),
                evse: None,
            }]
        );
        assert_eq!(state.active_transaction_count(), 1);
        assert_eq!(TransactionId::Number(7).to_string(), "7");
        assert_eq!(
            serde_json::to_string(&TransactionId::Text("a-1".into())).unwrap(),
            r#""a-1""#
        );
        assert_eq!(
            serde_json::to_string(&TransactionId::Number(7)).unwrap(),
            "7"
        );
        let record: &TransactionRecord = &state.transactions[0];
        assert_eq!(
            TransactionRow::from(record).id_tag.as_deref(),
            Some("TAG-A")
        );
    }

    #[test]
    fn other_protocols_fold_nothing() {
        let mut state = StationState::default();
        let req = json!({"timestamp": at(0), "connectorStatus": "Available",
            "evseId": 1, "connectorId": 1});
        for protocol in [Protocol::V16, Protocol::V21] {
            assert!(!state.apply_v2(protocol, "StatusNotification", &req, &json!({}), now(), 5));
        }
        assert_eq!(state, StationState::default());
    }

    #[test]
    fn invalid_payloads_change_nothing() {
        let mut state = StationState::default();
        for action in [
            "BootNotification",
            "StatusNotification",
            "TransactionEvent",
            "MeterValues",
            "SecurityEventNotification",
            "NotifyReport",
            "NotifyEvent",
            "Unknown",
        ] {
            assert!(!fold(&mut state, action, &json!({"x": 1})), "{action}");
        }
        assert_eq!(state, StationState::default());
    }

    // ---- shared fields ----

    #[test]
    fn boot_fills_the_shared_fields() {
        let mut state = StationState::default();
        let req = json!({
            "reason": "PowerUp",
            "chargingStation": {
                "vendorName": "V".repeat(60), "model": "M".repeat(30),
                "serialNumber": "SN-9", "firmwareVersion": "2.1",
                "modem": {"iccid": "8931", "imsi": "2620"}
            }
        });
        let res = json!({"currentTime": at(0), "interval": 120, "status": "Pending"});
        assert!(fold_with(&mut state, "BootNotification", &req, &res, 5));
        let boot = state.boot.as_ref().unwrap();
        assert_eq!(boot.vendor.len(), MAX_VENDOR_NAME_LEN);
        assert_eq!(boot.model.len(), MAX_MODEL_LEN);
        assert_eq!(boot.charge_point_serial_number.as_deref(), Some("SN-9"));
        assert_eq!(boot.firmware_version.as_deref(), Some("2.1"));
        assert_eq!(boot.iccid.as_deref(), Some("8931"));
        assert_eq!(boot.imsi.as_deref(), Some("2620"));
        assert_eq!(boot.registration, Some(LegacyRegistrationStatus::Pending));
        assert_eq!(boot.interval, Some(120));
        assert_eq!(boot.booted_at, now());
        assert_eq!(v2(&state).boot_reason, Some(BootReason::PowerUp));
        assert_eq!(v2(&state).protocol, "ocpp2.0.1");
        assert_eq!(
            state.vendor_model().map(|(vendor, _)| vendor.len()),
            Some(50)
        );

        // An answer that is not a valid BootNotification.conf leaves the registration open.
        let min =
            json!({"reason": "Watchdog", "chargingStation": {"vendorName": "V", "model": "M"}});
        assert!(fold(&mut state, "BootNotification", &min));
        let boot = state.boot.as_ref().unwrap();
        assert_eq!(boot.registration, None);
        assert_eq!(boot.interval, None);
        assert_eq!(boot.charge_point_serial_number, None);
        assert_eq!(boot.iccid, None);
    }

    #[test]
    fn heartbeat_is_recorded_silently() {
        let mut state = StationState::default();
        assert!(!fold(&mut state, "Heartbeat", &json!({})));
        assert_eq!(state.last_heartbeat_at, Some(now()));
        assert!(state.v2.is_none());
    }

    #[test]
    fn security_events_are_shared_bounded_and_cut() {
        let mut state = StationState::default();
        for n in 0..(MAX_SECURITY_EVENTS + 5) {
            let req =
                json!({"type": "X".repeat(80), "timestamp": at(0), "techInfo": n.to_string()});
            assert!(fold(&mut state, "SecurityEventNotification", &req));
        }
        assert_eq!(state.security_events.len(), MAX_SECURITY_EVENTS);
        assert_eq!(
            state.security_events_total,
            (MAX_SECURITY_EVENTS + 5) as u64
        );
        assert_eq!(
            state.security_events[0].event_type.len(),
            MAX_EVENT_TYPE_LEN
        );
        assert_eq!(state.security_events[0].tech_info.as_deref(), Some("5"));
        let long = json!({"type": "T", "timestamp": at(0), "techInfo": "i".repeat(300)});
        assert!(fold(&mut state, "SecurityEventNotification", &long));
        assert_eq!(
            state
                .security_events
                .back()
                .unwrap()
                .tech_info
                .as_ref()
                .map(String::len),
            Some(MAX_TECH_INFO_LEN)
        );
        assert!(state.v2.is_none());
    }

    #[test]
    fn notify_report_is_counted_and_its_values_are_not_stored() {
        let mut state = StationState::default();
        let req = json!({
            "requestId": 4, "generatedAt": at(0), "seqNo": 2, "tbc": true,
            "reportData": [{
                "component": {"name": "SecurityCtrlr"},
                "variable": {"name": "BasicAuthPassword"},
                "variableAttribute": [{"type": "Actual", "value": "hunter2-secret"}],
                "variableCharacteristics": {"dataType": "string", "supportsMonitoring": false}
            }]
        });
        assert!(fold(&mut state, "NotifyReport", &req));
        assert!(fold(
            &mut state,
            "NotifyReport",
            &json!({"requestId": 4, "generatedAt": at(1), "seqNo": 3})
        ));
        assert_eq!(v2(&state).counters.notify_reports, 2);
        let last = v2(&state).last_report.as_ref().unwrap();
        assert_eq!((last.request_id, last.seq_no, last.tbc), (4, 3, false));
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.contains("hunter2"), "{json}");
        assert!(!json.contains("BasicAuthPassword"), "{json}");
    }

    // ---- connectors ----

    fn status(evse: i32, connector: i32, status: &str, sec: u32) -> Value {
        json!({"timestamp": at(sec), "connectorStatus": status, "evseId": evse,
            "connectorId": connector})
    }

    #[test]
    fn status_notification_fills_the_connector_map() {
        let mut state = StationState::default();
        assert!(fold(
            &mut state,
            "StatusNotification",
            &status(1, 1, "Occupied", 10)
        ));
        assert!(fold(
            &mut state,
            "StatusNotification",
            &status(1, 2, "Available", 10)
        ));
        assert!(fold(
            &mut state,
            "StatusNotification",
            &status(2, 1, "Faulted", 10)
        ));
        assert_eq!(
            state.connector_statuses(),
            BTreeMap::from([
                ("1/1".to_owned(), "Occupied".to_owned()),
                ("1/2".to_owned(), "Available".to_owned()),
                ("2/1".to_owned(), "Faulted".to_owned()),
            ])
        );
        let stored = &v2(&state).evses[&1].connectors[&1];
        assert_eq!(
            (stored.status, stored.timestamp, stored.received_at),
            (ConnectorStatus::Occupied, time(10), now())
        );
    }

    #[test]
    fn status_notification_guards() {
        let mut state = StationState::default();
        for (evse, connector) in [(0, 1), (1, 0), (-1, 1), (1, -3)] {
            let req = status(evse, connector, "Available", 10);
            assert!(
                !fold(&mut state, "StatusNotification", &req),
                "{evse}/{connector}"
            );
        }
        assert!(state.v2.is_none());

        assert!(fold(
            &mut state,
            "StatusNotification",
            &status(1, 1, "Occupied", 20)
        ));
        // An older report (queued while offline) does not regress the state; an equal
        // timestamp is accepted.
        assert!(!fold(
            &mut state,
            "StatusNotification",
            &status(1, 1, "Available", 19)
        ));
        assert_eq!(state.connector_statuses()["1/1"], "Occupied");
        assert!(fold(
            &mut state,
            "StatusNotification",
            &status(1, 1, "Reserved", 20)
        ));
        assert_eq!(state.connector_statuses()["1/1"], "Reserved");
        assert!(fold(
            &mut state,
            "StatusNotification",
            &status(1, 1, "Available", 21)
        ));
        assert_eq!(state.connector_statuses()["1/1"], "Available");
    }

    #[test]
    fn connectors_are_capped() {
        let mut state = StationState::default();
        for n in 1..=i32::try_from(MAX_CONNECTORS).unwrap() {
            assert!(fold(
                &mut state,
                "StatusNotification",
                &status(n, 1, "Available", 1)
            ));
        }
        assert!(!fold(
            &mut state,
            "StatusNotification",
            &status(5000, 1, "Available", 1)
        ));
        assert!(!fold(
            &mut state,
            "StatusNotification",
            &status(1, 2, "Available", 1)
        ));
        assert_eq!(state.connector_statuses().len(), MAX_CONNECTORS);
        // Known connectors keep updating.
        assert!(fold(
            &mut state,
            "StatusNotification",
            &status(7, 1, "Faulted", 2)
        ));
        assert_eq!(state.connector_statuses()["7/1"], "Faulted");
    }

    fn notify_event(patch: &Value) -> Value {
        let mut data = json!({
            "eventId": 1, "timestamp": at(30), "trigger": "Delta",
            "actualValue": "Occupied", "eventNotificationType": "HardWiredNotification",
            "component": {"name": "Connector", "evse": {"id": 1, "connectorId": 2}},
            "variable": {"name": "AvailabilityState"}
        });
        overlay(&mut data, patch);
        json!({"generatedAt": at(31), "seqNo": 0, "eventData": [data]})
    }

    #[test]
    fn notify_event_reports_a_connector() {
        let mut state = StationState::default();
        assert!(fold(&mut state, "NotifyEvent", &notify_event(&json!({}))));
        assert_eq!(state.connector_statuses()["1/2"], "Occupied");
        assert_eq!(v2(&state).evses[&1].connectors[&2].timestamp, time(30));

        // Older than the stored state: ignored, as for a StatusNotification.
        let older = notify_event(&json!({"timestamp": at(29), "actualValue": "Available"}));
        assert!(!fold(&mut state, "NotifyEvent", &older));
        let old_report = status(1, 2, "Available", 10);
        assert!(!fold(&mut state, "StatusNotification", &old_report));
        assert_eq!(state.connector_statuses()["1/2"], "Occupied");
        let newer = notify_event(&json!({"timestamp": at(40), "actualValue": "Unavailable"}));
        assert!(fold(&mut state, "NotifyEvent", &newer));
        assert_eq!(state.connector_statuses()["1/2"], "Unavailable");
    }

    #[test]
    fn notify_event_ignores_everything_else() {
        let cases = [
            json!({"component": {"name": "EVSE"}}),
            json!({"variable": {"name": "Power"}}),
            json!({"trigger": "Periodic"}),
            json!({"actualValue": "Charging"}),
            json!({"component": {"evse": {"id": 0}}}),
            json!({"component": {"evse": {"connectorId": 0}}}),
            json!({"timestamp": "yesterday"}),
        ];
        for patch in cases {
            let mut state = StationState::default();
            assert!(
                !fold(&mut state, "NotifyEvent", &notify_event(&patch)),
                "{patch}"
            );
            assert!(state.v2.is_none(), "{patch}");
        }
        let mut no_evse = notify_event(&json!({}));
        no_evse["eventData"][0]["component"] = json!({"name": "Connector"});
        let mut state = StationState::default();
        assert!(!fold(&mut state, "NotifyEvent", &no_evse));
        assert!(state.v2.is_none());
        // Entries are judged one by one.
        let mut mixed = notify_event(&json!({}));
        mixed["eventData"] = json!([{"eventId": 1}, mixed["eventData"][0].clone()]);
        assert!(fold(&mut state, "NotifyEvent", &mixed));
        assert_eq!(state.connector_statuses().len(), 1);
    }

    // ---- transactions ----

    #[test]
    fn omitted_fields_survive_started_updated_ended() {
        let mut state = StationState::default();
        let started = event(
            "Started",
            0,
            10,
            &json!({
                "evse": {"id": 2, "connectorId": 1},
                "idToken": {"idToken": "TAG-1", "type": "ISO14443"},
                "reservationId": 9,
                "transactionInfo": {"chargingState": "EVConnected", "remoteStartId": 5}
            }),
        );
        let answer = json!({"idTokenInfo": {"status": "Accepted"}});
        assert!(fold_with(
            &mut state,
            "TransactionEvent",
            &started,
            &answer,
            5
        ));
        let tx = first_tx(&state);
        assert_eq!((tx.evse, tx.connector), (Some(2), Some(1)));
        assert_eq!(tx.id_token.as_deref(), Some("TAG-1"));
        assert_eq!(tx.id_token_type, Some(IdTokenType::Iso14443));
        assert_eq!(tx.id_token_status, Some(AuthorizationStatus::Accepted));
        assert_eq!((tx.reservation_id, tx.remote_start_id), (Some(9), Some(5)));
        assert_eq!(tx.charging_state, Some(ChargingState::EvConnected));
        assert_eq!(tx.status, TransactionStatus::Active);
        assert_eq!(tx.started_at, time(10));

        // Updated omits evse, token, reservation and remote start: all stay.
        let updated = event(
            "Updated",
            1,
            20,
            &json!({
                "triggerReason": "ChargingStateChanged",
                "transactionInfo": {"chargingState": "Charging", "timeSpentCharging": 5}
            }),
        );
        assert!(send(&mut state, &updated));
        let tx = first_tx(&state);
        assert_eq!((tx.evse, tx.connector), (Some(2), Some(1)));
        assert_eq!(tx.id_token.as_deref(), Some("TAG-1"));
        assert_eq!(tx.id_token_status, Some(AuthorizationStatus::Accepted));
        assert_eq!((tx.reservation_id, tx.remote_start_id), (Some(9), Some(5)));
        assert_eq!(tx.charging_state, Some(ChargingState::Charging));
        assert_eq!(tx.trigger_reason, TriggerReason::ChargingStateChanged);
        assert_eq!(tx.time_spent_charging, Some(5));

        // Ended omits the charging state: it stays; the reason is new.
        let ended = event(
            "Ended",
            2,
            30,
            &json!({
                "triggerReason": "EVDeparted",
                "transactionInfo": {"stoppedReason": "EVDisconnected"}
            }),
        );
        assert!(send(&mut state, &ended));
        let tx = first_tx(&state);
        assert_eq!(tx.status, TransactionStatus::Stopped);
        assert_eq!(tx.stopped_at, Some(time(30)));
        assert_eq!(tx.stopped_reason, Some(StopReason::EvDisconnected));
        assert_eq!(tx.charging_state, Some(ChargingState::Charging));
        assert_eq!((tx.evse, tx.connector), (Some(2), Some(1)));
        assert_eq!(tx.id_token.as_deref(), Some("TAG-1"));
        assert_eq!(tx.trigger_reason, TriggerReason::EvDeparted);
        assert_eq!(tx.started_at, time(10));
        assert_eq!(v2(&state).counters.transaction_events, 3);
        assert_eq!(tx.seq.missing(), 0);
    }

    #[test]
    fn ended_without_reason_is_a_normal_stop() {
        let mut state = StationState::default();
        assert!(send(&mut state, &event("Started", 0, 10, &json!({}))));
        assert!(send(&mut state, &event("Ended", 1, 20, &json!({}))));
        let tx = first_tx(&state);
        assert_eq!(tx.status, TransactionStatus::Stopped);
        assert_eq!(tx.stopped_reason, None);
    }

    #[test]
    fn a_repeated_seq_no_folds_once() {
        let mut state = StationState::default();
        let first = event(
            "Started",
            0,
            10,
            &json!({"evse": {"id": 1}, "meterValue": [
            {"timestamp": at(10), "sampledValue": [{"value": 100}]}]}),
        );
        assert!(send(&mut state, &first));
        // Same seqNo, different content: nothing is folded and the repeat is counted.
        let repeat = event(
            "Started",
            0,
            50,
            &json!({"evse": {"id": 3}, "meterValue": [
            {"timestamp": at(50), "sampledValue": [{"value": 999}]}]}),
        );
        assert!(!send(&mut state, &repeat));
        assert!(!send(&mut state, &repeat));
        let tx = first_tx(&state);
        assert_eq!(tx.evse, Some(1));
        assert_close(tx.energy_wh, 100.0);
        assert_eq!(tx.started_at, time(10));
        assert_eq!(tx.seq.duplicates, 2);
        assert_eq!(v2(&state).counters.transaction_events, 1);
        assert_eq!(v2(&state).counters.duplicate_events, 2);
        assert_eq!(v2(&state).transactions.len(), 1);
    }

    #[test]
    fn seq_no_does_not_have_to_start_at_zero_or_be_contiguous() {
        let mut state = StationState::default();
        assert!(send(&mut state, &event("Started", 40, 10, &json!({}))));
        assert!(send(&mut state, &event("Updated", 45, 20, &json!({}))));
        assert!(send(&mut state, &event("Updated", -3, 5, &json!({}))));
        let tx = first_tx(&state);
        assert_eq!(tx.seq.highest(), Some(45));
        assert_eq!(tx.seq.out_of_order, 1);
        assert_eq!(tx.seq.missing(), 42 + 4);
    }

    #[test]
    fn ended_before_started_creates_a_stopped_record() {
        let mut state = StationState::default();
        let ended = event(
            "Ended",
            2,
            30,
            &json!({
                "triggerReason": "EVDeparted",
                "transactionInfo": {"stoppedReason": "Local", "chargingState": "Idle"},
                "meterValue": [{"timestamp": at(30), "sampledValue": [
                    {"value": 5200, "context": "Transaction.End"}]}]
            }),
        );
        assert!(send(&mut state, &ended));
        let tx = first_tx(&state);
        assert_eq!(tx.status, TransactionStatus::Stopped);
        assert_eq!(tx.stopped_at, Some(time(30)));
        assert_eq!(tx.meter_stop_wh, Some(5200));

        // The late Started fills what is unknown, moves started_at back, and revives nothing.
        let started = event(
            "Started",
            0,
            10,
            &json!({
                "evse": {"id": 1, "connectorId": 1},
                "idToken": {"idToken": "TAG-1", "type": "Local"},
                "transactionInfo": {"chargingState": "EVConnected", "remoteStartId": 3},
                "meterValue": [{"timestamp": at(10), "sampledValue": [
                    {"value": 1000, "context": "Transaction.Begin"}]}]
            }),
        );
        assert!(send(&mut state, &started));
        let tx = first_tx(&state);
        assert_eq!(tx.status, TransactionStatus::Stopped);
        assert_eq!(tx.stopped_at, Some(time(30)));
        assert_eq!(tx.started_at, time(10));
        assert_eq!((tx.evse, tx.connector), (Some(1), Some(1)));
        assert_eq!(tx.id_token.as_deref(), Some("TAG-1"));
        assert_eq!(tx.remote_start_id, Some(3));
        // Already known values and the latest trigger stay.
        assert_eq!(tx.charging_state, Some(ChargingState::Idle));
        assert_eq!(tx.trigger_reason, TriggerReason::EvDeparted);
        assert_eq!(tx.meter_start_wh, Some(1000));
        assert_eq!(tx.meter_stop_wh, Some(5200));
        // The Begin sample is older than the End reading: the energy keeps the later one.
        assert_close(tx.energy_wh, 5200.0);
        assert_eq!(tx.seq.missing(), 1);
    }

    #[test]
    fn a_late_event_does_not_revive_or_restop() {
        let mut state = StationState::default();
        assert!(send(
            &mut state,
            &event("Started", 0, 10, &json!({"evse": {"id": 1}}))
        ));
        assert!(send(
            &mut state,
            &event(
                "Ended",
                3,
                40,
                &json!({
            "transactionInfo": {"stoppedReason": "Remote"}})
            )
        ));
        // A late Updated (seqNo 1, never seen) fills nothing known, keeps Stopped.
        let late = event(
            "Updated",
            1,
            25,
            &json!({
                "triggerReason": "MeterValuePeriodic",
                "evse": {"id": 9, "connectorId": 2},
                "transactionInfo": {"chargingState": "Charging", "timeSpentCharging": 7},
                "meterValue": [{"timestamp": at(25), "sampledValue": [{"value": 300}]}]
            }),
        );
        assert!(send(&mut state, &late));
        let tx = first_tx(&state);
        assert_eq!(tx.status, TransactionStatus::Stopped);
        assert_eq!(tx.stopped_at, Some(time(40)));
        assert_eq!(tx.evse, Some(1));
        assert_eq!(tx.connector, Some(2));
        assert_eq!(tx.trigger_reason, TriggerReason::Authorized);
        assert_eq!(tx.charging_state, Some(ChargingState::Charging));
        assert_eq!(tx.time_spent_charging, Some(7));
        assert_close(tx.energy_wh, 300.0);
        // A second Ended (a different seqNo) keeps the first stop time and reason.
        let again = event(
            "Ended",
            4,
            60,
            &json!({
            "transactionInfo": {"stoppedReason": "Other"}}),
        );
        assert!(send(&mut state, &again));
        let tx = first_tx(&state);
        assert_eq!(tx.stopped_at, Some(time(40)));
        assert_eq!(tx.stopped_reason, Some(StopReason::Remote));
        assert_eq!(tx.status, TransactionStatus::Stopped);
        // A repeat of the first Ended changes nothing at all.
        let mut expected = first_tx(&state).clone();
        assert!(!send(&mut state, &event("Ended", 3, 99, &json!({}))));
        expected.seq.duplicates += 1;
        assert_eq!(first_tx(&state), &expected);
    }

    #[test]
    fn an_older_event_does_not_replace_latest_values() {
        let mut state = StationState::default();
        assert!(send(&mut state, &event("Started", 0, 10, &json!({}))));
        assert!(send(
            &mut state,
            &event(
                "Updated",
                5,
                50,
                &json!({
            "triggerReason": "ChargingStateChanged",
            "transactionInfo": {"chargingState": "SuspendedEV"}})
            )
        ));
        assert!(send(
            &mut state,
            &event(
                "Updated",
                3,
                30,
                &json!({
            "triggerReason": "CablePluggedIn",
            "transactionInfo": {"chargingState": "Charging"}})
            )
        ));
        let tx = first_tx(&state);
        assert_eq!(tx.charging_state, Some(ChargingState::SuspendedEv));
        assert_eq!(tx.trigger_reason, TriggerReason::ChargingStateChanged);
        assert_eq!(tx.status, TransactionStatus::Active);
        assert_eq!(tx.seq.out_of_order, 1);
    }

    #[test]
    fn unknown_ids_create_records_whatever_the_event() {
        let mut state = StationState::default();
        assert!(send(
            &mut state,
            &event_for("a", "Updated", 7, 10, &json!({"evse": {"id": 1}}))
        ));
        assert!(send(
            &mut state,
            &event_for("b", "Started", 0, 20, &json!({}))
        ));
        assert!(send(
            &mut state,
            &event_for("c", "Ended", 4, 30, &json!({}))
        ));
        let ids: Vec<(&str, TransactionStatus)> = v2(&state)
            .transactions
            .iter()
            .map(|tx| (tx.id.as_str(), tx.status))
            .collect();
        assert_eq!(
            ids,
            [
                ("a", TransactionStatus::Active),
                ("b", TransactionStatus::Active),
                ("c", TransactionStatus::Stopped),
            ]
        );
    }

    #[test]
    fn transaction_id_length_is_bounded() {
        let mut state = StationState::default();
        let long = "x".repeat(MAX_TRANSACTION_ID_LEN + 1);
        for id in ["", long.as_str()] {
            assert!(!send(
                &mut state,
                &event_for(id, "Started", 0, 10, &json!({}))
            ));
        }
        assert_eq!(v2(&state).counters.rejected_events, 2);
        assert_eq!(v2(&state).transactions.len(), 0);

        let exact = "y".repeat(MAX_TRANSACTION_ID_LEN);
        assert!(send(
            &mut state,
            &event_for(&exact, "Started", 0, 10, &json!({}))
        ));
        // The limit counts characters, not bytes.
        let wide = "é".repeat(MAX_TRANSACTION_ID_LEN);
        assert!(send(
            &mut state,
            &event_for(&wide, "Started", 0, 10, &json!({}))
        ));
        assert_eq!(v2(&state).transactions.len(), 2);
        assert_eq!(v2(&state).counters.rejected_events, 2);
    }

    #[test]
    fn id_token_is_cut_and_its_status_follows_the_token() {
        let mut state = StationState::default();
        let token = |name: &str| json!({"idToken": {"idToken": name, "type": "Central"}});
        let accepted = json!({"idTokenInfo": {"status": "Accepted"}});
        let blocked = json!({"idTokenInfo": {"status": "Blocked"}});
        let long = "T".repeat(50);
        assert!(fold_with(
            &mut state,
            "TransactionEvent",
            &event("Started", 0, 10, &token(&long)),
            &accepted,
            5
        ));
        assert_eq!(
            first_tx(&state).id_token.as_ref().map(String::len),
            Some(MAX_ID_TOKEN_LEN)
        );
        assert_eq!(
            first_tx(&state).id_token_status,
            Some(AuthorizationStatus::Accepted)
        );
        // The same token answered without idTokenInfo keeps the known status.
        assert!(fold(
            &mut state,
            "TransactionEvent",
            &event("Updated", 1, 20, &token(&long))
        ));
        assert_eq!(
            first_tx(&state).id_token_status,
            Some(AuthorizationStatus::Accepted)
        );
        // Another token replaces token and status.
        assert!(fold_with(
            &mut state,
            "TransactionEvent",
            &event("Updated", 2, 30, &token("OTHER")),
            &blocked,
            5
        ));
        assert_eq!(first_tx(&state).id_token.as_deref(), Some("OTHER"));
        assert_eq!(
            first_tx(&state).id_token_status,
            Some(AuthorizationStatus::Blocked)
        );
        // An answer for an event without a token is not attributed to the old one.
        assert!(fold_with(
            &mut state,
            "TransactionEvent",
            &event("Updated", 3, 40, &json!({})),
            &accepted,
            5
        ));
        assert_eq!(
            first_tx(&state).id_token_status,
            Some(AuthorizationStatus::Blocked)
        );
    }

    #[test]
    fn evse_and_connector_below_one_are_ignored() {
        let mut state = StationState::default();
        let started = event(
            "Started",
            0,
            10,
            &json!({"evse": {"id": 0, "connectorId": 2}}),
        );
        assert!(send(&mut state, &started));
        assert_eq!(
            (first_tx(&state).evse, first_tx(&state).connector),
            (None, None)
        );
        let updated = event(
            "Updated",
            1,
            20,
            &json!({"evse": {"id": 1, "connectorId": 0}}),
        );
        assert!(send(&mut state, &updated));
        assert_eq!(
            (first_tx(&state).evse, first_tx(&state).connector),
            (Some(1), None)
        );
        let rows = state.transaction_rows();
        assert_eq!((rows[0].evse, rows[0].connector), (Some(1), None));
        let active = state.active_transactions();
        assert_eq!(active[0].connector, None);
    }

    #[test]
    fn active_transactions_are_capped_and_history_follows_the_cap() {
        let mut state = StationState::default();
        for n in 0..=MAX_ACTIVE_TRANSACTIONS {
            let id = format!("t{n}");
            assert!(send(
                &mut state,
                &event_for(&id, "Started", 0, 10, &json!({}))
            ));
        }
        assert_eq!(state.active_transaction_count(), MAX_ACTIVE_TRANSACTIONS);
        assert_eq!(first_tx(&state).id, "t1");

        // Stopped transactions beyond the history cap go, oldest first; running ones stay.
        let mut state = StationState::default();
        assert!(send(
            &mut state,
            &event_for("run", "Started", 0, 1, &json!({}))
        ));
        for n in 0..5 {
            let id = format!("s{n}");
            assert!(fold_with(
                &mut state,
                "TransactionEvent",
                &event_for(&id, "Started", 0, 2, &json!({})),
                &json!({}),
                2
            ));
            assert!(fold_with(
                &mut state,
                "TransactionEvent",
                &event_for(&id, "Ended", 1, 3, &json!({})),
                &json!({}),
                2
            ));
        }
        let ids: Vec<&str> = v2(&state)
            .transactions
            .iter()
            .map(|tx| tx.id.as_str())
            .collect();
        assert_eq!(ids, ["run", "s3", "s4"]);
        assert_eq!(state.active_transaction_count(), 1);
    }

    // ---- meters ----

    /// One sample of `value` in a `MeterValue` at second 10 of the day.
    fn sample_event(sample: &Value) -> Value {
        event(
            "Started",
            0,
            10,
            &json!({"meterValue": [{"timestamp": at(10), "sampledValue": [sample]}]}),
        )
    }

    #[test]
    fn meter_units_and_multipliers_are_converted() {
        let cases: [(Value, Option<f64>, Option<f64>); 14] = [
            (json!({"value": 1500}), Some(1500.0), None),
            (
                json!({"value": 1.5, "unitOfMeasure": {"unit": "kWh"}}),
                Some(1500.0),
                None,
            ),
            (
                json!({"value": 15, "unitOfMeasure": {"multiplier": 2}}),
                Some(1500.0),
                None,
            ),
            (
                json!({"value": 1_500_000, "unitOfMeasure": {"multiplier": -3, "unit": "Wh"}}),
                Some(1500.0),
                None,
            ),
            (
                json!({"value": 1500, "unitOfMeasure": {"unit": "kWh", "multiplier": -3}}),
                Some(1500.0),
                None,
            ),
            (
                json!({"value": 2, "measurand": "Power.Active.Import"}),
                None,
                Some(2.0),
            ),
            (
                json!({"value": 7.4, "measurand": "Power.Active.Import", "unitOfMeasure": {"unit": "kW"}}),
                None,
                Some(7400.0),
            ),
            (
                json!({"value": 74, "measurand": "Power.Active.Import", "unitOfMeasure": {"unit": "kW", "multiplier": -1}}),
                None,
                Some(7400.0),
            ),
            (
                json!({"value": 1, "unitOfMeasure": {"unit": "varh"}}),
                None,
                None,
            ),
            (
                json!({"value": 1, "measurand": "Power.Active.Import", "unitOfMeasure": {"unit": "kWh"}}),
                None,
                None,
            ),
            (json!({"value": 5, "measurand": "Voltage"}), None, None),
            (json!({"value": 5, "phase": "L1"}), None, None),
            (
                json!({"value": 1, "unitOfMeasure": {"multiplier": 400}}),
                None,
                None,
            ),
            (
                json!({"value": 1, "unitOfMeasure": {"multiplier": -2_147_483_648_i64}}),
                None,
                None,
            ),
        ];
        for (sample, energy, power) in cases {
            let mut state = StationState::default();
            assert!(send(&mut state, &sample_event(&sample)), "{sample}");
            let tx = first_tx(&state);
            assert_eq!(tx.energy_wh.is_some(), energy.is_some(), "{sample}");
            assert_eq!(tx.power_w.is_some(), power.is_some(), "{sample}");
            if let Some(energy) = energy {
                assert_close(tx.energy_wh, energy);
            }
            if let Some(power) = power {
                assert_close(tx.power_w, power);
            }
            assert_eq!(
                tx.last_meter_at.is_some(),
                energy.is_some() || power.is_some(),
                "{sample}"
            );
        }
    }

    #[test]
    fn begin_sample_fills_once_and_end_sample_replaces() {
        let mut state = StationState::default();
        let samples = |context: &str, kwh: f64| {
            json!({"meterValue": [{"timestamp": at(10), "sampledValue": [
                {"value": kwh, "context": context, "unitOfMeasure": {"unit": "kWh"}},
                {"value": 9, "context": context, "measurand": "Voltage"}]}]})
        };
        assert!(send(
            &mut state,
            &event("Started", 0, 10, &samples("Transaction.Begin", 1.2504))
        ));
        assert_eq!(first_tx(&state).meter_start_wh, Some(1250));
        assert_eq!(first_tx(&state).meter_stop_wh, None);
        assert!(send(
            &mut state,
            &event("Updated", 1, 11, &samples("Transaction.Begin", 7.0))
        ));
        assert_eq!(first_tx(&state).meter_start_wh, Some(1250));
        assert!(send(
            &mut state,
            &event(
                "Ended",
                2,
                12,
                &json!({"meterValue": [
            {"timestamp": at(12), "sampledValue": [{"value": 4800.6, "context": "Transaction.End"}]}]})
            )
        ));
        assert_eq!(first_tx(&state).meter_stop_wh, Some(4801));
        let row = &state.transaction_rows()[0];
        assert_eq!((row.meter_start, row.meter_stop), (Some(1250), Some(4801)));
        assert_close(row.energy_wh, 4800.6);
        // The End sample is not fill-once: a later one (another seqNo) replaces it.
        assert!(send(
            &mut state,
            &event("Ended", 3, 13, &samples("Transaction.End", 4.9))
        ));
        assert_eq!(first_tx(&state).meter_stop_wh, Some(4900));
        assert_eq!(first_tx(&state).meter_start_wh, Some(1250));
    }

    /// The payloads the tests are built from are valid 2.0.1 messages, so a misspelled key
    /// cannot pass vacuously through the lenient typed decode.
    #[test]
    fn fixtures_are_valid_payloads() {
        let schemas = kirby_ocpp_core::ocpp::SchemaRegistry::for_protocol(Protocol::V201).unwrap();
        let full = event(
            "Started",
            0,
            10,
            &json!({
                "evse": {"id": 2, "connectorId": 1},
                "idToken": {"idToken": "TAG-1", "type": "ISO14443"},
                "reservationId": 9,
                "transactionInfo": {"chargingState": "EVConnected", "remoteStartId": 5,
                    "timeSpentCharging": 3, "stoppedReason": "Local"},
                "meterValue": [{"timestamp": at(10), "sampledValue": [{
                    "value": 1.5, "context": "Transaction.Begin",
                    "measurand": "Energy.Active.Import.Register",
                    "unitOfMeasure": {"unit": "kWh", "multiplier": 0}}]}]
            }),
        );
        let payloads = [
            ("TransactionEvent", full),
            ("TransactionEvent", event("Updated", 1, 20, &json!({}))),
            ("TransactionEvent", event("Ended", 2, 30, &json!({}))),
            ("StatusNotification", status(1, 1, "Occupied", 10)),
            ("NotifyEvent", notify_event(&json!({}))),
            (
                "MeterValues",
                json!({"evseId": 1, "meterValue": [
                {"timestamp": at(1), "sampledValue": [{"value": 1}]}]}),
            ),
            (
                "SecurityEventNotification",
                json!({"type": "T", "timestamp": at(0)}),
            ),
            (
                "NotifyReport",
                json!({"requestId": 4, "generatedAt": at(0), "seqNo": 2}),
            ),
        ];
        for (action, payload) in payloads {
            assert!(
                schemas.validate_request(action, &payload).is_ok(),
                "{action}: {payload}"
            );
        }
    }

    #[test]
    fn meter_readings_never_move_back_in_time() {
        let mut state = StationState::default();
        assert!(send(
            &mut state,
            &event("Started", 0, 10, &energy(100, &json!(500)))
        ));
        // Older reading: ignored (the event is still folded).
        assert!(send(
            &mut state,
            &event("Updated", 1, 20, &energy(90, &json!(400)))
        ));
        assert_close(first_tx(&state).energy_wh, 500.0);
        assert_eq!(first_tx(&state).last_meter_at, Some(time(100)));
        // Equal timestamp: accepted.
        assert!(send(
            &mut state,
            &event("Updated", 2, 30, &energy(100, &json!(510)))
        ));
        assert_close(first_tx(&state).energy_wh, 510.0);
        assert!(send(
            &mut state,
            &event("Updated", 3, 40, &energy(110, &json!(520)))
        ));
        assert_close(first_tx(&state).energy_wh, 520.0);
        // One event with out-of-order entries keeps the newest.
        let mixed = event(
            "Updated",
            4,
            50,
            &json!({"meterValue": [
            {"timestamp": at(130), "sampledValue": [{"value": 600}]},
            {"timestamp": at(120), "sampledValue": [{"value": 550}]}]}),
        );
        assert!(send(&mut state, &mixed));
        assert_close(first_tx(&state).energy_wh, 600.0);
        assert_eq!(first_tx(&state).last_meter_at, Some(time(130)));
    }

    #[test]
    fn meter_values_fold_per_evse() {
        let mut state = StationState::default();
        let req = |evse: i32, sec: u32, value: f64| {
            json!({"evseId": evse, "meterValue": [{"timestamp": at(sec), "sampledValue": [
                {"value": value, "unitOfMeasure": {"unit": "kWh"}},
                {"value": 3.3, "measurand": "Power.Active.Import", "unitOfMeasure": {"unit": "kW"}},
                {"value": 99, "phase": "L1"}]}]})
        };
        assert!(fold(&mut state, "MeterValues", &req(1, 100, 2.0)));
        assert!(fold(&mut state, "MeterValues", &req(0, 100, 50.0)));
        let reading = &v2(&state).meters[&1];
        assert_close(reading.energy_wh, 2000.0);
        assert_close(reading.power_w, 3300.0);
        assert_eq!(reading.last_meter_at, Some(time(100)));
        assert_close(v2(&state).meters[&0].energy_wh, 50_000.0);
        // Older: ignored, but counted.
        assert!(!fold(&mut state, "MeterValues", &req(1, 90, 1.0)));
        assert_close(v2(&state).meters[&1].energy_wh, 2000.0);
        assert!(fold(&mut state, "MeterValues", &req(1, 100, 2.5)));
        assert_close(v2(&state).meters[&1].energy_wh, 2500.0);
        assert_eq!(v2(&state).counters.meter_values, 4);
        // Not attached to any transaction.
        assert_eq!(v2(&state).transactions.len(), 0);
        // A negative EVSE does not decode into anything.
        assert!(!fold(&mut state, "MeterValues", &req(-1, 100, 1.0)));
        assert_eq!(v2(&state).counters.meter_values, 4);
        // Only readings without a usable sample create no entry.
        let junk = json!({"evseId": 3, "meterValue": [{"timestamp": at(1), "sampledValue": [
            {"value": 1, "measurand": "Voltage"}]}]});
        assert!(!fold(&mut state, "MeterValues", &junk));
        assert!(!v2(&state).meters.contains_key(&3));
    }

    #[test]
    fn meter_entries_are_capped() {
        let mut state = StationState::default();
        let req = |evse: usize| json!({"evseId": evse, "meterValue": [{"timestamp": at(1), "sampledValue": [{"value": 1}]}]});
        for evse in 0..MAX_METER_EVSES {
            assert!(fold(&mut state, "MeterValues", &req(evse)));
        }
        assert!(!fold(&mut state, "MeterValues", &req(MAX_METER_EVSES)));
        assert_eq!(v2(&state).meters.len(), MAX_METER_EVSES);
        assert!(fold(&mut state, "MeterValues", &req(5)));
    }

    // ---- rows and accessors ----

    #[test]
    fn rows_and_accessors_for_2_0_1() {
        let mut state = StationState::default();
        assert!(send(
            &mut state,
            &event(
                "Started",
                0,
                10,
                &json!({
                    "evse": {"id": 2, "connectorId": 1},
                    "idToken": {"idToken": "TAG-1", "type": "Local"},
                    "meterValue": [{"timestamp": at(10), "sampledValue": [
                        {"value": 1000, "context": "Transaction.Begin"}]}]
                })
            )
        ));
        assert!(send(
            &mut state,
            &event_for(
                "tx-2",
                "Ended",
                1,
                20,
                &json!({
            "transactionInfo": {"stoppedReason": "Remote"}})
            )
        ));
        assert_eq!(state.active_transaction_count(), 1);
        assert_eq!(
            state.active_transactions(),
            [ActiveTransaction {
                id: TransactionId::Text("tx-1".into()),
                connector: Some(1),
                evse: Some(2),
            }]
        );
        let rows = state.transaction_rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, TransactionId::Text("tx-1".into()));
        assert_eq!(rows[0].id.to_string(), "tx-1");
        assert_eq!(rows[0].meter_start, Some(1000));
        assert_eq!(rows[1].stop_reason.as_deref(), Some("Remote"));
        assert_eq!(rows[1].status, TransactionStatus::Stopped);
        let json = serde_json::to_value(&rows[0]).unwrap();
        assert_eq!(json["id"], "tx-1");
        assert_eq!(json["evse"], 2);
        assert_eq!(json["id_tag"], "TAG-1");
        assert_eq!(json["id_tag_status"], Value::Null);
        let state_json = serde_json::to_value(&state).unwrap();
        assert_eq!(state_json["v2"]["transactions"][0]["id"], "tx-1");
        assert_eq!(state_json["v2"]["protocol"], "ocpp2.0.1");
    }

    // ---- SeqTracker ----

    fn ranges(tracker: &SeqTracker) -> Vec<(i32, i32)> {
        tracker.ranges.clone()
    }

    #[test]
    fn seq_tracker_merges_neighbors_and_counts() {
        let mut tracker = SeqTracker::default();
        for seq in [0, 1, 2] {
            assert!(tracker.record(seq));
        }
        assert_eq!(ranges(&tracker), [(0, 2)]);
        assert!(tracker.record(5));
        assert_eq!(ranges(&tracker), [(0, 2), (5, 5)]);
        assert_eq!(tracker.missing(), 2);
        assert!(tracker.record(4));
        assert_eq!(ranges(&tracker), [(0, 2), (4, 5)]);
        assert!(tracker.record(3));
        assert_eq!(ranges(&tracker), [(0, 5)]);
        assert_eq!(tracker.missing(), 0);
        assert_eq!(tracker.out_of_order, 2);
        for seq in [0, 3, 5] {
            assert!(!tracker.record(seq));
        }
        assert_eq!(tracker.duplicates, 3);
        assert_eq!(tracker.highest(), Some(5));
        assert_eq!(SeqTracker::default().highest(), None);
        assert_eq!(SeqTracker::default().missing(), 0);
    }

    #[test]
    fn seq_tracker_handles_the_ends_of_the_range() {
        let mut tracker = SeqTracker::default();
        assert!(tracker.record(i32::MAX));
        assert!(tracker.record(i32::MIN));
        assert!(tracker.record(i32::MAX - 1));
        assert_eq!(
            ranges(&tracker),
            [(i32::MIN, i32::MIN), (i32::MAX - 1, i32::MAX)]
        );
        assert_eq!(tracker.missing(), u64::from(u32::MAX) - 2);
        assert!(!tracker.record(i32::MIN));
    }

    #[test]
    fn seq_tracker_forgets_the_lowest_range_and_never_bridges_a_gap() {
        let mut tracker = SeqTracker::default();
        // MAX_SEQ_RANGES + 1 disjoint singles: 0, 2, 4, ...
        let top = i32::try_from(MAX_SEQ_RANGES).unwrap() * 2;
        for seq in (0..=top).step_by(2) {
            assert!(tracker.record(seq));
        }
        assert_eq!(tracker.ranges.len(), MAX_SEQ_RANGES);
        // 0 was forgotten; the gap between the surviving ranges is still a gap.
        assert_eq!(tracker.ranges.first(), Some(&(2, 2)));
        assert_eq!(tracker.ranges.last(), Some(&(top, top)));
        assert!(
            tracker.record(3),
            "a late event inside a gap is not a duplicate"
        );
        assert_eq!(tracker.ranges.first(), Some(&(2, 4)));
        assert_eq!(tracker.ranges.len(), MAX_SEQ_RANGES - 1);
        // The forgotten number is not a duplicate: it folds again.
        assert!(tracker.record(0));
        assert_eq!(tracker.ranges.first(), Some(&(0, 0)));
        assert_eq!(tracker.highest(), Some(top));
        assert!(tracker.missing() > 0);
    }

    #[test]
    fn a_tracker_that_overflows_still_dedupes_the_recent_range() {
        let mut tracker = SeqTracker::default();
        let top = i32::try_from(MAX_SEQ_RANGES).unwrap() * 4;
        for seq in (0..=top).step_by(2) {
            tracker.record(seq);
        }
        assert!(!tracker.record(top));
        assert!(!tracker.record(top - 2));
        assert_eq!(tracker.ranges.len(), MAX_SEQ_RANGES);
    }
}
