//! `Reset`, `TriggerMessage`, `UnlockConnector`, `ClearCache` and `DataTransfer`, plus the
//! outbound `DataTransfer` command.

#![allow(
    dead_code,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    reason = "placeholder module; remove this allow when the module is implemented"
)]

use serde_json::Value;

use crate::ocpp::responder::Responder;
use crate::ocpp::v2::messages::{
    ClearCache, ClearCacheRequest, DataTransfer, DataTransferRequest, Reset, ResetRequest,
    TriggerMessage, TriggerMessageRequest, UnlockConnector, UnlockConnectorRequest,
};
use crate::station2::actor::Station2Actor;
use crate::station2::commands::not_implemented;
use crate::station2::types::Reply2;

pub(in crate::station2) fn reset(
    _actor: &mut Station2Actor,
    _responder: Responder<Reset>,
    _request: ResetRequest,
) {
}

pub(in crate::station2) fn trigger_message(
    _actor: &mut Station2Actor,
    _responder: Responder<TriggerMessage>,
    _request: TriggerMessageRequest,
) {
}

pub(in crate::station2) fn unlock_connector(
    _actor: &mut Station2Actor,
    _responder: Responder<UnlockConnector>,
    _request: UnlockConnectorRequest,
) {
}

pub(in crate::station2) fn clear_cache(
    _actor: &mut Station2Actor,
    _responder: Responder<ClearCache>,
    _request: ClearCacheRequest,
) {
}

pub(in crate::station2) fn data_transfer(
    _actor: &mut Station2Actor,
    _responder: Responder<DataTransfer>,
    _request: DataTransferRequest,
) {
}

impl Station2Actor {
    /// `Command2::DataTransfer`.
    pub(in crate::station2) fn cmd_data_transfer(
        &mut self,
        _vendor_id: String,
        _message_id: Option<String>,
        _data: Option<Value>,
        reply: Reply2,
    ) {
        not_implemented(reply, "data_transfer");
    }
}
