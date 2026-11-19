//! Write pipeline of the station state files, shared by the 1.6 and the 2.x station actors.
//!
//! * **Open:** [`StateWriter::open_with`] reads the secrets file and the state file once, at
//!   startup, and reports what happened in `logs`. A corrupt file is quarantined by the store
//!   and the station starts fresh; a file that cannot be read, or that is newer, disables
//!   persistence for the run (the file is never overwritten). A secrets file that cannot be
//!   read is never replaced either.
//! * **Write:** the actor marks the state dirty, arms its debounce timer, builds the snapshot
//!   and hands it to [`StateWriter::spawn_write`], which saves it on `spawn_blocking`. At most
//!   one write is in flight; changes that arrive meanwhile cause one more write afterwards, so
//!   overlapping writes cannot leave stale data behind. The secrets file is written first.
//! * **Shutdown:** [`StateWriter::begin_final_write`] awaits the write in flight and says
//!   whether another one is due.
//!
//! The actors keep what differs: the timer, the snapshot and secrets builders and the
//! delivery of the write result.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::config::StationConfig;
use crate::station::LogLevel;
use crate::store::StoreError;

/// Debounce between the first change and the write.
pub(crate) const FLUSH_DELAY: Duration = Duration::from_millis(200);

/// Outcome of reading a state file.
pub(crate) enum LoadedState<S> {
    /// No file yet.
    Missing,
    Loaded(Box<S>),
    /// The file was unusable and has been moved to `backup`; start from scratch.
    Quarantined {
        backup: PathBuf,
        reason: String,
    },
}

/// The files of one protocol family: a state file and a secrets file.
pub(crate) trait StateFiles: Clone + Send + 'static {
    type State: Send + 'static;
    type Secrets: Send + 'static;

    fn path(&self) -> &Path;
    fn load_secrets(&self) -> Result<Self::Secrets, StoreError>;
    fn load_state(&self) -> Result<LoadedState<Self::State>, StoreError>;
    fn save_secrets(&self, secrets: &Self::Secrets) -> Result<(), StoreError>;
    fn save_state(&self, state: &Self::State) -> Result<(), StoreError>;
}

/// The state files of a station and the write bookkeeping.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent write-scheduling flags"
)]
pub(crate) struct StateWriter<F: StateFiles> {
    /// `None` when persistence is off, or the file must not be touched during this run.
    files: Option<F>,
    /// The state read at startup (restored once).
    pub(crate) loaded: Option<F::State>,
    /// Secrets read at startup (restored once).
    pub(crate) loaded_secrets: Option<F::Secrets>,
    /// The secrets file has to be rewritten with the next write.
    pub(crate) secrets_dirty: bool,
    /// The secrets file exists but could not be read (permissions, newer format): never
    /// overwrite it during this run.
    pub(crate) secrets_locked: bool,
    /// Startup got as far as reading the files (persistence on, state directory known).
    pub(crate) files_probed: bool,
    dirty: bool,
    in_flight: Option<JoinHandle<()>>,
    queued: bool,
}

/// A write that is due: the files to write and whether the secrets file goes with it.
pub(crate) struct DueWrite<F> {
    pub(crate) files: F,
    pub(crate) with_secrets: bool,
}

impl<F: StateFiles> StateWriter<F> {
    /// A writer with persistence off.
    pub(crate) fn disabled() -> Self {
        Self {
            files: None,
            loaded: None,
            loaded_secrets: None,
            secrets_dirty: false,
            secrets_locked: false,
            files_probed: false,
            dirty: false,
            in_flight: None,
            queued: false,
        }
    }

