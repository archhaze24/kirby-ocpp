//! `ReserveNow` and `CancelReservation` (rules in `station::reservation`).
//!
//! Expired reservations are released first (`expire_due`). The same reservationId on
//! another connector **moves** the reservation (OCPP 1.6 section 5.13): the old one is
//! released on a trial copy and the whole change is committed only if the new reservation is
//! accepted. Replies precede the status notifications.

use crate::ocpp::messages::{
    CancelReservation, CancelReservationRequest, CancelReservationResponse,
    CancelReservationStatus, ReservationStatus, ReserveNow, ReserveNowRequest, ReserveNowResponse,
};
use crate::ocpp::responder::Responder;
use crate::station::actor::{StationActor, TimerKey};
use crate::station::event::LogLevel;
use crate::station::reservation::{cancel_reservation, reserve_now};

pub(super) fn reserve_now_handler(
    actor: &mut StationActor,
    responder: Responder<ReserveNow>,
    request: ReserveNowRequest,
) {
    actor.release_expired_reservations();
    let now = actor.clock.now_utc();
    let mut trial = actor.state.connectors.clone();
    let moved_from = trial
        .iter()
        .find(|c| {
            c.reservation
                .as_ref()
                .is_some_and(|r| r.id == request.reservation_id)
                && c.id != request.connector_id
        })
        .map(|c| c.id);
    if moved_from.is_some() {
        cancel_reservation(&mut trial, request.reservation_id);
    }
    let zero_ok = actor.registry.reserve_connector_zero_supported();
    match reserve_now(&mut trial, &request, now, zero_ok) {
        Ok(target) => {
            actor.state.connectors = trial;
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
            if let Some(old) = moved_from {
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
    match cancel_reservation(&mut actor.state.connectors, request.reservation_id) {
        Some(id) => {
            actor.respond(
                responder,
                CancelReservationResponse {
                    status: CancelReservationStatus::Accepted,
                },
            );
            actor.timers.cancel(&TimerKey::ReservationExpiry(id));
            actor.flush_now();
            actor.status_changed(id);
        }
        None => actor.respond(
            responder,
            CancelReservationResponse {
                status: CancelReservationStatus::Rejected,
            },
        ),
    }
}
