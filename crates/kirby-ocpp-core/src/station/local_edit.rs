//! Local edits of the configuration, the local authorization list and the charging profiles
//! (`Command::SetConfiguration` and friends). They behave like the CSMS-triggered equivalents
//! (`ChangeConfiguration`, `SendLocalList`, `SetChargingProfile`, `ClearChargingProfile`) with
//! the same validation and side effects, without a CSMS round trip and without a CALL.
//!
//! Local list versions: an edit is applied as a *differential* update with version
//! `current + 1`, so every edit bumps the version by one (a CSMS that tracks versions sees a
//! mismatch and can be tested against it). `ClearLocalList` is a *full* update with an empty
//! list and version `0` ("no list").

use super::actor::StationActor;
use super::command::Outcome;
use super::event::LogLevel;
use super::local_auth_list::LocalListLimits;
use crate::error::StationError;
use crate::ids::{ConnectorId, IdTag};
use crate::ocpp::messages::{
    AuthorizationData, ChargingProfile, ChargingProfileStatus, ClearChargingProfileRequest,
    ClearChargingProfileStatus, IdTagInfo, SendLocalListRequest, UpdateStatus,
};
use crate::ocpp::types::{ConfigurationStatus, UpdateType};

fn invalid(text: String) -> StationError {
    StationError::InvalidArgument(text)
}

impl StationActor {
    pub(super) fn set_configuration(
        &mut self,
        key: &str,
        value: &str,
    ) -> Result<Outcome, StationError> {
        match self.apply_configuration_change(key, value) {
            ConfigurationStatus::Accepted => Ok(Outcome::Done),
            ConfigurationStatus::NotSupported => Err(invalid(format!("unknown key {key}"))),
            _ => Err(invalid(format!(
                "{key}: value rejected (read-only or invalid)"
            ))),
        }
    }

    pub(super) fn set_local_list_entry(
        &mut self,
        id_tag: IdTag,
        info: IdTagInfo,
    ) -> Result<Outcome, StationError> {
        let name = id_tag.to_string();
        let item = AuthorizationData {
            id_tag,
            id_tag_info: Some(info),
        };
        self.apply_local_list_edit(item)?;
        self.log(LogLevel::Info, format!("local list entry {name} set"));
        Ok(Outcome::Done)
    }

    pub(super) fn remove_local_list_entry(
        &mut self,
        id_tag: IdTag,
    ) -> Result<Outcome, StationError> {
        if self.local_list.get(&id_tag).is_none() {
            return Err(invalid(format!("{id_tag} is not in the local list")));
        }
        let name = id_tag.to_string();
        let item = AuthorizationData {
            id_tag,
            id_tag_info: None,
        };
        self.apply_local_list_edit(item)?;
        self.log(LogLevel::Info, format!("local list entry {name} removed"));
        Ok(Outcome::Done)
    }

    fn apply_local_list_edit(&mut self, item: AuthorizationData) -> Result<(), StationError> {
        let request = SendLocalListRequest {
            list_version: self.local_list.version().saturating_add(1),
            local_authorization_list: Some(vec![item]),
            update_type: UpdateType::Differential,
        };
        self.apply_local_list_request(&request)
    }

    pub(super) fn clear_local_list(&mut self) -> Result<Outcome, StationError> {
        let request = SendLocalListRequest {
            list_version: 0,
            local_authorization_list: Some(Vec::new()),
            update_type: UpdateType::Full,
        };
        self.apply_local_list_request(&request)?;
        self.log(LogLevel::Info, "local list cleared".to_owned());
        Ok(Outcome::Done)
    }

    fn apply_local_list_request(
        &mut self,
        request: &SendLocalListRequest,
    ) -> Result<(), StationError> {
        let limits = LocalListLimits::from_config(&self.registry);
        match self.local_list.apply(request, &limits) {
            UpdateStatus::Accepted => {
                self.local_list_dirty = true;
                self.flush_now();
                Ok(())
            }
            UpdateStatus::NotSupported => Err(invalid(
                "the local authorization list is disabled (LocalAuthListEnabled)".to_owned(),
            )),
            UpdateStatus::Failed => Err(invalid(format!(
                "the local list is full (LocalAuthListMaxLength {})",
                limits.max_entries
            ))),
            UpdateStatus::VersionMismatch => Err(invalid("local list version mismatch".to_owned())),
        }
    }

    pub(super) fn install_charging_profile(
        &mut self,
        connector: ConnectorId,
        profile: ChargingProfile,
    ) -> Result<Outcome, StationError> {
        let id = profile.charging_profile_id;
        match self.set_charging_profile(connector, profile) {
            ChargingProfileStatus::Accepted => {
                self.flush_now();
                self.log(
                    LogLevel::Info,
                    format!("charging profile {id} installed on connector {connector}"),
                );
                Ok(Outcome::Done)
            }
            status => Err(invalid(format!(
                "charging profile {id} {status} (see the SetChargingProfile rules)"
            ))),
        }
    }

    pub(super) fn clear_charging_profiles(
        &mut self,
        id: Option<i32>,
    ) -> Result<Outcome, StationError> {
        let filter = ClearChargingProfileRequest {
            id,
            ..ClearChargingProfileRequest::default()
        };
        let now = self.clock.now_utc();
        let status = self.profiles.clear(&filter, now);
        self.flush_now();
        if let (ClearChargingProfileStatus::Unknown, Some(id)) = (status, id) {
            return Err(invalid(format!("no charging profile with id {id}")));
        }
        self.log(
            LogLevel::Info,
            id.map_or_else(
                || "all charging profiles cleared".to_owned(),
                |id| format!("charging profile {id} cleared"),
            ),
        );
        Ok(Outcome::Done)
    }
}
