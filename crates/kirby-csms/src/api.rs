//! Local HTTP control API (`--api-listen`): JSON in and out, plain HTTP, optional bearer token.
//! It only uses [`CsmsHandle`] and the command code shared with the stdin handler.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use chrono::SecondsFormat;
use futures_util::{StreamExt, stream};
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use kirby_csms_core::{CsmsEvent, CsmsHandle, Direction, Reply, is_charge_point_action};
use kirby_ocpp_core::text::is_unsafe_char;
use kirby_ocpp_tui::text::escape_json_unsafe;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Sleep;

use crate::commands;
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

/// An accepted connection's socket: gives up when a write makes no progress for
/// [`WRITE_STALL`] (a client that stopped reading), so stalled streams free their slot.
struct StallIo<T> {
    inner: T,
    /// How long a write may make no progress.
    after: Duration,
    /// Armed while a write is pending; cleared by any write progress.
    stall: Option<Pin<Box<Sleep>>>,
}

impl<T> StallIo<T> {
    /// Applies the stall rule to the result of a write-side poll.
    fn guard<R>(
        &mut self,
        cx: &mut Context<'_>,
        polled: Poll<io::Result<R>>,
    ) -> Poll<io::Result<R>> {
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

impl<T: AsyncWrite + Unpin> AsyncWrite for StallIo<T> {
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

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_shutdown(cx);
        this.guard(cx, polled)
    }
}

/// Accepts connections until `stopped`, one task each (tracked in a [`JoinSet`], so dropping
/// this future closes every connection). A connection holds a slot of the cap, gets
/// [`HEAD_TIMEOUT`] from hyper for the request head and carries one request.
async fn serve(
    listener: TcpListener,
    app: Router,
    slots: Arc<Semaphore>,
    mut stopped: watch::Receiver<bool>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            () = wait_stopped(&mut stopped) => break,
            Some(_) = connections.join_next() => {}
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    // Out of descriptors or similar: do not spin.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                };
                // Over the cap the socket is dropped (closed) at once.
                let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
                    continue;
                };
                let io = TokioIo::new(StallIo { inner: stream, after: WRITE_STALL, stall: None });
                let service = TowerToHyperService::new(app.clone());
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
    while connections.join_next().await.is_some() {}
}

/// Serves the API on `listener` until [`Api::finish`].
pub fn spawn(
    listener: TcpListener,
    handle: CsmsHandle,
    slots: Arc<Semaphore>,
    token: Option<String>,
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
    };
    let app = router(state);
    let task = tokio::spawn(serve(
        listener,
        app,
        Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        stopped,
    ));
    Api { stop, task }
}

