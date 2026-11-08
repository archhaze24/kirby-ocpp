//! Rules (global and per station) and faults.

use axum::body::Body;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, Uri};
use axum::response::Response;
use kirby_csms_core::Scope;
use serde::Deserialize;
use serde_json::{Value, json};

use super::ApiState;
use super::error::{error, reply};
use super::json::{query_param, read_json, station_id};
use crate::commands;

pub(super) async fn list_rules(State(state): State<ApiState>) -> Response {
    let protocol_rules: Vec<Value> = global_protocol_rules(&state)
        .into_iter()
        .map(|(action, protocol)| json!({"action": action, "protocol": protocol}))
        .collect();
    reply(
        StatusCode::OK,
        &json!({
            "rules": state.handle.responder_actions(),
            "protocol_rules": protocol_rules,
        }),
    )
}

/// The global rules limited to one protocol, as (action, protocol), sorted by action.
fn global_protocol_rules(state: &ApiState) -> Vec<(String, String)> {
    state
        .handle
        .scoped_rules(None)
        .into_iter()
        .filter(|rule| rule.scope.station.is_none())
        .filter_map(|rule| rule.scope.protocol.map(|protocol| (rule.action, protocol)))
        .collect()
}

/// The path action of a rule, which must be an action a charge point can send.
fn rule_action(
    action: Result<Path<String>, PathRejection>,
    protocol: Option<&str>,
) -> Result<String, Box<Response>> {
    match action {
        Ok(Path(action)) if commands::is_rule_action(&action, protocol) => Ok(action),
        Ok(Path(action)) => Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "unsupported_action",
            &format!(
                "{} is not a known charge-point action for protocol {}",
                kirby_ocpp_tui::text::sanitize(&action),
                protocol.unwrap_or(kirby_csms_core::ACTIONS_PROTOCOL)
            ),
        ))),
        Err(_) => Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "invalid action in the path",
        ))),
    }
}

pub(super) async fn put_rule(
    State(state): State<ApiState>,
    action: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Response {
    let protocol = match protocol_param(request.uri()) {
        Ok(protocol) => protocol,
        Err(response) => return *response,
    };
    let action = match rule_action(action, protocol.as_deref()) {
        Ok(action) => action,
        Err(response) => return *response,
    };
    let payload = match read_rule_payload(request).await {
        Ok(payload) => payload,
        Err(response) => return *response,
    };
    let scope = Scope {
        station: None,
        protocol,
    };
    if let Err(message) = commands::set_rule(&state.handle, &scope, &action, payload) {
        return error(StatusCode::CONFLICT, "conflict", &message);
    }
    reply(StatusCode::OK, &json!({"action": action, "set": true}))
}

/// The body of a rule request: a JSON object, the whole CALLRESULT payload.
async fn read_rule_payload(request: Request<Body>) -> Result<Value, Box<Response>> {
    match read_json(request).await {
        Ok((payload, _)) if payload.is_object() => Ok(payload),
        Ok(_) => Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "the body must be a JSON object",
        ))),
        Err(response) => Err(response),
    }
}

/// The optional `?protocol=` of a per-station rule request, checked against the registry.
fn protocol_param(uri: &Uri) -> Result<Option<String>, Box<Response>> {
    let bad = |message: &str| Box::new(error(StatusCode::BAD_REQUEST, "bad_request", message));
    let protocol = query_param(uri, "protocol").map_err(|message| bad(&message))?;
    if let Some(protocol) = &protocol {
        commands::check_protocol(protocol)
            .map_err(|message| bad(&kirby_ocpp_tui::text::sanitize(&message)))?;
    }
    Ok(protocol)
}

/// The checked station id of a per-station rule path (the station need not be known).
fn rule_station(id: Result<Path<String>, PathRejection>) -> Result<String, Box<Response>> {
    let id = station_id(id)?;
    commands::check_station_id(&id)
        .map_err(|message| Box::new(error(StatusCode::BAD_REQUEST, "bad_request", &message)))?;
    Ok(id)
}

/// A `Path` of two parameters, `{id}` and `{action}`.
type StationAction = Result<Path<(String, String)>, PathRejection>;

/// The checked station id and rule action of a per-station rule path.
fn station_and_action(
    path: StationAction,
    protocol: Option<&str>,
) -> Result<(String, String), Box<Response>> {
    let Ok(Path((id, action))) = path else {
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "invalid station id or action in the path",
        )));
    };
    let id = rule_station(Ok(Path(id)))?;
    let action = rule_action(Ok(Path(action)), protocol)?;
    Ok((id, action))
}

