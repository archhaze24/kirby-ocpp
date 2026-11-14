//! The transaction state machine, `TransactionEvent` generation and metering.
//!
//! Each EVSE has at most one transaction. The start and stop points of the device model decide
//! when it starts and ends (`TxStartPoint`, `TxStopPoint`); the authorization module only
//! reports that a token was accepted or ended and never evaluates the points itself.
//!
//! # Events
//!
//! Every change of the transaction is one `TransactionEvent`; `seqNo` counts from 0 per
//! transaction and is assigned when the event is generated. When a trigger also lets the energy
//! offer start (a cable plugged in or a token accepted on an authorized, connected EVSE), the
//! trigger event reports the state before the offer (`EVConnected`) and a second event with
//! `ChargingStateChanged` and `Charging` follows (E03.FR.12, figures 42 and 45).
//!
//! Optional fields follow the merge rule of Part 2 section E.1.5: `evse` goes out once after the
//! EV connected, `idToken` once when the transaction is authorized and once more when
//! authorization ends by a token, `remoteStartId` once.
//!
//! # Metering
//!
//! A charging EVSE draws its nominal power; the energy register is the integral of that power
//! over monotonic time ([`Station2Actor::settle_tx_meters`] runs at the start and the end of
//! every actor loop iteration). The register is per EVSE and also counts outside
//! transactions; `Command2::AdvanceMeter` adds to it.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Number;
use tokio::time::Instant;
use uuid::Uuid;

use super::actor::{Station2Actor, TimerKey2};
use super::command::Outcome2;
use super::snapshot::TransactionSnapshot;
use super::tx_queue::QueuedEvent;
use super::types::{IdTokenRef, Origin2, Reply2, StartStopPoint, TxId2};
use crate::error::StationError;
use crate::ocpp::v2::messages::{
    ConnectorStatus, MeterValuesRequest, TransactionEventKind, TransactionEventRequest,
    TriggerReason,
};
use crate::ocpp::v2::types::{
    ChargingState, Evse as WireEvse, IdToken, Measurand, MeterValue, ReadingContext, SampledValue,
    StopReason, Transaction, UnitOfMeasure,
};
use crate::station::LogLevel;
use crate::station::metering::{PowerModel, clock_aligned_delay, energy_wh_for};

/// Stopped transaction ids kept for `GetTransactionStatus`.
const STOPPED_HISTORY: usize = 64;

/// What starts a transaction.
#[derive(Debug, Clone, PartialEq)]
pub struct StartSpec {
    pub token: Option<IdTokenRef>,
    pub remote_start_id: Option<i32>,
    pub trigger: TriggerReason,
    pub authorized: bool,
}

/// Optional parts of a `TransactionEvent`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EventExtra {
    pub meter: Vec<MeterValue>,
    pub id_token: Option<IdTokenRef>,
    pub stopped_reason: Option<StopReason>,
    pub charging_state: Option<ChargingState>,
    pub remote_start_id: Option<i32>,
}

/// A transaction in progress on an EVSE.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent flags of one transaction record"
)]
pub struct ActiveTx {
    pub id: TxId2,
    pub evse: u32,
    /// `seqNo` of the next event.
    pub next_seq_no: i32,
    pub started_at: DateTime<Utc>,
    pub charging_state: ChargingState,
    pub token: Option<IdTokenRef>,
    pub remote_start_id: Option<i32>,
    pub authorized: bool,
    pub ev_connected: bool,
    /// The `evse` field went out in an event already.
    pub evse_sent: bool,
    /// The `idToken` field went out in an event already.
    pub token_sent: bool,
    pub meter_start_wh: u64,
    pub energy_wh: f64,
}

/// What `GetTransactionStatus` needs to know about a transaction id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxLookup {
    pub ongoing: bool,
}

/// Transactions in progress and the ids of the last stopped ones, as persisted.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TxPersist {
    pub active: Vec<ActiveTx>,
    pub stopped_ids: Vec<String>,
}

/// A token that was accepted before a start point could be reached.
#[derive(Debug, Clone)]
struct PendingAuth {
    token: IdTokenRef,
    remote_start_id: Option<i32>,
}

/// Per-transaction state that is not persisted.
#[derive(Debug, Default)]
struct TxRuntime {
    /// `remoteStartId` went out in an event already.
    remote_sent: bool,
    /// The `Transaction.Begin` sample, kept for the `Ended` event.
    begin_sample: Option<MeterValue>,
    /// The begin sample waits for the first event that carries the EVSE (E01.FR.17).
    begin_pending: bool,
}

/// The simulated meter of one EVSE.
#[derive(Debug, Default)]
struct MeterRt {
    register_wh: f64,
    last: Option<Instant>,
    /// The EVSE was charging at `last`.
    charging: bool,
}

/// The transactions of the station.
#[derive(Debug, Default)]
pub(crate) struct TxEngine {
    active: BTreeMap<u32, ActiveTx>,
    pending: BTreeMap<u32, PendingAuth>,
    runtime: BTreeMap<u32, TxRuntime>,
    meters: BTreeMap<u32, MeterRt>,
    stopped: VecDeque<String>,
    /// `(TxUpdatedInterval, AlignedDataCtrlr.Interval)` the timers were armed for.
    seen_intervals: Option<(u32, u32)>,
}

/// Which start point was reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reached {
    Connected,
    Authorized,
    PowerPath,
    Energy,
}

/// The first configured start point whose condition holds. `can_charge` means the EV is
/// connected and authorized and the connector is operative and not faulted.
fn start_reached(
    points: &[StartStopPoint],
    connected: bool,
    authorized: bool,
    can_charge: bool,
) -> Option<Reached> {
    points.iter().find_map(|point| match point {
        StartStopPoint::EvConnected if connected => Some(Reached::Connected),
        StartStopPoint::Authorized if authorized => Some(Reached::Authorized),
        StartStopPoint::PowerPathClosed if can_charge => Some(Reached::PowerPath),
        StartStopPoint::EnergyTransfer if can_charge => Some(Reached::Energy),
        _ => None,
    })
}

/// Whether ending the authorization ends the transaction: `Authorized` and `PowerPathClosed`
/// stop on it, `EnergyTransfer` when energy flows.
fn authorization_stops(points: &[StartStopPoint], charging: bool) -> bool {
    points.iter().any(|point| match point {
        StartStopPoint::Authorized | StartStopPoint::PowerPathClosed => true,
        StartStopPoint::EnergyTransfer => charging,
        StartStopPoint::EvConnected => false,
    })
}

/// Whether the loss of the EV connection ends the transaction (E09) rather than suspends it
/// (E10).
fn disconnect_stops(points: &[StartStopPoint], stop_on_ev_side: bool, charging: bool) -> bool {
    stop_on_ev_side
        || points.iter().any(|point| match point {
            StartStopPoint::EvConnected | StartStopPoint::PowerPathClosed => true,
            StartStopPoint::EnergyTransfer => charging,
            StartStopPoint::Authorized => false,
        })
}

/// Whole Wh of a register value; the fraction stays in the register.
fn whole_wh(wh: f64) -> u64 {
    if wh.is_finite() && wh > 0.0 {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "checked finite and positive; a register never reaches 2^64 Wh"
        )]
        {
            wh.floor() as u64
        }
    } else {
        0
    }
}

/// Wh as a float for the integration.
fn wh_from(wh: u64) -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a register of 2^53 Wh is far beyond anything simulated"
    )]
    {
        wh as f64
    }
}

/// A reading with one decimal; `None` for a value JSON cannot hold.
fn decimal(value: f64) -> Option<Number> {
    Number::from_f64((value * 10.0).round() / 10.0)
}

impl Station2Actor {
    // ----- queries ---------------------------------------------------------------------

    /// The transaction in progress on `evse`.
    pub(super) fn active_tx(&self, evse: u32) -> Option<&ActiveTx> {
        self.tx.active.get(&evse)
    }

    /// A transaction by id: ongoing, stopped, or unknown (`None`).
    pub(super) fn tx_by_id(&self, id: &str) -> Option<TxLookup> {
        if self.tx.active.values().any(|tx| tx.id.0 == id) {
            Some(TxLookup { ongoing: true })
        } else if self.tx.stopped.iter().any(|stopped| stopped == id) {
            Some(TxLookup { ongoing: false })
        } else {
            None
        }
    }

