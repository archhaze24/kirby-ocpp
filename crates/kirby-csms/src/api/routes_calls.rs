//! `POST /v1/stations/{id}/calls`: run one OCPP call and answer with its outcome.

use axum::body::Body;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use kirby_csms_core::CsmsError;
use serde_json::{Value, json};

use super::ApiState;
use super::error::{error, reply, unknown_station};
use super::json::{read_json, station_id};
use crate::commands::{self, Outcome};

pub(super) async fn call(
    State(state): State<ApiState>,
    id: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Response {
    let id = match station_id(id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let (body, body_bytes) = match read_json(request).await {
        Ok(body) => body,
        Err(response) => return *response,
    };
    let Value::Object(mut object) = body else {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "the body must be a JSON object",
        );
    };
    let action = match object.remove("action") {
        Some(Value::String(action)) if !action.is_empty() => action,
        _ => {
            return error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "needs a string field `action`",
            );
        }
    };
    // Take the payload out of the body and drop the rest, so that only the payload stays
    // alive while the call runs.
    let payload = object.remove("payload").unwrap_or_else(|| json!({}));
    let timeout = match commands::parse_timeout(object.get("timeout_ms")) {
        Ok(timeout) => timeout,
        Err(message) => return error(StatusCode::BAD_REQUEST, "bad_request", message),
    };
    drop(object);
    let _permit = match state.commands.try_reserve(1, body_bytes) {
        Ok(permit) => permit,
        Err(message) => {
            return error(StatusCode::TOO_MANY_REQUESTS, "too_many_requests", message);
        }
    };
    let answer = commands::call(&state.handle, &id, &action, payload, timeout).await;
    if let Some(body) = answer.api_body() {
        return reply(StatusCode::OK, &body);
    }
    match answer.outcome {
        Outcome::Failed(CsmsError::UnknownStation { .. }) => unknown_station(),
        Outcome::Failed(
            error @ (CsmsError::NotConnected { .. } | CsmsError::ConnectionClosed { .. }),
        ) => self::error(
            StatusCode::CONFLICT,
            "station_not_connected",
            &error.to_string(),
        ),
        Outcome::Failed(error @ CsmsError::QueueFull { .. }) => self::error(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_requests",
            &error.to_string(),
        ),
        Outcome::Failed(error) => self::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &error.to_string(),
        ),
        Outcome::Result | Outcome::CallError | Outcome::Timeout => self::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "call outcome without a body",
        ),
    }
}

/// Sends a bounded batch after reserving capacity for the entire selection.
pub(super) async fn bulk(State(state): State<ApiState>, request: Request<Body>) -> Response {
    let (body, bytes) = match read_json(request).await {
        Ok(body) => body,
        Err(response) => return *response,
    };
    let parsed = parse_bulk(body);
    let (pattern, protocol, action, payload, timeout) = match parsed {
        Ok(parsed) => parsed,
        Err(message) => return error(StatusCode::BAD_REQUEST, "bad_request", &message),
    };
    let stations = match commands::select_stations(&state.handle, &pattern, protocol.as_deref()) {
        Ok(stations) => stations,
        Err(message) => return error(StatusCode::BAD_REQUEST, "bad_request", &message),
    };
    let Some(bytes) = bytes.checked_mul(stations.len()) else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_requests",
            "request data exceeds the command budget",
        );
    };
    let _permit = match state.commands.try_reserve(stations.len(), bytes) {
        Ok(permit) => permit,
        Err(message) => return error(StatusCode::TOO_MANY_REQUESTS, "too_many_requests", message),
    };
    let results = commands::call_many(&state.handle, &stations, &action, &payload, timeout).await;
    reply(StatusCode::OK, &json!({"action":action,"results":results}))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BulkRequest {
    station: String,
    action: String,
    #[serde(default)]
    protocol: Option<String>,
    #[serde(default = "empty_payload")]
    payload: Value,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

fn empty_payload() -> Value {
    json!({})
}

type ParsedBulk = (
    String,
    Option<String>,
    String,
    Value,
    Option<std::time::Duration>,
);

fn parse_bulk(body: Value) -> Result<ParsedBulk, String> {
    let request: BulkRequest =
        serde_json::from_value(body).map_err(|_| "invalid bulk call fields".to_owned())?;
    commands::check_pattern(&request.station)?;
    commands::check_call_action(&request.action)?;
    if let Some(protocol) = &request.protocol {
        commands::check_protocol(protocol)?;
    }
    let timeout = commands::parse_timeout(request.timeout_ms.map(Value::from).as_ref())
        .map_err(str::to_owned)?;
    Ok((
        request.station,
        request.protocol,
        request.action,
        request.payload,
        timeout,
    ))
}
