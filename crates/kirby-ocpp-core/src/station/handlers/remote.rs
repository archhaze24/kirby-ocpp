//! `RemoteStartTransaction`, `RemoteStopTransaction`, `Reset`.
//!
//! RemoteStart (TS `handleRemoteStartTransaction` + fixes): the target is the named connector
//! or the first idle, operative, `Available` one; `can_remote_start` applies the reservation
//! rules including parentIdTag; a `chargingProfile` must be a `TxProfile` without
//! `transactionId` (it is stored, bound to the new transaction, after the start); with
//! `AuthorizeRemoteTxRequests` the tag is authorized first and the answer waits for it.
//! The reply precedes the start itself.
//!
//! Reset: reply `Accepted` first, then (see `StationActor::perform_reset`) running transactions
//! are stopped through the guarded stop path (a failure there is logged, it cannot abort the
//! reset), reservations are released, the registration is dropped and `BootNotification` is
//! sent again after 500 ms, repeated up to `ResetRetries` times while not `Accepted`.

use std::time::Duration;

use crate::ids::{ConnectorId, IdTag, TxId};
use crate::ocpp::messages::{
    ChargingProfile, RemoteStartStopStatus, RemoteStartTransaction, RemoteStartTransactionRequest,
    RemoteStartTransactionResponse, RemoteStopTransaction, RemoteStopTransactionRequest,
    RemoteStopTransactionResponse, Reset, ResetRequest, ResetResponse, ResetStatus,
};
use crate::ocpp::responder::Responder;
use crate::ocpp::types::{ChargingProfilePurposeType, ResetType, StopReason};
use crate::station::actor::{StationActor, TimerKey};
use crate::station::authorization::known_parent;
use crate::station::command::Outcome;
use crate::station::event::LogLevel;
use crate::station::reservation::{can_remote_start, clear_reservation};
use crate::station::transactions::{Done, ignore};

pub(super) fn remote_start(
    actor: &mut StationActor,
    responder: Responder<RemoteStartTransaction>,
    request: RemoteStartTransactionRequest,
) {
    let now = actor.clock.now_utc();
    let id_tag = request.id_tag;
    let target: Option<ConnectorId> = match request.connector_id {
        Some(id) if !id.is_station() => Some(id),
        _ => actor
            .state
            .connectors
            .iter()
            .find(|c| {
                c.transaction.is_idle()
                    && c.availability == crate::ocpp::types::AvailabilityType::Operative
                    && c.status == crate::ocpp::types::ConnectorStatus::Available
            })
            .map(|c| c.id),
    };
    let parent = known_parent(&id_tag, now, &actor.local_list, &actor.auth_cache);
    let connector = target.and_then(|id| actor.connector(id).ok());
    let profile_ok = request.charging_profile.as_ref().is_none_or(|p| {
        p.charging_profile_purpose == ChargingProfilePurposeType::TxProfile
            && p.transaction_id.is_none()
    });
    let reply = |actor: &StationActor, responder, status| {
        actor.respond(responder, RemoteStartTransactionResponse { status });
    };
    let Some(id) =
        target.filter(|_| profile_ok && can_remote_start(connector, &id_tag, parent.as_ref(), now))
    else {
        return reply(actor, responder, RemoteStartStopStatus::Rejected);
    };
    let profile = request.charging_profile;
    if actor.registry.authorize_remote_tx_requests() {
        let tag = id_tag.clone();
        actor.authorize(
            id_tag,
            Box::new(move |actor, result| match result {
                Ok(Outcome::Authorized(decision)) if decision.accepted => {
                    reply(actor, responder, RemoteStartStopStatus::Accepted);
                    actor.begin_remote_start(id, tag, profile);
                }
                _ => reply(actor, responder, RemoteStartStopStatus::Rejected),
            }),
        );
    } else {
        reply(actor, responder, RemoteStartStopStatus::Accepted);
        actor.begin_remote_start(id, id_tag, profile);
    }
}

