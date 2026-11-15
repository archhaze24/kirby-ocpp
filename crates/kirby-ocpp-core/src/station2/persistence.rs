//! Wiring of [`StationStore`] into the OCPP 2.0.1 actor.
//!
//! * **When:** state changes call [`Station2Actor::mark_dirty`]; the first one arms a 200 ms
//!   timer and the write happens when it fires, so a burst of changes is one write. Changes
//!   that must survive a crash (availability, a device-model change, transaction start and
//!   end, every queue mutation that changes what must survive) call
//!   [`Station2Actor::flush_now`] instead.
//! * **How:** the snapshot is built in the actor (no locks), the write runs on
//!   `spawn_blocking` and its result returns as `Internal2::Saved`. At most one write is in
//!   flight; changes that arrive meanwhile cause one more write afterwards, so overlapping
//!   writes cannot leave stale data behind.
//! * **Shutdown:** [`Station2Actor::finish_persistence`] awaits the in-flight write, then
//!   writes once more if anything is dirty.
//! * **Startup:** [`Persistence2::open`] reads the file synchronously and
//!   [`Station2Actor::restore_persisted`] applies it, so the first snapshot is the restored
//!   one. A corrupt or foreign file is moved to `.bak` and the station starts fresh; a file of
//!   a newer version disables persistence for the run and is never overwritten. The secrets
//!   file follows the same rule: one that cannot be read, or is newer, is never replaced.
//!
//! Restored: availability, device-model values, the authorization cache (expired entries are
//! dropped), transactions in progress (their owner ends them after the boot is accepted), the
//! event queue (every entry flagged `offline`: the process was down), the stopped ids and the
//! energy register of every EVSE.

use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::task::JoinHandle;

use super::actor::{Internal2, Station2Actor, TimerKey2};
use super::authorization::AuthCachePersist;
use super::device_model::DeviceModelPersist;
use super::transactions::TxPersist;
use super::tx_queue::QueuePersist;
use crate::config::StationConfig;
use crate::station::LogLevel;
use crate::store::{
    FileKind, LoadOutcome2, PersistedStation2, StationSecrets2, StationStore, StoreError,
};

/// Debounce between the first change and the write.
const FLUSH_DELAY: Duration = Duration::from_millis(200);

/// The state file of the station and the write bookkeeping.
#[derive(Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent write-scheduling flags"
)]
pub(crate) struct Persistence2 {
    /// `None` when persistence is off, or the file must not be touched during this run.
    store: Option<StationStore>,
    loaded: Option<PersistedStation2>,
    /// Secrets read at startup (restored once).
    loaded_secrets: Option<StationSecrets2>,
    /// The secrets file exists but could not be read (permissions, newer format): never
    /// overwrite it during this run.
    secrets_locked: bool,
    /// The secrets file has to be rewritten with the next write.
    secrets_dirty: bool,
    dirty: bool,
    in_flight: Option<JoinHandle<()>>,
    queued: bool,
    boot_count: u64,
}

