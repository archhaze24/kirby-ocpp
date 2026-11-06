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

use std::time::Duration;

use tokio::task::JoinHandle;

use super::actor::{Internal, RestoredParts, StationActor, TimerKey};
use super::connector::{charge_point_status, new_connector};
use super::event::LogLevel;
use super::snapshot::Snapshot;
use crate::config::StationConfig;
use crate::ocpp::types::{AvailabilityType, ConnectorStatus};
use crate::station::config_registry::ConfigSnapshot;
use crate::station::security::{MAX_QUEUED_EVENTS, PendingSecurityEvent};
use crate::store::{
    LoadOutcome, PersistedConnector, PersistedStation, StationSecrets, StationStore, StoreError,
};

/// Debounce between the first change and the write.
pub(super) const FLUSH_DELAY: Duration = Duration::from_millis(200);

/// A state file cannot make the station allocate more connectors than this (corrupt file guard).
const MAX_RESTORED_CONNECTORS: u32 = 4096;

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent write-scheduling flags"
)]
pub(super) struct Persistence {
    store: Option<StationStore>,
    loaded: Option<PersistedStation>,
    dirty: bool,
    in_flight: Option<JoinHandle<()>>,
    queued: bool,
    /// Secrets read at startup (restored once).
    loaded_secrets: Option<StationSecrets>,
    /// The secrets file has to be rewritten with the next write.
    pub(super) secrets_dirty: bool,
    /// The secrets file exists but could not be read (permissions, newer format): never
    /// overwrite it during this run.
    secrets_locked: bool,
}

