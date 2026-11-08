//! `POST /v1/stations/{id}/calls`: run one OCPP call and answer with its outcome.

use std::sync::Arc;

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
    let Ok(_permit) = Arc::clone(&state.slots).try_acquire_owned() else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_requests",
            "too many commands in flight",
        );
    };
    let Ok(_bytes) = u32::try_from(body_bytes).map_err(|_| ()).and_then(|bytes| {
        Arc::clone(&state.payload)
            .try_acquire_many_owned(bytes)
            .map_err(|_| ())
    }) else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_requests",
            "too much request data held by commands in flight (at most 32 MiB)",
        );
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
