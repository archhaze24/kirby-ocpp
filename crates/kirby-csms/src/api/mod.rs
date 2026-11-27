//! Local HTTP control API (`--api-listen`): JSON in and out, plain HTTP or HTTPS
//! (`--api-tls-cert`), optional bearer token.
//! It only uses [`CsmsHandle`] and the command code shared with the stdin handler.
//!
//! Layout: `server` (accept loop, socket limits), `limits` (constants), `auth`, `meta` (route
//! table, index, common headers), `error`/`json` (response and request helpers),
//! `routes_*` (handlers) and `sse` (event stream).

mod auth;
mod error;
mod event_log;
mod export;
mod json;
mod limits;
mod meta;
mod metrics;
mod routes_actions;
mod routes_calls;
mod routes_history;
mod routes_rules;
mod routes_stations;
mod server;
mod sse;
#[cfg(test)]
mod tests;
mod web;

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::routing::{delete, get, post, put};
use kirby_csms_core::{CsmsEvent, CsmsHandle};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, broadcast, watch};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

/// The rustls server configuration of the API (see `--api-tls-cert`).
pub type TlsConfig = tokio_rustls::rustls::ServerConfig;

use self::auth::auth;
use self::error::error;
use self::event_log::{EventLog, spawn_pump};
use self::limits::{Limits, MAX_CONNECTIONS, MAX_STREAMS};
use self::meta::{finish_response, health, index, options};
use self::metrics::{Metrics, metrics};
use self::routes_actions::actions;
use self::routes_calls::{bulk, call};
use self::routes_history::{history, transactions};
use self::routes_rules::{
    clear_faults, delete_rule, delete_station_rule, faults_remaining, get_station_rule,
    inject_fault, list_rules, list_station_rules, put_rule, put_station_rule, remove_fault,
};
use self::routes_stations::{forget_station, get_station, list_stations};
use self::server::serve;
use self::sse::events;

