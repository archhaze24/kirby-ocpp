//! `RequestStartTransaction` and `RequestStopTransaction` (F01, F02, F03).
//!
//! The response always goes out first; authorization and the transaction follow. Start and
//! stop points are the transaction engine's business: this module only decides whether the
//! request is accepted and hands the token over.

use serde_json::Value;

use super::actor::Station2Actor;
use super::authorization::{AuthDecision, group_in};
use super::link::sanitize;
use super::types::IdTokenRef;
use crate::ocpp::responder::Responder;
use crate::ocpp::v2::messages::{
    RequestStartStopStatus, RequestStartTransaction, RequestStartTransactionRequest,
    RequestStartTransactionResponse, RequestStopTransaction, RequestStopTransactionRequest,
    RequestStopTransactionResponse, TriggerReason,
};
use crate::ocpp::v2::types::{IdTokenType, StatusInfo};
use crate::station::LogLevel;

fn status_info(reason: &str) -> StatusInfo {
    StatusInfo {
        reason_code: reason.to_owned(),
        additional_info: None,
        custom_data: None,
    }
}

impl Station2Actor {
    /// Why the EVSE cannot take a remote start, or `Ok` (F01.FR.23, F01.FR.24).
    fn remote_start_refusal(&self, evse: u32) -> Result<(), &'static str> {
        let Some(model) = self.evse(evse) else {
            return Err("UnknownEvse");
        };
        let unavailable = !self.evse_operative(evse)
            || model.connectors.iter().any(|connector| {
                matches!(
                    self.connector_status_text(evse, connector.id),
                    Some("Unavailable" | "Faulted")
                )
            });
        if unavailable {
            return Err("Unavailable");
        }
        // A running transaction adds no start event, so only an EVSE without one is refused.
        if self.active_tx(evse).is_none() && self.queue_full() {
            return Err("QueueFull");
        }
        // A transaction that is not authorized yet can be matched with the request.
        let authorized = self.active_tx(evse).is_some_and(|tx| tx.authorized)
            || self.authorized_token(evse).is_some();
        if authorized || self.tx.remote_authorizing.contains_key(&evse) {
            return Err("Occupied");
        }
        Ok(())
    }

    /// The EVSE a remote start applies to: the requested one, or the first that accepts it
    /// (F01.FR.06).
    fn remote_start_evse(
        &self,
        requested: Option<i32>,
        token: &IdTokenRef,
    ) -> Result<u32, &'static str> {
        if !self.registration_accepted() {
            // B02.FR.05
            return Err("NotRegistered");
        }
        // H03: an EVSE held for another token is not offered; the group is the one the
        // station knows for the token.
        let group = self.group_of_token(token, None);
        let refusal = |id: u32| {
            self.remote_start_refusal(id)?;
            if self.reservation_blocks(id, token, group.as_deref()) {
                return Err("Reserved");
            }
            Ok(())
        };
        match requested {
            Some(id) => {
                let evse = u32::try_from(id)
                    .ok()
                    .filter(|id| *id > 0)
                    .ok_or("UnknownEvse")?;
                refusal(evse).map(|()| evse)
            }
            None => self
                .evses
                .iter()
                .map(|e| e.id)
                .find(|id| refusal(*id).is_ok())
                .ok_or("NoFreeEvse"),
        }
    }

    /// Authorizes the token of a remote start (unless the CSMS checks it in the event) and
    /// hands it to the transaction engine.
    fn begin_remote_start(&mut self, evse: u32, token: IdTokenRef, remote_start_id: i32) {
        let trigger = TriggerReason::RemoteStart;
        // F01.FR.02/03, C05.FR.01: the CSMS checks the token in the `TransactionEvent`.
        if token.kind == IdTokenType::Central || !self.authorize_remote_start() {
            self.start_with_reservation(evse, &token, None, false, remote_start_id, trigger);
            return;
        }
        let presented = token.clone();
        self.tx
            .remote_authorizing
            .insert(evse, Some(remote_start_id));
        self.authorize_asking_csms(token, Some(evse), move |actor, decision| {
            if actor.tx.remote_authorizing.remove(&evse).flatten() != Some(remote_start_id) {
                actor.forget_driver_tariff(&presented);
                return;
            }
            let accepted = matches!(
                decision,
                AuthDecision::Accepted(_) | AuthDecision::Offline { accepted: true }
            );
            if !accepted {
                actor.sc_forget_pending(evse, remote_start_id);
                actor.log(
                    LogLevel::Warn,
                    format!("remote start on EVSE {evse}: the token was not authorized"),
                );
                return;
            }
            // C16.FR.03: a master pass token starts no transaction.
            if actor.is_master_pass(&presented, &decision) {
                actor.sc_forget_pending(evse, remote_start_id);
                actor.forget_driver_tariff(&presented);
                actor.log(
                    LogLevel::Warn,
                    format!("remote start on EVSE {evse}: a master pass token starts nothing"),
                );
                return;
            }
            if let Err(reason) = actor.remote_start_refusal(evse) {
                actor.sc_forget_pending(evse, remote_start_id);
                actor.forget_driver_tariff(&presented);
                actor.log(
                    LogLevel::Warn,
                    format!("remote start on EVSE {evse} failed: {reason}"),
                );
                return;
            }
            let info = match decision {
                AuthDecision::Accepted(info) => info,
                _ => None,
            };
            actor.start_with_reservation(
                evse,
                &presented,
                info.as_ref(),
                true,
                remote_start_id,
                trigger,
            );
        });
    }

    /// Cancels an outstanding remote authorization and its profile reservation.
    pub(super) fn cancel_remote_authorization(&mut self, evse: u32) {
        if let Some(id) = self
            .tx
            .remote_authorizing
            .get_mut(&evse)
            .and_then(Option::take)
        {
            self.sc_forget_pending(evse, id);
        }
    }

    /// Hands an authorized token to the transaction engine, using the reservation the token
    /// holds. An EVSE held for another token (known only now, from the group the CSMS named)
    /// is not started; the CALL was answered already.
    fn start_with_reservation(
        &mut self,
        evse: u32,
        token: &IdTokenRef,
        info: Option<&Value>,
        note_group: bool,
        remote_start_id: i32,
        trigger: TriggerReason,
    ) {
        let group = self.group_of_token(token, group_in(info));
        if self.reservation_blocks(evse, token, group.as_deref()) {
            self.sc_forget_pending(evse, remote_start_id);
            self.forget_driver_tariff(token);
            self.log(
                LogLevel::Warn,
                format!("remote start on EVSE {evse}: the EVSE is reserved for another token"),
            );
            return;
        }
        self.reservation_claim(evse, token, group.as_deref());
        match self.on_authorized(evse, token.clone(), Some(remote_start_id), trigger) {
            Ok(()) => {
                if note_group {
                    self.note_authorization_group(evse, token, info);
                }
            }
            Err(error) => {
                self.sc_forget_pending(evse, remote_start_id);
                self.reservation_release_claim(evse);
                self.forget_driver_tariff(token);
                self.log(
                    LogLevel::Warn,
                    format!("remote start on EVSE {evse} failed: {error}"),
                );
            }
        }
    }
}