    /// The simulated power of `evse` in watts.
    pub(super) fn evse_power_w(&self, evse: u32) -> f64 {
        let charging = self
            .tx
            .active
            .get(&evse)
            .is_some_and(|tx| tx.charging_state == ChargingState::Charging);
        if charging {
            PowerModel::from_config(&self.config).nominal_w
        } else {
            0.0
        }
    }

    /// The token that authorizes `evse` now: of the running transaction, or of an
    /// authorization waiting for the EV.
    pub(super) fn authorized_token(&self, evse: u32) -> Option<&IdTokenRef> {
        match self.tx.active.get(&evse) {
            Some(tx) => tx.token.as_ref().filter(|_| tx.authorized),
            None => self.tx.pending.get(&evse).map(|pending| &pending.token),
        }
    }

    fn evse_connected(&self, evse: u32) -> bool {
        self.evse(evse)
            .is_some_and(|e| e.connectors.iter().any(|c| c.ev_connected))
    }

    /// The connector is occupied, operative and not faulted.
    fn connector_ready(&self, evse: u32) -> bool {
        self.connector_status(evse, 1) == Some(ConnectorStatus::Occupied)
    }

    // ----- meters ----------------------------------------------------------------------

    /// Adds the energy of the time that passed to every running meter.
    pub(super) fn settle_tx_meters(&mut self) {
        let now = Instant::now();
        let power = PowerModel::from_config(&self.config).nominal_w;
        let ids: Vec<u32> = self.evses.iter().map(|e| e.id).collect();
        for id in ids {
            let charging_now = self
                .tx
                .active
                .get(&id)
                .is_some_and(|tx| tx.charging_state == ChargingState::Charging);
            let meter = self.tx.meters.entry(id).or_default();
            if let (Some(last), true) = (meter.last, meter.charging) {
                let wh = energy_wh_for(power, now.saturating_duration_since(last));
                meter.register_wh += wh;
                if let Some(tx) = self.tx.active.get_mut(&id) {
                    tx.energy_wh += wh;
                }
            }
            meter.last = Some(now);
            meter.charging = charging_now;
        }
        if !self.tx.active.is_empty() {
            let current = (self.tx_updated_interval(), self.aligned_interval());
            if self.tx.seen_intervals != Some(current) {
                self.rearm_meter_timers();
            }
        }
        self.sync_tx_snapshot();
    }

    /// Mirrors the transactions and the meters into the snapshot.
    fn sync_tx_snapshot(&mut self) {
        for entry in &mut self.state.evses {
            entry.transaction = self.tx.active.get(&entry.id).map(|tx| TransactionSnapshot {
                transaction_id: tx.id.0.clone(),
                next_seq_no: tx.next_seq_no,
                charging_state: tx.charging_state,
                id_token: tx
                    .token
                    .as_ref()
                    .filter(|_| tx.authorized)
                    .map(|token| token.id_token.clone()),
                started_at: tx.started_at,
                energy_wh: whole_wh(tx.energy_wh),
                remote_start_id: tx.remote_start_id,
            });
            entry.meter_wh = self
                .tx
                .meters
                .get(&entry.id)
                .map_or(0, |meter| whole_wh(meter.register_wh));
        }
    }

    /// One `MeterValue` with the readings of `evse` for the measurands this station can
    /// produce; `None` when none of them applies (the schema forbids an empty list).
    fn sample_value(
        &self,
        evse: u32,
        measurands: &[Measurand],
        context: ReadingContext,
        at: DateTime<Utc>,
    ) -> Option<MeterValue> {
        let model = PowerModel::from_config(&self.config);
        let register = self.tx.meters.get(&evse).map_or(0.0, |m| m.register_wh);
        let power = self.evse_power_w(evse);
        let sampled: Vec<SampledValue> = measurands
            .iter()
            .filter_map(|measurand| {
                let (value, unit) = match measurand {
                    Measurand::EnergyActiveImportRegister => {
                        (Number::from(whole_wh(register)), "Wh")
                    }
                    Measurand::PowerActiveImport => (decimal(power)?, "W"),
                    Measurand::PowerOffered => (decimal(model.nominal_w)?, "W"),
                    Measurand::CurrentImport => (decimal(model.watts_to_amps(power))?, "A"),
                    Measurand::Voltage => (decimal(model.voltage_v)?, "V"),
                    _ => return None,
                };
                Some(SampledValue {
                    value,
                    context: Some(context),
                    measurand: Some(*measurand),
                    phase: None,
                    location: None,
                    signed_meter_value: None,
                    unit_of_measure: Some(UnitOfMeasure {
                        unit: Some(unit.to_owned()),
                        multiplier: None,
                        custom_data: None,
                    }),
                    custom_data: None,
                })
            })
            .collect();
        (!sampled.is_empty()).then_some(MeterValue {
            sampled_value: sampled,
            timestamp: at,
            custom_data: None,
        })
    }

    /// Arms the periodic timer of `evse` for the configured `TxUpdatedInterval`.
    fn arm_periodic(&mut self, evse: u32) {
        let interval = self.tx_updated_interval();
        if interval > 0 {
            self.timers.set(
                TimerKey2::MeterPeriodic(evse),
                Duration::from_secs(u64::from(interval)),
            );
        } else {
            self.timers.cancel(&TimerKey2::MeterPeriodic(evse));
        }
    }

    /// Brings the periodic and clock-aligned timers in line with the configuration. A timer
    /// that is already running keeps its schedule unless its interval changed.
    fn rearm_meter_timers(&mut self) {
        if self.tx.active.is_empty() {
            self.timers.cancel(&TimerKey2::MeterAligned);
            self.tx.seen_intervals = None;
            return;
        }
        let current = (self.tx_updated_interval(), self.aligned_interval());
        let (old_updated, old_aligned) = self.tx.seen_intervals.unwrap_or(current);
        let ids: Vec<u32> = self.tx.active.keys().copied().collect();
        for id in ids {
            let key = TimerKey2::MeterPeriodic(id);
            if current.0 == 0 {
                self.timers.cancel(&key);
            } else {
                let delay = Duration::from_secs(u64::from(current.0));
                if current.0 == old_updated {
                    self.timers.set_if_idle(key, delay);
                } else {
                    self.timers.set(key, delay);
                }
            }
        }
        let aligned_delay = clock_aligned_delay(self.clock.now_utc(), current.1)
            .and_then(|delay| delay.to_std().ok());
        match aligned_delay {
            Some(delay) if current.1 == old_aligned => {
                self.timers.set_if_idle(TimerKey2::MeterAligned, delay);
            }
            Some(delay) => self.timers.set(TimerKey2::MeterAligned, delay),
            None => {
                self.timers.cancel(&TimerKey2::MeterAligned);
            }
        }
        self.tx.seen_intervals = Some(current);
    }

    /// The periodic sample of `evse` is due.
    pub(super) fn on_meter_periodic(&mut self, evse: u32) {
        if !self.tx.active.contains_key(&evse) {
            return;
        }
        self.settle_tx_meters();
        let sample = self.sample_value(
            evse,
            &self.tx_updated_measurands(),
            ReadingContext::SamplePeriodic,
            self.clock.now_utc(),
        );
        self.tx_event(
            evse,
            TransactionEventKind::Updated,
            TriggerReason::MeterValuePeriodic,
            EventExtra {
                meter: sample.into_iter().collect(),
                ..EventExtra::default()
            },
        );
        self.rearm_meter_timers();
    }

    /// The clock-aligned sample is due.
    pub(super) fn on_meter_aligned(&mut self) {
        if self.tx.active.is_empty() {
            return;
        }
        self.settle_tx_meters();
        let now = self.clock.now_utc();
        let measurands = self.aligned_measurands();
        let ids: Vec<u32> = self.tx.active.keys().copied().collect();
        for id in ids {
            if let Some(sample) =
                self.sample_value(id, &measurands, ReadingContext::SampleClock, now)
            {
                self.tx_event(
                    id,
                    TransactionEventKind::Updated,
                    TriggerReason::MeterValueClock,
                    EventExtra {
                        meter: vec![sample],
                        ..EventExtra::default()
                    },
                );
            }
        }
        self.rearm_meter_timers();
    }

