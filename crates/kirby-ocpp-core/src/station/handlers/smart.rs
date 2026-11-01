//! `SetChargingProfile`, `ClearChargingProfile`, `GetCompositeSchedule` over the
//! [`ChargingProfileStore`](crate::station::charging_profiles::ChargingProfileStore) with the
//! limits from the configuration registry. The profiles also drive the simulated power and
//! energy (`station/power.rs`); the meter is settled at the top of every actor iteration, so
//! a change made here applies from this moment on.

use crate::ids::ConnectorId;
use crate::ocpp::messages::{
    ChargingProfile, ChargingProfileStatus, ClearChargingProfile, ClearChargingProfileRequest,
    ClearChargingProfileResponse, GetCompositeSchedule, GetCompositeScheduleRequest,
    GetCompositeScheduleResponse, GetCompositeScheduleStatus, SetChargingProfile,
    SetChargingProfileRequest, SetChargingProfileResponse,
};
use crate::ocpp::responder::Responder;
use crate::ocpp::types::ChargingRateUnitType;
use crate::station::actor::StationActor;
use crate::station::charging_profiles::{ProfileContext, StoreLimits};
use crate::station::smart_charging::CompositeRequest;
use crate::station::transaction::TransactionState;

impl StationActor {
    fn store_limits(&self) -> StoreLimits {
        let r = &self.registry;
        StoreLimits {
            max_stack_level: i32::try_from(r.charge_profile_max_stack_level()).unwrap_or(i32::MAX),
            max_installed: usize::try_from(r.max_charging_profiles_installed())
                .unwrap_or(usize::MAX),
            max_periods: usize::try_from(r.charging_schedule_max_periods()).unwrap_or(usize::MAX),
            allowed_units: [ChargingRateUnitType::A, ChargingRateUnitType::W]
                .into_iter()
                .filter(|unit| r.charging_rate_unit_allowed(*unit))
                .collect(),
        }
    }

    fn connector_count(&self) -> u32 {
        u32::try_from(self.state.connectors.len()).unwrap_or(u32::MAX)
    }

    /// Stores a profile; persists when accepted.
    pub(in crate::station) fn set_charging_profile(
        &mut self,
        connector: ConnectorId,
        profile: ChargingProfile,
    ) -> ChargingProfileStatus {
        let limits = self.store_limits();
        let ctx = ProfileContext {
            now: self.clock.now_utc(),
            connector_count: self.connector_count(),
            active_tx: self
                .find_connector(connector)
                .and_then(crate::station::ConnectorSnapshot::tx_id),
            limits: &limits,
        };
        let status = self.profiles.set(connector, profile, &ctx);
        if status == ChargingProfileStatus::Accepted {
            self.mark_dirty();
        }
        status
    }
}

pub(super) fn set_profile(
    actor: &mut StationActor,
    responder: Responder<SetChargingProfile>,
    request: SetChargingProfileRequest,
) {
    let status = actor.set_charging_profile(request.connector_id, request.cs_charging_profiles);
    actor.respond(responder, SetChargingProfileResponse { status });
}

pub(super) fn clear_profile(
    actor: &mut StationActor,
    responder: Responder<ClearChargingProfile>,
    request: ClearChargingProfileRequest,
) {
    let now = actor.clock.now_utc();
    let status = actor.profiles.clear(&request, now);
    actor.mark_dirty();
    actor.respond(responder, ClearChargingProfileResponse { status });
}

pub(super) fn composite_schedule(
    actor: &mut StationActor,
    responder: Responder<GetCompositeSchedule>,
    request: GetCompositeScheduleRequest,
) {
    let (active_tx, tx_started_at) =
        actor
            .find_connector(request.connector_id)
            .map_or((None, None), |c| match &c.transaction {
                TransactionState::Active(a) => (Some(a.tx_id), Some(a.started_at)),
                TransactionState::PendingStart(p) => (Some(p.tx_id), Some(p.started_at)),
                _ => (None, None),
            });
    let query = CompositeRequest {
        connector: request.connector_id,
        duration_s: request.duration,
        unit: request.charging_rate_unit,
        now: actor.clock.now_utc(),
        connector_count: actor.connector_count(),
        active_tx,
        tx_started_at,
        model: actor.power_model(),
    };
    let before = actor.profiles.len();
    let result = actor.profiles.composite_schedule(&query);
    if actor.profiles.len() != before {
        actor.mark_dirty();
    }
    let response = match result {
        Ok(schedule) => GetCompositeScheduleResponse {
            status: GetCompositeScheduleStatus::Accepted,
            connector_id: Some(schedule.connector_id),
            schedule_start: Some(schedule.schedule_start),
            charging_schedule: Some(schedule.charging_schedule),
        },
        Err(_) => GetCompositeScheduleResponse {
            status: GetCompositeScheduleStatus::Rejected,
            connector_id: None,
            schedule_start: None,
            charging_schedule: None,
        },
    };
    actor.respond(responder, response);
}
