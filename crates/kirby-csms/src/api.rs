//! Local HTTP control API (`--api-listen`): JSON in and out, plain HTTP, optional bearer token.
//! It only uses [`CsmsHandle`] and the command code shared with the stdin handler.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::rejection::PathRejection;
use axum::extract::{Extension, Path, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use chrono::SecondsFormat;
use futures_util::{StreamExt, stream};
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use kirby_csms_core::{
    ACTIONS_PROTOCOL, CsmsError, CsmsEvent, CsmsHandle, Direction, Reply, is_charge_point_action,
};
use kirby_ocpp_core::text::is_unsafe_char;
use kirby_ocpp_tui::text::escape_json_unsafe;
use serde::Deserialize;
use serde_json::{Value, json};
use socket2::{SockRef, TcpKeepalive};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Instant, Sleep};

use crate::commands::{self, Outcome};
use crate::headless::{Renderer, frame_json, station_summary};

/// Largest request body accepted, bytes.
const MAX_BODY_BYTES: usize = 1024 * 1024;
/// How long the shutdown waits for open requests.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
/// Most event streams open at once.
const MAX_STREAMS: usize = 16;
/// Interval of the SSE keep-alive comments.
const KEEP_ALIVE: Duration = Duration::from_secs(15);
/// Most connections open at once; further ones are closed at once.
const MAX_CONNECTIONS: usize = 512;
/// Time a client gets to send the complete request head after connecting (hyper's
/// `header_read_timeout`, which only a real request line and a complete head satisfy).
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
/// A write that makes no progress for this long ends the connection.
const WRITE_STALL: Duration = Duration::from_secs(30);
/// Most time a response (an event stream excepted) may take to be written, from its first
/// byte; a client that drains a byte at a time cannot hold a connection longer.
const RESPONSE_DEADLINE: Duration = Duration::from_secs(60);
/// An event stream ends after this long; the last event tells the client to reconnect.
const STREAM_MAX_AGE: Duration = Duration::from_secs(60 * 60);
/// After a response the connection waits at most this long, and reads at most
/// [`DRAIN_BYTES`], for the client to finish sending the request before it closes. Closing
/// with unread request data would reset the connection and could destroy an early error
/// response (`401`, `403`, `413`) the client has not read yet.
const DRAIN_TIME: Duration = Duration::from_millis(100);
const DRAIN_BYTES: usize = 64 * 1024;
/// Most request-body bytes held by `POST .../calls` requests at once (the body of each, as
/// received, while its call runs); further requests get `429`.
const PAYLOAD_BUDGET: usize = 32 * 1024 * 1024;
/// Time a client gets to send the whole request body.
const BODY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct ApiState {
    handle: CsmsHandle,
    slots: Arc<Semaphore>,
    token: Option<Arc<str>>,
    streams: Arc<Semaphore>,
    stopped: watch::Receiver<bool>,
    /// The address the API listens on.
    bound: IpAddr,
    /// Request-body bytes still available to in-flight calls, see [`PAYLOAD_BUDGET`].
    payload: Arc<Semaphore>,
    /// How long an event stream lasts.
    stream_max_age: Duration,
    /// Source of the SSE `id:` values: one counter for the whole server.
    event_ids: Arc<AtomicU64>,
}

/// Timing limits of the server; tests use short ones.
#[derive(Debug, Clone, Copy)]
struct Limits {
    write_stall: Duration,
    response_deadline: Duration,
    stream_max_age: Duration,
    payload_budget: usize,
}

impl Limits {
    const DEFAULT: Self = Self {
        write_stall: WRITE_STALL,
        response_deadline: RESPONSE_DEADLINE,
        stream_max_age: STREAM_MAX_AGE,
        payload_budget: PAYLOAD_BUDGET,
    };
}

/// Set by the handler of a response that is meant to last (an event stream): the connection
/// is then exempt from [`RESPONSE_DEADLINE`].
#[derive(Clone)]
struct Streaming(Arc<AtomicBool>);

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

/// An accepted connection's socket. It gives up when
/// - a write makes no progress for `after` (a client that stopped reading),
/// - the response takes longer than `total` from its first write, unless the handler marked
///   the connection as [`Streaming`],
///
/// and at the end it drains the client's unread request data for a moment (see
/// [`DRAIN_TIME`]) before the socket is closed.
struct StallIo<T> {
    inner: T,
    /// How long a write may make no progress.
    after: Duration,
    /// Armed while a write is pending; cleared by any write progress.
    stall: Option<Pin<Box<Sleep>>>,
    /// How long a response may take to be written.
    total: Duration,
    /// Armed by the first write.
    deadline: Option<Pin<Box<Sleep>>>,
    streaming: Arc<AtomicBool>,
    /// The write side was shut down; the drain is running.
    shut: bool,
    drain: Option<Pin<Box<Sleep>>>,
    drained: usize,
}

