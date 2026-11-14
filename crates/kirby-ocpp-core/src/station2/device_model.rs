//! The device model: variables, their values and the typed accessors the other modules read.

#![allow(
    dead_code,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    reason = "placeholder module; remove this allow when the module is implemented"
)]

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::actor::Station2Actor;
use super::commands::not_implemented;
use super::types::{ItemsPer, Reply2, StartStopPoint};
use crate::ocpp::v2::types::{Component, Measurand, Variable};

/// The values that differ from their defaults and are not secret, keyed by
/// `component[.instance].variable` text.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceModelPersist {
    pub values: BTreeMap<String, String>,
}

/// The variable table and the current values.
#[derive(Debug, Default)]
pub(crate) struct DeviceModel {}

impl Station2Actor {
    /// `Command2::SetVariable`.
    pub(super) fn cmd_set_variable(
        &mut self,
        _component: Component,
        _variable: Variable,
        _value: String,
        reply: Reply2,
    ) {
        not_implemented(reply, "set_variable");
    }

    /// The next `NotifyReport` chunk is due.
    pub(super) fn on_report_chunk(&mut self) {}

    /// Rebuilds `Snapshot2::variables` from the device model.
    pub(super) fn refresh_variable_views(&mut self) {}

    /// `OCPPCommCtrlr.HeartbeatInterval`.
    pub(super) fn heartbeat_interval_secs(&self) -> u32 {
        0
    }

    /// `TxCtrlr.TxStartPoint`.
    pub(super) fn tx_start_points(&self) -> Vec<StartStopPoint> {
        Vec::new()
    }

    /// `TxCtrlr.TxStopPoint`.
    pub(super) fn tx_stop_points(&self) -> Vec<StartStopPoint> {
        Vec::new()
    }

    /// `OCPPCommCtrlr.MessageAttempts` of `TransactionEvent`.
    pub(super) fn tx_attempts(&self) -> u32 {
        0
    }

    /// `OCPPCommCtrlr.MessageAttemptInterval` of `TransactionEvent`.
    pub(super) fn tx_attempt_interval(&self) -> Duration {
        Duration::ZERO
    }

    /// `SampledDataCtrlr.TxUpdatedInterval`, seconds.
    pub(super) fn tx_updated_interval(&self) -> u32 {
        0
    }

    /// `SampledDataCtrlr.TxStartedMeasurands`.
    pub(super) fn tx_started_measurands(&self) -> Vec<Measurand> {
        Vec::new()
    }

    /// `SampledDataCtrlr.TxUpdatedMeasurands`.
    pub(super) fn tx_updated_measurands(&self) -> Vec<Measurand> {
        Vec::new()
    }

    /// `SampledDataCtrlr.TxEndedMeasurands`.
    pub(super) fn tx_ended_measurands(&self) -> Vec<Measurand> {
        Vec::new()
    }

    /// `AlignedDataCtrlr.Interval`, seconds; 0 is off.
    pub(super) fn aligned_interval(&self) -> u32 {
        0
    }

    /// `OCPPCommCtrlr.OfflineThreshold`.
    pub(super) fn offline_threshold(&self) -> Duration {
        Duration::ZERO
    }

    /// `AuthCtrlr.AuthorizeRemoteStart`.
    pub(super) fn authorize_remote_start(&self) -> bool {
        false
    }

    /// `AuthCtrlr.LocalAuthorizeOffline`.
    pub(super) fn local_authorize_offline(&self) -> bool {
        false
    }

    /// `AuthCtrlr.LocalPreAuthorize`.
    pub(super) fn local_pre_authorize(&self) -> bool {
        false
    }

    /// `AuthCtrlr.OfflineTxForUnknownIdEnabled`.
    pub(super) fn offline_tx_for_unknown_id(&self) -> bool {
        false
    }

    /// `TxCtrlr.EVConnectionTimeOut`.
    pub(super) fn ev_connection_timeout(&self) -> Duration {
        Duration::ZERO
    }

    /// `TxCtrlr.StopTxOnEVSideDisconnect`.
    pub(super) fn stop_tx_on_ev_side_disconnect(&self) -> bool {
        false
    }

    /// `TxCtrlr.StopTxOnInvalidId`.
    pub(super) fn stop_tx_on_invalid_id(&self) -> bool {
        false
    }

    /// `SecurityCtrlr.BasicAuthPassword`.
    pub(super) fn basic_auth_password(&self) -> Option<&str> {
        None
    }

    /// `DeviceDataCtrlr.ItemsPerMessage` of one message type.
    pub(super) fn items_per_message(&self, _which: ItemsPer) -> usize {
        0
    }

    /// `OCPPCommCtrlr.WebSocketPingInterval`; `None` is off.
    pub(super) fn ws_ping_interval(&self) -> Option<Duration> {
        None
    }
}
