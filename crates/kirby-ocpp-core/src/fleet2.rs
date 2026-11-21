//! Fleet mode for OCPP 2.0.1 and 2.1 stations.
//!
//! Each station has an independent connection, state file and actor. The fleet
//! expands ids from a shared configuration, merges events and coordinates shutdown.

use std::sync::Arc;
use std::time::Duration;

use futures_util::future::join_all;
use tokio::task::JoinHandle;

use crate::clock::Clock;
use crate::config::StationConfig;
use crate::error::StationError;
use crate::fleet::{Fleet, FleetError, FleetEvents, FleetSpec, FleetSummary, StationSummary};
use crate::station2::{Station2, Station2Handle};

/// A running OCPP 2.x fleet. Dropping it without [`Fleet2::shutdown`] leaves the actors running
/// until their handles are dropped.
#[derive(Debug)]
pub struct Fleet2 {
    stations: Vec<(String, Station2Handle)>,
    joins: Vec<JoinHandle<()>>,
    stagger: Duration,
}

impl Fleet2 {
    /// Validates everything first, then spawns one station per id. Stations do not
    /// connect by themselves: see [`Fleet2::connect_all`]. Must run inside a tokio runtime.
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
            let (handle, join) = Station2::spawn(config, Arc::clone(clock));
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
    /// See [`Fleet2::spawn`].
    pub fn configs(
        base: &StationConfig,
        spec: &FleetSpec,
    ) -> Result<Vec<StationConfig>, FleetError> {
        Fleet::configs(base, spec)
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
    pub fn stations(&self) -> &[(String, Station2Handle)] {
        &self.stations
    }

    /// The handle of the station with this charge point id.
    pub fn station(&self, id: &str) -> Option<&Station2Handle> {
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
        FleetEvents::new(
            self.stations
                .iter()
                .map(|(id, handle)| (id.clone(), handle.subscribe())),
        )
    }

    /// Aggregate of the stations' latest snapshots.
    pub fn summary(&self) -> FleetSummary {
        FleetSummary::from_stations(self.stations.iter().map(|(id, handle)| {
            let snapshot = handle.snapshot();
            StationSummary {
                id: id.clone(),
                connection: snapshot.connection,
                registered: snapshot.booted(),
                active_transactions: snapshot
                    .evses
                    .iter()
                    .filter(|evse| evse.transaction.is_some())
                    .count(),
                pending_offline_messages: snapshot.queued_events,
            }
        }))
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
    use crate::clock::SystemClock;
    use crate::ocpp::Protocol;
    use crate::station::ConnectionState;

    fn config() -> StationConfig {
        let mut config = StationConfig::new("ws://localhost:1/ocpp").without_persistence();
        config.protocol = Protocol::V201;
        Protocol::V201
            .token()
            .clone_into(&mut config.ws_subprotocol);
        config
    }

    #[test]
    fn invalid_ids_are_rejected_before_spawning() {
        // There is no runtime, so any attempt to spawn a station would panic.
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let err = Fleet2::spawn(&config(), &FleetSpec::new("CP-001", 2), &clock).unwrap_err();
        assert!(matches!(err, FleetError::NoPlaceholder(..)));
        let err = Fleet2::spawn(&config(), &FleetSpec::new("CP-\n{n}", 2), &clock).unwrap_err();
        assert!(matches!(err, FleetError::Config { .. }));
        let spec = FleetSpec {
            first_index: u32::MAX,
            ..FleetSpec::new("CP-{n}", 2)
        };
        assert!(matches!(
            Fleet2::spawn(&config(), &spec, &clock),
            Err(FleetError::IndexOverflow { .. })
        ));
    }

    #[tokio::test]
    async fn fresh_fleet_has_expanded_ids_and_disconnected_stations() {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let spec = FleetSpec {
            first_index: 9,
            stagger_ms: 5,
            ..FleetSpec::new("CP-{n:03}", 2)
        };
        let mut fleet = Fleet2::spawn(&config(), &spec, &clock).unwrap();
        assert_eq!(fleet.len(), 2);
        assert!(!fleet.is_empty());
        assert_eq!(fleet.stagger(), Duration::from_millis(5));
        let ids: Vec<_> = fleet.stations().iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["CP-009", "CP-010"]);
        assert_eq!(
            fleet.station("CP-010").unwrap().config().charge_point_id,
            "CP-010"
        );
        assert!(fleet.station("CP-011").is_none());

        assert_eq!(
            fleet.summary(),
            FleetSummary {
                total: 2,
                connected: 0,
                connecting: 0,
                backoff: 0,
                disconnected: 2,
                registered: 0,
                active_transactions: 0,
                stations: ids
                    .into_iter()
                    .map(|id| StationSummary {
                        id: id.to_owned(),
                        connection: ConnectionState::Disconnected,
                        registered: false,
                        active_transactions: 0,
                        pending_offline_messages: 0,
                    })
                    .collect(),
            }
        );
        tokio::time::timeout(Duration::from_secs(10), fleet.shutdown())
            .await
            .unwrap();
    }
}
