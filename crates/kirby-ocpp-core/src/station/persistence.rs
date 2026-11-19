//! Wiring of [`StationStore`] into the actor.
//!
//! TS reference: `src/station/persistence.ts`.
//!
//! * **When:** state changes call `mark_dirty()`; the first one arms a ~200 ms timer and the
//!   write happens when it fires, so a burst of changes is one write. Rare, important events
//!   (configuration changes, local list, availability, connector added, boot accepted)
//!   call `flush_now()` instead. Nothing writes per meter tick (the meter energy is only marked dirty;
//!   the debounce coalesces it).
//! * **How:** the snapshot of the persisted data is built in the actor (no locks), the write
//!   runs on `spawn_blocking`, its result returns as `Internal::Saved`. At most one write is
//!   in flight; changes that arrive meanwhile cause one more write afterwards, so overlapping
//!   writes cannot leave stale data behind.
//! * **Shutdown:** the in-flight write is awaited, then a final write if anything is dirty.
//! * **Startup:** the file is read synchronously in `StationActor::new` (small, once) so the
//!   first snapshot is already the restored one. A corrupt file is quarantined by the store
//!   and the station starts fresh; a file from a *newer* version disables persistence for
//!   this run (the file is never overwritten).
//!
//! Restored: configuration values, local list and cache, charging profiles, availability,
//! connector count (only grows), the offline queue and per connector status, EV, meter,
//! reservation (expired ones are dropped after start) and transaction (a `Stopping` one becomes
//! a queued stop).

use std::path::Path;

use super::actor::{Internal, RestoredParts, StationActor, TimerKey};
use super::connector::{charge_point_status, new_connector};
use super::event::LogLevel;
use super::snapshot::Snapshot;
use crate::config::StationConfig;
use crate::ids::ConnectorId;
use crate::ocpp::types::{AvailabilityType, ConnectorStatus};
use crate::persist_driver::{FLUSH_DELAY, LoadedState, StateFiles, StateWriter};
use crate::station::TransactionState;
use crate::station::config_registry::ConfigSnapshot;
use crate::station::security::{MAX_QUEUED_EVENTS, PendingSecurityEvent};
use crate::store::{
    LoadOutcome, PersistedConnector, PersistedStation, StationSecrets, StationStore, StoreError,
};

/// A state file cannot make the station allocate more connectors than this (corrupt file guard).
const MAX_RESTORED_CONNECTORS: u32 = 4096;

/// The 1.6 state and secrets files.
#[derive(Clone)]
pub(super) struct Files(StationStore);

impl StateFiles for Files {
    type State = PersistedStation;
    type Secrets = StationSecrets;

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn load_secrets(&self) -> Result<StationSecrets, StoreError> {
        self.0.load_secrets()
    }

    fn load_state(&self) -> Result<LoadedState<PersistedStation>, StoreError> {
        Ok(match self.0.load()? {
            LoadOutcome::Loaded(state) => LoadedState::Loaded(state),
            LoadOutcome::Quarantined { backup, reason } => {
                LoadedState::Quarantined { backup, reason }
            }
            _ => LoadedState::Missing,
        })
    }

    fn save_secrets(&self, secrets: &StationSecrets) -> Result<(), StoreError> {
        self.0.save_secrets(secrets)
    }

    fn save_state(&self, state: &PersistedStation) -> Result<(), StoreError> {
        self.0.save(state)
    }
}

/// The state files of the station and the write bookkeeping.
pub(super) type Persistence = StateWriter<Files>;

impl Persistence {
    /// Opens the store of `config` (if persistence is on) and reads the state file.
    pub(super) fn open(config: &StationConfig, logs: &mut Vec<(LogLevel, String)>) -> Self {
        Self::open_with(config, |dir, id| Files(StationStore::new(dir, id)), logs)
    }

