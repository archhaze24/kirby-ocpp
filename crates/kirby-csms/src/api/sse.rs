//! `GET /v1/events`: the Server-Sent Events stream.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::extract::{Extension, State};
use axum::http::{StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream;
use kirby_csms_core::CsmsEvent;
use kirby_ocpp_tui::text::escape_json_unsafe;
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, broadcast, watch};
use tokio::time::Instant;

use super::ApiState;
use super::error::error;
use super::json::{query_param, stringify_transaction_ids};
use super::limits::Streaming;
use super::server::wait_stopped;
use crate::headless::Renderer;

/// Interval of the SSE keep-alive comments.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

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
    ///
    /// [`STREAM_MAX_AGE`]: super::limits::STREAM_MAX_AGE
    ends_at: Instant,
    /// The server stopped: send what is queued and what the channel still holds, then end.
    ending: bool,
    ids: Arc<AtomicU64>,
    /// Held for the life of the stream: dropping the stream frees the slot.
    _permit: OwnedSemaphorePermit,
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

pub(super) async fn events(
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
