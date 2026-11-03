//! The read model: an immutable copy of the station state for UIs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::certificates::CertificateSummary;
use super::maintenance::{DiagnosticsOutcome, FirmwareOutcome};
use super::reservation::Reservation;
use super::response_rules::ResponseRule;
use super::transaction::TransactionState;
use super::views::{ConfigItem, LocalListView, ProfileSummary};
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
/// Connector `0` is the charge point itself and is not in `connectors`; its state is
/// `charge_point` (only status, error fields and availability are meaningful) and
/// `availability` mirrors `charge_point.availability`.
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
    /// Connector 0 as it is reported in `StatusNotification` (`Available`, `Unavailable`, `Faulted`).
    pub charge_point: ConnectorSnapshot,
    pub local_list_version: i32,
    pub diagnostics_status: DiagnosticsStatus,
    pub firmware_status: FirmwareStatus,
    pub diagnostics_outcome: DiagnosticsOutcome,
    pub firmware_outcome: FirmwareOutcome,
    /// Transaction messages waiting for a connection.
    pub pending_offline_messages: u32,
    /// Active response rules (runtime only; see `response_rules`), in the order they were set.
    pub response_rules: Vec<ResponseRule>,
    /// Every configuration key with its value, in registry order.
    pub config: Vec<ConfigItem>,
    /// The local authorization list (capped, see `views::LOCAL_LIST_VIEW_MAX`).
    pub local_list: LocalListView,
    /// Installed charging profiles, in installation order.
    pub charging_profiles: Vec<ProfileSummary>,
    /// Security events waiting for a connection.
    pub pending_security_events: u32,
    /// `SecurityProfile` (0..=3); takes effect at the next (re)connect.
    pub security_profile: u8,
    /// Installed certificates (roots and the station's own), no key material.
    pub certificates: Vec<CertificateSummary>,
    /// A certificate request (`SignCertificate`) is waiting for its `CertificateSigned`.
    pub certificate_request_pending: bool,
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
            charge_point: ConnectorSnapshot::new(ConnectorId::STATION),
            local_list_version: 0,
            diagnostics_status: DiagnosticsStatus::Idle,
            firmware_status: FirmwareStatus::Idle,
            diagnostics_outcome: DiagnosticsOutcome::default(),
            firmware_outcome: FirmwareOutcome::default(),
            pending_offline_messages: 0,
            response_rules: config.response_rules.clone(),
            config: Vec::new(),
            local_list: LocalListView::default(),
            charging_profiles: Vec::new(),
            pending_security_events: 0,
            security_profile: 0,
            certificates: Vec::new(),
            certificate_request_pending: false,
            connectors: (1..=count)
                .map(|id| ConnectorSnapshot::new(ConnectorId::new(id)))
                .collect(),
        }
    }

    /// `true` once a `BootNotification` was `Accepted`.
    pub fn booted(&self) -> bool {
        self.registration == Some(RegistrationStatus::Accepted)
    }

    /// A connector by id; `ConnectorId::STATION` gives the charge point itself.
    pub fn connector(&self, id: ConnectorId) -> Option<&ConnectorSnapshot> {
        if id.is_station() {
            return Some(&self.charge_point);
        }
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
        assert_eq!(
            snapshot.connector(ConnectorId::STATION),
            Some(&snapshot.charge_point)
        );
        assert_eq!(snapshot.charge_point.id, ConnectorId::STATION);
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