impl Persistence2 {
    /// Opens the store of `config` (if persistence is on) and reads the state and secrets
    /// files. Messages for the log are appended to `logs`.
    pub(super) fn open(config: &StationConfig, logs: &mut Vec<(LogLevel, String)>) -> Self {
        let mut this = Self::default();
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
        let store = StationStore::new_for(FileKind::V2, &dir, &config.charge_point_id);
        match store.load_secrets2() {
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
        match store.load2() {
            Ok(LoadOutcome2::Loaded(state)) => {
                logs.push((
                    LogLevel::Info,
                    format!("restored station state from {}", store.path().display()),
                ));
                this.loaded = Some(*state);
                this.store = Some(store);
            }
            Ok(LoadOutcome2::Missing) => this.store = Some(store),
            Ok(LoadOutcome2::Quarantined { backup, reason }) => {
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
        this.boot_count = this
            .loaded
            .as_ref()
            .map_or(0, |state| state.boot_count)
            .saturating_add(1);
        this
    }
}

/// Drops the cache entries that are expired at `now`.
fn drop_expired(cache: &mut AuthCachePersist, now: DateTime<Utc>) {
    cache
        .entries
        .retain(|(_, entry)| entry.expires_at.is_none_or(|expires| expires > now));
}

impl Station2Actor {
    /// Applies the state read by [`Persistence2::open`] to the freshly built actor. Call it
    /// once, after the EVSEs and the device model exist.
    pub(super) fn restore_persisted(&mut self) {
        if let Some(secrets) = self.persistence.loaded_secrets.take() {
            self.restore_basic_auth_password(secrets.basic_auth_password);
        }
        let Some(mut persisted) = self.persistence.loaded.take() else {
            self.mark_dirty();
            return;
        };
        let now = self.clock.now_utc();
        drop_expired(&mut persisted.auth_cache, now);
        // The process was down: nothing queued has reached the CSMS in this session.
        for event in &mut persisted.queue.events {
            event.offline = true;
        }
        self.restore_availability(persisted.station_operative, &persisted.evse_availability);
        self.restore_device_model(persisted.device_model);
        self.restore_auth_cache(persisted.auth_cache);
        self.restore_transactions(persisted.tx);
        self.restore_queue(persisted.queue);
        // The new boot count is written with the next flush.
        self.mark_dirty();
    }

    /// The state worth keeping.
    fn build_persisted(&self) -> PersistedStation2 {
        PersistedStation2 {
            charge_point_id: self.config.charge_point_id.clone(),
            station_operative: self.station_operative(),
            evse_availability: self.availability_snapshot(),
            device_model: self.persist_device_model(),
            auth_cache: self.persist_auth_cache(),
            tx: self.persist_transactions(),
            queue: self.persist_queue(),
            boot_count: self.persistence.boot_count,
            ..PersistedStation2::default()
        }
    }

    fn build_secrets(&self) -> StationSecrets2 {
        StationSecrets2 {
            version: 0,
            basic_auth_password: self.basic_auth_password().map(str::to_owned),
        }
    }

    /// Takes the "secrets changed" flag; always false while the secrets file is locked.
    fn take_secrets_dirty(&mut self) -> bool {
        let dirty = std::mem::take(&mut self.persistence.secrets_dirty);
        dirty && !self.persistence.secrets_locked
    }

    /// A secret (the `BasicAuthPassword`) changed: write the secrets file soon.
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
        self.timers
            .set_if_idle(TimerKey2::PersistFlush, FLUSH_DELAY);
    }

    /// An important change: write now (still off the actor task, and still one at a time).
    pub(super) fn flush_now(&mut self) {
        self.mark_dirty();
        self.timers.cancel(&TimerKey2::PersistFlush);
        self.flush_persistence();
    }

    /// The debounce delay of the state file write elapsed.
    pub(super) fn on_persist_flush(&mut self) {
        self.flush_persistence();
    }

    /// Starts a write if there is something to write and none is running.
    fn flush_persistence(&mut self) {
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
        let secrets = self.take_secrets_dirty().then(|| self.build_secrets());
        let internal = self.internal_tx.clone();
        self.persistence.in_flight = Some(tokio::task::spawn_blocking(move || {
            // Secrets first: a crash in between leaves a password without its state, not the
            // reverse.
            let mut result = Ok(());
            if let Some(secrets) = secrets {
                result = store.save_secrets2(&secrets);
            }
            let result = result.and_then(|()| store.save2(&data));
            let _ = internal.send(Internal2::Saved(result));
        }));
    }

    /// A state file write finished.
    pub(super) fn on_store_saved(&mut self, result: Result<(), StoreError>) {
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
        self.timers.cancel(&TimerKey2::PersistFlush);
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
            .then(|| self.build_secrets());
        let write = move || match secrets {
            Some(secrets) => store
                .save_secrets2(&secrets)
                .and_then(|()| store.save2(&data)),
            None => store.save2(&data),
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

/// Adapters between the state file and the parts that own the data.
///
/// Each part owns one slice of the persisted state and must give it out and take it back; the
/// bodies forward to the owner's file (`device_model.rs`, `transactions.rs`, `tx_queue.rs`,
/// `authorization.rs`; `evse.rs` provides `availability_snapshot` and `restore_availability`).
/// Restoring transactions includes ending each restored active transaction with `PowerLoss` /
/// `AbnormalCondition` as the first queued event after an accepted boot.
impl Station2Actor {
    /// Device-model values that differ from their defaults, never secrets.
    fn persist_device_model(&self) -> DeviceModelPersist {
        self.dm.persist()
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "every restore adapter takes its part by value"
    )]
    fn restore_device_model(&mut self, values: DeviceModelPersist) {
        self.dm.restore(&values);
    }

    /// The `BasicAuthPassword` read from the secrets file.
    fn restore_basic_auth_password(&mut self, password: Option<String>) {
        self.dm.set_password(password);
    }

    /// The authorization cache.
    fn persist_auth_cache(&self) -> AuthCachePersist {
        self.auth_cache_persist()
    }

    fn restore_auth_cache(&mut self, cache: AuthCachePersist) {
        self.auth_cache_restore(cache);
    }

    /// Active transactions and the ids of the last stopped ones.
    fn persist_transactions(&self) -> TxPersist {
        self.tx_persist()
    }

    fn restore_transactions(&mut self, tx: TxPersist) {
        self.tx_restore(tx);
    }

    /// The queued transaction events.
    fn persist_queue(&self) -> QueuePersist {
        self.queue_persist()
    }

    fn restore_queue(&mut self, queue: QueuePersist) {
        self.queue_restore(queue);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::station2::CacheEntry;

    #[test]
    fn expired_cache_entries_are_dropped() {
        let now = Utc::now();
        let entry = |expires_at| CacheEntry {
            status: "Accepted".to_owned(),
            expires_at,
            group_id: None,
        };
        let mut cache = AuthCachePersist {
            entries: vec![
                (
                    "old".to_owned(),
                    entry(Some(now - chrono::Duration::seconds(1))),
                ),
                ("edge".to_owned(), entry(Some(now))),
                (
                    "new".to_owned(),
                    entry(Some(now + chrono::Duration::seconds(60))),
                ),
                ("forever".to_owned(), entry(None)),
            ],
        };
        drop_expired(&mut cache, now);
        let keys: Vec<_> = cache.entries.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["new", "forever"]);
    }
}
