//! `GetVariables`, `SetVariables`, `GetBaseReport` and `GetReport`.

use std::collections::VecDeque;
use std::time::Duration;

use serde_json::{Value, json};

use crate::ocpp::Protocol;
use crate::ocpp::frame::ErrorCode;
use crate::ocpp::responder::{ReplyHandle, Responder};
use crate::ocpp::v2::messages::{
    GenericDeviceModelStatus, GetBaseReport, GetBaseReportRequest, GetBaseReportResponse,
    GetVariableData, GetVariableResult, GetVariableStatus, GetVariables, GetVariablesRequest,
    GetVariablesResponse, SetVariableData, SetVariableResult, SetVariableStatus, SetVariables,
    SetVariablesRequest, SetVariablesResponse,
};
use crate::ocpp::v2::types::{AttributeType, StatusInfo};
use crate::station::LogLevel;
use crate::station2::actor::{Station2Actor, TimerKey2};
use crate::station2::device_model::{
    Applied, DeviceModel, LiveFn, Lookup, ReportJob, SetFailure, is_basic_auth_password,
};
use crate::station2::types::ItemsPer;

fn status_info(reason: &str, info: impl Into<String>) -> StatusInfo {
    StatusInfo {
        reason_code: reason.to_owned(),
        additional_info: Some(info.into()),
        custom_data: None,
    }
}

/// The answer to one `GetVariableData` (B06).
pub(in crate::station2) fn answer_get(
    model: &DeviceModel,
    data: &GetVariableData,
    live: LiveFn<'_>,
) -> GetVariableResult {
    let kind = data.attribute_type.unwrap_or(AttributeType::Actual);
    let result = |status, value: Option<String>, info: Option<StatusInfo>| GetVariableResult {
        attribute_status_info: info,
        attribute_status: status,
        attribute_type: Some(kind),
        attribute_value: value,
        component: data.component.clone(),
        variable: data.variable.clone(),
        custom_data: None,
    };
    match model.find(&data.component, &data.variable) {
        Lookup::UnknownComponent => result(GetVariableStatus::UnknownComponent, None, None),
        Lookup::UnknownVariable => result(GetVariableStatus::UnknownVariable, None, None),
        Lookup::Found(_) if kind != AttributeType::Actual => {
            result(GetVariableStatus::NotSupportedAttributeType, None, None)
        }
        Lookup::Found(target) => match model.read(&target, live) {
            Some(value) => result(GetVariableStatus::Accepted, Some(value), None),
            None => result(
                GetVariableStatus::Rejected,
                None,
                Some(status_info("WriteOnly", "the variable is write-only")),
            ),
        },
    }
}

/// The answer to one `SetVariableData` (B05), and what was applied.
pub(in crate::station2) fn answer_set(
    model: &mut DeviceModel,
    data: &SetVariableData,
) -> (SetVariableResult, Option<Applied>) {
    let kind = data.attribute_type.unwrap_or(AttributeType::Actual);
    let outcome = model.set_value(
        &data.component,
        &data.variable,
        data.attribute_type,
        &data.attribute_value,
    );
    let (status, info, applied) = match outcome {
        Ok(applied) => (SetVariableStatus::Accepted, None, Some(applied)),
        Err(SetFailure::UnknownComponent) => (SetVariableStatus::UnknownComponent, None, None),
        Err(SetFailure::UnknownVariable) => (SetVariableStatus::UnknownVariable, None, None),
        Err(SetFailure::AttributeType) => {
            (SetVariableStatus::NotSupportedAttributeType, None, None)
        }
        Err(SetFailure::Rejected { reason, info }) => (
            SetVariableStatus::Rejected,
            Some(status_info(reason, info)),
            None,
        ),
    };
    let result = SetVariableResult {
        attribute_type: Some(kind),
        attribute_status: status,
        attribute_status_info: info,
        component: data.component.clone(),
        variable: data.variable.clone(),
        custom_data: None,
    };
    (result, applied)
}