    /// Applies the loaded state (if any) to the freshly built actor state.
    #[allow(clippy::too_many_lines, reason = "one linear restore sequence")]
    pub(super) fn restore(
        &mut self,
        state: &mut Snapshot,
        parts: &mut RestoredParts,
        logs: &mut Vec<(LogLevel, String)>,
        restored_at: chrono::DateTime<chrono::Utc>,
    ) {
        if let Some(secrets) = self.loaded_secrets.take() {
            if let Some(key) = &secrets.authorization_key {
                parts.registry.set_authorization_key(key);
            }
            parts.secrets = secrets;
        }
        let Some(persisted) = self.loaded.take() else {
            return;
        };
        parts.certs = persisted.certificates;
        parts.security_events = persisted.security_events;
        parts.pending_profile_switch = persisted.pending_profile_switch;

        // The connector count only grows.
        let highest_saved = persisted
            .connectors
            .iter()
            .map(|c| c.id.get())
            .max()
            .unwrap_or(0);
        let count = persisted
            .connector_count
            .max(highest_saved)
            .min(MAX_RESTORED_CONNECTORS)
            .max(u32::try_from(state.connectors.len()).unwrap_or(u32::MAX));
        for id in (u32::try_from(state.connectors.len()).unwrap_or(0) + 1)..=count {
            state.connectors.push(new_connector(id));
        }
        parts.registry.set_connector_count(count);

        // Before format version 6 the key was stored but unused, so a saved value says
        // nothing: the station configuration (`--ws-ping-interval`) stays in force.
        let mut configuration = persisted.configuration;
        if persisted.version < 6 {
            configuration.retain(|key, _| {
                !key.eq_ignore_ascii_case(crate::station::config_registry::WEB_SOCKET_PING_INTERVAL)
            });
        }
        let skipped = parts.registry.restore(&ConfigSnapshot {
            values: configuration,
        });
        if !skipped.is_empty() {
            logs.push((
                LogLevel::Warn,
                format!(
                    "ignored persisted configuration keys: {}",
                    skipped.join(", ")
                ),
            ));
        }

        state.availability = persisted.charge_point_availability;
        state.charge_point.availability = persisted.charge_point_availability;
        state.charge_point.status = charge_point_status(
            persisted.charge_point_availability,
            ConnectorStatus::Available,
        );
        for saved in persisted.connectors {
            // Connector `0` holds the connector-0 reservation (no format change: older
            // builds skip the entry).
            if saved.id.is_station() {
                state.charge_point.reservation = saved.reservation;
                continue;
            }
            let Some(connector) = state.connectors.iter_mut().find(|c| c.id == saved.id) else {
                continue;
            };
            connector.availability = saved.availability;
            connector.pending_availability = saved.pending_availability;
            connector.meter_wh = saved.meter_wh;
            connector.last_id_tag = saved.last_id_tag;
            connector.ev_connected = saved.ev_connected;
            connector.reservation = saved.reservation;
            connector.transaction = saved.transaction;
            connector.status = saved.status;
            if saved.status == ConnectorStatus::Faulted {
                connector.error_code = crate::ocpp::types::ChargePointErrorCode::OtherError;
            }
            if saved.availability == AvailabilityType::Inoperative
                && connector.transaction.is_idle()
            {
                connector.status = ConnectorStatus::Unavailable;
            }
        }
        parts.queue = persisted.offline_queue;
        parts.samples = persisted.offline_samples;
        parts.samples.renumber();
        // An online start whose CALL was in flight at a crash left no queue entry: nobody
        // would ever resolve it, so the connector would stay blocked. It never happened.
        for connector in &mut state.connectors {
            if let crate::station::transaction::TransactionState::PendingStart(pending) =
                &connector.transaction
                && parts.queue.find_by_tx(pending.tx_id).is_none()
            {
                connector.transaction = crate::station::transaction::TransactionState::Idle;
                connector.status = if connector.availability == AvailabilityType::Inoperative {
                    ConnectorStatus::Unavailable
                } else if connector.ev_connected {
                    ConnectorStatus::Preparing
                } else {
                    ConnectorStatus::Available
                };
            }
        }
        // A stop that was in flight when the station died is queued, not repeated blindly.
        // Entries are matched per connector: CSMS transaction ids may repeat across
        // connectors (a hard Reset stops them all, and a CSMS may reuse one id).
        for connector in &mut state.connectors {
            if let crate::station::transaction::TransactionState::Stopping(stopping) =
                connector.transaction.clone()
            {
                connector.transaction = crate::station::transaction::TransactionState::Idle;
                let stop = crate::station::offline::StopInfo {
                    id_tag: stopping.id_tag.clone(),
                    meter_stop_wh: stopping.meter_stop_wh.unwrap_or(connector.meter_wh),
                    timestamp: stopping.stopped_at.unwrap_or(restored_at),
                    reason: stopping.reason,
                    transaction_data: Vec::new(),
                };
                if let Some(entry) = parts.queue.find_by_tx_on(connector.id, stopping.tx_id) {
                    entry.stop = Some(stop);
                    continue;
                }
                let key = parts.queue.keys().into_iter().max().unwrap_or(0) + 1;
                parts.queue.push(crate::station::offline::OfflineTx {
                    key,
                    connector: connector.id,
                    tx_id: stopping.tx_id,
                    id_tag: stopping
                        .id_tag
                        .unwrap_or_else(|| crate::ids::IdTag::new("UNKNOWN").expect("valid tag")),
                    started_at: stopping.stopped_at.unwrap_or(restored_at),
                    meter_start_wh: 0,
                    reservation_id: None,
                    start_pending: false,
                    stop: Some(stop),
                });
            }
        }

        {
            // A sample is kept only while its transaction is running on the connector or has an
            // offline queue entry; anything else is an orphan that would be re-sent forever.
            let queue = &parts.queue;
            let connectors = &state.connectors;
            parts.samples.retain_known(|c, t| {
                queue
                    .entries()
                    .iter()
                    .any(|e| e.connector == c && e.tx_id == t)
                    || connectors.iter().any(|k| k.id == c && k.tx_id() == Some(t))
            });
        }

        // A change that waited for a transaction which no longer exists (reset above) has
        // nothing left to wait for: apply it now.
        for connector in &mut state.connectors {
            if connector.transaction.is_idle()
                && let Some(kind) = connector.pending_availability.take()
            {
                connector.availability = kind;
                connector.status =
                    crate::station::connector::status_after_availability_change(connector, kind);
            }
        }

        parts.local_list = persisted.local_authorization_list;
        parts.auth_cache = persisted.authorization_cache;
        parts.profiles = persisted.charging_profiles;
    }
}