impl<T> StallIo<T> {
    fn new(inner: T, limits: Limits, streaming: Arc<AtomicBool>) -> Self {
        Self {
            inner,
            after: limits.write_stall,
            stall: None,
            total: limits.response_deadline,
            deadline: None,
            streaming,
            shut: false,
            drain: None,
            drained: 0,
        }
    }

    /// Applies the stall and deadline rules to the result of a write-side poll.
    fn guard<R>(
        &mut self,
        cx: &mut Context<'_>,
        polled: Poll<io::Result<R>>,
    ) -> Poll<io::Result<R>> {
        if !self.streaming.load(Ordering::Acquire) {
            let total = self.total;
            let deadline = self
                .deadline
                .get_or_insert_with(|| Box::pin(tokio::time::sleep(total)));
            if deadline.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(io::ErrorKind::TimedOut.into()));
            }
        }
        match polled {
            Poll::Pending => {
                let sleep = self
                    .stall
                    .get_or_insert_with(|| Box::pin(tokio::time::sleep(self.after)));
                if sleep.as_mut().poll(cx).is_ready() {
                    Poll::Ready(Err(io::ErrorKind::TimedOut.into()))
                } else {
                    Poll::Pending
                }
            }
            ready @ Poll::Ready(_) => {
                self.stall = None;
                ready
            }
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for StallIo<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> AsyncWrite for StallIo<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_write(cx, buf);
        this.guard(cx, polled)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_flush(cx);
        this.guard(cx, polled)
    }

    /// Shuts the write side down, then reads and discards what the client still sends for up
    /// to [`DRAIN_TIME`] and [`DRAIN_BYTES`] (or until it closes).
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if !this.shut {
            let polled = Pin::new(&mut this.inner).poll_shutdown(cx);
            match this.guard(cx, polled) {
                Poll::Ready(Ok(())) => this.shut = true,
                other => return other,
            }
        }
        let mut scratch = [0_u8; 4096];
        while this.drained < DRAIN_BYTES {
            let mut buf = ReadBuf::new(&mut scratch);
            match Pin::new(&mut this.inner).poll_read(cx, &mut buf) {
                Poll::Ready(Ok(())) if !buf.filled().is_empty() => {
                    this.drained += buf.filled().len();
                }
                Poll::Ready(_) => return Poll::Ready(Ok(())),
                Poll::Pending => break,
            }
        }
        if this.drained >= DRAIN_BYTES {
            return Poll::Ready(Ok(()));
        }
        let drain = this
            .drain
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(DRAIN_TIME)));
        if drain.as_mut().poll(cx).is_ready() {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }
}

/// Turns on TCP keepalive (first probe after 30 s idle, then every 10 s, 3 probes) so that a
/// vanished peer (half-open connection) is detected and its slot freed. Best effort.
fn enable_keepalive(stream: &TcpStream) {
    let params = TcpKeepalive::new().with_time(Duration::from_secs(30));
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    let params = params.with_interval(Duration::from_secs(10));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let params = params.with_retries(3);
    let _ = SockRef::from(stream).set_tcp_keepalive(&params);
}

