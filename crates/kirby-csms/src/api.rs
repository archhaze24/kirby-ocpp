//! Local HTTP control API (`--api-listen`): JSON in and out, plain HTTP, optional bearer token.
//! It only uses [`CsmsHandle`] and the command code shared with the stdin handler.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::SecondsFormat;
use kirby_csms_core::{CsmsHandle, Direction};
use kirby_ocpp_core::text::is_unsafe_char;
use kirby_ocpp_tui::text::escape_json_unsafe;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;

use crate::commands;
use crate::headless::{frame_json, station_summary};

/// Largest request body accepted, bytes.
const MAX_BODY_BYTES: usize = 1024 * 1024;
/// How long the shutdown waits for open requests.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

#[derive(Clone)]
struct ApiState {
    handle: CsmsHandle,
    slots: Arc<Semaphore>,
    token: Option<Arc<str>>,
}

/// A running API server.
#[derive(Debug)]
pub struct Api {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

/// Binds the API listener (before the CSMS starts, so a busy port fails early).
pub async fn bind(addr: SocketAddr) -> io::Result<TcpListener> {
    TcpListener::bind(addr).await
}

/// Serves the API on `listener` until [`Api::finish`].
pub fn spawn(
    listener: TcpListener,
    handle: CsmsHandle,
    slots: Arc<Semaphore>,
    token: Option<String>,
) -> Api {
    let state = ApiState {
        handle,
        slots,
        token: token.map(Arc::from),
    };
    let app = router(state);
    let (stop, mut stopped) = watch::channel(false);
    let task = tokio::spawn(async move {
        let shutdown = async move {
            let _ = stopped.wait_for(|stop| *stop).await;
        };
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await;
    });
    Api { stop, task }
}

impl Api {
    /// Stops accepting, waits a bounded time for open requests, then drops the rest.
    pub async fn finish(mut self) {
        let _ = self.stop.send(true);
        if tokio::time::timeout(SHUTDOWN_GRACE, &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
        }
    }
}

fn router(state: ApiState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/stations", get(list_stations))
        .route("/v1/stations/{id}", get(get_station).delete(forget_station))
        .route("/v1/stations/{id}/history", get(history))
        .route("/v1/stations/{id}/calls", post(call))
        .fallback(|| async { error(StatusCode::NOT_FOUND, "not found") })
        .method_not_allowed_fallback(|| async {
            error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
        })
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state)
}

/// A JSON response; text from stations gets the same extra escaping as the `--json` lines.
fn reply(status: StatusCode, value: &Value) -> Response {
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

fn error(status: StatusCode, message: &str) -> Response {
    reply(status, &json!({"error": message}))
}

/// Equality that does not stop at the first differing byte.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for (index, byte) in b.iter().enumerate() {
        diff |= usize::from(byte ^ a.get(index).copied().unwrap_or(0));
    }
    diff == 0
}

/// With a token every request needs `Authorization: Bearer TOKEN`; without one, a request
/// carrying `Origin` (a browser page) is refused.
async fn auth(State(state): State<ApiState>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    if let Some(token) = &state.token {
        let given = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, given)| given.trim());
        if !given.is_some_and(|given| constant_time_eq(given.as_bytes(), token.as_bytes())) {
            let mut response = error(StatusCode::UNAUTHORIZED, "missing or wrong bearer token");
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            return response;
        }
    } else if headers.contains_key(header::ORIGIN) {
        return error(
            StatusCode::FORBIDDEN,
            "requests with an Origin header are refused without an API token",
        );
    }
    next.run(request).await
}

/// The decoded station id of the path, refused when it holds control or bidi characters.
fn station_id(id: Result<Path<String>, PathRejection>) -> Result<String, Box<Response>> {
    let Ok(Path(id)) = id else {
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "invalid station id in the path",
        )));
    };
    if id.chars().any(is_unsafe_char) {
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "station id has control or bidirectional characters",
        )));
    }
    Ok(id)
}

async fn health(State(state): State<ApiState>) -> Response {
    reply(
        StatusCode::OK,
        &json!({
            "ok": true,
            "version": env!("CARGO_PKG_VERSION"),
            "stations": state.handle.stations().len(),
        }),
    )
}

async fn list_stations(State(state): State<ApiState>) -> Response {
    reply(
        StatusCode::OK,
        &json!({"stations": commands::stations(&state.handle)}),
    )
}