    /// Sends `MeterValues` outside a transaction for `evse` (every EVSE for `None`); never
    /// queued offline. Used by `Command2::AdvanceMeter` without a transaction and by
    /// `TriggerMessage(MeterValues)`.
    pub(super) fn send_meter_values(&mut self, evse: Option<u32>, origin: Origin2) {
        self.settle_tx_meters();
        let ids: Vec<u32> = match evse {
            Some(id) if self.evse(id).is_some() => vec![id],
            Some(_) => Vec::new(),
            None => self.evses.iter().map(|e| e.id).collect(),
        };
        let mut measurands = self.aligned_measurands();
        if measurands.is_empty() {
            measurands = self.tx_updated_measurands();
        }
        if measurands.is_empty() {
            measurands = vec![Measurand::EnergyActiveImportRegister];
        }
        let context = if origin == Origin2::Triggered {
            ReadingContext::Trigger
        } else {
            ReadingContext::Other
        };
        let now = self.clock.now_utc();
        for id in ids {
            let Some(sample) = self.sample_value(id, &measurands, context, now) else {
                continue;
            };
            let request = MeterValuesRequest {
                evse_id: i32::try_from(id).unwrap_or(i32::MAX),
                meter_value: vec![sample],
                custom_data: None,
            };
            let Ok(payload) = serde_json::to_value(&request) else {
                continue;
            };
            // A refused or failed CALL is already logged by the call engine.
            self.call_raw(origin, "MeterValues", payload, true, Box::new(|_, _| {}));
        }
    }

    // ----- events ----------------------------------------------------------------------

    /// Builds the next event of the transaction on `evse`, merging the optional fields, and
    /// takes the next `seqNo`. `None` when there is no transaction.
    fn build_event(
        &mut self,
        evse: u32,
        kind: TransactionEventKind,
        trigger: TriggerReason,
        extra: EventExtra,
    ) -> Option<QueuedEvent> {
        let now = self.clock.now_utc();
        let offline = self.link.conn.is_none();
        let mut meter = extra.meter;
        if meter.is_empty()
            && kind == TransactionEventKind::Updated
            && trigger == TriggerReason::Trigger
        {
            meter.extend(self.sample_value(
                evse,
                &self.tx_updated_measurands(),
                ReadingContext::Trigger,
                now,
            ));
        }
        let tx = self.tx.active.get_mut(&evse)?;
        let runtime = self.tx.runtime.entry(evse).or_default();
        let seq_no = tx.next_seq_no;
        tx.next_seq_no = tx.next_seq_no.saturating_add(1);
        let include_evse = !tx.evse_sent && tx.ev_connected;
        if include_evse {
            tx.evse_sent = true;
            if runtime.begin_pending {
                runtime.begin_pending = false;
                if let Some(begin) = runtime.begin_sample.clone() {
                    meter.insert(0, begin);
                }
            }
        }
        let id_token = extra
            .id_token
            .or_else(|| tx.token.clone().filter(|_| tx.authorized && !tx.token_sent));
        if id_token.is_some() {
            tx.token_sent = true;
        }
        let remote_start_id = extra
            .remote_start_id
            .or_else(|| tx.remote_start_id.filter(|_| !runtime.remote_sent));
        if remote_start_id.is_some() {
            runtime.remote_sent = true;
        }
        let charging_state = extra.charging_state.or_else(|| {
            (matches!(
                kind,
                TransactionEventKind::Started | TransactionEventKind::Ended
            ) || trigger == TriggerReason::Trigger)
                .then_some(tx.charging_state)
        });
        let request = TransactionEventRequest {
            event_type: kind,
            meter_value: (!meter.is_empty()).then_some(meter),
            timestamp: now,
            trigger_reason: trigger,
            seq_no,
            offline: offline.then_some(true),
            number_of_phases_used: None,
            cable_max_current: None,
            reservation_id: None,
            transaction_info: Transaction {
                transaction_id: tx.id.0.clone(),
                charging_state,
                time_spent_charging: None,
                stopped_reason: extra.stopped_reason,
                remote_start_id,
                custom_data: None,
            },
            evse: include_evse.then(|| WireEvse {
                id: i32::try_from(evse).unwrap_or(i32::MAX),
                connector_id: Some(1),
                custom_data: None,
            }),
            id_token: id_token.map(|token| IdToken {
                additional_info: None,
                id_token: token.id_token,
                kind: token.kind,
                custom_data: None,
            }),
            custom_data: None,
        };
        let payload = serde_json::to_value(&request).ok()?;
        Some(QueuedEvent {
            payload,
            evse,
            transaction_id: tx.id.0.clone(),
            kind,
            seq_no,
            attempts: 0,
            offline,
        })
    }

    /// Generates a `TransactionEvent` for the transaction on `evse` and queues it. An
    /// `Updated` event with trigger `Trigger` and no meter data gets the latest values.
    pub(super) fn tx_event(
        &mut self,
        evse: u32,
        kind: TransactionEventKind,
        trigger: TriggerReason,
        extra: EventExtra,
    ) {
        self.settle_tx_meters();
        match self.build_event(evse, kind, trigger, extra) {
            Some(event) => self.queue_push(event),
            None => self.log(
                LogLevel::Warn,
                format!("no transaction on EVSE {evse}: TransactionEvent not generated"),
            ),
        }
        self.sync_tx_snapshot();
    }

    // ----- start -----------------------------------------------------------------------