/// Accepts connections until `stopped`, one task each (tracked in a [`JoinSet`]). A connection
/// holds a slot of the cap, gets [`HEAD_TIMEOUT`] from hyper for the request head and carries
/// one request. After the stop signal the listener is dropped at once, open requests get
/// [`SHUTDOWN_GRACE`], then every connection is aborted and awaited: nothing outlives this
/// future.
async fn serve(
    listener: TcpListener,
    app: Router,
    slots: Arc<Semaphore>,
    limits: Limits,
    mut stopped: watch::Receiver<bool>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            () = wait_stopped(&mut stopped) => break,
            Some(_) = connections.join_next() => {}
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    // Out of descriptors or similar: do not spin (but do not miss a stop).
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_millis(50)) => continue,
                        () = wait_stopped(&mut stopped) => break,
                    }
                };
                // Over the cap the socket is dropped (closed) at once.
                let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
                    continue;
                };
                enable_keepalive(&stream);
                let streaming = Arc::new(AtomicBool::new(false));
                let io = TokioIo::new(StallIo::new(stream, limits, Arc::clone(&streaming)));
                let service = TowerToHyperService::new(
                    app.clone().layer(Extension(Streaming(streaming))),
                );
                let mut stop = stopped.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    let connection = http1::Builder::new()
                        .timer(TokioTimer::new())
                        .header_read_timeout(HEAD_TIMEOUT)
                        .keep_alive(false)
                        .serve_connection(io, service);
                    tokio::pin!(connection);
                    tokio::select! {
                        _ = connection.as_mut() => {}
                        () = wait_stopped(&mut stop) => {
                            connection.as_mut().graceful_shutdown();
                            let _ = connection.await;
                        }
                    }
                });
            }
        }
    }
    drop(listener);
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    connections.shutdown().await;
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
        .route("/v1/rules", get(list_rules))
        .route("/v1/rules/{action}", put(put_rule).delete(delete_rule))
        .route(
            "/v1/faults",
            get(faults_remaining)
                .post(inject_fault)
                .delete(clear_faults),
        )
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

/// The error body of every API error: `{"error": {"code": "snake_case", "message": "text"}}`.
/// The code is stable; the message is for people.
fn error(status: StatusCode, code: &str, message: &str) -> Response {
    reply(
        status,
        &json!({"error": {"code": code, "message": message}}),
    )
}

/// The version of the API contract (the `/v1` prefix).
const API_VERSION: u32 = 1;

/// Every route: path pattern and the methods its handlers answer (`HEAD` for a `GET` route and
/// `OPTIONS` are implied). The index and the `OPTIONS` answers read this table.
const ROUTES: &[(&str, &[&str])] = &[
    ("/v1", &["GET"]),
    ("/v1/health", &["GET"]),
    ("/v1/stations", &["GET"]),
    ("/v1/stations/{id}", &["GET", "DELETE"]),
    ("/v1/stations/{id}/history", &["GET"]),
    ("/v1/stations/{id}/calls", &["POST"]),
    ("/v1/events", &["GET"]),
    ("/v1/rules", &["GET"]),
    ("/v1/rules/{action}", &["PUT", "DELETE"]),
    ("/v1/faults", &["GET", "POST", "DELETE"]),
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
async fn options(request: Request, next: Next) -> Response {
    if request.method() == Method::OPTIONS && route_methods(request.uri().path()).is_some() {
        return StatusCode::NO_CONTENT.into_response();
    }
    next.run(request).await
}

/// Turns the integer `id` of every transaction object into a string: transaction ids are
/// integers in OCPP 1.6 and strings in 2.x, and the API serves strings for both. Applies to
/// a station object (`active_transactions`, `state.transactions`) and a `state` event.
fn stringify_transaction_ids(value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if let Some(Value::Object(state)) = object.get_mut("state") {
        state.get_mut("transactions").map(stringify_list);
    }
    object.get_mut("active_transactions").map(stringify_list);
}

fn stringify_list(list: &mut Value) {
    let Value::Array(items) = list else {
        return;
    };
    for item in items {
        if let Some(Value::Number(id)) = item.get("id").cloned()
            && let Some(object) = item.as_object_mut()
        {
            object.insert("id".to_owned(), id.to_string().into());
        }
    }
}

/// Equality that does not stop at the first differing byte.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for (index, byte) in b.iter().enumerate() {
        diff |= usize::from(byte ^ a.get(index).copied().unwrap_or(0));
    }
    diff == 0
}

