//! Local HTTP control API (`--api-listen`): JSON in and out, plain HTTP, optional bearer token.
//! It only uses [`CsmsHandle`] and the command code shared with the stdin handler.
//!
//! Layout: `server` (accept loop, socket limits), `limits` (constants), `auth`, `meta` (route
//! table, index, common headers), `error`/`json` (response and request helpers),
//! `routes_*` (handlers) and `sse` (event stream).

mod auth;
mod error;
mod json;
mod limits;
mod meta;
mod routes_calls;
mod routes_rules;
mod routes_stations;
mod server;
mod sse;
#[cfg(test)]
mod tests;

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::middleware;
use axum::routing::{delete, get, post, put};
use kirby_csms_core::CsmsHandle;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;

use self::auth::auth;
use self::error::error;
use self::limits::{Limits, MAX_CONNECTIONS, MAX_STREAMS};
use self::meta::{finish_response, health, index, options};
use self::routes_calls::call;
use self::routes_rules::{
    clear_faults, delete_rule, delete_station_rule, faults_remaining, get_station_rule,
    inject_fault, list_rules, list_station_rules, put_rule, put_station_rule, remove_fault,
};
use self::routes_stations::{forget_station, get_station, history, list_stations};
use self::server::serve;
use self::sse::events;

#[derive(Clone)]
struct ApiState {
    handle: CsmsHandle,
    slots: Arc<Semaphore>,
    token: Option<Arc<str>>,
    streams: Arc<Semaphore>,
    stopped: watch::Receiver<bool>,
    /// The address the API listens on.
    bound: IpAddr,
    /// Request-body bytes still available to in-flight calls, see [`limits::PAYLOAD_BUDGET`].
    payload: Arc<Semaphore>,
    /// How long an event stream lasts.
    stream_max_age: Duration,
    /// Source of the SSE `id:` values: one counter for the whole server.
    event_ids: Arc<AtomicU64>,
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
    spawn_with(listener, handle, slots, token, Limits::DEFAULT)
}

fn spawn_with(
    listener: TcpListener,
    handle: CsmsHandle,
    slots: Arc<Semaphore>,
    token: Option<String>,
    limits: Limits,
) -> Api {
    let (stop, stopped) = watch::channel(false);
    let bound = listener
        .local_addr()
        .map_or(IpAddr::V4(Ipv4Addr::LOCALHOST), |addr| addr.ip());
    let state = ApiState {
        handle,
        slots,
        token: token.map(Arc::from),
        streams: Arc::new(Semaphore::new(MAX_STREAMS)),
        stopped: stopped.clone(),
        bound,
        payload: Arc::new(Semaphore::new(limits.payload_budget)),
        stream_max_age: limits.stream_max_age,
        event_ids: Arc::new(AtomicU64::new(0)),
    };
    let app = router(state);
    let task = tokio::spawn(serve(
        listener,
        app,
        Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        limits,
        stopped,
    ));
    Api { stop, task }
}

impl Api {
    /// Stops accepting and closes the listener, waits [`SHUTDOWN_GRACE`] for open requests,
    /// then drops (closes) every connection still open. No connection outlives the call.
    ///
    /// [`SHUTDOWN_GRACE`]: limits::SHUTDOWN_GRACE
    pub async fn finish(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }
}

fn router(state: ApiState) -> Router {
    // `finish_response` wraps the whole router: layers added with `Router::layer` sit inside
    // each route's method router, which sets its own `Allow` after them.
    let routes = Router::new()
        .route("/v1", get(index))
        .route("/v1/health", get(health))
        .route("/v1/stations", get(list_stations))
        .route("/v1/stations/{id}", get(get_station).delete(forget_station))
        .route("/v1/stations/{id}/history", get(history))
        .route("/v1/stations/{id}/calls", post(call))
        .route("/v1/events", get(events))
        .route("/v1/stations/{id}/rules", get(list_station_rules))
        .route(
            "/v1/stations/{id}/rules/{action}",
            get(get_station_rule)
                .put(put_station_rule)
                .delete(delete_station_rule),
        )
        .route("/v1/rules", get(list_rules))
        .route("/v1/rules/{action}", put(put_rule).delete(delete_rule))
        .route(
            "/v1/faults",
            get(faults_remaining)
                .post(inject_fault)
                .delete(clear_faults),
        )
        .route("/v1/faults/{id}", delete(remove_fault))
        .fallback(|| async { error(StatusCode::NOT_FOUND, "not_found", "not found") })
        .method_not_allowed_fallback(|| async {
            error(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "method not allowed",
            )
        })
        .layer(middleware::from_fn(options))
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state);
    Router::new()
        .fallback_service(routes)
        .layer(middleware::from_fn(finish_response))
}
