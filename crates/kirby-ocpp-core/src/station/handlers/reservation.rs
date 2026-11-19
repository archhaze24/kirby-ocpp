//! `ReserveNow` and `CancelReservation` (rules in `station::reservation`).
//!
//! Expired reservations are released first (`expire_due`). The same reservationId on
//! another connector, or on connector 0, **moves** the reservation (OCPP 1.6 section 5.13): the
//! old one is released on a trial copy and the whole change is committed only if the new
//! reservation is accepted. A reservation on connector 0 is kept on the charge point; the
//! connector it holds is derived afterwards (`StationActor::sync_zero_hold`). Replies precede
//! the status notifications.

use crate::ids::ConnectorId;
use crate::ocpp::messages::{
    CancelReservation, CancelReservationRequest, CancelReservationResponse,
    CancelReservationStatus, ReservationStatus, ReserveNow, ReserveNowRequest, ReserveNowResponse,
};
use crate::ocpp::responder::Responder;
use crate::station::actor::{StationActor, TimerKey};
use crate::station::event::LogLevel;
use crate::station::reservation::{cancel_all, reserve_now, reserve_zero};

pub(super) fn reserve_now_handler(
    actor: &mut StationActor,
    responder: Responder<ReserveNow>,
    request: ReserveNowRequest,
) {
    actor.release_expired_reservations();
    let now = actor.clock.now_utc();
    let mut trial = actor.state.connectors.clone();
    let mut trial_station = actor.state.charge_point.clone();
    // An existing reservation with this id is replaced. On the same connector `reserve_now`
    // does it in place; anywhere else (another connector, connector 0, a held copy) it is
    // released first.
    let in_place = !request.connector_id.is_station()
        && trial_station.reservation.as_ref().map(|r| r.id) != Some(request.reservation_id)
        && trial.iter().any(|c| {
            c.id == request.connector_id
                && c.reservation
                    .as_ref()
                    .is_some_and(|r| r.id == request.reservation_id)
        });
    let released = if in_place {
        Vec::new()
    } else {
        cancel_all(&mut trial, &mut trial_station, request.reservation_id)
    };
    let result = if request.connector_id.is_station() {
        let zero_ok = actor.registry.reserve_connector_zero_supported();
        reserve_zero(&trial, &mut trial_station, &request, now, zero_ok)
            .map(|()| ConnectorId::STATION)
    } else {
        reserve_now(&mut trial, &request, now)
    };
    match result {
        Ok(target) => {
            actor.state.connectors = trial;
            actor.state.charge_point = trial_station;
            actor.respond(
                responder,
                ReserveNowResponse {
                    status: ReservationStatus::Accepted,
                },
            );
            actor.log(
                LogLevel::Info,
                format!(
                    "reservation {} on connector {target}",
                    request.reservation_id
                ),
            );
            // Before the notifications below: a replaced connector-0 reservation re-holds its
            // connector at once and must not flash `Available`.
            actor.sync_zero_hold();
            for old in released.into_iter().filter(|old| *old != target) {
                actor.timers.cancel(&TimerKey::ReservationExpiry(old));
                actor.status_changed(old);
            }
            actor.arm_reservation(target);
            actor.flush_now();
            actor.status_changed(target);
        }
        Err(status) => actor.respond(responder, ReserveNowResponse { status }),
    }
}

pub(super) fn cancel_reservation_handler(
    actor: &mut StationActor,
    responder: Responder<CancelReservation>,
    request: CancelReservationRequest,
) {
    let released = cancel_all(
        &mut actor.state.connectors,
        &mut actor.state.charge_point,
        request.reservation_id,
    );
    if released.is_empty() {
        actor.respond(
            responder,
            CancelReservationResponse {
                status: CancelReservationStatus::Rejected,
            },
        );
        return;
    }
    actor.respond(
        responder,
        CancelReservationResponse {
            status: CancelReservationStatus::Accepted,
        },
    );
    actor.flush_now();
    for id in released {
        actor.timers.cancel(&TimerKey::ReservationExpiry(id));
        actor.status_changed(id);
    }
}
