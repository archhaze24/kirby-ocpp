//! `ChangeConfiguration` and `GetConfiguration` (TS: `configuration-commands.ts`).
//!
//! Key names are matched case-insensitively (spec) by the registry and reported in their
//! canonical spelling. `GetConfiguration` naming more keys than `GetConfigurationMaxKeys`
//! is answered with CALLERROR `OccurenceConstraintViolation` (the spec has no status for it).

use crate::ocpp::frame::ErrorCode;
use crate::ocpp::messages::{
    ChangeConfiguration, ChangeConfigurationRequest, ChangeConfigurationResponse, ConfigurationKey,
    GetConfiguration, GetConfigurationRequest, GetConfigurationResponse,
};
use crate::ocpp::responder::Responder;
use crate::ocpp::types::ConfigurationStatus;
use crate::ocpp::types::RegistrationStatus;
use crate::station::actor::StationActor;
use crate::station::config_registry::{
    CLOCK_ALIGNED_DATA_INTERVAL, HEARTBEAT_INTERVAL, METER_VALUE_SAMPLE_INTERVAL,
    MINIMUM_STATUS_DURATION,
};
use crate::station::event::LogLevel;

pub(super) fn change_configuration(
    actor: &mut StationActor,
    responder: Responder<ChangeConfiguration>,
    request: ChangeConfigurationRequest,
) {
    let status = actor.apply_configuration_change(&request.key, &request.value);
    actor.respond(responder, ChangeConfigurationResponse { status });
}

impl StationActor {
    /// `ChangeConfiguration` without the CALL: validates and stores the value, logs, persists
    /// and runs the side effects. Shared by the CSMS command and the local `SetConfiguration`.
    pub(in crate::station) fn apply_configuration_change(
        &mut self,
        key: &str,
        value: &str,
    ) -> ConfigurationStatus {
        if let Err(reason) = self.check_security_change(key, value) {
            self.log(
                LogLevel::Warn,
                format!("ChangeConfiguration {key} -> Rejected: {reason}"),
            );
            return ConfigurationStatus::Rejected;
        }
        let previous_profile = self.registry.security_profile();
        let status = self.registry.change(key, value);
        if status != ConfigurationStatus::Accepted {
            self.log(
                LogLevel::Warn,
                format!("ChangeConfiguration {key} -> {status}"),
            );
            return status;
        }
        let (canonical, secret) = self
            .registry
            .get(key)
            .map_or((key, false), |entry| (entry.key, entry.secret()));
        let canonical = canonical.to_owned();
        let shown = if secret { "********" } else { value };
        self.log(
            LogLevel::Info,
            format!("configuration {canonical} set to {shown}"),
        );
        self.on_security_key_changed(&canonical, previous_profile);
        self.flush_now();
        self.on_configuration_changed(&canonical);
        status
    }

    /// Side effects of an accepted configuration change.
    fn on_configuration_changed(&mut self, key: &str) {
        match key {
            HEARTBEAT_INTERVAL => {
                self.state.heartbeat_interval_secs = self.registry.heartbeat_interval_secs();
                if self.state.registration == Some(RegistrationStatus::Accepted) {
                    self.arm_heartbeat();
                }
            }
            MINIMUM_STATUS_DURATION if self.registry.minimum_status_duration().is_zero() => {
                self.flush_pending_statuses();
            }
            METER_VALUE_SAMPLE_INTERVAL | CLOCK_ALIGNED_DATA_INTERVAL => self.restart_metering(),
            _ => {}
        }
    }
}

pub(super) fn get_configuration(
    actor: &mut StationActor,
    responder: Responder<GetConfiguration>,
    request: GetConfigurationRequest,
) {
    let requested = request.key.unwrap_or_default();
    let report = match actor.registry.get_configuration(&requested) {
        Ok(report) => report,
        Err(too_many) => {
            actor.respond_error(
                responder,
                ErrorCode::OccurenceConstraintViolation,
                too_many.to_string(),
            );
            return;
        }
    };
    let configuration_key: Vec<ConfigurationKey> = report
        .configuration_key
        .iter()
        .map(|entry| ConfigurationKey {
            key: entry.key.to_owned(),
            readonly: entry.readonly,
            value: (!entry.secret()).then(|| entry.value.clone()),
        })
        .collect();
    let unknown_key = report.unknown_key;
    let response = GetConfigurationResponse {
        configuration_key: (!configuration_key.is_empty()).then_some(configuration_key),
        unknown_key: (!unknown_key.is_empty()).then_some(unknown_key),
    };
    actor.respond(responder, response);
}