pub(super) fn request_start_transaction(
    actor: &mut Station2Actor,
    responder: Responder<RequestStartTransaction>,
    request: RequestStartTransactionRequest,
) {
    let token = IdTokenRef {
        id_token: request.id_token.id_token,
        kind: request.id_token.kind,
    };
    let evse = match actor.remote_start_evse(request.evse_id, &token) {
        Ok(evse) => evse,
        Err(reason) => {
            actor.log(
                LogLevel::Info,
                format!("RequestStartTransaction rejected: {reason}"),
            );
            actor.respond(
                responder,
                RequestStartTransactionResponse {
                    status: RequestStartStopStatus::Rejected,
                    status_info: Some(status_info(reason)),
                    transaction_id: None,
                    custom_data: None,
                },
            );
            return;
        }
    };
    // K05: a charging profile must be a valid TxProfile; it takes effect with the transaction.
    if let Some(profile) = &request.charging_profile
        && let Err(info) = actor.sc_remote_start_profile(evse, profile, request.remote_start_id)
    {
        actor.log(
            LogLevel::Info,
            format!(
                "RequestStartTransaction rejected: {}",
                sanitize(&info.reason_code)
            ),
        );
        actor.respond(
            responder,
            RequestStartTransactionResponse {
                status: RequestStartStopStatus::Rejected,
                status_info: Some(info),
                transaction_id: None,
                custom_data: None,
            },
        );
        return;
    }
    // F01.FR.13: the id of a transaction that exists already.
    let transaction_id = actor.active_tx(evse).map(|tx| tx.id.0.clone());
    actor.respond(
        responder,
        RequestStartTransactionResponse {
            status: RequestStartStopStatus::Accepted,
            status_info: None,
            transaction_id,
            custom_data: None,
        },
    );
    actor.begin_remote_start(evse, token, request.remote_start_id);
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "handlers share the signature `fn(&mut Station2Actor, Responder<A>, A::Req)`"
)]
pub(super) fn request_stop_transaction(
    actor: &mut Station2Actor,
    responder: Responder<RequestStopTransaction>,
    request: RequestStopTransactionRequest,
) {
    // B02.FR.05: nothing remote is accepted before the boot was.
    let evse = actor
        .registration_accepted()
        .then(|| {
            actor.evses.iter().map(|e| e.id).find(|id| {
                actor
                    .active_tx(*id)
                    .is_some_and(|tx| tx.id.0 == request.transaction_id)
            })
        })
        .flatten();
    let Some(evse) = evse else {
        actor.log(
            LogLevel::Info,
            format!(
                "RequestStopTransaction rejected: no transaction {}",
                sanitize(&request.transaction_id)
            ),
        );
        actor.respond(
            responder,
            RequestStopTransactionResponse {
                status: RequestStartStopStatus::Rejected,
                status_info: Some(status_info("UnknownTransaction")),
                custom_data: None,
            },
        );
        return;
    };
    actor.respond(
        responder,
        RequestStopTransactionResponse {
            status: RequestStartStopStatus::Accepted,
            status_info: None,
            custom_data: None,
        },
    );
    actor.remote_stop(evse);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use tokio::sync::{broadcast, mpsc};

    use super::*;
    use crate::clock::ManualClock;
    use crate::config::StationConfig;
    use crate::ocpp::Protocol;
    use crate::station2::transactions::StartSpec;

    /// A station without a connection and without persistence.
    fn actor() -> Station2Actor {
        let mut config = StationConfig::new("ws://127.0.0.1:1")
            .with_connectors(2)
            .without_persistence();
        config.protocol = Protocol::V201;
        let (_commands, receiver) = mpsc::channel(4);
        let (events, _) = broadcast::channel(64);
        Station2Actor::new(
            config,
            Arc::new(ManualClock::new(Utc::now())),
            receiver,
            events,
            None,
        )
        .0
    }

    #[tokio::test]
    async fn a_full_queue_refuses_only_an_evse_without_a_transaction() {
        let mut a = actor();
        a.set_queue_cap(1);
        a.begin_transaction(
            1,
            StartSpec {
                token: None,
                remote_start_id: None,
                trigger: TriggerReason::CablePluggedIn,
                authorized: false,
            },
        )
        .expect("started");
        assert!(a.queue_full(), "the started event fills the queue");
        assert_eq!(a.remote_start_refusal(2), Err("QueueFull"));
        // The transaction exists already: the request only authorizes it and adds no start.
        assert_eq!(a.remote_start_refusal(1), Ok(()));
    }
}
