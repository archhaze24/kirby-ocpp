//! `ChangeAvailability` (G03, G04).

use crate::ocpp::responder::Responder;
use crate::ocpp::v2::messages::{
    ChangeAvailability, ChangeAvailabilityRequest, ChangeAvailabilityResponse,
    ChangeAvailabilityStatus, OperationalStatus,
};
use crate::station::LogLevel;
use crate::station2::actor::Station2Actor;
use crate::station2::evse::{Scope, apply_availability};

/// The scope a request addresses: no EVSE, or EVSE 0 without a connector, is the station.
fn scope_of(request: &ChangeAvailabilityRequest) -> Option<Scope> {
    let Some(evse) = &request.evse else {
        return Some(Scope::Station);
    };
    let id = u32::try_from(evse.id).ok()?;
    match (id, evse.connector_id) {
        (0, None) => Some(Scope::Station),
        (0, Some(_)) => None,
        (id, None) => Some(Scope::Evse(id)),
        (id, Some(connector)) => Some(Scope::Connector(id, u32::try_from(connector).ok()?)),
    }
}

pub(in crate::station2) fn change_availability(
    actor: &mut Station2Actor,
    responder: Responder<ChangeAvailability>,
    request: ChangeAvailabilityRequest,
) {
    let operative = request.operational_status == OperationalStatus::Operative;
    let busy_ids: Vec<u32> = actor
        .evses
        .iter()
        .map(|e| e.id)
        .filter(|&id| actor.evse_busy(id))
        .collect();
    let result = scope_of(&request).ok_or(()).and_then(|scope| {
        let mut station = actor.state.availability_operative;
        let busy = |evse| busy_ids.contains(&evse);
        let status = apply_availability(&mut actor.evses, &mut station, scope, operative, &busy);
        actor.state.availability_operative = station;
        status
    });
    let status = result.unwrap_or(ChangeAvailabilityStatus::Rejected);
    actor.log(
        LogLevel::Info,
        format!(
            "ChangeAvailability {:?} -> {status:?}",
            request.operational_status
        ),
    );
    // Reply first; the notifications follow it on the wire.
    if responder
        .accept(ChangeAvailabilityResponse {
            status,
            status_info: None,
            custom_data: None,
        })
        .is_err()
    {
        actor.log(
            LogLevel::Warn,
            "could not answer ChangeAvailability".to_owned(),
        );
    }
    actor.after_model_change();
}