impl Api {
    /// Stops accepting, waits a bounded time for open requests, then aborts the server task,
    /// which drops (closes) every connection still open.
    pub async fn finish(mut self) {
        let _ = self.stop.send(true);
        if tokio::time::timeout(SHUTDOWN_GRACE, &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
            let _ = self.task.await;
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
        .route("/v1/events", get(events))
        .route("/v1/rules", get(list_rules))
        .route("/v1/rules/{action}", put(put_rule).delete(delete_rule))
        .route(
            "/v1/faults",
            get(faults_remaining)
                .post(inject_fault)
                .delete(clear_faults),
        )
        .fallback(|| async { error(StatusCode::NOT_FOUND, "not found") })
        .method_not_allowed_fallback(|| async {
            error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
        })
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .layer(middleware::from_fn(close_connection))
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

/// Every response closes its connection (see [`GuardedIo`]).
async fn close_connection(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
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
    } else if headers
        .get("sec-fetch-site")
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"cross-site"))
    {
        return error(
            StatusCode::FORBIDDEN,
            "cross-site requests are refused without an API token",
        );
    } else if headers.get(header::HOST).is_some_and(|host| {
        !host
            .to_str()
            .is_ok_and(|host| is_loopback_host(host, state.bound))
    }) {
        return error(
            StatusCode::FORBIDDEN,
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
                    "`limit` must be a non-negative integer",
                );
            }
        },
        Ok(None) => None,
        Err(message) => return error(StatusCode::BAD_REQUEST, &message),
    };
    let Some(entries) = state.handle.history_tail(&id, limit.unwrap_or(usize::MAX)) else {
        return error(StatusCode::NOT_FOUND, "unknown station");
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
        None => error(StatusCode::NOT_FOUND, "unknown station"),
        Some(false) => error(
            StatusCode::CONFLICT,
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
    let body = match read_json(request).await {
        Ok(body) => body,
        Err(response) => return *response,
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

/// The request body (at most 1 MiB, read within 10 s) parsed as JSON.
async fn read_json(request: Request<Body>) -> Result<Value, Box<Response>> {
    let too_large = || {
        Box::new(error(
            StatusCode::PAYLOAD_TOO_LARGE,
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
                "the request body did not arrive in 10 s",
            )));
        }
    };
    serde_json::from_slice(&bytes).map_err(|e| {
        Box::new(error(
            StatusCode::BAD_REQUEST,
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
/// Any other parameter is an error.
fn query_param(uri: &Uri, name: &str) -> Result<Option<String>, String> {
    let Some(query) = uri.query() else {
        return Ok(None);
    };
    let mut found = None;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        match pair.split_once('=') {
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

/// One SSE event: `event:` is the `type` of the JSON object in `data:`.
fn sse_event(value: &Value) -> Event {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    Event::default()
        .event(kind)
        .data(escape_json_unsafe(&value.to_string()))
}

struct StreamState {
    rx: broadcast::Receiver<CsmsEvent>,
    renderer: Renderer,
    filter: Option<String>,
    queue: std::collections::VecDeque<Event>,
    stopped: watch::Receiver<bool>,
    /// The server stopped: send what is queued and what the channel still holds, then end.
    ending: bool,
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
            for (value, _) in self.renderer.render(event) {
                self.queue.push_back(sse_event(&value));
            }
        }
    }

    /// Events were lost: say so, and forget the rendered state views so that the next state
    /// event is complete instead of a diff against a view the client never saw.
    fn lagged(&mut self, missed: u64) {
        self.renderer.reset();
        self.queue.push_back(sse_event(
            &json!({"type": "note", "text": format!("{missed} events were dropped")}),
        ));
    }
}

async fn events(State(state): State<ApiState>, uri: Uri) -> Response {
    let filter = match query_param(&uri, "station") {
        Ok(filter) => filter,
        Err(message) => return error(StatusCode::BAD_REQUEST, &message),
    };
    let Ok(permit) = Arc::clone(&state.streams).try_acquire_owned() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "too many event streams (at most 16)",
        );
    };
    let init = StreamState {
        rx: state.handle.subscribe(),
        renderer: Renderer::new(&state.handle),
        filter,
        queue: std::collections::VecDeque::new(),
        stopped: state.stopped.clone(),
        ending: false,
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
            tokio::select! {
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
        _ => Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "not an action a charge point can send",
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
        Ok(payload) if payload.is_object() => payload,
        Ok(_) => return error(StatusCode::BAD_REQUEST, "the body must be a JSON object"),
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
    let existed = state.handle.responder_actions().contains(&action);
    state.handle.clear_responder(&action);
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
        Ok(body) => body,
        Err(response) => return *response,
    };
    let fault: FaultRequest = match serde_json::from_value(body) {
        Ok(fault) => fault,
        Err(e) => return error(StatusCode::BAD_REQUEST, &format!("invalid fault: {e}")),
    };
    if let Err(message) = commands::check_fault(
        &fault.action,
        fault.kind,
        fault.payload.as_ref(),
        fault.times,
    ) {
        return error(StatusCode::BAD_REQUEST, &message);
    }
    if let Err(message) = state.handle.try_inject(commands::build_fault(
        &fault.action,
        fault.kind,
        fault.payload,
        fault.times,
    )) {
        return error(StatusCode::CONFLICT, &message);
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

    #[tokio::test]
    async fn a_write_without_progress_times_out() {
        // The peer end of the pipe is never read: 8 bytes fit, the rest of the write blocks.
        let (near, _far) = tokio::io::duplex(8);
        let mut io = StallIo {
            inner: near,
            after: Duration::from_millis(150),
            stall: None,
        };
        let started = Instant::now();
        let error = io.write_all(&[0_u8; 64]).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= Duration::from_millis(100));
    }

    #[tokio::test]
    async fn a_slow_but_moving_write_is_left_alone() {
        let (near, mut far) = tokio::io::duplex(8);
        let mut io = StallIo {
            inner: near,
            after: Duration::from_millis(300),
            stall: None,
        };
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
