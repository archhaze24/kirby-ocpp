//! Read-model views of the configuration keys, the local authorization list and the installed
//! charging profiles, for UIs (`Snapshot::config`, `Snapshot::local_list`,
//! `Snapshot::charging_profiles`).
//!
//! The snapshot is cloned often, so the views are kept small: the local list is capped at
//! [`LOCAL_LIST_VIEW_MAX`] entries (`LocalListView::total` says how many exist), and a profile
//! is a summary plus its compact JSON instead of the full tree.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::charging_profiles::StoredProfile;
use super::config_registry::ConfigEntry;
use super::local_auth_list::LocalAuthList;
use crate::ids::ConnectorId;
use crate::ocpp::types::{
    AuthorizationStatus, ChargingProfileKindType, ChargingProfilePurposeType, ChargingRateUnitType,
};

/// Entries of the local list copied into the snapshot (the rest is only counted).
pub const LOCAL_LIST_VIEW_MAX: usize = 500;

/// One configuration key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigItem {
    pub key: String,
    pub value: String,
    pub readonly: bool,
    /// Short description of the accepted values.
    pub hint: String,
}

impl From<&ConfigEntry> for ConfigItem {
    fn from(entry: &ConfigEntry) -> Self {
        Self {
            key: entry.key.to_owned(),
            value: entry.value.clone(),
            readonly: entry.readonly,
            hint: entry.hint().to_owned(),
        }
    }
}

/// One entry of the local authorization list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalListEntry {
    pub id_tag: String,
    pub status: AuthorizationStatus,
    pub expiry_date: Option<DateTime<Utc>>,
    pub parent_id_tag: Option<String>,
}

/// The local authorization list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalListView {
    /// Version (`0` = no list); same as `Snapshot::local_list_version`.
    pub version: i32,
    /// Number of entries in the list.
    pub total: usize,
    /// The first [`LOCAL_LIST_VIEW_MAX`] entries, ordered by idTag.
    pub entries: Vec<LocalListEntry>,
}

impl LocalListView {
    pub(super) fn from_list(list: &LocalAuthList) -> Self {
        Self {
            version: list.version(),
            total: list.len(),
            entries: list
                .iter()
                .take(LOCAL_LIST_VIEW_MAX)
                .map(|(tag, info)| LocalListEntry {
                    id_tag: tag.to_string(),
                    status: info.status,
                    expiry_date: info.expiry_date,
                    parent_id_tag: info.parent_id_tag.as_ref().map(ToString::to_string),
                })
                .collect(),
        }
    }
}

/// An installed charging profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSummary {
    pub id: i32,
    /// `0` = the whole charge point.
    pub connector: ConnectorId,
    pub purpose: ChargingProfilePurposeType,
    pub kind: ChargingProfileKindType,
    pub stack_level: i32,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub unit: ChargingRateUnitType,
    /// Number of schedule periods.
    pub periods: usize,
    /// Short form of the schedule, e.g. `0s:16A 3600s:8A`.
    pub schedule: String,
    /// The profile as compact JSON (`csChargingProfiles`), for details and editing.
    pub json: String,
}

impl From<&StoredProfile> for ProfileSummary {
    fn from(stored: &StoredProfile) -> Self {
        let profile = &stored.profile;
        let unit = profile.charging_schedule.charging_rate_unit;
        let suffix = match unit {
            ChargingRateUnitType::A => "A",
            ChargingRateUnitType::W => "W",
        };
        let schedule = profile
            .charging_schedule
            .charging_schedule_period
            .iter()
            .map(|p| format!("{}s:{}{suffix}", p.start_period, p.limit))
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            id: profile.charging_profile_id,
            connector: stored.connector_id,
            purpose: profile.charging_profile_purpose,
            kind: profile.charging_profile_kind,
            stack_level: profile.stack_level,
            valid_from: profile.valid_from,
            valid_to: profile.valid_to,
            unit,
            periods: profile.charging_schedule.charging_schedule_period.len(),
            schedule,
            json: serde_json::to_string(profile).unwrap_or_default(),
        }
    }
}

/// Brings the views in `state` up to date with their sources. The configuration and the
/// profiles are small and rebuilt on every call (assigned only when different); the local list
/// is rebuilt only when `list_dirty` is set (it is cleared here).
pub(super) fn refresh(
    state: &mut super::Snapshot,
    registry: &super::config_registry::ConfigRegistry,
    local_list: &LocalAuthList,
    profiles: &super::charging_profiles::ChargingProfileStore,
    list_dirty: &mut bool,
) {
    let config: Vec<ConfigItem> = registry.entries().iter().map(ConfigItem::from).collect();
    if state.config != config {
        state.config = config;
    }
    let installed: Vec<ProfileSummary> = profiles.profiles().iter().map(Into::into).collect();
    if state.charging_profiles != installed {
        state.charging_profiles = installed;
    }
    if std::mem::take(list_dirty) {
        state.local_list = LocalListView::from_list(local_list);
    }
    state.local_list_version = local_list.version();
}
