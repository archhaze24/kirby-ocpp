//! Station resources: list, get, history, forget.

use axum::extract::Path;
use axum::extract::State;
use axum::extract::rejection::PathRejection;
use axum::http::{StatusCode, Uri};
use axum::response::Response;
use serde_json::{Value, json};

use super::ApiState;
use super::error::{error, reply, unknown_station};
use super::json::{query_param, station_id, stringify_transaction_ids};
use crate::commands;
use crate::headless::station_summary;

pub(super) async fn list_stations(State(state): State<ApiState>) -> Response {
    let mut stations = commands::stations(&state.handle);
    if let Value::Array(list) = &mut stations {
        list.iter_mut().for_each(stringify_transaction_ids);
    }
    reply(StatusCode::OK, &json!({"stations": stations}))
}

pub(super) async fn get_station(
    State(state): State<ApiState>,
    id: Result<Path<String>, PathRejection>,
) -> Response {
    let id = match station_id(id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(info) = state.handle.station(&id) else {
        return unknown_station();
    };
    let station_state = state.handle.state(&id);
    let mut value = station_summary(&info, station_state.as_ref());
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "state".to_owned(),
            station_state.as_ref().map_or(Value::Null, |station_state| {
                serde_json::to_value(station_state).unwrap_or(Value::Null)
            }),
        );
    }
    stringify_transaction_ids(&mut value);
    reply(StatusCode::OK, &value)
}

pub(super) async fn history(
    State(state): State<ApiState>,
    id: Result<Path<String>, PathRejection>,
    uri: Uri,
) -> Response {
    let id = match station_id(id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let limit = match query_param(&uri, "limit") {
        Ok(Some(value)) => match value
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| value.parse::<usize>())
        {
            Some(Ok(limit)) => Some(limit),
            Some(Err(_)) | None => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    "`limit` must be a non-negative integer",
                );
            }
        },
        Ok(None) => None,
        Err(message) => return error(StatusCode::BAD_REQUEST, "bad_request", &message),
    };
    let Some(entries) = state.handle.history_tail(&id, limit.unwrap_or(usize::MAX)) else {
        return unknown_station();
    };
    let frames = commands::history_frames(&id, &entries);
    reply(StatusCode::OK, &json!({"station": id, "frames": frames}))
}

pub(super) async fn forget_station(
    State(state): State<ApiState>,
    id: Result<Path<String>, PathRejection>,
) -> Response {
    let id = match station_id(id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    match state.handle.forget_if_disconnected(&id) {
        None => unknown_station(),
        Some(false) => error(
            StatusCode::CONFLICT,
            "conflict",
            "station is connected; only a disconnected station can be forgotten",
        ),
        Some(true) => reply(StatusCode::OK, &json!({"station": id, "forgotten": true})),
    }
}
