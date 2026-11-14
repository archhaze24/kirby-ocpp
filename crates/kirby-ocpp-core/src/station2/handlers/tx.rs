//! `GetTransactionStatus` (E14).

use crate::ocpp::responder::Responder;
use crate::ocpp::v2::messages::{
    GetTransactionStatus, GetTransactionStatusRequest, GetTransactionStatusResponse,
};
use crate::station::LogLevel;
use crate::station2::actor::Station2Actor;

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
