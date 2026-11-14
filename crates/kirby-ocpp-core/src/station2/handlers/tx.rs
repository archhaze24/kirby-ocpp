//! `GetTransactionStatus`.

#![allow(
    dead_code,
    clippy::needless_pass_by_value,
    reason = "placeholder module; remove this allow when the module is implemented"
)]

use crate::ocpp::responder::Responder;
use crate::ocpp::v2::messages::{GetTransactionStatus, GetTransactionStatusRequest};
use crate::station2::actor::Station2Actor;

pub(in crate::station2) fn get_transaction_status(
    _actor: &mut Station2Actor,
    _responder: Responder<GetTransactionStatus>,
    _request: GetTransactionStatusRequest,
) {
}