/// The one place that shapes headers common to every response: `Connection: close` (so one
/// connection serves one request) and `Allow`. Any `Allow` the router set is dropped; it is
/// written back only on a `405` and on the `204` answer to `OPTIONS`, derived from [`ROUTES`].
async fn finish_response(request: Request, next: Next) -> Response {
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

/// Whether a `Host` header value names this machine: a loopback IP literal (`127.0.0.1`,
/// `[::1]`, `[0:0:0:0:0:0:0:1]`), the address the API is bound to, or `localhost` (with an
/// optional trailing dot), each with an optional port. Anything else (a DNS name that
/// rebinds to 127.0.0.1) is not.
fn is_loopback_host(value: &str, bound: IpAddr) -> bool {
    let known = |ip: IpAddr| ip.is_loopback() || (ip == bound && !ip.is_unspecified());
    let (host_ok, port) = if let Some(rest) = value.strip_prefix('[') {
        let Some((ip, tail)) = rest.split_once(']') else {
            return false;
        };
        (
            ip.parse::<Ipv6Addr>().is_ok_and(|ip| known(IpAddr::V6(ip))),
            tail,
        )
    } else {
        let (host, port) = value.split_at(value.find(':').unwrap_or(value.len()));
        let named =
            host.eq_ignore_ascii_case("localhost") || host.eq_ignore_ascii_case("localhost.");
        (
            named
                || host
                    .parse::<Ipv4Addr>()
                    .is_ok_and(|ip| known(IpAddr::V4(ip))),
            port,
        )
    };
    host_ok && (port.is_empty() || valid_port(port))
}

/// `:` and up to five digits.
fn valid_port(text: &str) -> bool {
    text.strip_prefix(':').is_some_and(|port| {
        (1..=5).contains(&port.len()) && port.bytes().all(|b| b.is_ascii_digit())
    })
}

/// With a token every request needs exactly one `Authorization: Bearer TOKEN`. Without one the
/// API trusts the loopback address only, so a browser must not be able to reach it: a request
/// with an `Origin` header, a `Sec-Fetch-Site: cross-site` header or a `Host` that is not a
/// loopback name (DNS rebinding) is refused.
async fn auth(State(state): State<ApiState>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    if headers.get_all(header::HOST).iter().nth(1).is_some() {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "more than one Host header",
        );
    }
    if let Some(token) = &state.token {
        let mut values = headers.get_all(header::AUTHORIZATION).iter();
        let given = match (values.next(), values.next()) {
            (Some(value), None) => value
                .to_str()
                .ok()
                .and_then(|value| value.split_once(' '))
                .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
                .map(|(_, given)| given),
            _ => None,
        };
        if !given.is_some_and(|given| constant_time_eq(given.as_bytes(), token.as_bytes())) {
            let mut response = error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "missing or wrong bearer token",
            );
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            return response;
        }
    } else if headers.contains_key(header::ORIGIN) {
        return error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "requests with an Origin header are refused without an API token",
        );
    } else if headers
        .get("sec-fetch-site")
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"cross-site"))
    {
        return error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "cross-site requests are refused without an API token",
        );
    } else if headers.get(header::HOST).is_some_and(|host| {
        !host
            .to_str()
            .is_ok_and(|host| is_loopback_host(host, state.bound))
    }) {
        return error(
            StatusCode::FORBIDDEN,
            "forbidden",
            &format!(
                "without an API token the Host header must be 127.0.0.1, [::1], localhost \
                 (any loopback IP) or the API address {}, with an optional port",
                state.bound
            ),
        );
    }
    next.run(request).await
}

/// The decoded station id of the path, refused when it holds control or bidi characters.
fn station_id(id: Result<Path<String>, PathRejection>) -> Result<String, Box<Response>> {
    let Ok(Path(id)) = id else {
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "invalid station id in the path",
        )));
    };
    if id.chars().any(is_unsafe_char) {
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "station id has control or bidirectional characters",
        )));
    }
    Ok(id)
}

