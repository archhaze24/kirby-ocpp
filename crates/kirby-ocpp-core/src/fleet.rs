//! Fleet mode: many independent stations started from one base configuration.
//!
//! A [`Fleet`] is nothing more than N ordinary [`Station`]s, one actor each, that
//! differ in their charge point id (expanded from a template, see [`FleetSpec`]). UIs
//! keep using [`StationHandle`] per station; the fleet adds what is shared: id
//! expansion, a merged and tagged event stream ([`FleetEvents`]), an aggregate
//! [`FleetSummary`] and one graceful shutdown.
//!
//! Every station persists to its own state file: the file name derives from the
//! (escaped) charge point id, and expanded ids are distinct.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::join_all;
use serde::Serialize;
use thiserror::Error;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

use crate::clock::Clock;
use crate::config::{ConfigError, StationConfig};
use crate::error::StationError;
use crate::station::{ConnectionState, Event, Station, StationHandle, TransactionState};

/// Most stations one fleet may hold.
pub const MAX_COUNT: u32 = 1000;
/// Widest zero padding accepted in `{n:WIDTH}`.
const MAX_WIDTH: usize = 9;
/// Capacity of the merged event queue (back-pressure for the per-station forwarders).
const EVENT_QUEUE: usize = 4096;

/// A fleet that cannot be started.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FleetError {
    #[error("count must be between 1 and {MAX_COUNT}, got {0}")]
    CountOutOfRange(u32),
    #[error(
        "the charge point id `{0}` has no `{{n}}` placeholder, so {1} stations would share one id; \
         use e.g. `CP-{{n:03}}`"
    )]
    NoPlaceholder(String, u32),
    #[error("charge point id template `{template}`: {reason}")]
    BadTemplate { template: String, reason: String },
    #[error("station index {first_index} + {count} stations overflows")]
    IndexOverflow { first_index: u32, count: u32 },
    #[error("station `{id}`: {source}")]
    Config {
        id: String,
        #[source]
        source: ConfigError,
    },
}

/// How many stations to start and how to name them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetSpec {
    /// Charge point id with `{n}` (index) or `{n:03}` (zero-padded to width 3)
    /// placeholders; `{{` and `}}` are literal braces.
    pub id_template: String,
    /// Index substituted into the first station's id.
    pub first_index: u32,
    /// Number of stations, `1..=`[`MAX_COUNT`].
    pub count: u32,
    /// Delay between starting two stations (see [`Fleet::connect_all`]).
    pub stagger_ms: u64,
}

impl FleetSpec {
    /// A spec with `first_index` 1 and no stagger.
    pub fn new(id_template: impl Into<String>, count: u32) -> Self {
        Self {
            id_template: id_template.into(),
            first_index: 1,
            count,
            stagger_ms: 0,
        }
    }

    /// The delay between two station starts.
    pub fn stagger(&self) -> Duration {
        Duration::from_millis(self.stagger_ms)
    }

    /// Checks the bounds and the template and returns the expanded ids in order.
    ///
    /// # Errors
    /// [`FleetError`] for a bad count, a bad template, or a template without a
    /// placeholder when `count > 1`.
    pub fn ids(&self) -> Result<Vec<String>, FleetError> {
        if !(1..=MAX_COUNT).contains(&self.count) {
            return Err(FleetError::CountOutOfRange(self.count));
        }
        let last =
            self.first_index
                .checked_add(self.count - 1)
                .ok_or(FleetError::IndexOverflow {
                    first_index: self.first_index,
                    count: self.count,
                })?;
        let template = Template::parse(&self.id_template)?;
        if self.count > 1 && !template.has_placeholder() {
            return Err(FleetError::NoPlaceholder(
                self.id_template.clone(),
                self.count,
            ));
        }
        Ok((self.first_index..=last)
            .map(|n| template.expand(n))
            .collect())
    }
}

/// Expands `template` for one index.
///
/// # Errors
/// [`FleetError::BadTemplate`] when the template is malformed.
pub fn expand_id(template: &str, index: u32) -> Result<String, FleetError> {
    Ok(Template::parse(template)?.expand(index))
}