impl StationActor {
    fn begin_remote_start(
        &mut self,
        id: ConnectorId,
        id_tag: IdTag,
        profile: Option<ChargingProfile>,
    ) {
        let done: Done = Box::new(move |actor, result| {
            if let (Ok(Outcome::TransactionStarted { tx_id, .. }), Some(profile)) =
                (result, profile)
            {
                actor.install_remote_profile(id, tx_id, profile);
            }
        });
        self.start_transaction(id, id_tag, done);
    }

    fn install_remote_profile(&mut self, id: ConnectorId, tx: TxId, mut profile: ChargingProfile) {
        profile.transaction_id = tx.wire();
        let status = self.set_charging_profile(id, profile);
        self.log(
            LogLevel::Info,
            format!("RemoteStart charging profile: {status}"),
        );
    }
}

pub(super) fn remote_stop(
    actor: &mut StationActor,
    responder: Responder<RemoteStopTransaction>,
    request: RemoteStopTransactionRequest,
) {
    let tx = TxId::Csms(request.transaction_id);
    let connector = actor
        .state
        .connectors
        .iter()
        .find(|c| c.transaction.tx_id() == Some(tx))
        .map(|c| c.id);
    let status = if connector.is_some() {
        RemoteStartStopStatus::Accepted
    } else {
        RemoteStartStopStatus::Rejected
    };
    actor.respond(responder, RemoteStopTransactionResponse { status });
    if let Some(id) = connector {
        actor.stop_transaction(id, StopReason::Remote, None, ignore());
    }
}

pub(super) fn reset(actor: &mut StationActor, responder: Responder<Reset>, request: ResetRequest) {
    actor.respond(
        responder,
        ResetResponse {
            status: ResetStatus::Accepted,
        },
    );
    actor.perform_reset(request.kind);
}

impl StationActor {
    fn perform_reset(&mut self, kind: ResetType) {
        let reason = match kind {
            ResetType::Hard => StopReason::HardReset,
            ResetType::Soft => StopReason::SoftReset,
        };
        self.log(
            LogLevel::Warn,
            format!("{kind} reset accepted; rebooting station"),
        );
        self.timers.cancel(&TimerKey::Heartbeat);
        self.timers.cancel(&TimerKey::BootRetry);
        self.timers.cancel(&TimerKey::Maintenance);
        self.maintenance.clear();
        self.timers
            .cancel_where(|k| matches!(k, TimerKey::MeterPeriodic(_) | TimerKey::MeterAligned));

        let running: Vec<ConnectorId> = self
            .state
            .connectors
            .iter()
            .filter(|c| !c.transaction.is_idle())
            .map(|c| c.id)
            .collect();
        for id in running {
            // Failures are logged inside; the reset goes on.
            self.stop_transaction(id, reason, None, ignore());
            if let Some(connector) = self.find_connector_mut(id) {
                connector.ev_connected = false;
                connector.status = if connector.availability
                    == crate::ocpp::types::AvailabilityType::Inoperative
                {
                    crate::ocpp::types::ConnectorStatus::Unavailable
                } else {
                    crate::ocpp::types::ConnectorStatus::Available
                };
            }
            self.status_changed(id);
        }
        let reserved: Vec<ConnectorId> = self
            .state
            .connectors
            .iter()
            .filter(|c| c.reservation.is_some())
            .map(|c| c.id)
            .collect();
        for id in reserved {
            if let Some(connector) = self.find_connector_mut(id) {
                clear_reservation(connector);
            }
            self.timers.cancel(&TimerKey::ReservationExpiry(id));
            self.status_changed(id);
        }
        self.state.registration = None;
        self.reset_retries = Some(self.registry.reset_retries());
        if self.link.conn.is_some() {
            self.timers
                .set(TimerKey::BootRetry, Duration::from_millis(500));
        }
        self.flush_now();
    }
}