async fn index() -> Response {
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

async fn health(State(state): State<ApiState>) -> Response {
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

async fn list_stations(State(state): State<ApiState>) -> Response {
    let mut stations = commands::stations(&state.handle);
    if let Value::Array(list) = &mut stations {
        list.iter_mut().for_each(stringify_transaction_ids);
    }
    reply(StatusCode::OK, &json!({"stations": stations}))
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

fn unknown_station() -> Response {
    error(StatusCode::NOT_FOUND, "unknown_station", "unknown station")
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
    let frames: Vec<Value> = entries
        .iter()
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

async fn forget_station(
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

async fn call(
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

/// The request body (at most 1 MiB, read within 10 s) parsed as JSON.
async fn read_json(request: Request<Body>) -> Result<(Value, usize), Box<Response>> {
    let too_large = || {
        Box::new(error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "request body is larger than 1 MiB",
        ))
    };
    let declared = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if declared.is_some_and(|length| length > MAX_BODY_BYTES) {
        return Err(too_large());
    }
    let collect = async {
        let mut chunks = request.into_body().into_data_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|_| {
                Box::new(error(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    "cannot read the request body",
                ))
            })?;
            if bytes.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
                return Err(too_large());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    };
    let bytes = match tokio::time::timeout(BODY_TIMEOUT, collect).await {
        Ok(bytes) => bytes?,
        Err(_) => {
            return Err(Box::new(error(
                StatusCode::REQUEST_TIMEOUT,
                "request_timeout",
                "the request body did not arrive in 10 s",
            )));
        }
    };
    let length = bytes.len();
    serde_json::from_slice(&bytes)
        .map(|value| (value, length))
        .map_err(|e| {
            Box::new(error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                &format!("invalid JSON: {e}"),
            ))
        })
}

/// The station id of an event, if it names one.
fn event_station(event: &CsmsEvent) -> Option<&str> {
    match event {
        CsmsEvent::Connected { station }
        | CsmsEvent::Disconnected { station, .. }
        | CsmsEvent::Frame { station, .. }
        | CsmsEvent::Malformed { station, .. }
        | CsmsEvent::Forgotten { station }
        | CsmsEvent::StateChanged { station } => Some(station),
        CsmsEvent::AuthFailed { station, .. } => station.as_deref(),
    }
}

/// The value of the query parameter `name`, percent-decoded (`%XX` only; `+` stays a plus).
/// Any other parameter, and a repeated one, is an error.
fn query_param(uri: &Uri, name: &str) -> Result<Option<String>, String> {
    let Some(query) = uri.query() else {
        return Ok(None);
    };
    let mut found = None;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        match pair.split_once('=') {
            Some((key, _)) if key == name && found.is_some() => {
                return Err(format!("repeated query parameter `{name}`"));
            }
            Some((key, value)) if key == name => found = Some(percent_decode(value)),
            _ => return Err(format!("unknown query parameter; supported: {name}")),
        }
    }
    Ok(found)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        let hex = |offset: usize| {
            bytes
                .get(index + offset)
                .and_then(|digit| char::from(*digit).to_digit(16))
        };
        if byte == b'%'
            && let (Some(high), Some(low)) = (hex(1), hex(2))
        {
            out.push(u8::try_from(high * 16 + low).unwrap_or(b'?'));
            index += 3;
        } else {
            out.push(byte);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One SSE event: `id:` comes from the server-wide counter, `event:` is the `type` of the
/// JSON object in `data:`.
fn sse_event(ids: &AtomicU64, value: &Value) -> Event {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    Event::default()
        .id((ids.fetch_add(1, Ordering::Relaxed) + 1).to_string())
        .event(kind)
        .data(escape_json_unsafe(&value.to_string()))
}

struct StreamState {
    rx: broadcast::Receiver<CsmsEvent>,
    renderer: Renderer,
    filter: Option<String>,
    queue: std::collections::VecDeque<Event>,
    stopped: watch::Receiver<bool>,
    /// The stream ends then (see [`STREAM_MAX_AGE`]).
    ends_at: Instant,
    /// The server stopped: send what is queued and what the channel still holds, then end.
    ending: bool,
    ids: Arc<AtomicU64>,
    /// Held for the life of the stream: dropping the stream frees the slot.
    _permit: OwnedSemaphorePermit,
}

/// Resolves once the server stops.
async fn wait_stopped(stopped: &mut watch::Receiver<bool>) {
    let _ = stopped.wait_for(|stop| *stop).await;
}

impl StreamState {
    /// Queues the SSE messages of `event` when it passes the station filter.
    fn accept(&mut self, event: &CsmsEvent) {
        if self
            .filter
            .as_deref()
            .is_none_or(|id| event_station(event) == Some(id))
        {
            for (mut value, _) in self.renderer.render(event) {
                if value["type"] == "state" {
                    stringify_transaction_ids(&mut value);
                }
                self.queue.push_back(sse_event(&self.ids, &value));
            }
        }
    }

    /// Events were lost: say so, and forget the rendered state views so that the next state
    /// event is complete instead of a diff against a view the client never saw.
    fn lagged(&mut self, missed: u64) {
        self.renderer.reset();
        self.queue.push_back(sse_event(
            &self.ids,
            &json!({"type": "note", "text": format!("{missed} events were dropped")}),
        ));
    }
}

async fn events(
    State(state): State<ApiState>,
    Extension(Streaming(streaming)): Extension<Streaming>,
    uri: Uri,
) -> Response {
    let filter = match query_param(&uri, "station") {
        Ok(filter) => filter,
        Err(message) => return error(StatusCode::BAD_REQUEST, "bad_request", &message),
    };
    let Ok(permit) = Arc::clone(&state.streams).try_acquire_owned() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "too_many_streams",
            "too many event streams (at most 16)",
        );
    };
    // The response is meant to last: no total deadline for this connection.
    streaming.store(true, Ordering::Release);
    let init = StreamState {
        rx: state.handle.subscribe(),
        renderer: Renderer::new(&state.handle),
        filter,
        queue: std::collections::VecDeque::new(),
        stopped: state.stopped.clone(),
        ends_at: Instant::now() + state.stream_max_age,
        ending: false,
        ids: Arc::clone(&state.event_ids),
        _permit: permit,
    };
    let events = stream::unfold(init, |mut st| async move {
        loop {
            if let Some(event) = st.queue.pop_front() {
                return Some((Ok::<Event, std::convert::Infallible>(event), st));
            }
            if st.ending {
                return None;
            }
            let ends_at = st.ends_at;
            tokio::select! {
                () = tokio::time::sleep_until(ends_at) => {
                    st.queue.push_back(sse_event(&st.ids, &json!({
                        "type": "note",
                        "text": "the stream reached its maximum age; reconnect to continue",
                    })));
                    st.ending = true;
                }
                received = st.rx.recv() => match received {
                    Ok(event) => st.accept(&event),
                    Err(broadcast::error::RecvError::Lagged(missed)) => st.lagged(missed),
                    Err(broadcast::error::RecvError::Closed) => return None,
                },
                () = wait_stopped(&mut st.stopped) => {
                    st.ending = true;
                    loop {
                        match st.rx.try_recv() {
                            Ok(event) => st.accept(&event),
                            Err(broadcast::error::TryRecvError::Lagged(missed)) => st.lagged(missed),
                            Err(_) => break,
                        }
                    }
                }
            }
        }
    });
    Sse::new(events)
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE).text("keep-alive"))
        .into_response()
}

