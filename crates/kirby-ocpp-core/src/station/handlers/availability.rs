//! `ChangeAvailability` and `UnlockConnector`.
//!
//! `ChangeAvailability` (TS `handleChangeAvailability`, with the connector 0 fix):
//!
//! * `connectorId` 0 changes the charge point itself **and** every connector;
//! * a connector without a transaction changes at once (`Faulted` stays `Faulted`);
//! * a connector with a transaction keeps running: the request is stored as
//!   `pending_availability` and the answer is `Scheduled`; the pending change is applied by
//!   [`StationActor::apply_pending_availability`] when the transaction ends (stage 2b calls it);
//! * an unknown connector is `Rejected`; the change is persisted and every changed
//!   connector (including 0) gets a `StatusNotification` after the reply.

use crate::ids::ConnectorId;
use crate::ocpp::messages::{
    AvailabilityStatus, ChangeAvailability, ChangeAvailabilityRequest, ChangeAvailabilityResponse,
    UnlockConnector, UnlockConnectorRequest, UnlockConnectorResponse, UnlockStatus,
};
use crate::ocpp::responder::Responder;
use crate::station::actor::StationActor;
use crate::station::connector::{charge_point_status, status_after_availability_change};
use crate::station::event::LogLevel;

pub(super) fn change_availability(
    actor: &mut StationActor,
    responder: Responder<ChangeAvailability>,
    request: ChangeAvailabilityRequest,
) {
    let kind = request.kind;
    let whole_station = request.connector_id.is_station();
    if !whole_station && actor.connector(request.connector_id).is_err() {
        actor.respond(
            responder,
            ChangeAvailabilityResponse {
                status: AvailabilityStatus::Rejected,
            },
        );
        return;
    }

    let targets: Vec<ConnectorId> = if whole_station {
        actor.state.connectors.iter().map(|c| c.id).collect()
    } else {
        vec![request.connector_id]
    };

    let mut scheduled = false;
    let mut changed: Vec<ConnectorId> = Vec::new();
    if whole_station {
        let station = &mut actor.state.charge_point;
        station.availability = kind;
        station.status = charge_point_status(kind, station.status);
        actor.state.availability = kind;
        changed.push(ConnectorId::STATION);
    }
    for id in targets {
        let Some(connector) = actor.find_connector_mut(id) else {
            continue;
        };
        if !connector.transaction.is_idle() {
            connector.pending_availability = Some(kind);
            scheduled = true;
            continue;
        }
        connector.pending_availability = None;
        connector.availability = kind;
        connector.status = status_after_availability_change(connector, kind);
        changed.push(id);
    }

    let status = if scheduled {
        AvailabilityStatus::Scheduled
    } else {
        AvailabilityStatus::Accepted
    };
    actor.respond(responder, ChangeAvailabilityResponse { status });
    actor.log(
        LogLevel::Info,
        format!(
            "ChangeAvailability connector {} -> {kind}: {status}",
            request.connector_id
        ),
    );
    actor.flush_now();
    for id in changed {
        actor.status_changed(id);
    }
}

impl StationActor {
    /// Applies an availability change that was waiting for the transaction on `id` to end.
    // stage 2b: called by the transaction-end path.
    #[allow(dead_code, reason = "called by the stage 2b transaction handling")]
    pub(in crate::station) fn apply_pending_availability(&mut self, id: ConnectorId) {
        let Some(connector) = self.find_connector_mut(id) else {
            return;
        };
        let Some(kind) = connector.pending_availability.take() else {
            return;
        };
        connector.availability = kind;
        connector.status = status_after_availability_change(connector, kind);
        self.flush_now();
        self.status_changed(id);
    }
}

pub(super) fn unlock_connector(
    actor: &mut StationActor,
    responder: Responder<UnlockConnector>,
    request: UnlockConnectorRequest,
) {
    let status = match actor.connector(request.connector_id) {
        Err(_) => UnlockStatus::NotSupported,
        // stage 2b: with a running transaction, stop it (reason UnlockCommand) after replying.
        Ok(_) => UnlockStatus::Unlocked,
    };
    actor.respond(responder, UnlockConnectorResponse { status });
}