    /// Starts a transaction on `evse` now, whatever the start points say. The `Started`
    /// event is followed by `Updated`/`ChargingStateChanged` when the energy offer can start.
    #[allow(
        dead_code,
        reason = "kept for starting a transaction regardless of the start points; no caller yet"
    )]
    pub(super) fn begin_transaction(
        &mut self,
        evse: u32,
        spec: StartSpec,
    ) -> Result<TxId2, StationError> {
        self.begin_inner(evse, spec, None)
    }

    fn begin_inner(
        &mut self,
        evse: u32,
        spec: StartSpec,
        initial: Option<ChargingState>,
    ) -> Result<TxId2, StationError> {
        if self.evse(evse).is_none() {
            return Err(StationError::InvalidArgument(format!(
                "EVSE {evse} does not exist"
            )));
        }
        if self.tx.active.contains_key(&evse) {
            return Err(StationError::InvalidState {
                connector: None,
                reason: format!("a transaction is already running on EVSE {evse}"),
            });
        }
        self.settle_tx_meters();
        let now = self.clock.now_utc();
        let connected = self.evse_connected(evse);
        let id = TxId2(Uuid::new_v4().hyphenated().to_string());
        let register = self.tx.meters.entry(evse).or_default().register_wh;
        let state = initial.unwrap_or(if connected {
            ChargingState::EvConnected
        } else {
            ChargingState::Idle
        });
        self.tx.pending.remove(&evse);
        self.tx.active.insert(
            evse,
            ActiveTx {
                id: id.clone(),
                evse,
                next_seq_no: 0,
                started_at: now,
                charging_state: state,
                token: spec.token,
                remote_start_id: spec.remote_start_id,
                authorized: spec.authorized,
                ev_connected: connected,
                evse_sent: false,
                token_sent: false,
                meter_start_wh: whole_wh(register),
                energy_wh: 0.0,
            },
        );
        let begin = self.sample_value(
            evse,
            &self.tx_started_measurands(),
            ReadingContext::TransactionBegin,
            now,
        );
        self.tx.runtime.insert(
            evse,
            TxRuntime {
                remote_sent: false,
                begin_sample: begin.clone(),
                begin_pending: !connected && begin.is_some(),
            },
        );
        if connected {
            self.timers.cancel(&TimerKey2::EvConnectTimeout(evse));
        } else if spec.authorized {
            self.timers.set(
                TimerKey2::EvConnectTimeout(evse),
                self.ev_connection_timeout(),
            );
        }
        self.arm_periodic(evse);
        self.rearm_meter_timers();
        self.tx_event(
            evse,
            TransactionEventKind::Started,
            spec.trigger,
            EventExtra {
                meter: if connected {
                    begin.into_iter().collect()
                } else {
                    Vec::new()
                },
                charging_state: Some(state),
                ..EventExtra::default()
            },
        );
        self.maybe_start_energy(evse);
        Ok(id)
    }

    /// Starts the energy offer when the transaction is authorized, connected and nothing
    /// holds it back: the state moves from `EVConnected` to `Charging`.
    fn maybe_start_energy(&mut self, evse: u32) {
        let ready = self.tx.active.get(&evse).is_some_and(|tx| {
            tx.charging_state == ChargingState::EvConnected && tx.authorized && tx.ev_connected
        }) && self.connector_ready(evse);
        if ready {
            self.set_charging_state_internal(
                evse,
                ChargingState::Charging,
                TriggerReason::ChargingStateChanged,
            );
        }
    }

    /// Authorization decided `token` is valid on `evse`. Starts the transaction when this
    /// reaches a start point, adds the token to the running one (an event with `idToken`), or
    /// keeps the authorization until the EV connects and arms `EvConnectTimeout`. The
    /// authorization module never evaluates start or stop points itself.
    ///
    /// The event carries `trigger`; when the start point is reached by this authorization the
    /// `Started` event carries it too. An energy offer that starts as a result follows in a
    /// separate `ChargingStateChanged` event.
    pub(super) fn on_authorized(
        &mut self,
        evse: u32,
        token: IdTokenRef,
        remote_start_id: Option<i32>,
        trigger: TriggerReason,
    ) -> Result<(), StationError> {
        if self.evse(evse).is_none() {
            return Err(StationError::InvalidArgument(format!(
                "EVSE {evse} does not exist"
            )));
        }
        self.settle_tx_meters();
        if let Some(tx) = self.tx.active.get_mut(&evse) {
            if tx.authorized {
                return Ok(());
            }
            tx.authorized = true;
            tx.token = Some(token);
            tx.token_sent = false;
            let connected = tx.ev_connected;
            if remote_start_id.is_some() {
                tx.remote_start_id = remote_start_id;
                self.tx.runtime.entry(evse).or_default().remote_sent = false;
            }
            if !connected {
                self.timers.set(
                    TimerKey2::EvConnectTimeout(evse),
                    self.ev_connection_timeout(),
                );
            }
            self.tx_event(
                evse,
                TransactionEventKind::Updated,
                trigger,
                EventExtra::default(),
            );
            self.maybe_start_energy(evse);
            return Ok(());
        }
        let connected = self.evse_connected(evse);
        let can_charge = connected && self.connector_ready(evse);
        if let Some(reached) = start_reached(&self.tx_start_points(), connected, true, can_charge) {
            let (trigger, initial) = if reached == Reached::Energy {
                (
                    TriggerReason::ChargingStateChanged,
                    Some(ChargingState::Charging),
                )
            } else {
                (trigger, None)
            };
            self.begin_inner(
                evse,
                StartSpec {
                    token: Some(token),
                    remote_start_id,
                    trigger,
                    authorized: true,
                },
                initial,
            )?;
        } else {
            self.tx.pending.insert(
                evse,
                PendingAuth {
                    token,
                    remote_start_id,
                },
            );
            if !connected {
                self.timers.set(
                    TimerKey2::EvConnectTimeout(evse),
                    self.ev_connection_timeout(),
                );
            }
        }
        Ok(())
    }

    /// An EV was plugged in on `evse`.
    ///
    /// Without a transaction this starts one when `EVConnected` is a start point, or when the
    /// token that was accepted earlier completes another start point (`CablePluggedIn`). A
    /// running transaction gets an `Updated` event with `CablePluggedIn` and the `evse` field.
    pub(super) fn on_ev_connected(&mut self, evse: u32) {
        self.settle_tx_meters();
        if let Some(tx) = self.tx.active.get_mut(&evse) {
            self.timers.cancel(&TimerKey2::EvConnectTimeout(evse));
            tx.ev_connected = true;
            tx.evse_sent = false;
            tx.charging_state = ChargingState::EvConnected;
            self.tx_event(
                evse,
                TransactionEventKind::Updated,
                TriggerReason::CablePluggedIn,
                EventExtra {
                    charging_state: Some(ChargingState::EvConnected),
                    ..EventExtra::default()
                },
            );
            self.maybe_start_energy(evse);
            return;
        }
        let pending = self.tx.pending.get(&evse).cloned();
        let authorized = pending.is_some();
        let can_charge = authorized && self.connector_ready(evse);
        let Some(reached) = start_reached(&self.tx_start_points(), true, authorized, can_charge)
        else {
            // A waiting authorization keeps its timer: it still has to expire.
            if !authorized {
                self.timers.cancel(&TimerKey2::EvConnectTimeout(evse));
            }
            return;
        };
        self.timers.cancel(&TimerKey2::EvConnectTimeout(evse));
        let (trigger, initial) = if reached == Reached::Energy {
            (
                TriggerReason::ChargingStateChanged,
                Some(ChargingState::Charging),
            )
        } else {
            (TriggerReason::CablePluggedIn, None)
        };
        let (token, remote_start_id) = match pending {
            Some(pending) => (Some(pending.token), pending.remote_start_id),
            None => (None, None),
        };
        let spec = StartSpec {
            token,
            remote_start_id,
            trigger,
            authorized,
        };
        if let Err(error) = self.begin_inner(evse, spec, initial) {
            self.log(LogLevel::Warn, format!("transaction not started: {error}"));
        }
    }

    /// The EV on `evse` was unplugged. With `StopTxOnEVSideDisconnect` or a stop point that the
    /// connection loss ends, the transaction ends (`EVCommunicationLost`, `EVDisconnected`);
    /// otherwise it is suspended (`SuspendedEV`) and resumes when the cable is plugged in
    /// again (E10).
    pub(super) fn on_ev_disconnected(&mut self, evse: u32) {
        self.settle_tx_meters();
        let Some(tx) = self.tx.active.get_mut(&evse) else {
            return;
        };
        tx.ev_connected = false;
        let charging = tx.charging_state == ChargingState::Charging;
        if disconnect_stops(
            &self.tx_stop_points(),
            self.stop_tx_on_ev_side_disconnect(),
            charging,
        ) {
            self.finish(
                evse,
                StopReason::EvDisconnected,
                TriggerReason::EvCommunicationLost,
                None,
            );
        } else {
            self.set_charging_state_internal(
                evse,
                ChargingState::SuspendedEv,
                TriggerReason::EvCommunicationLost,
            );
        }
    }

    /// The EV did not connect in time on `evse`: a transaction ends with `EVConnectTimeout`
    /// and `Timeout` (E03.FR.05, F02.FR.07); an authorization without a transaction is
    /// dropped.
    pub(super) fn on_ev_connect_timeout(&mut self, evse: u32) {
        self.settle_tx_meters();
        let waiting = self.tx.active.get(&evse).is_some_and(|tx| !tx.ev_connected);
        if waiting {
            self.finish(
                evse,
                StopReason::Timeout,
                TriggerReason::EvConnectTimeout,
                None,
            );
        } else if self.tx.pending.remove(&evse).is_some() {
            self.log(
                LogLevel::Info,
                format!("EV did not connect to EVSE {evse} in time: authorization dropped"),
            );
        }
    }

    // ----- state changes ---------------------------------------------------------------

    /// Changes the charging state of the transaction on `evse` from inside the station and
    /// sends the event. When energy stops and `EnergyTransfer` is a stop point the transaction
    /// ends instead.
    pub(super) fn set_charging_state_internal(
        &mut self,
        evse: u32,
        state: ChargingState,
        trigger: TriggerReason,
    ) {
        self.settle_tx_meters();
        let Some(tx) = self.tx.active.get_mut(&evse) else {
            return;
        };
        if tx.charging_state == state {
            return;
        }
        let was_charging = tx.charging_state == ChargingState::Charging;
        tx.charging_state = state;
        if was_charging
            && state != ChargingState::Charging
            && self
                .tx_stop_points()
                .contains(&StartStopPoint::EnergyTransfer)
        {
            let reason = if state == ChargingState::SuspendedEv {
                StopReason::StoppedByEv
            } else {
                StopReason::Other
            };
            self.finish(evse, reason, trigger, None);
            return;
        }
        self.tx_event(
            evse,
            TransactionEventKind::Updated,
            trigger,
            EventExtra {
                charging_state: Some(state),
                ..EventExtra::default()
            },
        );
    }

    /// `Command2::SetChargingState`.
    pub(super) fn cmd_set_charging_state(
        &mut self,
        evse: u32,
        state: ChargingState,
        reply: Reply2,
    ) {
        let result = if self.tx.active.contains_key(&evse) {
            self.set_charging_state_internal(evse, state, TriggerReason::ChargingStateChanged);
            Ok(Outcome2::Done)
        } else {
            Err(no_transaction(evse))
        };
        // The requester may have gone away; nothing to do about that.
        let _ = reply.send(result);
    }

    /// Authorization on `evse` ended (the token was presented again, or the CSMS reported it
    /// invalid). Ends the transaction when a stop point ends, otherwise sends an event with the
    /// token once more; drops a pending authorization that has no transaction yet.
    ///
    /// With `StopAuthorized` the stopping event carries the `idToken` of the transaction. In
    /// the event that does not end the transaction the charging state returns to
    /// `EVConnected`.
    pub(super) fn on_deauthorized(
        &mut self,
        evse: u32,
        trigger: TriggerReason,
        reason: StopReason,
    ) {
        self.settle_tx_meters();
        let Some(tx) = self.tx.active.get(&evse) else {
            self.tx.pending.remove(&evse);
            self.timers.cancel(&TimerKey2::EvConnectTimeout(evse));
            return;
        };
        if !tx.authorized {
            return;
        }
        let charging = tx.charging_state == ChargingState::Charging;
        let stop_token = (trigger == TriggerReason::StopAuthorized)
            .then(|| tx.token.clone())
            .flatten();
        if authorization_stops(&self.tx_stop_points(), charging) {
            self.finish(evse, reason, trigger, stop_token);
        } else {
            self.stop_energy(evse, trigger, stop_token);
        }
    }

    /// `RequestStopTransaction` accepted for the transaction on `evse`: `Ended` when a stop
    /// point ends, otherwise `Updated` with `RemoteStop` and energy delivery stops (F03.FR.02,
    /// F03.FR.03).
    pub(super) fn remote_stop(&mut self, evse: u32) {
        self.settle_tx_meters();
        let Some(tx) = self.tx.active.get(&evse) else {
            return;
        };
        let charging = tx.charging_state == ChargingState::Charging;
        if authorization_stops(&self.tx_stop_points(), charging) {
            self.finish(evse, StopReason::Remote, TriggerReason::RemoteStop, None);
        } else {
            self.stop_energy(evse, TriggerReason::RemoteStop, None);
        }
    }

    /// Ends the authorization of a transaction that keeps running: energy stops and the state
    /// returns to `EVConnected` (or `Idle` without an EV), reported in one event.
    fn stop_energy(&mut self, evse: u32, trigger: TriggerReason, token: Option<IdTokenRef>) {
        let Some(tx) = self.tx.active.get_mut(&evse) else {
            return;
        };
        let state = if tx.ev_connected {
            ChargingState::EvConnected
        } else {
            ChargingState::Idle
        };
        tx.authorized = false;
        tx.charging_state = state;
        self.tx_event(
            evse,
            TransactionEventKind::Updated,
            trigger,
            EventExtra {
                charging_state: Some(state),
                id_token: token,
                ..EventExtra::default()
            },
        );
    }

    /// A fault was injected on `evse`: the transaction ends (`AbnormalCondition`, `Other`)
    /// and a waiting authorization is dropped.
    pub(super) fn on_fault(&mut self, evse: u32) {
        self.settle_tx_meters();
        self.tx.pending.remove(&evse);
        if self.tx.active.contains_key(&evse) {
            self.finish(
                evse,
                StopReason::Other,
                TriggerReason::AbnormalCondition,
                None,
            );
        }
    }

    // ----- end -------------------------------------------------------------------------

    /// Ends the transaction on `evse`.
    pub(super) fn end_transaction(
        &mut self,
        evse: u32,
        reason: StopReason,
        trigger: TriggerReason,
    ) -> Result<(), StationError> {
        self.end_with(evse, reason, trigger, None)
    }

    /// `end_transaction` for the paths that cannot report an error to a caller.
    fn finish(
        &mut self,
        evse: u32,
        reason: StopReason,
        trigger: TriggerReason,
        token: Option<IdTokenRef>,
    ) {
        if let Err(error) = self.end_with(evse, reason, trigger, token) {
            self.log(LogLevel::Warn, format!("transaction not ended: {error}"));
        }
    }

    /// Ends the transaction and then runs the hooks that react to it: scheduled availability
    /// (`on_transaction_ended`) and a reset that waited for idle.
    fn end_with(
        &mut self,
        evse: u32,
        reason: StopReason,
        trigger: TriggerReason,
        token: Option<IdTokenRef>,
    ) -> Result<(), StationError> {
        self.end_core(evse, reason, trigger, token, false)?;
        self.flush_now();
        self.on_transaction_ended(evse);
        self.reset_on_transaction_ended(evse);
        Ok(())
    }

    /// Generates the `Ended` event with the begin and end samples, removes the transaction and
    /// queues the event. With `quiet` nothing is written or sent (state restore).
    fn end_core(
        &mut self,
        evse: u32,
        reason: StopReason,
        trigger: TriggerReason,
        token: Option<IdTokenRef>,
        quiet: bool,
    ) -> Result<(), StationError> {
        if !self.tx.active.contains_key(&evse) {
            return Err(no_transaction(evse));
        }
        if !quiet {
            self.settle_tx_meters();
        }
        let now = self.clock.now_utc();
        let mut meter: Vec<MeterValue> = self
            .tx
            .runtime
            .get(&evse)
            .and_then(|runtime| runtime.begin_sample.clone())
            .into_iter()
            .collect();
        meter.extend(self.sample_value(
            evse,
            &self.tx_ended_measurands(),
            ReadingContext::TransactionEnd,
            now,
        ));
        let state = match self.tx.active.get_mut(&evse) {
            Some(tx) => {
                tx.charging_state = if tx.ev_connected {
                    ChargingState::EvConnected
                } else {
                    ChargingState::Idle
                };
                tx.charging_state
            }
            None => return Err(no_transaction(evse)),
        };
        let event = self.build_event(
            evse,
            TransactionEventKind::Ended,
            trigger,
            EventExtra {
                meter,
                id_token: token,
                stopped_reason: Some(reason),
                charging_state: Some(state),
                remote_start_id: None,
            },
        );
        // The transaction is gone before the queue is written, so a saved state never holds
        // both the transaction and its `Ended` event.
        if let Some(tx) = self.tx.active.remove(&evse) {
            self.tx.stopped.push_back(tx.id.0);
            while self.tx.stopped.len() > STOPPED_HISTORY {
                self.tx.stopped.pop_front();
            }
        }
        self.tx.runtime.remove(&evse);
        self.tx.pending.remove(&evse);
        if let Some(meter) = self.tx.meters.get_mut(&evse) {
            meter.charging = false;
        }
        self.timers.cancel(&TimerKey2::MeterPeriodic(evse));
        self.timers.cancel(&TimerKey2::EvConnectTimeout(evse));
        if self.tx.active.is_empty() {
            self.timers.cancel(&TimerKey2::MeterAligned);
            self.tx.seen_intervals = None;
        }
        self.sync_tx_snapshot();
        match event {
            Some(event) if quiet => self.queue_enqueue(event),
            Some(event) => self.queue_push(event),
            None => self.log(
                LogLevel::Warn,
                format!("EVSE {evse}: Ended event could not be generated"),
            ),
        }
        Ok(())
    }

    /// `Command2::StopTransaction`: a local stop. The trigger follows the reason.
    pub(super) fn cmd_stop_transaction(&mut self, evse: u32, reason: StopReason, reply: Reply2) {
        let result = self.stop_locally(evse, reason);
        // The requester may have gone away; nothing to do about that.
        let _ = reply.send(result.map(|()| Outcome2::Done));
    }

    fn stop_locally(&mut self, evse: u32, reason: StopReason) -> Result<(), StationError> {
        let Some(tx) = self.tx.active.get(&evse) else {
            return Err(no_transaction(evse));
        };
        let trigger = match reason {
            StopReason::Local => TriggerReason::StopAuthorized,
            StopReason::EvDisconnected => TriggerReason::EvCommunicationLost,
            StopReason::Remote => TriggerReason::RemoteStop,
            StopReason::DeAuthorized => TriggerReason::Deauthorized,
            StopReason::Timeout => TriggerReason::EvConnectTimeout,
            StopReason::ImmediateReset | StopReason::Reboot => TriggerReason::ResetCommand,
            _ => TriggerReason::AbnormalCondition,
        };
        let token = (trigger == TriggerReason::StopAuthorized)
            .then(|| tx.token.clone())
            .flatten();
        self.end_with(evse, reason, trigger, token)
    }

    // ----- meter commands --------------------------------------------------------------

    /// `Command2::AdvanceMeter`: adds to the register of `evse` (and to the running
    /// transaction). Without a transaction the new reading goes out as `MeterValues`.
    pub(super) fn cmd_advance_meter(&mut self, evse: u32, wh_delta: u64, reply: Reply2) {
        if self.evse(evse).is_none() {
            // The requester may have gone away; nothing to do about that.
            let _ = reply.send(Err(StationError::InvalidArgument(format!(
                "EVSE {evse} does not exist"
            ))));
            return;
        }
        self.settle_tx_meters();
        let delta = wh_from(wh_delta);
        self.tx.meters.entry(evse).or_default().register_wh += delta;
        let in_transaction = match self.tx.active.get_mut(&evse) {
            Some(tx) => {
                tx.energy_wh += delta;
                true
            }
            None => false,
        };
        self.sync_tx_snapshot();
        let _ = reply.send(Ok(Outcome2::Done));
        if in_transaction {
            // The delivered energy is part of the state a restart keeps.
            self.mark_dirty();
        } else {
            self.send_meter_values(Some(evse), Origin2::Station);
        }
    }

    // ----- persistence -----------------------------------------------------------------

    /// Active transactions and the ids of the last stopped ones, for the state file.
    pub(super) fn tx_persist(&self) -> TxPersist {
        TxPersist {
            active: self.tx.active.values().cloned().collect(),
            stopped_ids: self.tx.stopped.iter().cloned().collect(),
        }
    }

    /// Takes back the persisted transactions at start. The process was down, so the EV counts
    /// as disconnected and every restored transaction ends at once with `PowerLoss` and
    /// `AbnormalCondition`; its `Ended` event continues the `seqNo` and waits in the queue
    /// until the boot has been accepted.
    pub(super) fn tx_restore(&mut self, tx: TxPersist) {
        let skip = tx.stopped_ids.len().saturating_sub(STOPPED_HISTORY);
        self.tx.stopped = tx.stopped_ids.into_iter().skip(skip).collect();
        for mut active in tx.active {
            let evse = active.evse;
            if self.tx.active.contains_key(&evse) {
                continue;
            }
            active.ev_connected = false;
            self.tx.meters.entry(evse).or_default().register_wh =
                wh_from(active.meter_start_wh) + active.energy_wh;
            self.tx.active.insert(evse, active);
            self.tx.runtime.insert(evse, TxRuntime::default());
            if let Err(error) = self.end_core(
                evse,
                StopReason::PowerLoss,
                TriggerReason::AbnormalCondition,
                None,
                true,
            ) {
                self.log(
                    LogLevel::Warn,
                    format!("restored transaction not ended: {error}"),
                );
            }
        }
        self.sync_tx_snapshot();
    }
}

