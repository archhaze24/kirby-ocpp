//! Versioned, atomically written station state on disk.
//!
//! One JSON file per charge point: `<dir>/<escaped charge point id>.station.json`.
//! The `.station.json` suffix is new on purpose: the TS implementation wrote
//! `<base64url(id)>.json` and its files are neither read nor overwritten.
//!
//! * **Atomic:** the file is written to a temp file in the same directory,
//!   fsynced, then renamed over the target (`tempfile::NamedTempFile::persist`).
//!   A crash leaves either the old or the new file, never a torn one.
//! * **Private:** temp files are created `0600` on Unix, and `persist` keeps that.
//! * **Versioned:** [`PersistedStation::version`] is checked before the rest is
//!   decoded. A *newer* version is an error and the file is left alone; a file that
//!   is corrupt, or belongs to another charge point, is moved to `<file>.bak` and
//!   [`LoadOutcome::Quarantined`] is returned so the station can start fresh without
//!   silently destroying data (a TS bug).
//!
//! All methods are blocking file I/O; call them from `spawn_blocking`. Debouncing
//! writes is the station's job, not the store's.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::ids::{ConnectorId, IdTag};
use crate::ocpp::types::{AvailabilityType, ConnectorStatus};
use crate::station::authorization::AuthorizationCache;
use crate::station::charging_profiles::ChargingProfileStore;
use crate::station::local_auth_list::LocalAuthList;
use crate::station::offline::OfflineQueue;
use crate::station::reservation::Reservation;
use crate::station::transaction::TransactionState;

/// Current on-disk format version.
pub const STORE_VERSION: u32 = 1;

/// Everything that survives a restart.
///
/// Everything is restored on start: configuration, lists, profiles, availability, connector
/// count, and per connector status, EV, meter, reservation, transaction, plus the offline queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedStation {
    pub version: u32,
    pub charge_point_id: String,
    /// Availability of connector 0 (`ChangeAvailability` with `connectorId` 0).
    pub charge_point_availability: AvailabilityType,
    /// Number of connectors (`connectors.len()` normally equals it; the count only grows).
    pub connector_count: u32,
    pub connectors: Vec<PersistedConnector>,
    /// The local authorization list including its version.
    pub local_authorization_list: LocalAuthList,
    pub authorization_cache: AuthorizationCache,
    pub charging_profiles: ChargingProfileStore,
    /// Transactions whose start and/or stop still has to reach the CSMS.
    pub offline_queue: OfflineQueue,
    /// Mutable configuration key values.
    pub configuration: BTreeMap<String, String>,
}

impl PersistedStation {
    /// An empty state for `charge_point_id` at the current version.
    pub fn new(charge_point_id: impl Into<String>) -> Self {
        Self {
            version: STORE_VERSION,
            charge_point_id: charge_point_id.into(),
            ..Self::default()
        }
    }
}

impl Default for PersistedStation {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            charge_point_id: String::new(),
            charge_point_availability: AvailabilityType::Operative,
            connector_count: 0,
            connectors: Vec::new(),
            local_authorization_list: LocalAuthList::default(),
            authorization_cache: AuthorizationCache::default(),
            charging_profiles: ChargingProfileStore::default(),
            offline_queue: OfflineQueue::default(),
            configuration: BTreeMap::new(),
        }
    }
}

/// Persisted part of a connector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedConnector {
    pub id: ConnectorId,
    pub status: ConnectorStatus,
    /// Persisted (the TS code lost it across restarts).
    pub availability: AvailabilityType,
    pub ev_connected: bool,
    pub meter_wh: u64,
    pub last_id_tag: Option<IdTag>,
    pub reservation: Option<Reservation>,
    pub transaction: TransactionState,
}

/// Why a store operation failed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("cannot serialize station state: {0}")]
    Serialize(serde_json::Error),
    /// The file was written by a newer version of this program; it was left untouched.
    #[error("{path} has format version {found}, this build supports up to {supported}")]
    UnsupportedVersion {
        path: PathBuf,
        found: u64,
        supported: u32,
    },
}

/// Result of [`StationStore::load`].
#[derive(Debug)]
#[non_exhaustive]
pub enum LoadOutcome {
    /// No file yet.
    Missing,
    Loaded(Box<PersistedStation>),
    /// The file was unusable and has been moved to `backup`; start from scratch.
    Quarantined {
        backup: PathBuf,
        reason: String,
    },
}