async fn list_rules(State(state): State<ApiState>) -> Response {
    reply(
        StatusCode::OK,
        &json!({"rules": state.handle.responder_actions()}),
    )
}

/// The path action of a rule, which must be an action a charge point can send.
fn rule_action(action: Result<Path<String>, PathRejection>) -> Result<String, Box<Response>> {
    match action {
        Ok(Path(action)) if is_charge_point_action(&action) => Ok(action),
        Ok(Path(action)) => Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "unsupported_action",
            &format!(
                "{} is not a known charge-point action for protocol {ACTIONS_PROTOCOL}",
                kirby_ocpp_tui::text::sanitize(&action)
            ),
        ))),
        Err(_) => Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "invalid action in the path",
        ))),
    }
}

async fn put_rule(
    State(state): State<ApiState>,
    action: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Response {
    let action = match rule_action(action) {
        Ok(action) => action,
        Err(response) => return *response,
    };
    let payload = match read_json(request).await {
        Ok((payload, _)) if payload.is_object() => payload,
        Ok(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "the body must be a JSON object",
            );
        }
        Err(response) => return *response,
    };
    state
        .handle
        .respond(&action, move |_| Reply::Result(payload.clone()));
    reply(StatusCode::OK, &json!({"action": action, "set": true}))
}

async fn delete_rule(
    State(state): State<ApiState>,
    action: Result<Path<String>, PathRejection>,
) -> Response {
    let action = match rule_action(action) {
        Ok(action) => action,
        Err(response) => return *response,
    };
    let existed = state.handle.clear_responder(&action);
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
}

fn one() -> usize {
    1
}

async fn inject_fault(State(state): State<ApiState>, request: Request<Body>) -> Response {
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
    if let Err(message) = state.handle.try_inject(commands::build_fault(
        &fault.action,
        fault.kind,
        fault.payload,
        fault.times,
    )) {
        return error(StatusCode::CONFLICT, "conflict", &message);
    }
    reply(
        StatusCode::OK,
        &json!({"injected": true, "remaining": state.handle.faults_remaining()}),
    )
}

async fn faults_remaining(State(state): State<ApiState>) -> Response {
    reply(
        StatusCode::OK,
        &json!({"remaining": state.handle.faults_remaining()}),
    )
}

