//! `GET /v1/metrics`: counters and gauges in the Prometheus text exposition format 0.0.4.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use kirby_csms_core::registry;
use kirby_csms_core::{CsmsEvent, Direction, Frame};

use super::ApiState;
use super::export::text_response;
use super::limits::MAX_STREAMS;

/// The label value of every action the server has no table entry for.
const OTHER: &str = "other";

/// The counters the API keeps itself; the rest is read from the engine when scraped.
pub(super) struct Metrics {
    started: Instant,
    received: Mutex<BTreeMap<&'static str, u64>>,
    sent: Mutex<BTreeMap<&'static str, u64>>,
    events_dropped: AtomicU64,
    stream_events_dropped: AtomicU64,
    /// Responses by status class: index 0 is `1xx`, 4 is `5xx`.
    requests: [AtomicU64; 5],
}

/// The table name of `action` when some protocol knows it (an action the server sends or one a
/// station sends), else [`OTHER`]. Stations choose the action names of the frames they send, so
/// only names from the tables may become label values.
fn known_action(action: &str) -> &'static str {
    for protocol in registry::protocols() {
        let Some(table) = registry::protocol_actions(protocol) else {
            continue;
        };
        if let Some(spec) = table.csms_actions.iter().find(|spec| spec.name == action) {
            return spec.name;
        }
        if let Some(name) = table.station_actions.iter().find(|name| **name == action) {
            return name;
        }
    }
    OTHER
}