/// The error for a command that needs a transaction on `evse`.
fn no_transaction(evse: u32) -> StationError {
    StationError::InvalidState {
        connector: None,
        reason: format!("no transaction is running on EVSE {evse}"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Value, json};
    use tokio::sync::{broadcast, mpsc, oneshot};

    use super::*;
    use crate::clock::ManualClock;
    use crate::config::StationConfig;
    use crate::ocpp::Protocol;
    use crate::ocpp::v2::types::{Component, IdTokenType, Variable};

    /// A station without a connection and without persistence: every event is queued offline.
    fn actor() -> Station2Actor {
        let mut config = StationConfig::new("ws://127.0.0.1:1")
            .with_connectors(2)
            .without_persistence();
        config.protocol = Protocol::V201;
        let (_commands, receiver) = mpsc::channel(4);
        let (events, _) = broadcast::channel(64);
        Station2Actor::new(
            config,
            Arc::new(ManualClock::new(Utc::now())),
            receiver,
            events,
            None,
        )
        .0
    }

    fn token(id: &str) -> IdTokenRef {
        IdTokenRef {
            id_token: id.to_owned(),
            kind: IdTokenType::Local,
        }
    }

    fn set_var(actor: &mut Station2Actor, component: &str, variable: &str, value: &str) {
        let component = Component {
            evse: None,
            name: component.to_owned(),
            instance: None,
            custom_data: None,
        };
        let variable = Variable {
            name: variable.to_owned(),
            instance: None,
            custom_data: None,
        };
        actor
            .dm
            .set_value(&component, &variable, None, value)
            .expect("variable accepted");
    }

    fn plug(actor: &mut Station2Actor, evse: u32) {
        let (reply, _answer) = oneshot::channel();
        actor.cmd_plug_in(evse, reply);
    }

    fn unplug(actor: &mut Station2Actor, evse: u32) {
        let (reply, _answer) = oneshot::channel();
        actor.cmd_unplug(evse, reply);
    }

    fn payloads(actor: &Station2Actor) -> Vec<Value> {
        actor
            .queued_events()
            .into_iter()
            .map(|event| event.payload)
            .collect()
    }

    /// `(eventType, triggerReason, seqNo, chargingState)` of each queued event.
    fn summary(actor: &Station2Actor) -> Vec<(String, String, i64, Option<String>)> {
        payloads(actor)
            .iter()
            .map(|p| {
                (
                    p["eventType"].as_str().unwrap_or_default().to_owned(),
                    p["triggerReason"].as_str().unwrap_or_default().to_owned(),
                    p["seqNo"].as_i64().unwrap_or(-1),
                    p["transactionInfo"]["chargingState"]
                        .as_str()
                        .map(str::to_owned),
                )
            })
            .collect()
    }

    fn row(
        kind: &str,
        trigger: &str,
        seq: i64,
        state: Option<&str>,
    ) -> (String, String, i64, Option<String>) {
        (
            kind.to_owned(),
            trigger.to_owned(),
            seq,
            state.map(str::to_owned),
        )
    }

    #[test]
    fn start_points_follow_the_configuration() {
        use StartStopPoint::{Authorized, EnergyTransfer, EvConnected, PowerPathClosed};
        assert_eq!(
            start_reached(&[EvConnected], true, false, false),
            Some(Reached::Connected)
        );
        assert_eq!(start_reached(&[EvConnected], false, true, false), None);
        assert_eq!(
            start_reached(&[Authorized], false, true, false),
            Some(Reached::Authorized)
        );
        assert_eq!(start_reached(&[PowerPathClosed], true, true, false), None);
        assert_eq!(
            start_reached(&[PowerPathClosed], true, true, true),
            Some(Reached::PowerPath)
        );
        assert_eq!(
            start_reached(&[EnergyTransfer], true, true, true),
            Some(Reached::Energy)
        );
        assert!(authorization_stops(&[Authorized], false));
        assert!(authorization_stops(&[EvConnected, PowerPathClosed], false));
        assert!(!authorization_stops(&[EvConnected], true));
        assert!(!authorization_stops(&[EnergyTransfer], false));
        assert!(authorization_stops(&[EnergyTransfer], true));
        assert!(disconnect_stops(&[Authorized], true, false));
        assert!(!disconnect_stops(&[Authorized], false, false));
        assert!(disconnect_stops(&[EvConnected], false, false));
    }

    #[tokio::test]
    async fn default_session_has_ordered_events_and_merged_fields() {
        let mut a = actor();
        plug(&mut a, 1);
        assert!(summary(&a).is_empty(), "plug alone starts nothing");
        a.on_authorized(1, token("TAG"), None, TriggerReason::Authorized)
            .expect("authorized");
        unplug(&mut a, 1);
        assert_eq!(
            summary(&a),
            [
                row("Started", "Authorized", 0, Some("EVConnected")),
                row("Updated", "ChargingStateChanged", 1, Some("Charging")),
                row("Ended", "EVCommunicationLost", 2, Some("Idle")),
            ]
        );
        let events = payloads(&a);
        let id = events[0]["transactionInfo"]["transactionId"]
            .as_str()
            .expect("transaction id");
        assert_eq!(id.len(), 36);
        assert!(
            events
                .iter()
                .all(|e| e["transactionInfo"]["transactionId"] == id)
        );
        // Merge rule: `evse` and `idToken` only in the first event.
        assert_eq!(events[0]["evse"], json!({"id": 1, "connectorId": 1}));
        assert_eq!(
            events[0]["idToken"],
            json!({"idToken": "TAG", "type": "Local"})
        );
        for later in &events[1..] {
            assert!(later.get("evse").is_none(), "{later}");
            assert!(later.get("idToken").is_none(), "{later}");
        }
        // Generated without a link: flagged offline.
        assert!(events.iter().all(|e| e["offline"] == true));
        // Begin and end samples in the final event.
        let contexts: Vec<&str> = events[2]["meterValue"]
            .as_array()
            .expect("meter values")
            .iter()
            .map(|m| m["sampledValue"][0]["context"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(contexts, ["Transaction.Begin", "Transaction.End"]);
        assert_eq!(
            events[2]["transactionInfo"]["stoppedReason"],
            "EVDisconnected"
        );
        assert_eq!(a.tx_by_id(id), Some(TxLookup { ongoing: false }));
        assert!(a.state.evses[0].transaction.is_none());
    }

    #[tokio::test]
    async fn token_first_waits_for_the_cable() {
        let mut a = actor();
        a.on_authorized(1, token("TAG"), Some(7), TriggerReason::RemoteStart)
            .expect("authorized");
        assert_eq!(summary(&a).len(), 0);
        assert!(a.timers.is_armed(&TimerKey2::EvConnectTimeout(1)));
        assert_eq!(a.authorized_token(1), Some(&token("TAG")));
        plug(&mut a, 1);
        assert!(!a.timers.is_armed(&TimerKey2::EvConnectTimeout(1)));
        assert_eq!(
            summary(&a),
            [
                row("Started", "CablePluggedIn", 0, Some("EVConnected")),
                row("Updated", "ChargingStateChanged", 1, Some("Charging")),
            ]
        );
        let events = payloads(&a);
        assert_eq!(events[0]["transactionInfo"]["remoteStartId"], 7);
        assert!(events[1]["transactionInfo"].get("remoteStartId").is_none());
        assert_eq!(events[0]["idToken"]["idToken"], "TAG");
    }

    #[tokio::test]
    async fn connect_timeout_drops_a_waiting_authorization() {
        let mut a = actor();
        a.on_authorized(1, token("TAG"), None, TriggerReason::Authorized)
            .expect("authorized");
        a.on_ev_connect_timeout(1);
        assert!(a.authorized_token(1).is_none());
        assert_eq!(summary(&a).len(), 0);
        plug(&mut a, 1);
        assert!(summary(&a).is_empty(), "the dropped token starts nothing");
    }

    #[tokio::test]
    async fn a_plug_that_starts_nothing_keeps_the_connect_timeout_armed() {
        let mut a = actor();
        set_var(&mut a, "TxCtrlr", "TxStartPoint", "PowerPathClosed");
        a.on_authorized(1, token("TAG"), None, TriggerReason::Authorized)
            .expect("authorized");
        assert!(a.timers.is_armed(&TimerKey2::EvConnectTimeout(1)));
        // The connector is not occupied, so the power path stays open.
        a.on_ev_connected(1);
        assert!(a.tx.active.is_empty(), "nothing starts");
        assert!(a.tx.pending.contains_key(&1), "the token is still waiting");
        assert!(
            a.timers.is_armed(&TimerKey2::EvConnectTimeout(1)),
            "the pending token must still expire"
        );
    }

    #[tokio::test]
    async fn authorized_start_point_ends_with_the_connect_timeout() {
        let mut a = actor();
        set_var(&mut a, "TxCtrlr", "TxStartPoint", "Authorized");
        a.on_authorized(1, token("TAG"), None, TriggerReason::Authorized)
            .expect("authorized");
        a.on_ev_connect_timeout(1);
        assert_eq!(
            summary(&a),
            [
                row("Started", "Authorized", 0, Some("Idle")),
                row("Ended", "EVConnectTimeout", 1, Some("Idle")),
            ]
        );
        let events = payloads(&a);
        assert!(events[0].get("evse").is_none(), "the EVSE is not known yet");
        assert!(events[0].get("meterValue").is_none());
        assert_eq!(events[1]["transactionInfo"]["stoppedReason"], "Timeout");
    }

    #[tokio::test]
    async fn begin_sample_waits_for_the_first_event_with_the_evse() {
        let mut a = actor();
        set_var(&mut a, "TxCtrlr", "TxStartPoint", "Authorized");
        a.on_authorized(1, token("TAG"), None, TriggerReason::Authorized)
            .expect("authorized");
        plug(&mut a, 1);
        assert_eq!(
            summary(&a),
            [
                row("Started", "Authorized", 0, Some("Idle")),
                row("Updated", "CablePluggedIn", 1, Some("EVConnected")),
                row("Updated", "ChargingStateChanged", 2, Some("Charging")),
            ]
        );
        let events = payloads(&a);
        assert_eq!(events[1]["evse"]["id"], 1);
        assert_eq!(
            events[1]["meterValue"][0]["sampledValue"][0]["context"],
            "Transaction.Begin"
        );
        assert!(events[2].get("meterValue").is_none());
    }

    #[tokio::test]
    async fn cable_start_then_late_token() {
        let mut a = actor();
        set_var(&mut a, "TxCtrlr", "TxStartPoint", "EVConnected");
        plug(&mut a, 1);
        a.on_authorized(1, token("TAG"), None, TriggerReason::Authorized)
            .expect("authorized");
        a.on_deauthorized(1, TriggerReason::StopAuthorized, StopReason::Local);
        assert_eq!(
            summary(&a),
            [
                row("Started", "CablePluggedIn", 0, Some("EVConnected")),
                row("Updated", "Authorized", 1, None),
                row("Updated", "ChargingStateChanged", 2, Some("Charging")),
                row("Ended", "StopAuthorized", 3, Some("EVConnected")),
            ]
        );
        let events = payloads(&a);
        assert!(events[0].get("idToken").is_none());
        assert_eq!(events[1]["idToken"]["idToken"], "TAG");
        assert_eq!(events[3]["idToken"]["idToken"], "TAG");
        assert_eq!(events[3]["transactionInfo"]["stoppedReason"], "Local");
    }

    #[tokio::test]
    async fn deauthorization_keeps_a_transaction_that_stops_on_the_cable() {
        let mut a = actor();
        set_var(&mut a, "TxCtrlr", "TxStopPoint", "EVConnected");
        plug(&mut a, 1);
        a.on_authorized(1, token("TAG"), None, TriggerReason::Authorized)
            .expect("authorized");
        a.on_deauthorized(1, TriggerReason::Deauthorized, StopReason::DeAuthorized);
        assert!(a.active_tx(1).is_some());
        assert!(a.authorized_token(1).is_none());
        assert_eq!(
            summary(&a).last().cloned(),
            Some(row("Updated", "Deauthorized", 2, Some("EVConnected")))
        );
        unplug(&mut a, 1);
        assert_eq!(
            summary(&a).last().cloned(),
            Some(row("Ended", "EVCommunicationLost", 3, Some("Idle")))
        );
    }

    #[tokio::test]
    async fn remote_stop_ends_or_updates_by_the_stop_points() {
        let mut ends = actor();
        plug(&mut ends, 1);
        ends.on_authorized(1, token("TAG"), None, TriggerReason::RemoteStart)
            .expect("authorized");
        ends.remote_stop(1);
        let last = payloads(&ends).pop().expect("event");
        assert_eq!(last["eventType"], "Ended");
        assert_eq!(last["triggerReason"], "RemoteStop");
        assert_eq!(last["transactionInfo"]["stoppedReason"], "Remote");

        let mut stays = actor();
        set_var(&mut stays, "TxCtrlr", "TxStopPoint", "EVConnected");
        plug(&mut stays, 1);
        stays
            .on_authorized(1, token("TAG"), None, TriggerReason::RemoteStart)
            .expect("authorized");
        stays.remote_stop(1);
        let last = payloads(&stays).pop().expect("event");
        assert_eq!(last["eventType"], "Updated");
        assert_eq!(last["triggerReason"], "RemoteStop");
        assert_eq!(last["transactionInfo"]["chargingState"], "EVConnected");
        assert!(stays.active_tx(1).is_some());
    }

    #[tokio::test]
    async fn unplug_suspends_when_the_transaction_survives_the_cable() {
        let mut a = actor();
        set_var(&mut a, "TxCtrlr", "TxStopPoint", "Authorized");
        set_var(&mut a, "TxCtrlr", "StopTxOnEVSideDisconnect", "false");
        plug(&mut a, 1);
        a.on_authorized(1, token("TAG"), None, TriggerReason::Authorized)
            .expect("authorized");
        unplug(&mut a, 1);
        plug(&mut a, 1);
        let rows = summary(&a);
        assert_eq!(
            rows[2..],
            [
                row("Updated", "EVCommunicationLost", 2, Some("SuspendedEV")),
                row("Updated", "CablePluggedIn", 3, Some("EVConnected")),
                row("Updated", "ChargingStateChanged", 4, Some("Charging")),
            ]
        );
        let events = payloads(&a);
        assert_eq!(
            events[3]["evse"]["id"], 1,
            "evse is sent again after replug"
        );
    }

    #[tokio::test]
    async fn fault_ends_the_transaction() {
        let mut a = actor();
        plug(&mut a, 1);
        a.on_authorized(1, token("TAG"), None, TriggerReason::Authorized)
            .expect("authorized");
        a.on_fault(1);
        let last = payloads(&a).pop().expect("event");
        assert_eq!(last["eventType"], "Ended");
        assert_eq!(last["triggerReason"], "AbnormalCondition");
        assert_eq!(last["transactionInfo"]["stoppedReason"], "Other");
    }

    #[tokio::test]
    async fn trigger_event_gets_the_latest_meter_values() {
        let mut a = actor();
        plug(&mut a, 1);
        a.on_authorized(1, token("TAG"), None, TriggerReason::Authorized)
            .expect("authorized");
        let before = a.queued_events().len();
        a.tx_event(
            1,
            TransactionEventKind::Updated,
            TriggerReason::Trigger,
            EventExtra::default(),
        );
        let events = payloads(&a);
        let last = &events[before];
        assert_eq!(last["triggerReason"], "Trigger");
        assert_eq!(last["transactionInfo"]["chargingState"], "Charging");
        assert_eq!(
            last["meterValue"][0]["sampledValue"][0]["context"],
            "Trigger"
        );
    }

    #[tokio::test]
    async fn local_stop_picks_the_trigger_from_the_reason() {
        let mut a = actor();
        plug(&mut a, 1);
        a.on_authorized(1, token("TAG"), None, TriggerReason::Authorized)
            .expect("authorized");
        let (reply, _answer) = oneshot::channel();
        a.cmd_stop_transaction(1, StopReason::Local, reply);
        let last = payloads(&a).pop().expect("event");
        assert_eq!(last["triggerReason"], "StopAuthorized");
        assert_eq!(last["transactionInfo"]["stoppedReason"], "Local");
        let (reply, answer) = oneshot::channel();
        a.cmd_stop_transaction(1, StopReason::Local, reply);
        assert!(answer.await.expect("answered").is_err(), "nothing to stop");
    }

    #[tokio::test]
    async fn advance_meter_moves_the_register_and_the_transaction_energy() {
        let mut a = actor();
        plug(&mut a, 1);
        a.on_authorized(1, token("TAG"), None, TriggerReason::Authorized)
            .expect("authorized");
        let (reply, _answer) = oneshot::channel();
        a.cmd_advance_meter(1, 1500, reply);
        let snapshot = &a.state.evses[0];
        assert!(snapshot.meter_wh >= 1500);
        assert!(
            snapshot
                .transaction
                .as_ref()
                .expect("transaction")
                .energy_wh
                >= 1500
        );
        // The other EVSE is untouched.
        assert_eq!(a.state.evses[1].meter_wh, 0);
        let (reply, answer) = oneshot::channel();
        a.cmd_advance_meter(9, 1, reply);
        assert!(answer.await.expect("answered").is_err());
    }

    #[tokio::test]
    async fn a_second_transaction_on_the_same_evse_is_refused() {
        let mut a = actor();
        let spec = StartSpec {
            token: None,
            remote_start_id: None,
            trigger: TriggerReason::CablePluggedIn,
            authorized: false,
        };
        let first = a.begin_transaction(1, spec.clone()).expect("started");
        assert!(a.begin_transaction(1, spec.clone()).is_err());
        let other = a.begin_transaction(2, spec).expect("started");
        assert_ne!(first, other, "ids are unique");
        assert_eq!(first.0.len(), 36);
    }

    #[tokio::test]
    async fn queue_cap_thins_updated_and_keeps_started_and_ended() {
        let mut a = actor();
        a.set_queue_cap(4);
        plug(&mut a, 1);
        a.on_authorized(1, token("TAG"), None, TriggerReason::Authorized)
            .expect("authorized");
        for _ in 0..20 {
            a.on_meter_periodic(1);
        }
        let kinds: Vec<TransactionEventKind> = a.queued_events().iter().map(|e| e.kind).collect();
        assert!(kinds.len() <= 5, "bounded: {}", kinds.len());
        assert_eq!(kinds[0], TransactionEventKind::Started);
        let seqs: Vec<i32> = a.queued_events().iter().map(|e| e.seq_no).collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "order kept: {seqs:?}");
        unplug(&mut a, 1);
        let last = a.queued_events().pop().expect("event");
        assert_eq!(last.kind, TransactionEventKind::Ended);
        assert_eq!(a.state.queued_events as usize, a.queued_events().len());
    }

    fn restored(seq: i32) -> ActiveTx {
        ActiveTx {
            id: TxId2("restored-id".to_owned()),
            evse: 1,
            next_seq_no: seq,
            started_at: Utc::now(),
            charging_state: ChargingState::Charging,
            token: Some(token("TAG")),
            remote_start_id: None,
            authorized: true,
            ev_connected: true,
            evse_sent: true,
            token_sent: true,
            meter_start_wh: 100,
            energy_wh: 50.0,
        }
    }

    #[tokio::test]
    async fn restored_transaction_ends_with_power_loss_after_the_restored_queue() {
        let mut a = actor();
        let queued = |seq: i32| QueuedEvent {
            payload: json!({"seqNo": seq}),
            evse: 1,
            transaction_id: "restored-id".to_owned(),
            kind: TransactionEventKind::Updated,
            seq_no: seq,
            attempts: 2,
            offline: false,
        };
        // The order of `restore_persisted`: transactions first, then the queue.
        a.tx_restore(TxPersist {
            active: vec![restored(5)],
            stopped_ids: (0..70).map(|i| format!("old-{i}")).collect(),
        });
        a.queue_restore(crate::station2::QueuePersist {
            events: vec![queued(3), queued(4)],
        });
        assert!(a.active_tx(1).is_none());
        assert_eq!(a.tx_by_id("restored-id"), Some(TxLookup { ongoing: false }));
        assert_eq!(a.tx_by_id("old-5"), None, "history keeps the last 64");
        assert_eq!(a.tx_by_id("old-69"), Some(TxLookup { ongoing: false }));
        let events = a.queued_events();
        assert_eq!(
            events.iter().map(|e| e.seq_no).collect::<Vec<_>>(),
            [3, 4, 5]
        );
        assert!(
            events
                .iter()
                .all(|e| e.offline && e.payload["offline"] == true)
        );
        assert_eq!(events[0].attempts, 2, "attempts are kept");
        let ended = &events[2].payload;
        assert_eq!(ended["eventType"], "Ended");
        assert_eq!(ended["triggerReason"], "AbnormalCondition");
        assert_eq!(ended["transactionInfo"]["stoppedReason"], "PowerLoss");
        assert_eq!(ended["transactionInfo"]["chargingState"], "Idle");
        // The end sample shows the register: start 100 + 50 Wh delivered.
        assert_eq!(ended["meterValue"][0]["sampledValue"][0]["value"], 150);
        assert!(a.queue_has_messages_for(Some("restored-id")));
        assert!(!a.queue_has_messages_for(Some("other")));
    }

    #[tokio::test]
    async fn persisted_state_holds_active_transactions_and_stopped_ids() {
        let mut a = actor();
        plug(&mut a, 1);
        a.on_authorized(1, token("TAG"), None, TriggerReason::Authorized)
            .expect("authorized");
        let persisted = a.tx_persist();
        assert_eq!(persisted.active.len(), 1);
        assert_eq!(persisted.active[0].next_seq_no, 2);
        assert_eq!(persisted.stopped_ids.len(), 0);
        unplug(&mut a, 1);
        let persisted = a.tx_persist();
        assert_eq!(persisted.active.len(), 0);
        assert_eq!(persisted.stopped_ids.len(), 1);
        assert_eq!(a.queue_persist().events.len(), 3);
    }
}