#[derive(Debug)]
enum Part {
    Literal(String),
    Index { width: usize },
}

#[derive(Debug)]
struct Template(Vec<Part>);

impl Template {
    fn parse(template: &str) -> Result<Self, FleetError> {
        let bad = |reason: &str| FleetError::BadTemplate {
            template: template.to_owned(),
            reason: reason.to_owned(),
        };
        let mut parts = Vec::new();
        let mut literal = String::new();
        let mut chars = template.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    literal.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    literal.push('}');
                }
                '}' => return Err(bad("unmatched `}` (write `}}` for a literal brace)")),
                '{' => {
                    let mut inner = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(c) => inner.push(c),
                            None => return Err(bad("unclosed `{`")),
                        }
                    }
                    let width = match inner.strip_prefix('n') {
                        Some("") => 0,
                        Some(spec) => spec
                            .strip_prefix(':')
                            .filter(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_digit()))
                            .and_then(|w| w.parse::<usize>().ok())
                            .filter(|w| *w <= MAX_WIDTH)
                            .ok_or_else(|| {
                                bad(&format!(
                                    "`{{{inner}}}` is not a placeholder; use `{{n}}` or `{{n:WIDTH}}` \
                                     with WIDTH up to {MAX_WIDTH}"
                                ))
                            })?,
                        None => {
                            return Err(bad(&format!(
                                "unknown placeholder `{{{inner}}}`; only `{{n}}` and `{{n:WIDTH}}` exist"
                            )));
                        }
                    };
                    if !literal.is_empty() {
                        parts.push(Part::Literal(std::mem::take(&mut literal)));
                    }
                    parts.push(Part::Index { width });
                }
                c => literal.push(c),
            }
        }
        if !literal.is_empty() {
            parts.push(Part::Literal(literal));
        }
        Ok(Self(parts))
    }

    fn has_placeholder(&self) -> bool {
        self.0.iter().any(|p| matches!(p, Part::Index { .. }))
    }

    fn expand(&self, index: u32) -> String {
        let mut out = String::new();
        for part in &self.0 {
            match part {
                Part::Literal(text) => out.push_str(text),
                Part::Index { width } => {
                    let _ = write!(out, "{index:0width$}");
                }
            }
        }
        out
    }
}

/// An [`Event`] and the station it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct FleetEvent {
    pub station_id: String,
    pub event: Event,
}

/// One item of a [`FleetEvents`] stream.
#[derive(Debug, Clone, PartialEq)]
pub enum FleetItem {
    Event(FleetEvent),
    /// The consumer was too slow and `missed` events of this station were dropped.
    Lagged {
        station_id: String,
        missed: u64,
    },
}

/// Merged events of all stations of a fleet (see [`Fleet::subscribe`]).
#[derive(Debug)]
pub struct FleetEvents {
    rx: mpsc::Receiver<FleetItem>,
    stop: watch::Sender<bool>,
}

impl FleetEvents {
    /// The next item; `None` once [`FleetEvents::stop`] was called and everything queued
    /// has been delivered (or every station's event channel closed).
    pub async fn recv(&mut self) -> Option<FleetItem> {
        self.rx.recv().await
    }

    /// Asks the forwarders to deliver what is already queued per station and end.
    /// Keep calling [`FleetEvents::recv`] until it returns `None`.
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }
}

async fn forward(
    id: String,
    mut rx: broadcast::Receiver<Event>,
    tx: mpsc::Sender<FleetItem>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        let item = tokio::select! {
            biased;
            received = rx.recv() => match received {
                Ok(event) => FleetItem::Event(FleetEvent { station_id: id.clone(), event }),
                Err(broadcast::error::RecvError::Lagged(missed)) => FleetItem::Lagged {
                    station_id: id.clone(),
                    missed,
                },
                Err(broadcast::error::RecvError::Closed) => return,
            },
            _ = stop.changed() => break,
        };
        if tx.send(item).await.is_err() {
            return;
        }
    }
    // Stopped: whatever is still queued (the closing frames of a shutdown).
    loop {
        let item = match rx.try_recv() {
            Ok(event) => FleetItem::Event(FleetEvent {
                station_id: id.clone(),
                event,
            }),
            Err(broadcast::error::TryRecvError::Lagged(missed)) => FleetItem::Lagged {
                station_id: id.clone(),
                missed,
            },
            Err(_) => return,
        };
        if tx.send(item).await.is_err() {
            return;
        }
    }
}