impl Metrics {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            started: Instant::now(),
            received: Mutex::new(BTreeMap::new()),
            sent: Mutex::new(BTreeMap::new()),
            events_dropped: AtomicU64::new(0),
            stream_events_dropped: AtomicU64::new(0),
            requests: std::array::from_fn(|_| AtomicU64::new(0)),
        })
    }

    /// Counts the CALL frames of an engine event.
    pub(super) fn count_event(&self, event: &CsmsEvent) {
        let CsmsEvent::Frame {
            direction,
            frame: Frame::Call { action, .. },
            ..
        } = event
        else {
            return;
        };
        let table = match direction {
            Direction::In => &self.received,
            Direction::Out => &self.sent,
        };
        *table
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(known_action(action))
            .or_insert(0) += 1;
    }

    /// Engine events the log never saw (the pump fell behind the engine).
    pub(super) fn add_events_dropped(&self, count: u64) {
        self.events_dropped.fetch_add(count, Ordering::Relaxed);
    }

    /// Events an event stream skipped because it fell behind the log.
    pub(super) fn add_stream_events_dropped(&self, count: u64) {
        self.stream_events_dropped
            .fetch_add(count, Ordering::Relaxed);
    }

    /// Counts one response of the router.
    pub(super) fn count_response(&self, status: StatusCode) {
        let class = usize::from(status.as_u16() / 100);
        if let Some(counter) = class.checked_sub(1).and_then(|at| self.requests.get(at)) {
            counter.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// What the engine and the server report at scrape time.
pub(super) struct Gauges {
    pub(super) stations_known: usize,
    pub(super) stations_connected: usize,
    pub(super) faults: usize,
    pub(super) fault_occurrences: usize,
    pub(super) rules_global: usize,
    pub(super) rules_station: usize,
    pub(super) history_dropped: u64,
    pub(super) streams_open: usize,
}

/// A label value in the text format: backslash, quote and newline are escaped.
fn label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn family(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = write!(out, "# HELP {name} {help}\n# TYPE {name} {kind}\n");
}

/// One family with a single sample.
fn single(out: &mut String, (name, kind, help, value): (&str, &str, &str, u64)) {
    family(out, name, kind, help);
    let _ = writeln!(out, "{name} {value}");
}

/// A counter family with one sample per action (at least one, so the family is never empty).
fn by_action(out: &mut String, name: &str, help: &str, table: &BTreeMap<&'static str, u64>) {
    family(out, name, "counter", help);
    if table.is_empty() {
        let _ = writeln!(out, "{name}{{action=\"{OTHER}\"}} 0");
    }
    for (action, count) in table {
        let _ = writeln!(out, "{name}{{action=\"{}\"}} {count}", label(action));
    }
}

impl Metrics {
    /// The text exposition of every metric.
    pub(super) fn render(&self, gauges: &Gauges) -> String {
        let mut out = String::new();
        for scalar in self.scalars(gauges) {
            single(&mut out, scalar);
        }
        self.render_labeled(&mut out);
        out
    }

    /// The families that have one sample: name, type, help, value.
    fn scalars<'a>(&self, gauges: &Gauges) -> [(&'a str, &'a str, &'a str, u64); 11] {
        [
            (
                "kirby_csms_uptime_seconds",
                "gauge",
                "Seconds since the API started.",
                self.started.elapsed().as_secs(),
            ),
            (
                "kirby_csms_stations_known",
                "gauge",
                "Stations in the registry, connected or not.",
                gauges.stations_known as u64,
            ),
            (
                "kirby_csms_stations_connected",
                "gauge",
                "Stations with an open connection.",
                gauges.stations_connected as u64,
            ),
            (
                "kirby_csms_faults_active",
                "gauge",
                "Stored faults that are not used up.",
                gauges.faults as u64,
            ),
            (
                "kirby_csms_fault_occurrences_remaining",
                "gauge",
                "Fault occurrences not triggered yet, over all faults.",
                gauges.fault_occurrences as u64,
            ),
            (
                "kirby_csms_history_frames_dropped_total",
                "counter",
                "Frames that left a station history by eviction or because they were too big.",
                gauges.history_dropped,
            ),
            (
                "kirby_csms_events_dropped_total",
                "counter",
                "Engine events the event log missed because it fell behind.",
                self.events_dropped.load(Ordering::Relaxed),
            ),
            (
                "kirby_csms_stream_events_dropped_total",
                "counter",
                "Events that event streams skipped because they fell behind the event log.",
                self.stream_events_dropped.load(Ordering::Relaxed),
            ),
            (
                "kirby_csms_event_streams",
                "gauge",
                "Open event streams.",
                gauges.streams_open as u64,
            ),
            (
                "kirby_csms_rules_global",
                "gauge",
                "Global response rules in force.",
                gauges.rules_global as u64,
            ),
            (
                "kirby_csms_rules_station",
                "gauge",
                "Per-station response rules in force.",
                gauges.rules_station as u64,
            ),
        ]
    }

    /// The families with labels: calls by action, responses by class, build info.
    fn render_labeled(&self, out: &mut String) {
        by_action(
            out,
            "kirby_csms_calls_received_total",
            "CALLs received from stations, by action (names outside the action tables are other).",
            &self.received.lock().unwrap_or_else(PoisonError::into_inner),
        );
        by_action(
            out,
            "kirby_csms_calls_sent_total",
            "CALLs sent to stations, by action (names outside the action tables are other).",
            &self.sent.lock().unwrap_or_else(PoisonError::into_inner),
        );
        family(
            out,
            "kirby_csms_api_requests_total",
            "counter",
            "API responses by status class.",
        );
        for (index, counter) in self.requests.iter().enumerate() {
            let _ = writeln!(
                out,
                "kirby_csms_api_requests_total{{class=\"{}xx\"}} {}",
                index + 1,
                counter.load(Ordering::Relaxed)
            );
        }
        family(
            out,
            "kirby_csms_build_info",
            "gauge",
            "Version of the running program.",
        );
        let _ = writeln!(
            out,
            "kirby_csms_build_info{{version=\"{}\"}} 1",
            label(env!("CARGO_PKG_VERSION"))
        );
    }
}