pub(in crate::station2) fn get_variables(
    actor: &mut Station2Actor,
    responder: Responder<GetVariables>,
    request: GetVariablesRequest,
) {
    actor.dm.security_profile = actor.state.security_profile;
    let limit = actor.items_per_message(ItemsPer::GetVariables);
    if request.get_variable_data.len() > limit {
        actor.respond_error(
            responder,
            ErrorCode::occurrence_constraint_violation(Protocol::V201),
            format!("more than {limit} variables in one request"),
        );
        return;
    }
    let results = {
        let live = |live| actor.live_value(live);
        request
            .get_variable_data
            .iter()
            .map(|data| answer_get(&actor.dm, data, &live))
            .collect()
    };
    actor.respond(
        responder,
        GetVariablesResponse {
            get_variable_result: results,
            custom_data: None,
        },
    );
}

pub(in crate::station2) fn set_variables(
    actor: &mut Station2Actor,
    responder: Responder<SetVariables>,
    request: SetVariablesRequest,
) {
    let limit = actor.items_per_message(ItemsPer::SetVariables);
    if request.set_variable_data.len() > limit {
        actor.respond_error(
            responder,
            ErrorCode::occurrence_constraint_violation(Protocol::V201),
            format!("more than {limit} variables in one request"),
        );
        return;
    }
    let mut results = Vec::with_capacity(request.set_variable_data.len());
    let mut applied_any = false;
    let mut password_changed = false;
    for data in &request.set_variable_data {
        let (result, applied) = answer_set(&mut actor.dm, data);
        if let Some(applied) = applied {
            applied_any = true;
            if applied.weak_secret {
                actor.log_weak_password();
            }
            password_changed |= is_basic_auth_password(applied.spec);
            actor.apply_variable_effects(applied.spec);
        }
        results.push(result);
    }
    actor.respond(
        responder,
        SetVariablesResponse {
            set_variable_result: results,
            custom_data: None,
        },
    );
    if applied_any {
        actor.mark_dirty();
    }
    if password_changed {
        // A01: reconnect with the new password once the answer is queued.
        actor.schedule_password_reconnect();
    }
}

pub(in crate::station2) fn get_base_report(
    actor: &mut Station2Actor,
    responder: Responder<GetBaseReport>,
    request: GetBaseReportRequest,
) {
    let reply = |status, info| GetBaseReportResponse {
        status,
        status_info: info,
        custom_data: None,
    };
    if actor.dm.report.is_some() {
        let info = status_info("Unspecified", "an earlier report is still being sent");
        actor.respond(
            responder,
            reply(GenericDeviceModelStatus::Rejected, Some(info)),
        );
        return;
    }
    actor.dm.security_profile = actor.state.security_profile;
    let items = {
        let live = |live| actor.live_value(live);
        actor.dm.report_items(request.report_base, &live)
    };
    if items.is_empty() {
        actor.respond(
            responder,
            reply(GenericDeviceModelStatus::EmptyResultSet, None),
        );
        return;
    }
    actor.dm.report = Some(ReportJob {
        request_id: request.request_id,
        remaining: VecDeque::from(items),
        seq_no: 0,
        in_flight: false,
    });
    actor.respond(responder, reply(GenericDeviceModelStatus::Accepted, None));
    // The first chunk goes out on the next loop turn, after the reply was queued.
    actor.timers.set(TimerKey2::ReportChunk, Duration::ZERO);
}

/// `GetReport` has no typed payload: the handler gets the raw reply and payload. Component
/// criteria are not supported.
pub(in crate::station2) fn get_report(
    actor: &mut Station2Actor,
    reply: ReplyHandle,
    _payload: Value,
) {
    let answer = json!({
        "status": "NotSupported",
        "statusInfo": {"reasonCode": "UnsupportedRequest"},
    });
    if let Err(error) = reply.accept_value(answer) {
        actor.log(
            LogLevel::Warn,
            format!("could not reply to GetReport: {error}"),
        );
    }
}
