//! `GetVariables`, `SetVariables`, `GetBaseReport` and `GetReport`.

#![allow(
    dead_code,
    clippy::needless_pass_by_value,
    reason = "placeholder module; remove this allow when the module is implemented"
)]

use serde_json::Value;

use crate::ocpp::responder::{ReplyHandle, Responder};
use crate::ocpp::v2::messages::{
    GetBaseReport, GetBaseReportRequest, GetVariables, GetVariablesRequest, SetVariables,
    SetVariablesRequest,
};
use crate::station2::actor::Station2Actor;

pub(in crate::station2) fn get_variables(
    _actor: &mut Station2Actor,
    _responder: Responder<GetVariables>,
    _request: GetVariablesRequest,
) {
}

pub(in crate::station2) fn set_variables(
    _actor: &mut Station2Actor,
    _responder: Responder<SetVariables>,
    _request: SetVariablesRequest,
) {
}

pub(in crate::station2) fn get_base_report(
    _actor: &mut Station2Actor,
    _responder: Responder<GetBaseReport>,
    _request: GetBaseReportRequest,
) {
}

/// `GetReport` has no typed payload: the handler gets the raw reply and payload.
pub(in crate::station2) fn get_report(
    _actor: &mut Station2Actor,
    _reply: ReplyHandle,
    _payload: Value,
) {
}