pub(super) async fn metrics(State(state): State<ApiState>) -> Response {
    let (stations_known, stations_connected) = state.handle.station_counts();
    let scoped = state.handle.scoped_rules(None);
    let gauges = Gauges {
        stations_known,
        stations_connected,
        faults: state.handle.faults().len(),
        fault_occurrences: state.handle.faults_remaining(),
        rules_global: state.handle.responder_actions().len()
            + scoped
                .iter()
                .filter(|rule| rule.scope.station.is_none())
                .count(),
        rules_station: scoped
            .iter()
            .filter(|rule| rule.scope.station.is_some())
            .count(),
        history_dropped: state.handle.history_dropped(),
        streams_open: MAX_STREAMS.saturating_sub(state.streams.available_permits()),
    };
    text_response(
        "text/plain; version=0.0.4; charset=utf-8",
        state.metrics.render(&gauges),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gauges() -> Gauges {
        Gauges {
            stations_known: 3,
            stations_connected: 2,
            faults: 1,
            fault_occurrences: 4,
            rules_global: 5,
            rules_station: 6,
            history_dropped: 7,
            streams_open: 1,
        }
    }

    fn call(direction: Direction, action: &str) -> CsmsEvent {
        CsmsEvent::Frame {
            station: "CP-1".to_owned(),
            direction,
            frame: Frame::Call {
                id: "1".into(),
                action: action.to_owned(),
                payload: serde_json::json!({}),
            },
        }
    }

    #[test]
    fn actions_outside_the_tables_share_one_label() {
        assert_eq!(known_action("Heartbeat"), "Heartbeat");
        assert_eq!(known_action("Reset"), "Reset");
        assert_eq!(known_action("Evil\"}\nname"), OTHER);
        assert_eq!(known_action(""), OTHER);
    }

    #[test]
    fn rendering_has_the_documented_families() {
        let metrics = Metrics::new();
        metrics.count_event(&call(Direction::In, "Heartbeat"));
        metrics.count_event(&call(Direction::In, "Heartbeat"));
        metrics.count_event(&call(Direction::In, "x\"y"));
        metrics.count_event(&call(Direction::Out, "Reset"));
        metrics.count_event(&CsmsEvent::Connected {
            station: "CP-1".to_owned(),
        });
        metrics.add_events_dropped(2);
        metrics.add_stream_events_dropped(3);
        metrics.count_response(StatusCode::OK);
        metrics.count_response(StatusCode::NOT_FOUND);
        metrics.count_response(StatusCode::NOT_FOUND);
        let text = metrics.render(&gauges());
        for line in [
            "kirby_csms_stations_known 3",
            "kirby_csms_stations_connected 2",
            "kirby_csms_faults_active 1",
            "kirby_csms_fault_occurrences_remaining 4",
            "kirby_csms_rules_global 5",
            "kirby_csms_rules_station 6",
            "kirby_csms_history_frames_dropped_total 7",
            "kirby_csms_events_dropped_total 2",
            "kirby_csms_stream_events_dropped_total 3",
            "kirby_csms_event_streams 1",
            "kirby_csms_calls_received_total{action=\"Heartbeat\"} 2",
            "kirby_csms_calls_received_total{action=\"other\"} 1",
            "kirby_csms_calls_sent_total{action=\"Reset\"} 1",
            "kirby_csms_api_requests_total{class=\"2xx\"} 1",
            "kirby_csms_api_requests_total{class=\"4xx\"} 2",
            "kirby_csms_api_requests_total{class=\"5xx\"} 0",
            "# TYPE kirby_csms_calls_received_total counter",
        ] {
            assert!(text.lines().any(|found| found == line), "{line}\n{text}");
        }
        assert!(
            text.lines()
                .any(|line| line.starts_with("kirby_csms_uptime_seconds "))
        );
        assert!(text.contains("kirby_csms_build_info{version=\""));
        assert!(text.ends_with('\n'));
        // Every sample line is `name{labels} value` or `name value`.
        for line in text.lines().filter(|line| !line.starts_with('#')) {
            let value = line.rsplit(' ').next().unwrap();
            assert!(value.parse::<u64>().is_ok(), "{line}");
        }
    }

    #[test]
    fn a_family_without_samples_still_has_one_line() {
        let text = Metrics::new().render(&gauges());
        assert!(text.contains("kirby_csms_calls_sent_total{action=\"other\"} 0\n"));
    }

    #[test]
    fn label_values_are_escaped() {
        assert_eq!(label("a\\b\"c\nd"), "a\\\\b\\\"c\\nd");
    }

    #[test]
    fn the_first_and_last_classes_are_counted() {
        let metrics = Metrics::new();
        metrics.count_response(StatusCode::from_u16(100).unwrap());
        metrics.count_response(StatusCode::from_u16(599).unwrap());
        let text = metrics.render(&gauges());
        assert!(text.contains("class=\"1xx\"} 1"));
        assert!(text.contains("class=\"5xx\"} 1"));
    }
}