impl StationActor {
    /// The state worth keeping.
    fn build_persisted(&self) -> PersistedStation {
        PersistedStation {
            charge_point_id: self.config.charge_point_id.clone(),
            charge_point_availability: self.state.charge_point.availability,
            connector_count: u32::try_from(self.state.connectors.len()).unwrap_or(u32::MAX),
            connectors: self
                .state
                .charge_point
                .reservation
                .iter()
                .map(|reservation| PersistedConnector {
                    id: ConnectorId::STATION,
                    status: self.state.charge_point.status,
                    availability: self.state.charge_point.availability,
                    pending_availability: None,
                    ev_connected: false,
                    meter_wh: 0,
                    last_id_tag: None,
                    reservation: Some(reservation.clone()),
                    transaction: TransactionState::Idle,
                })
                .chain(self.state.connectors.iter().map(|c| PersistedConnector {
                    id: c.id,
                    status: c.status,
                    availability: c.availability,
                    pending_availability: c.pending_availability,
                    ev_connected: c.ev_connected,
                    meter_wh: c.meter_wh,
                    last_id_tag: c.last_id_tag.clone(),
                    reservation: c.reservation.clone(),
                    transaction: c.transaction.clone(),
                }))
                .collect(),
            local_authorization_list: self.local_list.clone(),
            authorization_cache: self.auth_cache.clone(),
            charging_profiles: self.profiles.clone(),
            offline_queue: self.queue.clone(),
            offline_samples: self.samples.clone(),
            configuration: self.registry.snapshot().values,
            certificates: self.certs.clone(),
            security_events: self.persisted_security_events(),
            pending_profile_switch: self.pending_profile_switch,
            ..PersistedStation::default()
        }
    }

    /// Events in flight come first (they are older), then the queue; bounded like the queue.
    fn persisted_security_events(&self) -> Vec<PendingSecurityEvent> {
        let mut events: Vec<PendingSecurityEvent> = self
            .security_in_flight
            .iter()
            .map(|(_, e)| e.clone())
            .chain(self.security_events.iter().cloned())
            .collect();
        let excess = events.len().saturating_sub(MAX_QUEUED_EVENTS);
        events.drain(..excess);
        events
    }

    pub(super) fn persistence_secrets_locked(&self) -> bool {
        self.persistence.secrets_locked
    }

    /// A secret (authorization key, private key) changed: write the secrets file soon.
    pub(super) fn mark_secrets_dirty(&mut self) {
        self.persistence.secrets_dirty = true;
        self.mark_dirty();
    }

    /// Something worth persisting changed: write soon (debounced).
    pub(super) fn mark_dirty(&mut self) {
        if self.persistence.mark_dirty() {
            self.timers.set_if_idle(TimerKey::PersistFlush, FLUSH_DELAY);
        }
    }

    /// An important change: write now (still off the actor task, and still one at a time).
    pub(super) fn flush_now(&mut self) {
        self.mark_dirty();
        self.timers.cancel(&TimerKey::PersistFlush);
        self.flush_persistence();
    }

    /// The debounce timer fired, or a flush was requested: start a write if needed.
    pub(super) fn flush_persistence(&mut self) {
        self.flush_meters();
        let Some(due) = self.persistence.begin_write() else {
            return;
        };
        let data = self.build_persisted();
        let secrets = due.with_secrets.then(|| self.secrets.clone());
        let internal = self.internal_tx.clone();
        self.persistence
            .spawn_write(due.files, data, secrets, move |result| {
                let _ = internal.send(Internal::Saved(result));
            });
    }

    pub(super) fn on_saved(&mut self, result: Result<(), StoreError>) {
        let (error, rerun) = self.persistence.finish_write(result);
        if let Some(error) = error {
            self.log(
                LogLevel::Error,
                format!("could not save station state: {error}"),
            );
        }
        if rerun {
            self.flush_persistence();
        }
    }

    /// Final write at shutdown: wait for the one in flight, then write what is dirty.
    pub(super) async fn finish_persistence(&mut self) {
        self.timers.cancel(&TimerKey::PersistFlush);
        let Some(due) = self.persistence.begin_final_write().await else {
            return;
        };
        let data = self.build_persisted();
        let secrets = due.with_secrets.then(|| self.secrets.clone());
        if let Some(message) = self
            .persistence
            .run_final_write(due.files, data, secrets)
            .await
        {
            self.log(LogLevel::Error, message);
        }
    }
}