async fn clear_faults(State(state): State<ApiState>) -> Response {
    state.handle.clear_faults();
    reply(StatusCode::OK, &json!({"remaining": 0}))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use kirby_csms_core::{CsmsConfig, CsmsServer};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use super::*;

    /// The paths of `router()` are exactly the paths of `ROUTES`: the index lists every route
    /// and no phantom one.
    #[test]
    fn routes_table_matches_the_router_definition() {
        let source = include_str!("api.rs");
        let start = source.find("fn router(").unwrap();
        let end = start + source[start..].find("\n}\n").unwrap();
        let mut paths: Vec<&str> = Vec::new();
        let mut rest = &source[start..end];
        while let Some(at) = rest.find("\"/v1") {
            let tail = &rest[at + 1..];
            let close = tail.find('"').unwrap();
            paths.push(&tail[..close]);
            rest = &tail[close..];
        }
        let mut table: Vec<&str> = ROUTES.iter().map(|(path, _)| *path).collect();
        paths.sort_unstable();
        table.sort_unstable();
        assert_eq!(paths, table);
    }

    /// No limit gets in the way.
    const LONG: Limits = Limits {
        write_stall: Duration::from_secs(600),
        response_deadline: Duration::from_secs(600),
        stream_max_age: Duration::from_secs(600),
        payload_budget: PAYLOAD_BUDGET,
    };

    /// A peer that takes one byte at a time, fast enough never to stall a write, cannot hold
    /// a response longer than the response deadline; a streaming connection is exempt.
    async fn slow_drain(streaming: bool) -> io::Result<()> {
        let (near, mut far) = tokio::io::duplex(1);
        let flag = Arc::new(AtomicBool::new(streaming));
        let mut io = StallIo::new(
            near,
            Limits {
                response_deadline: Duration::from_millis(300),
                ..LONG
            },
            flag,
        );
        let reader = tokio::spawn(async move {
            let mut byte = [0_u8; 1];
            while far.read(&mut byte).await.is_ok_and(|n| n > 0) {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        });
        let result = io.write_all(&[0_u8; 24]).await;
        drop(io);
        reader.await.unwrap();
        result
    }

    #[tokio::test]
    async fn a_response_has_a_total_write_deadline() {
        let error = slow_drain(false).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn a_streaming_connection_has_no_total_write_deadline() {
        slow_drain(true).await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_drains_unread_request_data_but_only_so_much() {
        let (near, mut far) = tokio::io::duplex(8 * 1024);
        let mut io = StallIo::new(near, LONG, Arc::default());
        // A client that keeps sending: the server side must not wait for it to finish.
        let sender = tokio::spawn(async move {
            let chunk = [1_u8; 1024];
            let mut sent = 0;
            while far.write_all(&chunk).await.is_ok() {
                sent += chunk.len();
                if sent > 4 * 1024 * 1024 {
                    break;
                }
            }
            sent
        });
        let started = Instant::now();
        io.shutdown().await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(io.drained >= DRAIN_BYTES, "drained {}", io.drained);
        drop(io);
        assert!(sender.await.unwrap() < 4 * 1024 * 1024);
    }

    #[tokio::test]
    async fn shutdown_returns_at_once_when_the_client_is_done() {
        let (near, far) = tokio::io::duplex(1024);
        drop(far);
        let mut io = StallIo::new(near, LONG, Arc::default());
        let started = Instant::now();
        io.shutdown().await.unwrap();
        assert!(started.elapsed() < DRAIN_TIME);
    }

    /// The bodies of calls in flight count against the budget; beyond it a request gets 429,
    /// and a finished call gives its share back.
    #[tokio::test]
    async fn request_data_held_by_calls_in_flight_is_capped() {
        let csms = CsmsServer::start(CsmsConfig::new("127.0.0.1:0".parse().unwrap()))
            .await
            .unwrap();
        let station_addr = csms.local_addr();
        let mut silent = TcpStream::connect(station_addr).await.unwrap();
        silent
            .write_all(
                format!(
                    "GET /ocpp/CP-1 HTTP/1.1\r\nHost: {station_addr}\r\nConnection: Upgrade\r\n\
                     Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
                     Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                     Sec-WebSocket-Protocol: ocpp1.6\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut handshake = Vec::new();
        let mut byte = [0_u8; 1];
        while !handshake.ends_with(b"\r\n\r\n") {
            silent.read_exact(&mut byte).await.unwrap();
            handshake.push(byte[0]);
        }
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let api = spawn_with(
            listener,
            csms.clone(),
            Arc::new(Semaphore::new(16)),
            None,
            Limits {
                payload_budget: 3000,
                ..LONG
            },
        );
        let post = move |timeout_ms: u32| async move {
            let body = json!({
                "action": "DataTransfer",
                "payload": {"vendorId": "x", "data": "a".repeat(1000)},
                "timeout_ms": timeout_ms,
            })
            .to_string();
            let mut stream = TcpStream::connect(addr).await.unwrap();
            let request = format!(
                "POST /v1/stations/CP-1/calls HTTP/1.1\r\nHost: {addr}\r\n\
                 Connection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut raw = Vec::new();
            let _ = stream.read_to_end(&mut raw).await;
            String::from_utf8_lossy(&raw).into_owned()
        };
        // Two calls fit (about 1.1 KB each); they wait for the silent station.
        // A held call that loses the race for the budget to a probe tries again.
        let waiting: Vec<_> = (0..2)
            .map(|_| {
                tokio::spawn(async move {
                    while post(600_000).await.contains(" 429 ") {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
            })
            .collect();
        // Probes that slip in before both are held give their share back after 300 ms.
        let started = Instant::now();
        loop {
            let answer = post(300).await;
            if answer.contains(" 429 ") {
                assert!(answer.contains("request data"), "{answer}");
                break;
            }
            assert!(answer.contains(" 200 "), "{answer}");
            assert!(started.elapsed() < Duration::from_secs(30), "never saw 429");
            // Without a pause the next probe grabs the share the last one gave back before a
            // held call (retrying every 20 ms) can: the loop would then take many seconds.
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        for task in waiting {
            task.abort();
        }
        api.finish().await;
        csms.shutdown().await;
    }

    #[tokio::test]
    async fn event_streams_end_at_their_maximum_age_with_a_note() {
        let csms = CsmsServer::start(CsmsConfig::new("127.0.0.1:0".parse().unwrap()))
            .await
            .unwrap();
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let api = spawn_with(
            listener,
            csms.clone(),
            Arc::new(Semaphore::new(4)),
            None,
            Limits {
                stream_max_age: Duration::from_millis(300),
                ..LONG
            },
        );
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(format!("GET /v1/events HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut raw = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
            .await
            .expect("stream did not end")
            .ok();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("200 OK"), "{text}");
        assert!(text.contains("maximum age"), "{text}");
        api.finish().await;
        csms.shutdown().await;
    }

    #[tokio::test]
    async fn a_write_without_progress_times_out() {
        // The peer end of the pipe is never read: 8 bytes fit, the rest of the write blocks.
        let (near, _far) = tokio::io::duplex(8);
        let mut io = StallIo::new(
            near,
            Limits {
                write_stall: Duration::from_millis(150),
                ..LONG
            },
            Arc::default(),
        );
        let started = Instant::now();
        let error = io.write_all(&[0_u8; 64]).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= Duration::from_millis(100));
    }

    #[tokio::test]
    async fn a_slow_but_moving_write_is_left_alone() {
        let (near, mut far) = tokio::io::duplex(8);
        let mut io = StallIo::new(
            near,
            Limits {
                write_stall: Duration::from_millis(300),
                ..LONG
            },
            Arc::default(),
        );
        let reader = tokio::spawn(async move {
            let mut got = 0;
            let mut chunk = [0_u8; 8];
            while got < 64 {
                tokio::time::sleep(Duration::from_millis(40)).await;
                got += far.read(&mut chunk).await.unwrap();
            }
        });
        io.write_all(&[0_u8; 64]).await.unwrap();
        reader.await.unwrap();
    }

    #[tokio::test]
    async fn finish_closes_connections_that_are_still_open() {
        let csms = CsmsServer::start(CsmsConfig::new("127.0.0.1:0".parse().unwrap()))
            .await
            .unwrap();
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let api = spawn(listener, csms.clone(), Arc::new(Semaphore::new(4)), None);
        // One connection idle, one with a request body that never completes.
        let mut idle = TcpStream::connect(addr).await.unwrap();
        let mut stalled = TcpStream::connect(addr).await.unwrap();
        stalled
            .write_all(
                format!(
                    "POST /v1/rules/Heartbeat HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 50\r\n\r\n{{"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let started = Instant::now();
        api.finish().await;
        assert!(started.elapsed() < Duration::from_secs(6));
        // The listener is gone with the server.
        assert!(TcpStream::connect(addr).await.is_err());
        for stream in [&mut idle, &mut stalled] {
            // Closed by the server: end of stream or a reset, not a hang.
            let mut rest = Vec::new();
            let read = tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut rest))
                .await
                .expect("connection still open after finish");
            drop(read);
        }
        csms.shutdown().await;
    }
}
