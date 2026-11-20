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
//! dropped), reservations, transactions in progress (their owner ends them after the boot is accepted), the
//! event queue (every entry flagged `offline`: the process was down), the stopped ids and the
//! energy register of every EVSE.

use std::path::Path;

use chrono::{DateTime, Utc};

use super::actor::{Internal2, Station2Actor, TimerKey2};
use super::authorization::AuthCachePersist;
use super::device_model::DeviceModelPersist;
use super::transactions::TxPersist;
use super::tx_queue::QueuePersist;
use crate::config::StationConfig;
use crate::persist_driver::{FLUSH_DELAY, LoadedState, StateFiles, StateWriter};
use crate::station::LogLevel;
use crate::store::{
    FileKind, LoadOutcome2, PersistedStation2, StationSecrets2, StationStore, StoreError,
};

/// The 2.x state and secrets files.
#[derive(Clone)]
struct Files2(StationStore);

impl StateFiles for Files2 {
    type State = PersistedStation2;
    type Secrets = StationSecrets2;

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn load_secrets(&self) -> Result<StationSecrets2, StoreError> {
        self.0.load_secrets2()
    }

    fn load_state(&self) -> Result<LoadedState<PersistedStation2>, StoreError> {
        Ok(match self.0.load2()? {
            LoadOutcome2::Loaded(state) => LoadedState::Loaded(state),
            LoadOutcome2::Quarantined { backup, reason } => {
                LoadedState::Quarantined { backup, reason }
            }
            _ => LoadedState::Missing,
        })
    }

    fn save_secrets(&self, secrets: &StationSecrets2) -> Result<(), StoreError> {
        self.0.save_secrets2(secrets)
    }

    fn save_state(&self, state: &PersistedStation2) -> Result<(), StoreError> {
        self.0.save2(state)
    }
}

/// The state file of the station and the write bookkeeping.
pub(crate) struct Persistence2 {
    writer: StateWriter<Files2>,
    boot_count: u64,
}

impl Persistence2 {
    /// Opens the store of `config` (if persistence is on) and reads the state and secrets
    /// files. Messages for the log are appended to `logs`.
    pub(super) fn open(config: &StationConfig, logs: &mut Vec<(LogLevel, String)>) -> Self {
        let writer = StateWriter::open_with(
            config,
            |dir, id| Files2(StationStore::new_for(FileKind::V2, dir, id)),
            logs,
        );
        // Without persistence (or a state directory) the count stays 0.
        let boot_count = if writer.files_probed {
            writer
                .loaded
                .as_ref()
                .map_or(0, |state| state.boot_count)
                .saturating_add(1)
        } else {
            0
        };
        Self { writer, boot_count }
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
        if let Some(secrets) = self.persistence.writer.loaded_secrets.take() {
            self.restore_basic_auth_password(secrets.basic_auth_password);
        }
        let Some(mut persisted) = self.persistence.writer.loaded.take() else {
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
        self.reservations_restore(persisted.reservations);
        // The new boot count is written with the next flush.
        self.mark_dirty();
    }

    /// The state worth keeping.
    fn build_persisted(&self) -> PersistedStation2 {
        PersistedStation2 {
            charge_point_id: self.config.charge_point_id.clone(),
            protocol: self.protocol().token().to_owned(),
            station_operative: self.station_operative(),
            evse_availability: self.availability_snapshot(),
            device_model: self.persist_device_model(),
            auth_cache: self.persist_auth_cache(),
            tx: self.persist_transactions(),
            queue: self.persist_queue(),
            reservations: self.reservations_persist(),
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

    /// A secret (the `BasicAuthPassword`) changed: write the secrets file soon.
    pub(super) fn mark_secrets_dirty(&mut self) {
        self.persistence.writer.secrets_dirty = true;
        self.mark_dirty();
    }

    /// Something worth persisting changed: write soon (debounced).
    pub(super) fn mark_dirty(&mut self) {
        if self.persistence.writer.mark_dirty() {
            self.timers
                .set_if_idle(TimerKey2::PersistFlush, FLUSH_DELAY);
        }
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
        let Some(due) = self.persistence.writer.begin_write() else {
            return;
        };
        let data = self.build_persisted();
        let secrets = due.with_secrets.then(|| self.build_secrets());
        let internal = self.internal_tx.clone();
        self.persistence
            .writer
            .spawn_write(due.files, data, secrets, move |result| {
                let _ = internal.send(Internal2::Saved(result));
            });
    }

    /// A state file write finished.
    pub(super) fn on_store_saved(&mut self, result: Result<(), StoreError>) {
        let (error, rerun) = self.persistence.writer.finish_write(result);
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
        self.timers.cancel(&TimerKey2::PersistFlush);
        let Some(due) = self.persistence.writer.begin_final_write().await else {
            return;
        };
        let data = self.build_persisted();
        let secrets = due.with_secrets.then(|| self.build_secrets());
        if let Some(message) = self
            .persistence
            .writer
            .run_final_write(due.files, data, secrets)
            .await
        {
            self.log(LogLevel::Error, message);
        }
    }
}

/// Adapters between the state file and the parts that own the data.
///
/// Each part owns one slice of the persisted state and must give it out and take it back; the
/// bodies forward to the module that holds the data (`device_model.rs`, `transactions.rs`, `tx_queue.rs`,
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
    use crate::station2::{CacheEntry, LocalListPersist};

    #[test]
    fn expired_cache_entries_are_dropped() {
        let now = Utc::now();
        let entry = |expires_at| CacheEntry {
            status: "Accepted".to_owned(),
            expires_at,
            group_id: None,
        };
        let mut cache = AuthCachePersist {
            local_list: LocalListPersist::default(),
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
