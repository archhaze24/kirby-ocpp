//! `GetTransactionStatus` (E14) and `CostUpdated` (I02).

use crate::ocpp::responder::Responder;
use crate::ocpp::v2::messages::{
    CostUpdated, CostUpdatedRequest, CostUpdatedResponse, GetTransactionStatus,
    GetTransactionStatusRequest, GetTransactionStatusResponse,
};
use crate::station::LogLevel;
use crate::station2::actor::Station2Actor;

pub(in crate::station2) fn cost_updated(
    actor: &mut Station2Actor,
    responder: Responder<CostUpdated>,
    request: CostUpdatedRequest,
) {
    actor.respond(responder, CostUpdatedResponse { custom_data: None });
    actor.on_cost_updated(&request.transaction_id, request.total_cost);
}

/// With a transaction id: `ongoingIndicator` says whether it runs (false for an unknown or a
/// stopped one) and `messagesInQueue` whether events of it wait for delivery. Without an id
/// there is no `ongoingIndicator` and `messagesInQueue` covers the whole queue.
pub(in crate::station2) fn get_transaction_status(
    actor: &mut Station2Actor,
    responder: Responder<GetTransactionStatus>,
    request: GetTransactionStatusRequest,
) {
    let id = request.transaction_id.as_deref();
    let ongoing_indicator = id.map(|id| actor.tx_by_id(id).is_some_and(|tx| tx.ongoing));
    let messages_in_queue = actor.queue_has_messages_for(id);
    if responder
        .accept(GetTransactionStatusResponse {
            ongoing_indicator,
            messages_in_queue,
            custom_data: None,
        })
        .is_err()
    {
        actor.log(
            LogLevel::Warn,
            "could not answer GetTransactionStatus".to_owned(),
        );
    }
}