/// Aggregate view of a fleet, from the stations' latest snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FleetSummary {
    pub total: usize,
    pub connected: usize,
    pub connecting: usize,
    pub backoff: usize,
    pub disconnected: usize,
    /// Stations whose BootNotification was accepted.
    pub registered: usize,
    /// Connectors with a running transaction, over all stations.
    pub active_transactions: usize,
    pub stations: Vec<StationSummary>,
}

/// One row of a [`FleetSummary`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StationSummary {
    pub id: String,
    pub connection: ConnectionState,
    pub registered: bool,
    pub active_transactions: usize,
    pub pending_offline_messages: u32,
}

/// A running fleet. Dropping it without [`Fleet::shutdown`] leaves the actors running
/// until their handles are dropped.
#[derive(Debug)]
pub struct Fleet {
    stations: Vec<(String, StationHandle)>,
    joins: Vec<JoinHandle<()>>,
    stagger: Duration,
}

impl Fleet {
    /// Validates everything first, then spawns one station per id. Stations do not
    /// connect by themselves: see [`Fleet::connect_all`]. Must run inside a tokio runtime.
    ///
    /// # Errors
    /// [`FleetError`] if the spec or any station's expanded config is invalid; nothing
    /// is spawned then.
    pub fn spawn(
        base: &StationConfig,
        spec: &FleetSpec,
        clock: &Arc<dyn Clock>,
    ) -> Result<Self, FleetError> {
        let configs = Self::configs(base, spec)?;
        let mut stations = Vec::with_capacity(configs.len());
        let mut joins = Vec::with_capacity(configs.len());
        for config in configs {
            let id = config.charge_point_id.clone();
            let (handle, join) = Station::spawn(config, Arc::clone(clock));
            stations.push((id, handle));
            joins.push(join);
        }
        Ok(Self {
            stations,
            joins,
            stagger: spec.stagger(),
        })
    }

    /// The validated per-station configs of `spec` (one per expanded id).
    ///
    /// # Errors
    /// See [`Fleet::spawn`].
    pub fn configs(
        base: &StationConfig,
        spec: &FleetSpec,
    ) -> Result<Vec<StationConfig>, FleetError> {
        spec.ids()?
            .into_iter()
            .map(|id| {
                let mut config = base.clone();
                config.charge_point_id.clone_from(&id);
                config.normalize();
                config
                    .validate()
                    .map_err(|source| FleetError::Config { id, source })?;
                Ok(config)
            })
            .collect()
    }

    /// Number of stations.
    pub fn len(&self) -> usize {
        self.stations.len()
    }

    /// Never true: a fleet has at least one station.
    pub fn is_empty(&self) -> bool {
        self.stations.is_empty()
    }

    /// `(charge point id, handle)` of every station, in index order.
    pub fn stations(&self) -> &[(String, StationHandle)] {
        &self.stations
    }

    /// The handle of the station with this charge point id.
    pub fn station(&self, id: &str) -> Option<&StationHandle> {
        self.stations
            .iter()
            .find(|(station_id, _)| station_id == id)
            .map(|(_, handle)| handle)
    }

    /// Delay between starting two stations.
    pub fn stagger(&self) -> Duration {
        self.stagger
    }

    /// Connects the stations one after another, waiting the stagger delay between two.
    /// Returns the stations whose `Connect` command failed.
    pub async fn connect_all(&self) -> Vec<(String, StationError)> {
        let mut failed = Vec::new();
        for (index, (id, handle)) in self.stations.iter().enumerate() {
            if index > 0 && !self.stagger.is_zero() {
                tokio::time::sleep(self.stagger).await;
            }
            if let Err(error) = handle.connect().await {
                failed.push((id.clone(), error));
            }
        }
        failed
    }