/// Handle to the state file of one charge point.
#[derive(Debug, Clone)]
pub struct StationStore {
    path: PathBuf,
    charge_point_id: String,
}

impl StationStore {
    /// The store for `charge_point_id` inside `dir` (nothing is touched until used).
    pub fn new(dir: impl AsRef<Path>, charge_point_id: &str) -> Self {
        Self {
            path: dir.as_ref().join(file_name(charge_point_id)),
            charge_point_id: charge_point_id.to_owned(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the state file. See the module docs for the recovery rules.
    pub fn load(&self) -> Result<LoadOutcome, StoreError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(LoadOutcome::Missing),
            Err(source) => return Err(self.io_error(source)),
        };

        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(e) => return self.quarantine(format!("not valid JSON: {e}")),
        };
        match value.get("version").and_then(Value::as_u64) {
            Some(found) if found > u64::from(STORE_VERSION) => {
                return Err(StoreError::UnsupportedVersion {
                    path: self.path.clone(),
                    found,
                    supported: STORE_VERSION,
                });
            }
            Some(_) => {}
            None => return self.quarantine("missing format version".to_owned()),
        }
        let state: PersistedStation = match serde_json::from_value(value) {
            Ok(state) => state,
            Err(e) => return self.quarantine(format!("unexpected structure: {e}")),
        };
        if state.charge_point_id != self.charge_point_id {
            return self.quarantine(format!(
                "belongs to charge point `{}`, expected `{}`",
                state.charge_point_id, self.charge_point_id
            ));
        }
        Ok(LoadOutcome::Loaded(Box::new(state)))
    }

    /// Atomically replaces the state file (creating the directory if needed).
    pub fn save(&self, state: &PersistedStation) -> Result<(), StoreError> {
        let json = serde_json::to_vec_pretty(state).map_err(StoreError::Serialize)?;
        let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(dir).map_err(|e| StoreError::Io {
            path: dir.to_owned(),
            source: e,
        })?;
        let mut temp = tempfile::NamedTempFile::new_in(dir).map_err(|e| self.io_error(e))?;
        temp.write_all(&json).map_err(|e| self.io_error(e))?;
        temp.as_file().sync_all().map_err(|e| self.io_error(e))?;
        temp.persist(&self.path)
            .map_err(|e| self.io_error(e.error))?;
        Ok(())
    }

    fn quarantine(&self, reason: String) -> Result<LoadOutcome, StoreError> {
        let mut backup = self.path.clone().into_os_string();
        backup.push(".bak");
        let backup = PathBuf::from(backup);
        // `rename` does not replace an existing file on Windows.
        match fs::remove_file(&backup) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(StoreError::Io {
                    path: backup,
                    source,
                });
            }
        }
        fs::rename(&self.path, &backup).map_err(|e| self.io_error(e))?;
        Ok(LoadOutcome::Quarantined { backup, reason })
    }

    fn io_error(&self, source: io::Error) -> StoreError {
        StoreError::Io {
            path: self.path.clone(),
            source,
        }
    }
}

