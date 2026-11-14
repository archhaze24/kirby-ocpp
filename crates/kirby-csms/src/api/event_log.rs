//! The event log behind `GET /v1/events`: one task renders every engine event once, gives it
//! an id and keeps the newest ones in a bounded ring; every stream reads from the ring. A
//! reconnecting client resumes from the id it saw last.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use kirby_csms_core::{CsmsEvent, CsmsHandle};
use kirby_ocpp_tui::text::escape_json_unsafe;
use serde_json::{Value, json};
use tokio::sync::{broadcast, watch};

use super::json::stringify_transaction_ids;
use super::metrics::Metrics;
use super::server::wait_stopped;
use crate::headless::Renderer;

/// Events the ring holds at most.
pub(super) const RING_EVENTS: usize = 1024;
/// Bytes of event text the ring holds at most (an event larger than that is still kept alone).
pub(super) const RING_BYTES: usize = 8 * 1024 * 1024;

/// One event of the log, as sent to a client.
pub(super) struct Entry {
    pub(super) id: u64,
    /// The SSE `event:` name, the `type` of the JSON in `data`.
    pub(super) event: String,
    pub(super) data: String,
    /// The station the event is about; `None` for an event without one.
    pub(super) station: Option<String>,
    /// Sent to every stream, whatever its station filter (a note about lost events).
    pub(super) global: bool,
}

struct Ring {
    entries: VecDeque<Arc<Entry>>,
    bytes: usize,
    /// The id of the newest entry (the seed while the ring is empty). Ids of the entries are
    /// consecutive, so the entry with id `i` sits at index `i - (head + 1 - len)`.
    head: u64,
    /// The pump ended: no entry will be added.
    closed: bool,
}

impl Ring {
    fn oldest(&self) -> u64 {
        self.head + 1 - self.entries.len() as u64
    }
}

/// The log: the ring and the signal that wakes the streams.
pub(super) struct EventLog {
    ring: Mutex<Ring>,
    signal: watch::Sender<()>,
    max_events: usize,
    max_bytes: usize,
}

/// What a client asks for when it opens a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Resume {
    /// Nothing: events from now on.
    Fresh,
    /// The events after this id, which the client saw last.
    From(u64),
    /// A `Last-Event-ID` that is no id.
    Unusable,
}

/// How a stream starts.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Start {
    /// The id of the first entry the stream reads.
    pub(super) next: u64,
    /// The id of the newest entry when the requested id could not be resumed.
    pub(super) resync: Option<u64>,
}

/// One read of the ring.
#[derive(Debug)]
pub(super) struct Batch {
    pub(super) entries: Vec<Arc<Entry>>,
    /// The id of the first entry not read yet.
    pub(super) next: u64,
    /// Entries the reader lost because it fell behind the ring, and the id the read resumed at.
    pub(super) lagged: Option<(u64, u64)>,
    /// The log ended and the reader has read all of it.
    pub(super) done: bool,
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl EventLog {
    /// An empty log whose first entry gets the id `seed + 1`. Ids from an earlier run of the
    /// server are smaller than the seed of this one when the seed is the start time.
    pub(super) fn new(seed: u64) -> Arc<Self> {
        Self::with_caps(seed, RING_EVENTS, RING_BYTES)
    }

    pub(super) fn with_caps(seed: u64, max_events: usize, max_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            ring: Mutex::new(Ring {
                entries: VecDeque::new(),
                bytes: 0,
                head: seed,
                closed: false,
            }),
            signal: watch::channel(()).0,
            max_events: max_events.max(1),
            max_bytes,
        })
    }

    fn lock(&self) -> MutexGuard<'_, Ring> {
        self.ring.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A receiver that is woken by every new entry and by the end of the log.
    pub(super) fn subscribe(&self) -> watch::Receiver<()> {
        self.signal.subscribe()
    }

    /// Adds an event; the oldest ones go when the ring is over its caps.
    pub(super) fn push(&self, station: Option<String>, global: bool, value: &Value) {
        let event = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message")
            .to_owned();
        let data = escape_json_unsafe(&value.to_string());
        {
            let mut ring = self.lock();
            ring.head += 1;
            let entry = Entry {
                id: ring.head,
                event,
                data,
                station,
                global,
            };
            ring.bytes += entry.data.len();
            ring.entries.push_back(Arc::new(entry));
            while ring.entries.len() > 1
                && (ring.entries.len() > self.max_events || ring.bytes > self.max_bytes)
            {
                if let Some(old) = ring.entries.pop_front() {
                    ring.bytes = ring.bytes.saturating_sub(old.data.len());
                }
            }
        }
        self.signal.send_replace(());
    }

    /// Ends the log: streams read what is left and finish.
    pub(super) fn close(&self) {
        self.lock().closed = true;
        self.signal.send_replace(());
    }

    /// Where a new stream starts. A client that names the id it saw last resumes right after
    /// it when the entry after it is still held; otherwise the stream starts at the head and
    /// says so. Decided under the same lock as the later reads, so nothing is lost or
    /// repeated in between.
    pub(super) fn start(&self, resume: Resume) -> Start {
        let ring = self.lock();
        let fresh = Start {
            next: ring.head + 1,
            resync: None,
        };
        match resume {
            Resume::Fresh => fresh,
            Resume::From(id) if id <= ring.head && id + 1 >= ring.oldest() => Start {
                next: id + 1,
                resync: None,
            },
            Resume::From(_) | Resume::Unusable => Start {
                resync: Some(ring.head),
                ..fresh
            },
        }
    }

    /// Reads up to `max` entries from `next` on that pass `filter` (a station id; entries
    /// that are `global` always pass). The read covers every entry up to `Batch::next`,
    /// whether it passed or not.
    pub(super) fn read(&self, next: u64, filter: Option<&str>, max: usize) -> Batch {
        let ring = self.lock();
        let oldest = ring.oldest();
        let mut next = next;
        let mut lagged = None;
        if next < oldest {
            lagged = Some((oldest - next, oldest));
            next = oldest;
        }
        let mut entries = Vec::new();
        let mut index = usize::try_from(next - oldest).unwrap_or(usize::MAX);
        while entries.len() < max {
            let Some(entry) = ring.entries.get(index) else {
                break;
            };
            if entry.global || filter.is_none_or(|wanted| entry.station.as_deref() == Some(wanted))
            {
                entries.push(Arc::clone(entry));
            }
            index += 1;
        }
        let next = oldest + index as u64;
        Batch {
            entries,
            next,
            lagged,
            done: ring.closed && next > ring.head,
        }
    }

    /// The id of the newest entry.
    #[cfg(test)]
    pub(super) fn head(&self) -> u64 {
        self.lock().head
    }
}