#[derive(Clone)]
struct ApiState {
    handle: CsmsHandle,
    commands: crate::commands::CommandLimits,
    /// Bytes of request bodies that may be buffered at once, see [`limits::BODY_READ_BUDGET`].
    body_reads: Arc<Semaphore>,
    token: Option<Arc<str>>,
    streams: Arc<Semaphore>,
    /// The address the API listens on.
    bound: IpAddr,
    /// Whether the web UI is served (`--web`).
    web: bool,
    /// Whether the listener serves HTTPS.
    https: bool,
    /// How long an event stream lasts.
    stream_max_age: Duration,
    /// The events of `GET /v1/events`, with their ids.
    log: Arc<EventLog>,
    /// Counters served by `GET /v1/metrics`.
    metrics: Arc<Metrics>,
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

/// How the API authenticates, encrypts and what it serves besides `/v1`.
#[derive(Default)]
pub struct Access {
    /// The bearer token every request needs.
    pub token: Option<String>,
    /// With a configuration the listener serves HTTPS (HTTP/1.1 only), otherwise plain HTTP.
    pub tls: Option<Arc<TlsConfig>>,
    /// Serve the web UI (see `web`).
    pub web: bool,
}

/// Serves the API on `listener` until [`Api::finish`].
/// With `tls` the listener serves HTTPS (HTTP/1.1 only), otherwise plain HTTP.
#[cfg(test)]
pub fn spawn(
    listener: TcpListener,
    handle: CsmsHandle,
    commands: crate::commands::CommandLimits,
    token: Option<String>,
    tls: Option<Arc<TlsConfig>>,
) -> Api {
    let events = handle.subscribe();
    let access = Access {
        token,
        tls,
        web: false,
    };
    spawn_with_events(listener, handle, commands, access, events)
}

/// Serves the API with an event subscription taken before the first station was accepted.
pub fn spawn_with_events(
    listener: TcpListener,
    handle: CsmsHandle,
    commands: crate::commands::CommandLimits,
    access: Access,
    events: broadcast::Receiver<CsmsEvent>,
) -> Api {
    spawn_inner(listener, handle, commands, Limits::DEFAULT, access, events)
}

#[cfg(test)]
fn spawn_with(
    listener: TcpListener,
    handle: CsmsHandle,
    slots: Arc<Semaphore>,
    token: Option<String>,
    limits: Limits,
) -> Api {
    let commands = crate::commands::CommandLimits {
        slots,
        payload: Arc::new(Semaphore::new(limits.payload_budget)),
    };
    let events = handle.subscribe();
    let access = Access {
        token,
        ..Access::default()
    };
    spawn_inner(listener, handle, commands, limits, access, events)
}

fn spawn_inner(
    listener: TcpListener,
    handle: CsmsHandle,
    commands: crate::commands::CommandLimits,
    limits: Limits,
    access: Access,
    events: broadcast::Receiver<CsmsEvent>,
) -> Api {
    let Access { token, tls, web } = access;
    let https = tls.is_some();
    let tls = tls.map(|config| {
        let mut config = (*config).clone();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        TlsAcceptor::from(Arc::new(config))
    });
    let (stop, stopped) = watch::channel(false);
    let bound = listener
        .local_addr()
        .map_or(IpAddr::V4(Ipv4Addr::LOCALHOST), |addr| addr.ip());
    #[cfg(test)]
    let body_read_budget = limits.body_read_budget;
    #[cfg(not(test))]
    let body_read_budget = limits::BODY_READ_BUDGET;
    let state = ApiState {
        handle,
        commands,
        body_reads: Arc::new(Semaphore::new(body_read_budget)),
        token: token.map(Arc::from),
        streams: Arc::new(Semaphore::new(MAX_STREAMS)),
        bound,
        web,
        https,
        stream_max_age: limits.stream_max_age,
        log: EventLog::new(unix_micros()),
        metrics: Metrics::new(),
    };
    spawn_pump(
        &state.handle,
        events,
        Arc::clone(&state.log),
        Arc::clone(&state.metrics),
        stopped.clone(),
    );
    let app = router(state);
    let task = tokio::spawn(serve(
        listener,
        app,
        Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        limits,
        tls,
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
    // each route's method router, which sets its own `Allow` after them. The response counter
    // is the outermost layer, so it also counts the `401`, `403` and `404` answers.
    let counters = Arc::clone(&state.metrics);
    let web = state.web;
    let mut routes = Router::new()
        .route("/v1", get(index))
        .route("/v1/health", get(health))
        .route("/v1/stations", get(list_stations))
        .route("/v1/stations/{id}", get(get_station).delete(forget_station))
        .route("/v1/stations/{id}/history", get(history))
        .route("/v1/transactions", get(transactions))
        .route("/v1/metrics", get(metrics))
        .route("/v1/actions", get(actions))
        .route("/v1/calls", post(bulk))
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
        .route("/v1/faults/{id}", delete(remove_fault));
    // The web UI is not part of `/v1`: its pages are not in the route table or the OpenAPI
    // document. Without `--web` these paths stay unknown (`404`).
    if web {
        for path in web::paths() {
            routes = routes.route(path, get(web::asset));
        }
    }
    let routes = routes
        .fallback(|| async { error(StatusCode::NOT_FOUND, "not_found", "not found") })
        .method_not_allowed_fallback(|| async {
            error(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "method not allowed",
            )
        })
        .layer(middleware::from_fn_with_state(web, options))
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state);
    Router::new()
        .fallback_service(routes)
        .layer(middleware::from_fn_with_state(web, finish_response))
        .layer(middleware::from_fn(move |request: Request, next: Next| {
            let counters = Arc::clone(&counters);
            async move {
                let response = next.run(request).await;
                counters.count_response(response.status());
                response
            }
        }))
}

/// The time in microseconds since the Unix epoch: the seed of the event ids, so that an id
/// from an earlier run is smaller than every id of this one.
fn unix_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| u64::try_from(elapsed.as_micros()).unwrap_or(0))
}
