//! The transaction state machine, `TransactionEvent` generation and metering.

#![allow(
    dead_code,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    reason = "placeholder module; remove this allow when the module is implemented"
)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::actor::Station2Actor;
use super::commands::not_implemented;
use super::types::{IdTokenRef, Origin2, Reply2, TxId2};
use crate::error::StationError;
use crate::ocpp::v2::messages::{TransactionEventKind, TriggerReason};
use crate::ocpp::v2::types::{ChargingState, MeterValue, StopReason};

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

/// The transactions of the station.
#[derive(Debug, Default)]
pub(crate) struct TxEngine {}

impl Station2Actor {
    /// Starts a transaction on `evse`.
    pub(super) fn begin_transaction(
        &mut self,
        _evse: u32,
        _spec: StartSpec,
    ) -> Result<TxId2, StationError> {
        Err(StationError::NotImplemented {
            command: "begin_transaction",
        })
    }

    /// Ends the transaction on `evse`.
    pub(super) fn end_transaction(
        &mut self,
        _evse: u32,
        _reason: StopReason,
        _trigger: TriggerReason,
    ) -> Result<(), StationError> {
        Err(StationError::NotImplemented {
            command: "end_transaction",
        })
    }

    /// Generates a `TransactionEvent` for the transaction on `evse`.
    pub(super) fn tx_event(
        &mut self,
        _evse: u32,
        _kind: TransactionEventKind,
        _trigger: TriggerReason,
        _extra: EventExtra,
    ) {
    }

    /// The transaction in progress on `evse`.
    pub(super) fn active_tx(&self, _evse: u32) -> Option<&ActiveTx> {
        None
    }

    /// A transaction by id: ongoing, stopped, or unknown (`None`).
    pub(super) fn tx_by_id(&self, _id: &str) -> Option<TxLookup> {
        None
    }

    /// The simulated power of `evse` in watts.
    pub(super) fn evse_power_w(&self, _evse: u32) -> f64 {
        0.0
    }

    /// `Command2::StopTransaction`.
    pub(super) fn cmd_stop_transaction(&mut self, _evse: u32, _reason: StopReason, reply: Reply2) {
        not_implemented(reply, "stop_transaction");
    }

    /// `Command2::AdvanceMeter`.
    pub(super) fn cmd_advance_meter(&mut self, _evse: u32, _wh_delta: u64, reply: Reply2) {
        not_implemented(reply, "advance_meter");
    }

    /// `Command2::SetChargingState`.
    pub(super) fn cmd_set_charging_state(
        &mut self,
        _evse: u32,
        _state: ChargingState,
        reply: Reply2,
    ) {
        not_implemented(reply, "set_charging_state");
    }

    /// An EV was plugged in on `evse`.
    pub(super) fn on_ev_connected(&mut self, _evse: u32) {}

    /// The EV on `evse` was unplugged.
    pub(super) fn on_ev_disconnected(&mut self, _evse: u32) {}

    /// A fault was injected on `evse`.
    pub(super) fn on_fault(&mut self, _evse: u32) {}

    /// The periodic sample of `evse` is due.
    pub(super) fn on_meter_periodic(&mut self, _evse: u32) {}

    /// The clock-aligned sample is due.
    pub(super) fn on_meter_aligned(&mut self) {}

    /// The EV did not connect in time on `evse`.
    pub(super) fn on_ev_connect_timeout(&mut self, _evse: u32) {}

    /// Adds the energy of the time that passed to every running meter.
    pub(super) fn settle_tx_meters(&mut self) {}

    /// Authorization decided `token` is valid on `evse`. Starts the transaction when this
    /// reaches a start point, adds the token to the running one (an event with `idToken`), or
    /// keeps the authorization until the EV connects and arms `EvConnectTimeout`. The
    /// authorization module never evaluates start or stop points itself.
    pub(super) fn on_authorized(
        &mut self,
        _evse: u32,
        _token: IdTokenRef,
        _remote_start_id: Option<i32>,
        _trigger: TriggerReason,
    ) -> Result<(), StationError> {
        Err(StationError::NotImplemented {
            command: "on_authorized",
        })
    }

    /// Authorization on `evse` ended (the token was presented again, or the CSMS reported it
    /// invalid). Ends the transaction when a stop point ends, otherwise sends an event with the
    /// token once more; drops a pending authorization that has no transaction yet.
    pub(super) fn on_deauthorized(
        &mut self,
        _evse: u32,
        _trigger: TriggerReason,
        _reason: StopReason,
    ) {
    }

    /// `RequestStopTransaction` accepted for the transaction on `evse`: `Ended` when a stop
    /// point ends, otherwise `Updated` with `RemoteStop` and energy delivery stops.
    pub(super) fn remote_stop(&mut self, _evse: u32) {}

    /// Changes the charging state of the transaction on `evse` from inside the station and
    /// sends the event.
    pub(super) fn set_charging_state_internal(
        &mut self,
        _evse: u32,
        _state: ChargingState,
        _trigger: TriggerReason,
    ) {
    }

    /// The token that authorizes `evse` now: of the running transaction, or of an
    /// authorization waiting for the EV.
    pub(super) fn authorized_token(&self, _evse: u32) -> Option<&IdTokenRef> {
        None
    }

    /// Sends `MeterValues` outside a transaction for `evse` (every EVSE for `None`); never
    /// queued offline. Used by `Command2::AdvanceMeter` without a transaction and by
    /// `TriggerMessage(MeterValues)`.
    pub(super) fn send_meter_values(&mut self, _evse: Option<u32>, _origin: Origin2) {}

    /// Active transactions and the ids of the last stopped ones, for the state file.
    pub(super) fn tx_persist(&self) -> TxPersist {
        TxPersist::default()
    }

    /// Takes back the persisted transactions at start.
    pub(super) fn tx_restore(&mut self, _tx: TxPersist) {}
}