impl Persistence {
    /// Opens the store of `config` (if persistence is on) and reads the state file.
    pub(super) fn open(config: &StationConfig, logs: &mut Vec<(LogLevel, String)>) -> Self {
        let mut this = Self {
            store: None,
            loaded: None,
            dirty: false,
            in_flight: None,
            queued: false,
            loaded_secrets: None,
            secrets_dirty: false,
            secrets_locked: false,
        };
        if !config.persist {
            return this;
        }
        let Some(dir) = config.state_dir_or_default() else {
            logs.push((
                LogLevel::Warn,
                "no state directory could be determined; state is not persisted".to_owned(),
            ));
            return this;
        };
        let store = StationStore::new(&dir, &config.charge_point_id);
        match store.load_secrets() {
            Ok(secrets) => this.loaded_secrets = Some(secrets),
            Err(error) => {
                // A corrupt file was moved aside (reported as `InvalidData`), so writing is
                // safe; anything else leaves the original in place and must not be replaced.
                let moved_aside = matches!(
                    &error,
                    StoreError::Io { source, .. } if source.kind() == std::io::ErrorKind::InvalidData
                );
                this.secrets_locked = !moved_aside;
                let note = if moved_aside {
                    ""
                } else {
                    "; secrets are not persisted during this run"
                };
                logs.push((
                    LogLevel::Warn,
                    format!("could not read the station secrets: {error}{note}"),
                ));
            }
        }
        match store.load() {
            Ok(LoadOutcome::Loaded(state)) => {
                logs.push((
                    LogLevel::Info,
                    format!("restored station state from {}", store.path().display()),
                ));
                this.loaded = Some(*state);
                this.store = Some(store);
            }
            Ok(LoadOutcome::Missing) => this.store = Some(store),
            Ok(LoadOutcome::Quarantined { backup, reason }) => {
                logs.push((
                    LogLevel::Warn,
                    format!(
                        "state file was unusable ({reason}); moved to {} and starting fresh",
                        backup.display()
                    ),
                ));
                this.store = Some(store);
            }
            Err(error @ StoreError::UnsupportedVersion { .. }) => logs.push((
                LogLevel::Error,
                format!("{error}; state is not persisted during this run"),
            )),
            Err(error) => {
                logs.push((
                    LogLevel::Warn,
                    format!("could not read the station state: {error}"),
                ));
                this.store = Some(store);
            }
        }
        this
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
                .connectors
                .iter()
                .map(|c| PersistedConnector {
                    id: c.id,
                    status: c.status,
                    availability: c.availability,
                    pending_availability: c.pending_availability,
                    ev_connected: c.ev_connected,
                    meter_wh: c.meter_wh,
                    last_id_tag: c.last_id_tag.clone(),
                    reservation: c.reservation.clone(),
                    transaction: c.transaction.clone(),
                })
                .collect(),
            local_authorization_list: self.local_list.clone(),
            authorization_cache: self.auth_cache.clone(),
            charging_profiles: self.profiles.clone(),
            offline_queue: self.queue.clone(),
            offline_samples: self.samples.clone(),
            configuration: self.registry.snapshot().values,
            certificates: self.certs.clone(),
            security_events: self.persisted_security_events(),
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

    /// A secret (authorization key, private key) changed: write the secrets file soon.
    /// Takes the "secrets changed" flag; always false while the secrets file is locked.
    fn take_secrets_dirty(&mut self) -> bool {
        let dirty = std::mem::take(&mut self.persistence.secrets_dirty);
        dirty && !self.persistence.secrets_locked
    }

    pub(super) fn mark_secrets_dirty(&mut self) {
        self.persistence.secrets_dirty = true;
        self.mark_dirty();
    }

    /// Something worth persisting changed: write soon (debounced).
    pub(super) fn mark_dirty(&mut self) {
        if self.persistence.store.is_none() {
            return;
        }
        self.persistence.dirty = true;
        self.timers.set_if_idle(TimerKey::PersistFlush, FLUSH_DELAY);
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
        let Some(store) = self.persistence.store.clone() else {
            return;
        };
        if !self.persistence.dirty {
            return;
        }
        if self.persistence.in_flight.is_some() {
            self.persistence.queued = true;
            return;
        }
        self.persistence.dirty = false;
        let data = self.build_persisted();
        let secrets = self.take_secrets_dirty().then(|| self.secrets.clone());
        let internal = self.internal_tx.clone();
        self.persistence.in_flight = Some(tokio::task::spawn_blocking(move || {
            // Secrets first: a crash in between leaves a key without its chain, not the reverse.
            let mut result = Ok(());
            if let Some(secrets) = secrets {
                result = store.save_secrets(&secrets);
            }
            let result = result.and_then(|()| store.save(&data));
            let _ = internal.send(Internal::Saved(result));
        }));
    }

    pub(super) fn on_saved(&mut self, result: Result<(), StoreError>) {
        self.persistence.in_flight = None;
        if let Err(error) = result {
            // Keep the data marked dirty so a later change retries the write.
            self.persistence.dirty = true;
            self.persistence.secrets_dirty = true;
            self.log(
                LogLevel::Error,
                format!("could not save station state: {error}"),
            );
        }
        if self.persistence.queued {
            self.persistence.queued = false;
            self.flush_persistence();
        }
    }

    /// Final write at shutdown: wait for the one in flight, then write what is dirty.
    pub(super) async fn finish_persistence(&mut self) {
        self.timers.cancel(&TimerKey::PersistFlush);
        let mut had_in_flight = false;
        if let Some(handle) = self.persistence.in_flight.take() {
            let _ = handle.await;
            // Its result message is never processed, so do not trust it: write again.
            had_in_flight = true;
        }
        let Some(store) = self.persistence.store.clone() else {
            return;
        };
        let needs_write = self.persistence.dirty
            || self.persistence.queued
            || had_in_flight
            || self.persistence.secrets_dirty;
        if !needs_write {
            return;
        }
        let data = self.build_persisted();
        let secrets_changed = self.take_secrets_dirty();
        let secrets = ((secrets_changed || had_in_flight) && !self.persistence.secrets_locked)
            .then(|| self.secrets.clone());
        let write = move || match secrets {
            Some(secrets) => store
                .save_secrets(&secrets)
                .and_then(|()| store.save(&data)),
            None => store.save(&data),
        };
        match tokio::task::spawn_blocking(write).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => self.log(
                LogLevel::Error,
                format!("could not save station state: {error}"),
            ),
            Err(error) => self.log(LogLevel::Error, format!("state writer failed: {error}")),
        }
        self.persistence.dirty = false;
        self.persistence.queued = false;
    }
}
