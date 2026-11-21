//! Small types shared by the OCPP 2.0.1 station modules.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;

use super::actor::Station2Actor;
use super::command::Outcome2;
use crate::error::{CallError, StationError};
use crate::ocpp::v2::types::IdTokenType;

/// Transaction id of OCPP 2.0.1: 1 to 36 characters, a hyphenated v4 UUID when the station
/// generates it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TxId2(pub String);

/// An identification token as the station tracks it: the value and its type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IdTokenRef {
    pub id_token: String,
    pub kind: IdTokenType,
}

/// A condition of `TxStartPoint` / `TxStopPoint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StartStopPoint {
    Authorized,
    EvConnected,
    PowerPathClosed,
    EnergyTransfer,
}

/// Why a message is being sent; decides whether the registration gate lets it through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin2 {
    /// Spontaneous or user-initiated.
    Station,
    /// Asked for by the CSMS (`TriggerMessage`, `GetBaseReport`, `GetReport`).
    Triggered,
    /// `SendRaw`: no gating, no retries.
    Raw,
}

/// Runs once, inside the actor, when a CALL has been answered or failed.
pub type Continuation2 = Box<dyn FnOnce(&mut Station2Actor, Result<Value, CallError>) + Send>;

/// Answer channel of a command.
pub type Reply2 = oneshot::Sender<Result<Outcome2, StationError>>;

/// The `DeviceDataCtrlr` item limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemsPer {
    GetReport,
    GetVariables,
    SetVariables,
}

/// A device-model variable whose value is computed from the live station state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LiveVar {
    StationAvailability,
    EvseAvailability(u32),
    ConnectorAvailability(u32, u32),
    DateTime,
    EvsePower(u32),
    CertificateEntries,
    LocalListEntries,
    SmartChargingEntries,
    TariffProblem,
}