/// File name for a charge point id: `[A-Za-z0-9_-]` and non-leading `.` are kept, every
/// other byte becomes `%XX`, so any id maps to a distinct, filesystem-safe name.
fn file_name(charge_point_id: &str) -> String {
    let mut name = String::with_capacity(charge_point_id.len() + 13);
    for (index, byte) in charge_point_id.bytes().enumerate() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' => name.push(char::from(byte)),
            b'.' if index > 0 => name.push('.'),
            _ => {
                let _ = write!(name, "%{byte:02X}");
            }
        }
    }
    if name.is_empty() {
        name.push_str("%00empty");
    }
    name.push_str(".station.json");
    name
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::*;
    use crate::ids::TxId;
    use crate::station::transaction::ActiveTransaction;

    fn sample() -> PersistedStation {
        let mut state = PersistedStation::new("CP-1");
        state.connector_count = 1;
        state.charge_point_availability = AvailabilityType::Inoperative;
        state
            .configuration
            .insert("HeartbeatInterval".into(), "60".into());
        state.connectors.push(PersistedConnector {
            id: ConnectorId::new(1),
            status: ConnectorStatus::Charging,
            availability: AvailabilityType::Inoperative,
            ev_connected: true,
            meter_wh: 1234,
            last_id_tag: Some(IdTag::new("TAG").unwrap()),
            reservation: None,
            transaction: TransactionState::Active(ActiveTransaction {
                tx_id: TxId::Csms(0),
                id_tag: IdTag::new("TAG").unwrap(),
                started_at: DateTime::<Utc>::UNIX_EPOCH,
                meter_start_wh: 1000,
            }),
        });
        state
    }

    fn loaded(outcome: LoadOutcome) -> PersistedStation {
        match outcome {
            LoadOutcome::Loaded(state) => *state,
            other => panic!("expected Loaded, got {other:?}"),
        }
    }

    #[test]
    fn save_then_load_roundtrips_including_availability_and_tx_id_zero() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "CP-1");
        store.save(&sample()).unwrap();
        let state = loaded(store.load().unwrap());
        assert_eq!(state, sample());
        assert_eq!(
            state.connectors[0].availability,
            AvailabilityType::Inoperative
        );
        assert_eq!(state.connectors[0].transaction.tx_id(), Some(TxId::Csms(0)));
    }

    #[test]
    fn missing_file_is_reported_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path().join("nested"), "CP-1");
        assert!(matches!(store.load().unwrap(), LoadOutcome::Missing));
    }

    #[test]
    fn save_creates_the_directory_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        let store = StationStore::new(&nested, "CP-1");
        store.save(&sample()).unwrap();
        store.save(&sample()).unwrap();
        let names: Vec<_> = fs::read_dir(&nested)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["CP-1.station.json"]);
    }

    #[test]
    fn corrupt_file_is_quarantined_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "CP-1");
        fs::write(store.path(), b"{ torn").unwrap();
        let LoadOutcome::Quarantined { backup, reason } = store.load().unwrap() else {
            panic!("expected quarantine");
        };
        assert!(reason.contains("not valid JSON"));
        assert_eq!(fs::read(&backup).unwrap(), b"{ torn");
        assert!(!store.path().exists());
        assert!(matches!(store.load().unwrap(), LoadOutcome::Missing));
    }

    #[test]
    fn file_of_another_charge_point_or_without_version_is_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "CP-1");
        let other = StationStore::new(dir.path(), "CP-2");
        other.save(&PersistedStation::new("CP-2")).unwrap();
        fs::copy(other.path(), store.path()).unwrap();
        assert!(matches!(
            store.load().unwrap(),
            LoadOutcome::Quarantined { reason, .. } if reason.contains("CP-2")
        ));

        fs::write(store.path(), br#"{"charge_point_id":"CP-1"}"#).unwrap();
        assert!(matches!(
            store.load().unwrap(),
            LoadOutcome::Quarantined { .. }
        ));
    }

    #[test]
    fn newer_version_is_an_error_and_the_file_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "CP-1");
        let text = br#"{"version":99,"future":true}"#;
        fs::write(store.path(), text).unwrap();
        let err = store.load().unwrap_err();
        assert!(matches!(
            err,
            StoreError::UnsupportedVersion {
                found: 99,
                supported: STORE_VERSION,
                ..
            }
        ));
        assert_eq!(fs::read(store.path()).unwrap(), text);
    }

    #[test]
    fn missing_optional_sections_default_when_the_version_is_present() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "CP-1");
        fs::write(store.path(), br#"{"version":1,"charge_point_id":"CP-1"}"#).unwrap();
        let state = loaded(store.load().unwrap());
        assert_eq!(state, PersistedStation::new("CP-1"));
    }

    #[test]
    fn file_names_are_filesystem_safe_and_injective_enough() {
        assert_eq!(file_name("CP-001"), "CP-001.station.json");
        assert_eq!(
            file_name("../etc/passwd"),
            "%2E.%2Fetc%2Fpasswd.station.json"
        );
        assert_eq!(file_name("a b"), "a%20b.station.json");
        assert_eq!(file_name("é"), "%C3%A9.station.json");
        assert_ne!(file_name("a/b"), file_name("a%2Fb"));
        assert!(file_name("").starts_with("%00empty"));
    }

    #[cfg(unix)]
    #[test]
    fn state_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "CP-1");
        store.save(&sample()).unwrap();
        let mode = fs::metadata(store.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