/// The station id of an event, if it names one.
pub(super) fn event_station(event: &CsmsEvent) -> Option<&str> {
    match event {
        CsmsEvent::Connected { station }
        | CsmsEvent::Disconnected { station, .. }
        | CsmsEvent::Frame { station, .. }
        | CsmsEvent::Malformed { station, .. }
        | CsmsEvent::Forgotten { station }
        | CsmsEvent::StateChanged { station } => Some(station),
        CsmsEvent::AuthFailed { station, .. } => station.as_deref(),
        _ => None,
    }
}

/// Starts the task that feeds `log` from the engine events until `stopped` is set (after
/// the events that are queued) or the engine is gone.
pub(super) fn spawn_pump(
    handle: &CsmsHandle,
    mut rx: broadcast::Receiver<CsmsEvent>,
    log: Arc<EventLog>,
    metrics: Arc<Metrics>,
    mut stopped: watch::Receiver<bool>,
) {
    let mut renderer = Renderer::new(handle);
    tokio::spawn(async move {
        let publish = |renderer: &mut Renderer, event: &CsmsEvent| {
            metrics.count_event(event);
            let station = event_station(event).map(str::to_owned);
            for (mut value, _) in renderer.render(event) {
                if value["type"] == "state" {
                    stringify_transaction_ids(&mut value);
                }
                log.push(station.clone(), false, &value);
            }
        };
        let lost = |renderer: &mut Renderer, missed: u64| {
            renderer.reset();
            metrics.add_events_dropped(missed);
            log.push(
                None,
                true,
                &json!({"type": "note", "text": format!("{missed} events were dropped")}),
            );
        };
        loop {
            tokio::select! {
                received = rx.recv() => match received {
                    Ok(event) => publish(&mut renderer, &event),
                    Err(broadcast::error::RecvError::Lagged(missed)) => lost(&mut renderer, missed),
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                () = wait_stopped(&mut stopped) => {
                    loop {
                        match rx.try_recv() {
                            Ok(event) => publish(&mut renderer, &event),
                            Err(broadcast::error::TryRecvError::Lagged(missed)) => {
                                lost(&mut renderer, missed);
                            }
                            Err(_) => break,
                        }
                    }
                    break;
                }
            }
        }
        log.close();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pump_preserves_events_queued_before_it_starts() {
        use kirby_csms_core::{CsmsConfig, CsmsServer};
        use kirby_ocpp_core::ws::{ConnectOptions, Connection};

        let deadline = std::time::Duration::from_secs(5);
        let handle = CsmsServer::start(CsmsConfig::new("127.0.0.1:0".parse().unwrap()))
            .await
            .unwrap();
        let events = handle.subscribe();
        let mut observed = handle.subscribe();
        let (_connection, _connection_events) = tokio::time::timeout(
            deadline,
            Connection::connect(
                &handle.url(),
                "CP-EARLY",
                &kirby_ocpp_core::config::TlsConfig::default(),
                "ocpp1.6",
                ConnectOptions::default(),
            ),
        )
        .await
        .expect("handshake completed")
        .unwrap();
        tokio::time::timeout(deadline, async {
            loop {
                if matches!(observed.recv().await.unwrap(), CsmsEvent::Connected { .. }) {
                    break;
                }
            }
        })
        .await
        .expect("station connected before pump startup");

        let log = EventLog::new(0);
        let mut changes = log.subscribe();
        let (stop, stopped) = watch::channel(false);
        spawn_pump(&handle, events, Arc::clone(&log), Metrics::new(), stopped);
        let published = tokio::time::timeout(deadline, async {
            loop {
                if log
                    .read(1, None, 10)
                    .entries
                    .iter()
                    .any(|entry| entry.event == "connected")
                {
                    break;
                }
                changes.changed().await.unwrap();
            }
        })
        .await;
        stop.send(true).unwrap();
        handle.shutdown().await;
        assert!(published.is_ok(), "the pump lost the early connected event");
    }

    fn push(log: &EventLog, station: &str) {
        log.push(
            Some(station.to_owned()),
            false,
            &json!({"type": "frame", "n": 1}),
        );
    }

    fn ids(batch: &Batch) -> Vec<u64> {
        batch.entries.iter().map(|entry| entry.id).collect()
    }

    #[test]
    fn ids_are_consecutive_after_the_seed() {
        let log = EventLog::new(100);
        push(&log, "A");
        push(&log, "B");
        let batch = log.read(101, None, 10);
        assert_eq!(ids(&batch), [101, 102]);
        assert_eq!(batch.next, 103);
        assert_eq!(batch.lagged, None);
        assert!(!batch.done);
        assert_eq!(batch.entries[0].event, "frame");
        assert_eq!(log.head(), 102);
    }

    #[test]
    fn the_ring_drops_the_oldest_by_count_and_by_bytes() {
        let log = EventLog::with_caps(0, 3, usize::MAX);
        for _ in 0..5 {
            push(&log, "A");
        }
        let batch = log.read(1, None, 10);
        assert_eq!(batch.lagged, Some((2, 3)));
        assert_eq!(ids(&batch), [3, 4, 5]);

        let one = log.read(5, None, 10).entries[0].data.len();
        let log = EventLog::with_caps(0, 100, one * 2);
        for _ in 0..5 {
            push(&log, "A");
        }
        assert_eq!(ids(&log.read(1, None, 10)), [4, 5]);
        // One entry bigger than the byte cap is still kept.
        let log = EventLog::with_caps(0, 100, 1);
        push(&log, "A");
        assert_eq!(ids(&log.read(1, None, 10)), [1]);
    }

    #[test]
    fn start_resumes_in_range_and_resyncs_out_of_range() {
        let log = EventLog::with_caps(1000, 3, usize::MAX);
        // Empty log: the head is the seed, only the seed itself resumes.
        let at = |next, resync| Start { next, resync };
        assert_eq!(log.start(Resume::Fresh), at(1001, None));
        assert_eq!(log.start(Resume::From(1000)), at(1001, None));
        assert_eq!(log.start(Resume::From(999)).resync, Some(1000));
        for _ in 0..5 {
            push(&log, "A");
        }
        // Entries 1003..=1005 are held.
        assert_eq!(log.start(Resume::Fresh), at(1006, None));
        assert_eq!(log.start(Resume::From(1005)), at(1006, None));
        assert_eq!(log.start(Resume::From(1003)), at(1004, None));
        // The entry after 1002 is held, so 1002 resumes; 1001 does not.
        assert_eq!(log.start(Resume::From(1002)), at(1003, None));
        assert_eq!(log.start(Resume::From(1001)), at(1006, Some(1005)));
        // An id from the future, or text that is no id: resync at the head.
        assert_eq!(log.start(Resume::From(9999)), at(1006, Some(1005)));
        assert_eq!(log.start(Resume::Unusable), at(1006, Some(1005)));
        // An id from an earlier run (smaller than the seed) never resumes.
        assert_eq!(log.start(Resume::From(7)).resync, Some(1005));
    }

    #[test]
    fn reads_filter_by_station_and_still_advance() {
        let log = EventLog::new(0);
        push(&log, "A");
        push(&log, "B");
        log.push(None, true, &json!({"type": "note", "text": "x"}));
        push(&log, "A");
        let batch = log.read(1, Some("B"), 10);
        assert_eq!(ids(&batch), [2, 3]);
        assert_eq!(batch.next, 5);
        let batch = log.read(1, Some("GHOST"), 10);
        assert_eq!(ids(&batch), [3]);
        // `max` stops the read; `next` is the first entry not read.
        let batch = log.read(1, None, 2);
        assert_eq!(ids(&batch), [1, 2]);
        assert_eq!(batch.next, 3);
        let batch = log.read(5, None, 2);
        assert!(batch.entries.is_empty());
        assert_eq!(batch.next, 5);
    }

    #[test]
    fn a_closed_log_is_done_once_it_is_read() {
        let log = EventLog::new(0);
        push(&log, "A");
        log.close();
        let batch = log.read(1, None, 10);
        assert!(batch.done);
        assert!(!log.read(1, None, 0).done);
    }
}
