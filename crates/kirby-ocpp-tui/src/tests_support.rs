//! Shared fixtures for the unit tests.

use chrono::{TimeZone, Utc};
use kirby_ocpp_core::StationConfig;
use kirby_ocpp_core::ids::ConnectorId;
use kirby_ocpp_core::ocpp::types::{
    AuthorizationStatus, ChargingProfileKindType, ChargingProfilePurposeType, ChargingRateUnitType,
};
use kirby_ocpp_core::station::config_registry::ConfigRegistry;
use kirby_ocpp_core::station::{
    ConfigItem, LocalListEntry, LocalListView, ProfileSummary, Snapshot,
};

pub const PROFILE_JSON: &str = r#"{"chargingProfileId":7,"stackLevel":1,"chargingProfilePurpose":"ChargePointMaxProfile","chargingProfileKind":"Absolute","chargingSchedule":{"chargingRateUnit":"A","chargingSchedulePeriod":[{"startPeriod":0,"limit":16.0}]}}"#;

/// A snapshot with the stock configuration, two local list entries and one profile.
pub fn snapshot_with_data() -> Snapshot {
    let config = StationConfig::new("ws://h/p").with_connectors(2);
    let mut snap = Snapshot::initial(&config);
    snap.config = ConfigRegistry::new(&config)
        .entries()
        .iter()
        .map(ConfigItem::from)
        .collect();
    snap.local_list = LocalListView {
        version: 3,
        total: 2,
        entries: vec![
            LocalListEntry {
                id_tag: "TAG-A".into(),
                status: AuthorizationStatus::Accepted,
                expiry_date: None,
                parent_id_tag: None,
            },
            LocalListEntry {
                id_tag: "TAG-B".into(),
                status: AuthorizationStatus::Blocked,
                expiry_date: Some(Utc.with_ymd_and_hms(2030, 1, 2, 3, 4, 5).unwrap()),
                parent_id_tag: Some("PARENT".into()),
            },
        ],
    };
    snap.local_list_version = 3;
    snap.charging_profiles = vec![ProfileSummary {
        id: 7,
        connector: ConnectorId::STATION,
        purpose: ChargingProfilePurposeType::ChargePointMaxProfile,
        kind: ChargingProfileKindType::Absolute,
        stack_level: 1,
        valid_from: None,
        valid_to: None,
        unit: ChargingRateUnitType::A,
        periods: 1,
        schedule: "0s:16A".into(),
        json: PROFILE_JSON.into(),
    }];
    snap
}
