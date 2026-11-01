//! The read model: an immutable copy of the station state for UIs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::maintenance::{DiagnosticsOutcome, FirmwareOutcome};
use super::reservation::Reservation;
use super::transaction::TransactionState;
use crate::config::StationConfig;
use crate::ids::{ConnectorId, IdTag, TxId};
use crate::ocpp::types::{
    AvailabilityType, ChargePointErrorCode, ConnectorStatus, DiagnosticsStatus, FirmwareStatus,
    RegistrationStatus,
};
use crate::ws::ConnectionState;

/// State of one connector (modelled on the TS `ConnectorState`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorSnapshot {
    pub id: ConnectorId,
    pub status: ConnectorStatus,
    pub error_code: ChargePointErrorCode,
    pub info: Option<String>,
    pub vendor_id: Option<String>,
    pub vendor_error_code: Option<String>,
    pub ev_connected: bool,
    pub availability: AvailabilityType,
    /// Availability requested by the CSMS while a transaction was running; applied when it ends.
    pub pending_availability: Option<AvailabilityType>,
    pub transaction: TransactionState,
    pub last_id_tag: Option<IdTag>,
    /// Simulated meter, Wh.
    pub meter_wh: u64,
    pub reservation: Option<Reservation>,
}

impl ConnectorSnapshot {
    /// A fresh, idle, operative connector.
    pub fn new(id: ConnectorId) -> Self {
        Self {
            id,
            status: ConnectorStatus::Available,
            error_code: ChargePointErrorCode::NoError,
            info: None,
            vendor_id: None,
            vendor_error_code: None,
            ev_connected: false,
            availability: AvailabilityType::Operative,
            pending_availability: None,
            transaction: TransactionState::Idle,
            last_id_tag: None,
            meter_wh: 0,
            reservation: None,
        }
    }

    /// Id of the running (or starting/stopping) transaction, if any.
    pub fn tx_id(&self) -> Option<TxId> {
        self.transaction.tx_id()
    }
}

/// Snapshot of the whole station (modelled on the TS `StationState`).
///
/// Connector `0` is the charge point itself and is not in `connectors`; its
/// availability is `availability`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub charge_point_id: String,
    pub connection: ConnectionState,
    /// Result of the last `BootNotification`; `None` before any boot (TS: "Unknown").
    pub registration: Option<RegistrationStatus>,
    /// Effective heartbeat interval: from `BootNotification.conf`, else the configured fallback.
    pub heartbeat_interval_secs: u32,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    /// Availability of connector 0 (the whole charge point).
    pub availability: AvailabilityType,
    pub local_list_version: i32,
    pub diagnostics_status: DiagnosticsStatus,
    pub firmware_status: FirmwareStatus,
    pub diagnostics_outcome: DiagnosticsOutcome,
    pub firmware_outcome: FirmwareOutcome,
    /// Transaction messages waiting for a connection.
    pub pending_offline_messages: u32,
    /// Connectors `1..=n`, ordered by id.
    pub connectors: Vec<ConnectorSnapshot>,
}

impl Snapshot {
    /// The state of a station that has just been created from `config`.
    pub fn initial(config: &StationConfig) -> Self {
        let count = config
            .connector_count
            .max(config.initial_connector.get())
            .max(1);
        Self {
            charge_point_id: config.charge_point_id.clone(),
            connection: ConnectionState::Disconnected,
            registration: None,
            heartbeat_interval_secs: config.heartbeat_interval_secs,
            last_heartbeat_at: None,
            availability: AvailabilityType::Operative,
            local_list_version: 0,
            diagnostics_status: DiagnosticsStatus::Idle,
            firmware_status: FirmwareStatus::Idle,
            diagnostics_outcome: DiagnosticsOutcome::default(),
            firmware_outcome: FirmwareOutcome::default(),
            pending_offline_messages: 0,
            connectors: (1..=count)
                .map(|id| ConnectorSnapshot::new(ConnectorId::new(id)))
                .collect(),
        }
    }

    /// `true` once a `BootNotification` was `Accepted`.
    pub fn booted(&self) -> bool {
        self.registration == Some(RegistrationStatus::Accepted)
    }

    pub fn connector(&self, id: ConnectorId) -> Option<&ConnectorSnapshot> {
        self.connectors.iter().find(|c| c.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_snapshot_follows_the_config() {
        let mut config = StationConfig::new("ws://h/p").with_connectors(2);
        config.charge_point_id = "CP-9".into();
        let snapshot = Snapshot::initial(&config);
        assert_eq!(snapshot.charge_point_id, "CP-9");
        assert_eq!(snapshot.connection, ConnectionState::Disconnected);
        assert_eq!(snapshot.heartbeat_interval_secs, 30);
        assert!(!snapshot.booted());
        let ids: Vec<u32> = snapshot.connectors.iter().map(|c| c.id.get()).collect();
        assert_eq!(ids, [1, 2]);
        assert!(snapshot.connector(ConnectorId::new(2)).is_some());
        assert!(snapshot.connector(ConnectorId::STATION).is_none());
        assert!(
            snapshot
                .connectors
                .iter()
                .all(|c| c.transaction.is_idle() && c.tx_id().is_none())
        );
    }

    #[test]
    fn initial_connector_count_covers_the_initial_connector() {
        let mut config = StationConfig::new("ws://h/p");
        config.initial_connector = ConnectorId::new(3);
        config.connector_count = 1;
        assert_eq!(Snapshot::initial(&config).connectors.len(), 3);
    }

    #[test]
    fn snapshot_serde_roundtrip() {
        let snapshot = Snapshot::initial(&StationConfig::new("ws://h/p"));
        let json = serde_json::to_string(&snapshot).unwrap();
        assert_eq!(serde_json::from_str::<Snapshot>(&json).unwrap(), snapshot);
    }
}
