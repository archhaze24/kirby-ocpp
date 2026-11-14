//! `RequestStartTransaction` and `RequestStopTransaction`.

#![allow(
    dead_code,
    clippy::needless_pass_by_value,
    reason = "placeholder module; remove this allow when the module is implemented"
)]

use super::actor::Station2Actor;
use crate::ocpp::responder::Responder;
use crate::ocpp::v2::messages::{
    RequestStartTransaction, RequestStartTransactionRequest, RequestStopTransaction,
    RequestStopTransactionRequest,
};

pub(super) fn request_start_transaction(
    _actor: &mut Station2Actor,
    _responder: Responder<RequestStartTransaction>,
    _request: RequestStartTransactionRequest,
) {
}

pub(super) fn request_stop_transaction(
    _actor: &mut Station2Actor,
    _responder: Responder<RequestStopTransaction>,
    _request: RequestStopTransactionRequest,
) {
}
