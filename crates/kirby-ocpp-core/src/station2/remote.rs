//! `RequestStartTransaction` and `RequestStopTransaction` (F01, F02, F03).
//!
//! The response always goes out first; authorization and the transaction follow. Start and
//! stop points are the transaction engine's business: this module only decides whether the
//! request is accepted and hands the token over.

use super::actor::Station2Actor;
use super::authorization::AuthDecision;
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
        // A transaction that is not authorized yet can be matched with the request.
        let authorized = self.active_tx(evse).is_some_and(|tx| tx.authorized)
            || self.authorized_token(evse).is_some();
        if authorized {
            return Err("Occupied");
        }
        Ok(())
    }

    /// The EVSE a remote start applies to: the requested one, or the first that accepts it
    /// (F01.FR.06).
    fn remote_start_evse(&self, requested: Option<i32>) -> Result<u32, &'static str> {
        if !self.registration_accepted() {
            // B02.FR.05
            return Err("NotRegistered");
        }
        match requested {
            Some(id) => {
                let evse = u32::try_from(id)
                    .ok()
                    .filter(|id| *id > 0)
                    .ok_or("UnknownEvse")?;
                self.remote_start_refusal(evse).map(|()| evse)
            }
            None => self
                .evses
                .iter()
                .map(|e| e.id)
                .find(|id| self.remote_start_refusal(*id).is_ok())
                .ok_or("NoFreeEvse"),
        }
    }

    /// Authorizes the token of a remote start (unless the CSMS checks it in the event) and
    /// hands it to the transaction engine.
    fn begin_remote_start(&mut self, evse: u32, token: IdTokenRef, remote_start_id: i32) {
        let trigger = TriggerReason::RemoteStart;
        // F01.FR.02/03, C05.FR.01: the CSMS checks the token in the `TransactionEvent`.
        if token.kind == IdTokenType::Central || !self.authorize_remote_start() {
            if let Err(error) = self.on_authorized(evse, token, Some(remote_start_id), trigger) {
                self.log(
                    LogLevel::Warn,
                    format!("remote start on EVSE {evse} failed: {error}"),
                );
            }
            return;
        }
        let presented = token.clone();
        self.authorize_asking_csms(token, Some(evse), move |actor, decision| {
            let accepted = matches!(
                decision,
                AuthDecision::Accepted(_) | AuthDecision::Offline { accepted: true }
            );
            if !accepted {
                actor.log(
                    LogLevel::Warn,
                    format!("remote start on EVSE {evse}: the token was not authorized"),
                );
                return;
            }
            if let Err(error) = actor.on_authorized(evse, presented, Some(remote_start_id), trigger)
            {
                actor.log(
                    LogLevel::Warn,
                    format!("remote start on EVSE {evse} failed: {error}"),
                );
            }
        });
    }
}

pub(super) fn request_start_transaction(
    actor: &mut Station2Actor,
    responder: Responder<RequestStartTransaction>,
    request: RequestStartTransactionRequest,
) {
    let evse = match actor.remote_start_evse(request.evse_id) {
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
    if request.charging_profile.is_some() {
        actor.log(
            LogLevel::Warn,
            "RequestStartTransaction: the charging profile is ignored (no smart charging)"
                .to_owned(),
        );
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
    let token = IdTokenRef {
        id_token: request.id_token.id_token,
        kind: request.id_token.kind,
    };
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
                request.transaction_id
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