async fn get_station(
    State(state): State<ApiState>,
    id: Result<Path<String>, PathRejection>,
) -> Response {
    let id = match station_id(id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(info) = state.handle.station(&id) else {
        return error(StatusCode::NOT_FOUND, "unknown station");
    };
    let station_state = state.handle.state(&id);
    let mut value = station_summary(&info, station_state.as_ref());
    if let (Some(object), Some(station_state)) = (value.as_object_mut(), &station_state) {
        object.insert(
            "state".to_owned(),
            serde_json::to_value(station_state).unwrap_or(Value::Null),
        );
    }
    reply(StatusCode::OK, &value)
}

async fn history(
    State(state): State<ApiState>,
    id: Result<Path<String>, PathRejection>,
    uri: Uri,
) -> Response {
    let id = match station_id(id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let limit = match query_limit(&uri) {
        Ok(limit) => limit,
        Err(message) => return error(StatusCode::BAD_REQUEST, message),
    };
    let Some(entries) = state.handle.history(&id) else {
        return error(StatusCode::NOT_FOUND, "unknown station");
    };
    let skip = limit.map_or(0, |limit| entries.len().saturating_sub(limit));
    let frames: Vec<Value> = entries
        .iter()
        .skip(skip)
        .map(|entry| {
            let mut value = frame_json(&entry.frame);
            if let Some(object) = value.as_object_mut() {
                object.insert("type".to_owned(), "frame".into());
                object.insert(
                    "ts".to_owned(),
                    entry.ts.to_rfc3339_opts(SecondsFormat::Millis, true).into(),
                );
                object.insert("station".to_owned(), id.clone().into());
                object.insert(
                    "direction".to_owned(),
                    match entry.direction {
                        Direction::In => "in",
                        Direction::Out => "out",
                    }
                    .into(),
                );
            }
            value
        })
        .collect();
    reply(StatusCode::OK, &json!({"station": id, "frames": frames}))
}

/// `limit=N` of the query string.
fn query_limit(uri: &Uri) -> Result<Option<usize>, &'static str> {
    let Some(query) = uri.query() else {
        return Ok(None);
    };
    let mut limit = None;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        match pair.split_once('=') {
            Some(("limit", value)) => {
                limit = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| "`limit` must be a non-negative integer")?,
                );
            }
            _ => return Err("unknown query parameter; supported: limit"),
        }
    }
    Ok(limit)
}

async fn forget_station(
    State(state): State<ApiState>,
    id: Result<Path<String>, PathRejection>,
) -> Response {
    let id = match station_id(id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    match state.handle.station(&id) {
        None => error(StatusCode::NOT_FOUND, "unknown station"),
        Some(info) if info.connected => error(
            StatusCode::CONFLICT,
            "station is connected; only a disconnected station can be forgotten",
        ),
        Some(_) => {
            state.handle.forget(&id);
            reply(StatusCode::OK, &json!({"station": id, "forgotten": true}))
        }
    }
}

async fn call(
    State(state): State<ApiState>,
    id: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Response {
    let id = match station_id(id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let too_large = || {
        error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body is larger than 1 MiB",
        )
    };
    let declared = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if declared.is_some_and(|length| length > MAX_BODY_BYTES) {
        return too_large();
    }
    let bytes = match axum::body::to_bytes(request.into_body(), MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(e) if e.to_string().contains("length limit") => return too_large(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "cannot read the request body"),
    };
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(body) => body,
        Err(e) => return error(StatusCode::BAD_REQUEST, &format!("invalid JSON: {e}")),
    };
    let Some(object) = body.as_object() else {
        return error(StatusCode::BAD_REQUEST, "the body must be a JSON object");
    };
    let Some(action) = object
        .get("action")
        .and_then(Value::as_str)
        .filter(|action| !action.is_empty())
    else {
        return error(StatusCode::BAD_REQUEST, "needs a string field `action`");
    };
    let payload = object.get("payload").cloned().unwrap_or_else(|| json!({}));
    let timeout = match commands::parse_timeout(object.get("timeout_ms")) {
        Ok(timeout) => timeout,
        Err(message) => return error(StatusCode::BAD_REQUEST, message),
    };
    let Ok(_permit) = Arc::clone(&state.slots).try_acquire_owned() else {
        return error(StatusCode::TOO_MANY_REQUESTS, "too many commands in flight");
    };
    let answer = commands::call(&state.handle, &id, action, payload, timeout).await;
    let status = if answer.missing_station {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::OK
    };
    reply(status, &answer.value)
}
