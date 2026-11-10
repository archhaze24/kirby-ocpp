//! The route table, the index and health endpoints, and the headers common to every
//! response (`Allow`, `Connection`, `OPTIONS`).

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::ApiState;
use super::error::reply;

/// The version of the API contract (the `/v1` prefix).
const API_VERSION: u32 = 1;

/// Every route: path pattern and the methods its handlers answer (`HEAD` for a `GET` route and
/// `OPTIONS` are implied). The index and the `OPTIONS` answers read this table.
pub(super) const ROUTES: &[(&str, &[&str])] = &[
    ("/v1", &["GET"]),
    ("/v1/health", &["GET"]),
    ("/v1/stations", &["GET"]),
    ("/v1/stations/{id}", &["GET", "DELETE"]),
    ("/v1/stations/{id}/history", &["GET"]),
    ("/v1/transactions", &["GET"]),
    ("/v1/metrics", &["GET"]),
    ("/v1/calls", &["POST"]),
    ("/v1/stations/{id}/calls", &["POST"]),
    ("/v1/stations/{id}/rules", &["GET"]),
    (
        "/v1/stations/{id}/rules/{action}",
        &["GET", "PUT", "DELETE"],
    ),
    ("/v1/events", &["GET"]),
    ("/v1/rules", &["GET"]),
    ("/v1/rules/{action}", &["PUT", "DELETE"]),
    ("/v1/faults", &["GET", "POST", "DELETE"]),
    ("/v1/faults/{id}", &["DELETE"]),
];

/// The methods of the route that `path` matches (`{name}` matches one non-empty segment).
fn route_methods(path: &str) -> Option<&'static [&'static str]> {
    ROUTES.iter().find_map(|(pattern, methods)| {
        let mut want = pattern.split('/');
        let mut have = path.split('/');
        loop {
            match (want.next(), have.next()) {
                (None, None) => return Some(*methods),
                (Some(w), Some(h)) if w.starts_with('{') && !h.is_empty() => {}
                (Some(w), Some(h)) if w == h => {}
                _ => return None,
            }
        }
    })
}

/// The `Allow` value of a known path: its methods plus `HEAD` (for a `GET` route) and
/// `OPTIONS`, sorted, joined by a comma and a space.
fn allow_value(path: &str) -> Option<HeaderValue> {
    let methods = route_methods(path)?;
    let mut allow: Vec<&str> = methods.to_vec();
    if methods.contains(&"GET") {
        allow.push("HEAD");
    }
    allow.push("OPTIONS");
    allow.sort_unstable();
    HeaderValue::from_str(&allow.join(", ")).ok()
}

/// `OPTIONS` on a known path: `204` (the `Allow` header is set by [`finish_response`]) and no
/// CORS headers (browsers are refused by design, see docs/api.md). It runs inside [`auth`], so
/// the token and `Host` rules apply.
///
/// [`auth`]: super::auth::auth
pub(super) async fn options(request: Request, next: Next) -> Response {
    if request.method() == Method::OPTIONS && route_methods(request.uri().path()).is_some() {
        return StatusCode::NO_CONTENT.into_response();
    }
    next.run(request).await
}

/// The one place that shapes headers common to every response: `Connection: close` (so one
/// connection serves one request) and `Allow`. Any `Allow` the router set is dropped; it is
/// written back only on a `405` and on the `204` answer to `OPTIONS`, derived from [`ROUTES`].
pub(super) async fn finish_response(request: Request, next: Next) -> Response {
    let options = request.method() == Method::OPTIONS;
    let allow = allow_value(request.uri().path());
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CONNECTION, HeaderValue::from_static("close"));
    headers.remove(header::ALLOW);
    let status = response.status();
    if (status == StatusCode::METHOD_NOT_ALLOWED || (options && status == StatusCode::NO_CONTENT))
        && let Some(allow) = allow
    {
        response.headers_mut().insert(header::ALLOW, allow);
    }
    response
}

pub(super) async fn index() -> Response {
    let resources: Vec<Value> = ROUTES
        .iter()
        .map(|(path, methods)| json!({"path": path, "methods": methods}))
        .collect();
    reply(
        StatusCode::OK,
        &json!({
            "api_version": API_VERSION,
            "version": env!("CARGO_PKG_VERSION"),
            "resources": resources,
        }),
    )
}

pub(super) async fn health(State(state): State<ApiState>) -> Response {
    reply(
        StatusCode::OK,
        &json!({
            "ok": true,
            "api_version": API_VERSION,
            "version": env!("CARGO_PKG_VERSION"),
            "stations": state.handle.stations().len(),
        }),
    )
}
