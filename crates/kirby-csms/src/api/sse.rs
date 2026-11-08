//! `GET /v1/events`: the Server-Sent Events stream, read from the shared event log.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream;
use kirby_ocpp_tui::text::escape_json_unsafe;
use serde_json::json;
use tokio::sync::{OwnedSemaphorePermit, watch};
use tokio::time::Instant;

use super::ApiState;
use super::error::error;
use super::event_log::{Entry, EventLog, Resume};
use super::json::query_params;
use super::limits::Streaming;
use super::metrics::Metrics;

/// Interval of the SSE keep-alive comments.
const KEEP_ALIVE: Duration = Duration::from_secs(15);
/// Most entries one read of the log takes.
const READ_BATCH: usize = 256;

/// A message with an id that is not an entry of the log (a note or a `resync`): `id` is the
/// last entry the stream has accounted for, so a client that reconnects with it resumes
/// right after that point.
fn note(id: u64, kind: &str, text: &str) -> Event {
    let body = json!({"type": kind, "text": text});
    Event::default()
        .id(id.to_string())
        .event(kind)
        .data(escape_json_unsafe(&body.to_string()))
}

fn entry_event(entry: &Entry) -> Event {
    Event::default()
        .id(entry.id.to_string())
        .event(&entry.event)
        .data(&entry.data)
}

struct StreamState {
    log: Arc<EventLog>,
    wake: watch::Receiver<()>,
    metrics: Arc<Metrics>,
    /// The id of the first log entry this stream has not read yet.
    next: u64,
    filter: Option<String>,
    queue: VecDeque<Event>,
    /// The stream ends then (see [`STREAM_MAX_AGE`]).
    ///
    /// [`STREAM_MAX_AGE`]: super::limits::STREAM_MAX_AGE
    ends_at: Instant,
    /// Send what is queued, then end.
    ending: bool,
    /// Held for the life of the stream: dropping the stream frees the slot.
    _permit: OwnedSemaphorePermit,
}

impl StreamState {
    /// Queues what the log holds from `next` on (after a lag note when the stream fell behind
    /// the ring). Returns whether the log has ended and was read to its end.
    fn fill(&mut self) -> bool {
        let batch = self.log.read(self.next, self.filter.as_deref(), READ_BATCH);
        if let Some((skipped, resumed_at)) = batch.lagged {
            self.metrics.add_stream_events_dropped(skipped);
            self.queue.push_back(note(
                resumed_at - 1,
                "note",
                &format!("{skipped} events were dropped"),
            ));
        }
        for entry in &batch.entries {
            self.queue.push_back(entry_event(entry));
        }
        self.next = batch.next;
        batch.done
    }
}

/// The id the client says it saw last: the `Last-Event-ID` header, else `last_event_id`.
/// A header value that is no id gives [`Resume::Unusable`] (the stream starts with a `resync`:
/// a client that reconnects by itself cannot fix a `400`); a bad query value is an error.
fn resume_point(headers: &HeaderMap, query: Option<&str>) -> Result<Resume, String> {
    let digits = |text: &str| -> Option<u64> {
        (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse().ok())
            .flatten()
    };
    if let Some(value) = headers.get("last-event-id") {
        return Ok(value
            .to_str()
            .ok()
            .and_then(digits)
            .map_or(Resume::Unusable, Resume::From));
    }
    match query {
        None => Ok(Resume::Fresh),
        Some(text) => digits(text)
            .map(Resume::From)
            .ok_or_else(|| "`last_event_id` must be an event id (ASCII digits)".to_owned()),
    }
}

pub(super) async fn events(
    State(state): State<ApiState>,
    Extension(Streaming(streaming)): Extension<Streaming>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let [filter, last_event_id] = match query_params(&uri, ["station", "last_event_id"]) {
        Ok(values) => values,
        Err(message) => return error(StatusCode::BAD_REQUEST, "bad_request", &message),
    };
    let resume = match resume_point(&headers, last_event_id.as_deref()) {
        Ok(resume) => resume,
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
    let start = state.log.start(resume);
    let mut queue = VecDeque::new();
    if let Some(head) = start.resync {
        queue.push_back(note(
            head,
            "resync",
            "the requested event id is no longer held; read the current state and continue from here",
        ));
    }
    let init = StreamState {
        log: Arc::clone(&state.log),
        wake: state.log.subscribe(),
        metrics: Arc::clone(&state.metrics),
        next: start.next,
        filter,
        queue,
        ends_at: Instant::now() + state.stream_max_age,
        ending: false,
        _permit: permit,
    };
    let events = stream::unfold(init, |mut st| async move {
        loop {
            if let Some(event) = st.queue.pop_front() {
                return Some((Ok::<Event, Infallible>(event), st));
            }
            if st.ending {
                return None;
            }
            if Instant::now() >= st.ends_at {
                st.queue.push_back(note(
                    st.next - 1,
                    "note",
                    "the stream reached its maximum age; reconnect to continue",
                ));
                st.ending = true;
                continue;
            }
            // Mark the signal as seen before reading, so a push after the read wakes us.
            st.wake.borrow_and_update();
            let done = st.fill();
            if !st.queue.is_empty() {
                continue;
            }
            if done {
                return None;
            }
            let ends_at = st.ends_at;
            tokio::select! {
                () = tokio::time::sleep_until(ends_at) => {}
                changed = st.wake.changed() => {
                    if changed.is_err() {
                        return None;
                    }
                }
            }
        }
    });
    Sse::new(events)
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE).text("keep-alive"))
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    #[test]
    fn the_header_wins_over_the_query_and_a_bad_header_is_a_resync() {
        let mut headers = HeaderMap::new();
        assert_eq!(resume_point(&headers, None), Ok(Resume::Fresh));
        assert_eq!(resume_point(&headers, Some("7")), Ok(Resume::From(7)));
        assert!(resume_point(&headers, Some("x")).is_err());
        assert!(resume_point(&headers, Some("")).is_err());
        assert!(resume_point(&headers, Some("99999999999999999999999")).is_err());
        headers.insert("last-event-id", HeaderValue::from_static("12"));
        assert_eq!(resume_point(&headers, Some("7")), Ok(Resume::From(12)));
        for bad in ["", "abc", "-1", "1 2", "99999999999999999999999"] {
            headers.insert("last-event-id", HeaderValue::from_str(bad).unwrap());
            assert_eq!(
                resume_point(&headers, Some("7")),
                Ok(Resume::Unusable),
                "{bad:?}"
            );
        }
    }
}