    /// Opens the files of `config` (if persistence is on; `make_files` builds them from the
    /// state directory and the charge point id) and reads them. Messages for the log are
    /// appended to `logs`.
    pub(crate) fn open_with(
        config: &StationConfig,
        make_files: impl FnOnce(&Path, &str) -> F,
        logs: &mut Vec<(LogLevel, String)>,
    ) -> Self {
        let mut this = Self::disabled();
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
        let files = make_files(&dir, &config.charge_point_id);
        this.files_probed = true;
        match files.load_secrets() {
            Ok(secrets) => this.loaded_secrets = Some(secrets),
            Err(error @ StoreError::CaseCollision { .. }) => {
                logs.push((
                    LogLevel::Warn,
                    format!("{error}; state is not persisted during this run"),
                ));
                return this;
            }
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
        match files.load_state() {
            Ok(LoadedState::Loaded(state)) => {
                logs.push((
                    LogLevel::Info,
                    format!("restored station state from {}", files.path().display()),
                ));
                this.loaded = Some(*state);
                this.files = Some(files);
            }
            Ok(LoadedState::Missing) => this.files = Some(files),
            Ok(LoadedState::Quarantined { backup, reason }) => {
                logs.push((
                    LogLevel::Warn,
                    format!(
                        "state file was unusable ({reason}); moved to {} and starting fresh",
                        backup.display()
                    ),
                ));
                this.files = Some(files);
            }
            Err(error @ StoreError::UnsupportedVersion { .. }) => logs.push((
                LogLevel::Error,
                format!("{error}; state is not persisted during this run"),
            )),
            Err(error @ StoreError::CaseCollision { .. }) => {
                this.loaded_secrets = None;
                logs.push((
                    LogLevel::Warn,
                    format!("{error}; state is not persisted during this run"),
                ));
            }
            // The file exists but cannot be read: a save would replace it by rename.
            Err(error) => logs.push((
                LogLevel::Warn,
                format!("could not read the station state: {error}; state is not persisted during this run"),
            )),
        }
        this
    }

    /// Takes the "secrets changed" flag; always false while the secrets file is locked.
    pub(crate) fn take_secrets_dirty(&mut self) -> bool {
        let dirty = std::mem::take(&mut self.secrets_dirty);
        dirty && !self.secrets_locked
    }

    /// Marks the state as changed. Returns `true` when the debounce timer has to be armed
    /// (persistence is on).
    pub(crate) fn mark_dirty(&mut self) -> bool {
        if self.files.is_none() {
            return false;
        }
        self.dirty = true;
        true
    }

    /// A write is due now when the state is dirty and none is in flight. A change that
    /// arrives during a write is queued for one more write afterwards.
    pub(crate) fn begin_write(&mut self) -> Option<DueWrite<F>> {
        let files = self.files.clone()?;
        if !self.dirty {
            return None;
        }
        if self.in_flight.is_some() {
            self.queued = true;
            return None;
        }
        self.dirty = false;
        let with_secrets = self.take_secrets_dirty();
        Some(DueWrite {
            files,
            with_secrets,
        })
    }

    /// Saves `state` (and `secrets` first, when given) off the actor task; `done` receives
    /// the result.
    pub(crate) fn spawn_write(
        &mut self,
        files: F,
        state: F::State,
        secrets: Option<F::Secrets>,
        done: impl FnOnce(Result<(), StoreError>) + Send + 'static,
    ) {
        self.in_flight = Some(tokio::task::spawn_blocking(move || {
            // Secrets first: a crash in between leaves a secret without its state, not the
            // reverse.
            let mut result = Ok(());
            if let Some(secrets) = secrets {
                result = files.save_secrets(&secrets);
            }
            let result = result.and_then(|()| files.save_state(&state));
            done(result);
        }));
    }

    /// A write finished. Returns the error to log (the data stays marked dirty, so a later
    /// change retries) and whether a write that was queued meanwhile is due.
    pub(crate) fn finish_write(
        &mut self,
        result: Result<(), StoreError>,
    ) -> (Option<StoreError>, bool) {
        self.in_flight = None;
        let error = result.err();
        if error.is_some() {
            self.dirty = true;
            self.secrets_dirty = true;
        }
        let rerun = std::mem::take(&mut self.queued);
        (error, rerun)
    }

    /// Shutdown: waits for the write in flight, then says whether a final write is due.
    pub(crate) async fn begin_final_write(&mut self) -> Option<DueWrite<F>> {
        let mut had_in_flight = false;
        if let Some(handle) = self.in_flight.take() {
            let _ = handle.await;
            // Its result message is never processed, so do not trust it: write again.
            had_in_flight = true;
        }
        let files = self.files.clone()?;
        let needs_write = self.dirty || self.queued || had_in_flight || self.secrets_dirty;
        if !needs_write {
            return None;
        }
        let secrets_changed = self.take_secrets_dirty();
        let with_secrets = (secrets_changed || had_in_flight) && !self.secrets_locked;
        Some(DueWrite {
            files,
            with_secrets,
        })
    }

    /// Runs the final write and waits for it. Returns the message to log when it failed.
    pub(crate) async fn run_final_write(
        &mut self,
        files: F,
        state: F::State,
        secrets: Option<F::Secrets>,
    ) -> Option<String> {
        let write = move || match secrets {
            Some(secrets) => files
                .save_secrets(&secrets)
                .and_then(|()| files.save_state(&state)),
            None => files.save_state(&state),
        };
        let failure = match tokio::task::spawn_blocking(write).await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(format!("could not save station state: {error}")),
            Err(error) => Some(format!("state writer failed: {error}")),
        };
        self.dirty = false;
        self.queued = false;
        failure
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone)]
    struct FakeFiles {
        path: PathBuf,
        saved: Arc<Mutex<Vec<String>>>,
    }

    impl StateFiles for FakeFiles {
        type State = u32;
        type Secrets = u32;

        fn path(&self) -> &Path {
            &self.path
        }

        fn load_secrets(&self) -> Result<u32, StoreError> {
            Ok(0)
        }

        fn load_state(&self) -> Result<LoadedState<u32>, StoreError> {
            Ok(LoadedState::Missing)
        }

        fn save_secrets(&self, secrets: &u32) -> Result<(), StoreError> {
            self.saved
                .lock()
                .unwrap()
                .push(format!("secrets {secrets}"));
            Ok(())
        }

        fn save_state(&self, state: &u32) -> Result<(), StoreError> {
            self.saved.lock().unwrap().push(format!("state {state}"));
            Ok(())
        }
    }

    fn writer() -> (StateWriter<FakeFiles>, Arc<Mutex<Vec<String>>>) {
        let saved = Arc::new(Mutex::new(Vec::new()));
        let mut writer = StateWriter::disabled();
        writer.files = Some(FakeFiles {
            path: PathBuf::from("state.json"),
            saved: Arc::clone(&saved),
        });
        (writer, saved)
    }

    #[test]
    fn a_disabled_writer_never_writes() {
        let mut writer = StateWriter::<FakeFiles>::disabled();
        assert!(!writer.mark_dirty());
        assert!(writer.begin_write().is_none());
    }

    #[tokio::test]
    async fn changes_during_a_write_cause_one_more_write() {
        let (mut writer, saved) = writer();
        assert!(writer.mark_dirty());
        writer.secrets_dirty = true;
        let due = writer.begin_write().expect("a write is due");
        assert!(due.with_secrets);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        writer.spawn_write(due.files, 1, Some(7), move |result| {
            let _ = sender.send(result);
        });
        writer.mark_dirty();
        writer.mark_dirty();
        assert!(writer.begin_write().is_none(), "one write at a time");
        assert!(receiver.await.expect("write result").is_ok());
        let (error, rerun) = writer.finish_write(Ok(()));
        assert!(error.is_none());
        assert!(rerun, "the change made meanwhile is written afterwards");
        let due = writer.begin_write().expect("the second write is due");
        assert!(!due.with_secrets);
        assert_eq!(*saved.lock().unwrap(), ["secrets 7", "state 1"]);
    }

    #[tokio::test]
    async fn a_failed_write_keeps_the_data_dirty() {
        let (mut writer, _saved) = writer();
        writer.mark_dirty();
        writer.begin_write().expect("a write is due");
        let failure = StoreError::UnsupportedVersion {
            path: PathBuf::from("state.json"),
            found: 99,
            supported: 1,
        };
        let (error, rerun) = writer.finish_write(Err(failure));
        assert!(error.is_some());
        assert!(!rerun);
        assert!(writer.secrets_dirty);
        assert!(writer.begin_write().is_some(), "the next flush retries");
    }

    #[tokio::test]
    async fn the_final_write_covers_a_write_that_was_in_flight() {
        let (mut writer, saved) = writer();
        writer.mark_dirty();
        let due = writer.begin_write().expect("a write is due");
        writer.spawn_write(due.files, 1, None, |_| {});
        let due = writer
            .begin_final_write()
            .await
            .expect("an in-flight write is not trusted");
        assert!(due.with_secrets);
        let failure = writer.run_final_write(due.files, 2, Some(3)).await;
        assert!(failure.is_none());
        assert_eq!(*saved.lock().unwrap(), ["state 1", "secrets 3", "state 2"]);
        assert!(writer.begin_final_write().await.is_none());
    }
}
