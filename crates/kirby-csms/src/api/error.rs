//! JSON responses and the error shape.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use kirby_ocpp_tui::text::escape_json_unsafe;
use serde_json::{Value, json};

/// A JSON response; text from stations gets the same extra escaping as the `--json` lines.
pub(super) fn reply(status: StatusCode, value: &Value) -> Response {
    let body = escape_json_unsafe(&value.to_string());
    (
        status,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        body,
    )
        .into_response()
}

/// The error body of every API error: `{"error": {"code": "snake_case", "message": "text"}}`.
/// The code is stable; the message is for people.
pub(super) fn error(status: StatusCode, code: &str, message: &str) -> Response {
    reply(
        status,
        &json!({"error": {"code": code, "message": message}}),
    )
}

pub(super) fn unknown_station() -> Response {
    error(StatusCode::NOT_FOUND, "unknown_station", "unknown station")
}
