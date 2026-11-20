//! The read model of an OCPP 2.0.1 station: an immutable copy of its state for UIs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::StationConfig;
use crate::ocpp::Protocol;
use crate::ocpp::v2::messages::{BootReason, ConnectorStatus, Mutability, RegistrationStatus};
use crate::ocpp::v2::types::ChargingState;
use crate::ws::ConnectionState;

/// The OCPP 2.x version a station built from `config` speaks: 2.1 when `config` asks for it,
/// else 2.0.1. Schemas, the action catalog and the reported protocol follow it.
pub(super) fn wire_protocol(config: &StationConfig) -> Protocol {
    if config.protocol == Protocol::V21 {
        Protocol::V21
    } else {
        Protocol::V201
    }
}

/// Snapshot of the whole station.
///
/// `variables` lists station-level device-model variables only (at most 256); per-EVSE and
/// per-connector variables are reachable through `GetVariables`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot2 {
    pub charge_point_id: String,
    /// The WebSocket subprotocol token of the station: `ocpp2.0.1` or `ocpp2.1`.
    pub protocol: String,
    pub connection: ConnectionState,
    /// Result of the last `BootNotification`; `None` before any boot.
    pub registration: Option<RegistrationStatus>,
    /// Reason of the last `BootNotification` sent.
    pub boot_reason: Option<BootReason>,
    /// Effective heartbeat interval: from `BootNotification.conf`, else the configured fallback.
    pub heartbeat_interval_secs: u32,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    /// Availability of the whole station.
    pub availability_operative: bool,
    /// EVSEs `1..=n`, ordered by id.
    pub evses: Vec<EvseSnapshot>,
    /// Transaction events waiting to be delivered.
    pub queued_events: u32,
    pub variables: Vec<VariableView>,
    /// Why the automatic reconnect loop ended; `None` while the link is up, retrying or
    /// closed on purpose. Cleared by the next `Connect`.
    #[serde(default)]
    pub reconnect_stopped: Option<String>,
    /// `SecurityProfile` (0..=3).
    pub security_profile: u8,
}

/// State of one EVSE.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvseSnapshot {
    pub id: u32,
    pub operative: bool,
    pub connectors: Vec<ConnectorSnapshot2>,
    pub transaction: Option<TransactionSnapshot>,
    /// Simulated meter, Wh.
    pub meter_wh: u64,
}

/// State of one connector of an EVSE.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorSnapshot2 {
    pub id: u32,
    pub status: ConnectorStatus,
    pub ev_connected: bool,
    pub fault: bool,
}

/// The transaction running on an EVSE.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionSnapshot {
    pub transaction_id: String,
    /// `seqNo` of the next `TransactionEvent`.
    pub next_seq_no: i32,
    pub charging_state: ChargingState,
    pub id_token: Option<String>,
    pub started_at: DateTime<Utc>,
    /// Energy delivered in this transaction, Wh.
    pub energy_wh: u64,
    pub remote_start_id: Option<i32>,
}

/// One device-model variable as the snapshot shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariableView {
    pub component: String,
    pub instance: Option<String>,
    pub variable: String,
    /// `None` for a write-only variable.
    pub value: Option<String>,
    pub mutability: Mutability,
}

impl Snapshot2 {
    /// The state of a station that has just been created from `config`.
    pub fn initial(config: &StationConfig) -> Self {
        Self {
            charge_point_id: config.charge_point_id.clone(),
            protocol: wire_protocol(config).token().to_owned(),
            connection: ConnectionState::Disconnected,
            registration: None,
            boot_reason: None,
            heartbeat_interval_secs: config.heartbeat_interval_secs,
            last_heartbeat_at: None,
            availability_operative: true,
            evses: Vec::new(),
            queued_events: 0,
            variables: Vec::new(),
            reconnect_stopped: None,
            security_profile: 0,
        }
    }

    /// `true` once a `BootNotification` was `Accepted`.
    pub fn booted(&self) -> bool {
        self.registration == Some(RegistrationStatus::Accepted)
    }

    /// An EVSE by id.
    pub fn evse(&self, id: u32) -> Option<&EvseSnapshot> {
        self.evses.iter().find(|evse| evse.id == id)
    }
}