pub(super) async fn list_station_rules(
    State(state): State<ApiState>,
    id: Result<Path<String>, PathRejection>,
) -> Response {
    let id = match rule_station(id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let rules: Vec<Value> = state
        .handle
        .scoped_rules(Some(&id))
        .iter()
        .map(|rule| {
            json!({
                "action": rule.action,
                "protocol": rule.scope.protocol,
                "payload": rule.payload,
            })
        })
        .collect();
    reply(StatusCode::OK, &json!({"station": id, "rules": rules}))
}

pub(super) async fn get_station_rule(
    State(state): State<ApiState>,
    path: StationAction,
) -> Response {
    let (id, action) = match station_and_action(path, None) {
        Ok(pair) => pair,
        Err(response) => return *response,
    };
    match state.handle.scoped_rule(&id, &action) {
        Some(rule) => reply(
            StatusCode::OK,
            &json!({
                "station": id,
                "action": rule.action,
                "protocol": rule.scope.protocol,
                "payload": rule.payload,
            }),
        ),
        None => error(
            StatusCode::NOT_FOUND,
            "unknown_rule",
            "the station has no rule for this action",
        ),
    }
}

pub(super) async fn put_station_rule(
    State(state): State<ApiState>,
    path: StationAction,
    request: Request<Body>,
) -> Response {
    let protocol = match protocol_param(request.uri()) {
        Ok(protocol) => protocol,
        Err(response) => return *response,
    };
    let (id, action) = match station_and_action(path, protocol.as_deref()) {
        Ok(pair) => pair,
        Err(response) => return *response,
    };
    let payload = match read_rule_payload(request).await {
        Ok(payload) => payload,
        Err(response) => return *response,
    };
    let scope = Scope {
        station: Some(id.clone()),
        protocol: protocol.clone(),
    };
    if let Err(message) = commands::set_rule(&state.handle, &scope, &action, payload) {
        return error(StatusCode::CONFLICT, "conflict", &message);
    }
    reply(
        StatusCode::OK,
        &json!({"station": id, "action": action, "protocol": protocol, "set": true}),
    )
}

pub(super) async fn delete_station_rule(
    State(state): State<ApiState>,
    path: StationAction,
    uri: Uri,
) -> Response {
    let (id, action) = match station_and_action(path, None) {
        Ok(pair) => pair,
        Err(response) => return *response,
    };
    if uri.query().is_some() {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "this request takes no query parameters",
        );
    }
    let cleared = state.handle.clear_scoped_rule(Some(&id), &action);
    reply(
        StatusCode::OK,
        &json!({"station": id, "action": action, "cleared": cleared}),
    )
}

pub(super) async fn delete_rule(
    State(state): State<ApiState>,
    action: Result<Path<String>, PathRejection>,
    uri: Uri,
) -> Response {
    let protocol = match protocol_param(&uri) {
        Ok(protocol) => protocol,
        Err(response) => return *response,
    };
    let action = match rule_action(action, protocol.as_deref()) {
        Ok(action) => action,
        Err(response) => return *response,
    };
    let existed = match protocol {
        Some(protocol) => {
            let stored = global_protocol_rules(&state)
                .into_iter()
                .any(|(held, scope)| held == action && scope == protocol);
            stored && state.handle.clear_scoped_rule(None, &action)
        }
        None => state.handle.clear_responder(&action),
    };
    reply(
        StatusCode::OK,
        &json!({"action": action, "cleared": existed}),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FaultRequest {
    action: String,
    kind: commands::FaultKind,
    #[serde(default)]
    payload: Option<Value>,
    #[serde(default = "one")]
    times: usize,
    #[serde(default)]
    station: Option<String>,
    #[serde(default)]
    protocol: Option<String>,
}

fn one() -> usize {
    1
}

pub(super) async fn inject_fault(
    State(state): State<ApiState>,
    request: Request<Body>,
) -> Response {
    let body = match read_json(request).await {
        Ok((body, _)) => body,
        Err(response) => return *response,
    };
    let fault: FaultRequest = match serde_json::from_value(body) {
        Ok(fault) => fault,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                &format!("invalid fault: {e}"),
            );
        }
    };
    if let Err(message) = commands::check_fault(
        &fault.action,
        fault.kind,
        fault.payload.as_ref(),
        fault.times,
    ) {
        return error(StatusCode::BAD_REQUEST, "bad_request", &message);
    }
    let scope = match commands::scope_of(fault.station.as_deref(), fault.protocol.as_deref()) {
        Ok(scope) => scope,
        Err(message) => {
            return error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                &kirby_ocpp_tui::text::sanitize(&message),
            );
        }
    };
    let id = match state.handle.try_inject_scoped(
        commands::build_fault(&fault.action, fault.kind, fault.payload, fault.times),
        scope,
    ) {
        Ok(id) => id,
        Err(message) => return error(StatusCode::CONFLICT, "conflict", &message),
    };
    reply(
        StatusCode::OK,
        &json!({"injected": true, "id": id, "remaining": state.handle.faults_remaining()}),
    )
}

pub(super) async fn faults_remaining(State(state): State<ApiState>) -> Response {
    reply(
        StatusCode::OK,
        &json!({
            "remaining": state.handle.faults_remaining(),
            "faults": commands::faults_list(&state.handle, None),
        }),
    )
}

pub(super) async fn remove_fault(
    State(state): State<ApiState>,
    id: Result<Path<String>, PathRejection>,
) -> Response {
    let Ok(Path(id)) = id else {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "invalid fault id in the path",
        );
    };
    let removed = state.handle.remove_fault(&id);
    reply(StatusCode::OK, &json!({"id": id, "removed": removed}))
}

pub(super) async fn clear_faults(State(state): State<ApiState>) -> Response {
    state.handle.clear_faults();
    reply(StatusCode::OK, &json!({"remaining": 0}))
}