    /// Subscribes to all stations now (nothing after this call is missed) and merges
    /// their events, each tagged with its station id.
    pub fn subscribe(&self) -> FleetEvents {
        let (tx, rx) = mpsc::channel(EVENT_QUEUE);
        let (stop, stop_rx) = watch::channel(false);
        for (id, handle) in &self.stations {
            tokio::spawn(forward(
                id.clone(),
                handle.subscribe(),
                tx.clone(),
                stop_rx.clone(),
            ));
        }
        FleetEvents { rx, stop }
    }

    /// Aggregate of the stations' latest snapshots.
    pub fn summary(&self) -> FleetSummary {
        let mut summary = FleetSummary {
            total: self.stations.len(),
            connected: 0,
            connecting: 0,
            backoff: 0,
            disconnected: 0,
            registered: 0,
            active_transactions: 0,
            stations: Vec::with_capacity(self.stations.len()),
        };
        for (id, handle) in &self.stations {
            let snapshot = handle.snapshot();
            match snapshot.connection {
                ConnectionState::Connected => summary.connected += 1,
                ConnectionState::Connecting => summary.connecting += 1,
                ConnectionState::Backoff { .. } => summary.backoff += 1,
                ConnectionState::Disconnected => summary.disconnected += 1,
            }
            let registered = snapshot.booted();
            summary.registered += usize::from(registered);
            let active = snapshot
                .connectors
                .iter()
                .filter(|c| matches!(c.transaction, TransactionState::Active(_)))
                .count();
            summary.active_transactions += active;
            summary.stations.push(StationSummary {
                id: id.clone(),
                connection: snapshot.connection,
                registered,
                active_transactions: active,
                pending_offline_messages: snapshot.pending_offline_messages,
            });
        }
        summary
    }

    /// Closes every connection (WebSocket Close) and waits for all stations to end.
    pub async fn shutdown(&mut self) {
        // A station may already be gone; either way we are leaving.
        join_all(self.stations.iter().map(|(_, handle)| handle.shutdown())).await;
        join_all(std::mem::take(&mut self.joins)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_plain_and_padded_placeholders() {
        assert_eq!(expand_id("CP-{n}", 7).unwrap(), "CP-7");
        assert_eq!(expand_id("CP-{n:03}", 7).unwrap(), "CP-007");
        assert_eq!(expand_id("CP-{n:03}", 1234).unwrap(), "CP-1234");
        assert_eq!(expand_id("{n}-{n:2}", 5).unwrap(), "5-05");
        assert_eq!(expand_id("A{{{n}}}", 2).unwrap(), "A{2}");
    }

    #[test]
    fn malformed_templates_are_rejected() {
        for bad in [
            "CP-{n",
            "CP-}",
            "CP-{x}",
            "CP-{n:}",
            "CP-{n:ab}",
            "CP-{n:-3}",
            "CP-{n:10}",
            "CP-{}",
        ] {
            assert!(
                matches!(expand_id(bad, 1), Err(FleetError::BadTemplate { .. })),
                "{bad}"
            );
        }
    }

    #[test]
    fn spec_checks_count_and_placeholder() {
        let spec = |template: &str, first, count| FleetSpec {
            id_template: template.to_owned(),
            first_index: first,
            count,
            stagger_ms: 0,
        };
        assert_eq!(
            spec("CP-{n:02}", 9, 3).ids().unwrap(),
            ["CP-09", "CP-10", "CP-11"]
        );
        assert_eq!(spec("CP-001", 1, 1).ids().unwrap(), ["CP-001"]);
        assert!(matches!(
            spec("CP-001", 1, 2).ids(),
            Err(FleetError::NoPlaceholder(..))
        ));
        assert_eq!(
            spec("CP-{n}", 1, 0).ids(),
            Err(FleetError::CountOutOfRange(0))
        );
        assert_eq!(
            spec("CP-{n}", 1, 1001).ids(),
            Err(FleetError::CountOutOfRange(1001))
        );
        assert_eq!(spec("CP-{n}", 1, 1000).ids().unwrap().len(), 1000);
        assert!(matches!(
            spec("CP-{n}", u32::MAX, 2).ids(),
            Err(FleetError::IndexOverflow { .. })
        ));
    }
}
