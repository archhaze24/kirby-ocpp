//! Versioned, atomically written station state on disk.
//!
//! One JSON file per charge point: `<dir>/<escaped charge point id>.station.json`.
//! The `.station.json` suffix is new on purpose: the TS implementation wrote
//! `<base64url(id)>.json` and its files are neither read nor overwritten.
//!
//! * **Atomic:** the file is written to a temp file in the same directory,
//!   fsynced, then renamed over the target (`tempfile::NamedTempFile::persist`) and the
//!   directory is fsynced (Unix) so the rename itself survives a power loss.
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
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::ids::{ConnectorId, IdTag};
use crate::ocpp::Protocol;
use crate::ocpp::types::{AvailabilityType, ConnectorStatus};
use crate::station::authorization::AuthorizationCache;
use crate::station::certificates::CertificateStore;
use crate::station::charging_profiles::ChargingProfileStore;
use crate::station::local_auth_list::LocalAuthList;
use crate::station::offline::{OfflineQueue, SampleQueue};
use crate::station::reservation::Reservation;
use crate::station::security::PendingSecurityEvent;
use crate::station::transaction::TransactionState;
use crate::station2::{
    AuthCachePersist, DeviceModelPersist, EvseAvail, QueuePersist, ReservationPersist, TxPersist,
};

/// Current on-disk format version.
/// Version 3 added the install time of charging profiles (`StoredProfile::installed_at`).
/// Version 4 added the certificate store and the queued security events (older files load with
/// both empty; secrets live in a separate file, see [`StationSecrets`]).
/// Version 5 added the fallback flag of installed roots (`AdditionalRootCertificateCheck`;
/// older files load with no fallback).
/// Version 6 added the stop time and meter reading of a `Stopping` transaction (older files
/// load without them and use the restore time).
/// Version 7 added the previous charge point certificate chain and its key (the old/new
/// certificate alternation, `A02.FR.28`; older files load without them).
///
/// Version 8 added the queue of transaction-related `MeterValues` samples taken while offline
/// (`offline_samples`; older files load with it empty).
///
/// Version 9 added the pending security profile switch (`pending_profile_switch`, the
/// `A05.FR.08` fallback; older files load without one) and the previous `AuthorizationKey` in
/// the secrets file.
pub const STORE_VERSION: u32 = 9;

/// A change of `SecurityProfile` that has not connected yet (`A05.FR.08`): the profile that was
/// in force before it and the profile-attributable connection failures since.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingProfileSwitch {
    pub previous_profile: u8,
    pub failures: u32,
}

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
    /// `MeterValues` samples of those transactions taken while offline (added in version 8).
    pub offline_samples: SampleQueue,
    /// Mutable configuration key values.
    pub configuration: BTreeMap<String, String>,
    /// Installed root certificates and the station's own chain (public data; added in version 4).
    pub certificates: CertificateStore,
    /// Security events waiting for a connection (added in version 4).
    pub security_events: Vec<PendingSecurityEvent>,
    /// A security profile change waiting for its first successful connection (added in
    /// version 9).
    pub pending_profile_switch: Option<PendingProfileSwitch>,
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
            offline_samples: SampleQueue::default(),
            configuration: BTreeMap::new(),
            certificates: CertificateStore::default(),
            security_events: Vec::new(),
            pending_profile_switch: None,
        }
    }
}

/// Current on-disk format version of the OCPP 2.0.1 station state and its secrets file.
pub const STORE2_VERSION: u32 = 2;

/// Everything an OCPP 2.0.1 station keeps across a restart. It has its own file kind and
/// version, independent of [`PersistedStation`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedStation2 {
    pub version: u32,
    pub charge_point_id: String,
    /// The subprotocol token of the station that wrote the file: `ocpp2.0.1` or `ocpp2.1`. A
    /// 2.1 station reads and writes the same files as a 2.0.1 one; nothing checks this field
    /// on load.
    pub protocol: String,
    /// Availability of the whole station.
    pub station_operative: bool,
    pub evse_availability: Vec<EvseAvail>,
    /// Device-model values that differ from their defaults; never secrets.
    pub device_model: DeviceModelPersist,
    pub auth_cache: AuthCachePersist,
    /// Transactions in progress and the ids of the last stopped ones.
    pub tx: TxPersist,
    /// Transaction events waiting for delivery.
    pub queue: QueuePersist,
    /// Reservations (added without a version bump: a file without them loads as none).
    pub reservations: ReservationPersist,
    /// How many times the station booted.
    pub boot_count: u64,
}

impl PersistedStation2 {
    /// An empty state for `charge_point_id` at the current version.
    pub fn new(charge_point_id: impl Into<String>) -> Self {
        Self {
            charge_point_id: charge_point_id.into(),
            ..Self::default()
        }
    }
}

impl Default for PersistedStation2 {
    fn default() -> Self {
        Self {
            version: STORE2_VERSION,
            charge_point_id: String::new(),
            protocol: Protocol::V201.token().to_owned(),
            station_operative: true,
            evse_availability: Vec::new(),
            device_model: DeviceModelPersist::default(),
            auth_cache: AuthCachePersist::default(),
            tx: TxPersist::default(),
            queue: QueuePersist::default(),
            reservations: ReservationPersist::default(),
            boot_count: 0,
        }
    }
}

/// Secrets of one OCPP 2.0.1 charge point, kept out of the main state file, written atomically
/// with mode `0600` on Unix.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StationSecrets2 {
    pub version: u32,
    /// `SecurityCtrlr.BasicAuthPassword`.
    pub basic_auth_password: Option<String>,
}

impl std::fmt::Debug for StationSecrets2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StationSecrets2").finish_non_exhaustive()
    }
}

/// Secrets of one charge point, kept out of the main state file: `<name>.station.secrets.json`,
/// written atomically with mode `0600` on Unix (like the state file).
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StationSecrets {
    pub version: u32,
    /// The `AuthorizationKey` (Basic Auth password).
    pub authorization_key: Option<String>,
    /// Private key (PEM) of the installed charge point certificate.
    pub charge_point_key_pem: Option<String>,
    /// Private key (PEM) of a `SignCertificate` request that has not been answered yet.
    pub pending_key_pem: Option<String>,
    /// Private key (PEM) of the previous charge point certificate, kept until the new one has
    /// connected (added in version 7).
    pub previous_charge_point_key_pem: Option<String>,
    /// The `AuthorizationKey` before the latest change, kept until a connection proves the
    /// current one so the profile fallback can restore a working credential (added in
    /// version 9).
    pub previous_authorization_key: Option<String>,
}

impl std::fmt::Debug for StationSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StationSecrets").finish_non_exhaustive()
    }
}

impl StationSecrets {
    /// Nothing secret is held.
    pub fn is_empty(&self) -> bool {
        self.authorization_key.is_none()
            && self.charge_point_key_pem.is_none()
            && self.pending_key_pem.is_none()
            && self.previous_charge_point_key_pem.is_none()
            && self.previous_authorization_key.is_none()
    }
}

/// Persisted part of a connector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedConnector {
    pub id: ConnectorId,
    pub status: ConnectorStatus,
    /// Persisted (the TS code lost it across restarts).
    pub availability: AvailabilityType,
    /// A `ChangeAvailability` answered `Scheduled`, waiting for the transaction to end
    /// (added in format version 2).
    #[serde(default)]
    pub pending_availability: Option<AvailabilityType>,
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
    #[error(
        "state file \"{}\" belongs to a station id that differs only by case on a case-insensitive file system",
        crate::text::escape_unsafe(.found)
    )]
    CaseCollision { found: String },
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

/// Result of [`StationStore::inspect`]: like [`LoadOutcome`], but nothing is moved aside.
#[derive(Debug)]
#[non_exhaustive]
pub enum Inspection {
    /// No file.
    Missing,
    Loaded(Box<PersistedStation>),
    /// The file cannot be used (corrupt, or it belongs to another charge point).
    Unusable {
        reason: String,
    },
}

/// Which protocol family a state file belongs to. Each family has its own file names and
/// format version, so a station id used with both protocols keeps two independent files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// OCPP 1.6: `<id>.station.json`, version [`STORE_VERSION`].
    V16,
    /// OCPP 2.x: `<id>.station2.json`, version [`STORE2_VERSION`].
    V2,
}

impl FileKind {
    /// The kind of state file a station of `protocol` uses.
    #[must_use]
    pub const fn for_protocol(protocol: Protocol) -> Self {
        match protocol {
            Protocol::V16 => Self::V16,
            Protocol::V201 | Protocol::V21 => Self::V2,
        }
    }

    /// Suffix of the state file name.
    #[must_use]
    pub const fn state_suffix(self) -> &'static str {
        match self {
            Self::V16 => ".station.json",
            Self::V2 => ".station2.json",
        }
    }

    /// Suffix of the secrets file name.
    #[must_use]
    pub const fn secrets_suffix(self) -> &'static str {
        match self {
            Self::V16 => ".station.secrets.json",
            Self::V2 => ".station2.secrets.json",
        }
    }

    /// The newest format version this build reads and writes for the kind.
    #[must_use]
    pub const fn version(self) -> u32 {
        match self {
            Self::V16 => STORE_VERSION,
            Self::V2 => STORE2_VERSION,
        }
    }
}

/// Result of [`StationStore::load2`].
#[derive(Debug)]
#[non_exhaustive]
pub enum LoadOutcome2 {
    /// No file yet.
    Missing,
    Loaded(Box<PersistedStation2>),
    /// The file was unusable and has been moved to `backup`; start from scratch.
    Quarantined {
        backup: PathBuf,
        reason: String,
    },
}

/// Result of [`StationStore::inspect2`]: like [`LoadOutcome2`], but nothing is moved aside.
#[derive(Debug)]
#[non_exhaustive]
pub enum Inspection2 {
    /// No file.
    Missing,
    Loaded(Box<PersistedStation2>),
    /// The file cannot be used (corrupt, or it belongs to another charge point).
    Unusable {
        reason: String,
    },
}

/// Handle to the state file of one charge point.
#[derive(Debug, Clone)]
pub struct StationStore {
    path: PathBuf,
    charge_point_id: String,
    kind: FileKind,
}

impl StationStore {
    /// The OCPP 1.6 store for `charge_point_id` inside `dir` (nothing is touched until used).
    pub fn new(dir: impl AsRef<Path>, charge_point_id: &str) -> Self {
        Self::new_for(FileKind::V16, dir, charge_point_id)
    }

    /// The store of the given file kind for `charge_point_id` inside `dir`. Use the `*2`
    /// methods ([`StationStore::load2`], [`StationStore::save2`], ...) with [`FileKind::V2`].
    pub fn new_for(kind: FileKind, dir: impl AsRef<Path>, charge_point_id: &str) -> Self {
        Self {
            path: dir.as_ref().join(file_name(kind, charge_point_id)),
            charge_point_id: charge_point_id.to_owned(),
            kind,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file kind of this store.
    #[must_use]
    pub fn kind(&self) -> FileKind {
        self.kind
    }

    /// Reads the state file. See the module docs for the recovery rules.
    pub fn load(&self) -> Result<LoadOutcome, StoreError> {
        match self.read_state()? {
            Inspection::Missing => Ok(LoadOutcome::Missing),
            Inspection::Loaded(state) => Ok(LoadOutcome::Loaded(state)),
            Inspection::Unusable { reason } => {
                let backup = self.move_aside(&self.path, &self.secrets_path())?;
                Ok(LoadOutcome::Quarantined { backup, reason })
            }
        }
    }

    /// Reads the state file without changing anything: a file that [`StationStore::load`]
    /// would move aside is reported as [`Inspection::Unusable`] and stays where it is.
    pub fn inspect(&self) -> Result<Inspection, StoreError> {
        self.read_state()
    }

    fn read_state(&self) -> Result<Inspection, StoreError> {
        check_file_alias(&self.path)?;
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Inspection::Missing),
            Err(source) => return Err(self.io_error(source)),
        };
        let unusable = |reason: String| Ok(Inspection::Unusable { reason });

        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(e) => return unusable(format!("not valid JSON: {e}")),
        };
        self.check_case_collision(&self.path, &value)?;
        match value.get("version").and_then(Value::as_u64) {
            Some(found) if found > u64::from(STORE_VERSION) => {
                return Err(StoreError::UnsupportedVersion {
                    path: self.path.clone(),
                    found,
                    supported: STORE_VERSION,
                });
            }
            Some(_) => {}
            None => return unusable("missing format version".to_owned()),
        }
        let state: PersistedStation = match serde_json::from_value(value) {
            Ok(state) => state,
            Err(e) => return unusable(format!("unexpected structure: {e}")),
        };
        if state.charge_point_id != self.charge_point_id {
            return unusable(format!(
                "belongs to charge point `{}`, expected `{}`",
                crate::text::escape_unsafe(&state.charge_point_id),
                crate::text::escape_unsafe(&self.charge_point_id),
            ));
        }
        Ok(Inspection::Loaded(Box::new(state)))
    }

    /// Whether a secrets file exists next to the state file. Nothing is read.
    #[must_use]
    pub fn has_secrets(&self) -> bool {
        self.secrets_path().is_file()
    }

    /// The files [`StationStore::remove`] deletes: the state file, the secrets file and their
    /// `.bak` copies. All of them sit directly in the state directory.
    #[must_use]
    pub fn owned_files(&self) -> Vec<PathBuf> {
        let state = self.path.clone();
        let secrets = self.secrets_path();
        vec![
            state.clone(),
            backup_path(&state),
            secrets.clone(),
            backup_path(&secrets),
        ]
    }

    /// Deletes the persisted state of this charge point (see [`StationStore::owned_files`])
    /// and returns the files that existed. Nothing is deleted when the state file or the
    /// secrets file, or a `.bak` copy of either, has a case collision or was written
    /// by a newer version.
    pub fn remove(&self) -> Result<Vec<PathBuf>, StoreError> {
        let files = self.owned_files();
        check_file_aliases(&files)?;
        let supported = self.kind.version();
        for path in &files {
            let Ok(bytes) = fs::read(path) else { continue };
            let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            self.check_case_collision(path, &value)?;
            let found = value.get("version").and_then(Value::as_u64);
            if let Some(found) = found.filter(|found| *found > u64::from(supported)) {
                return Err(StoreError::UnsupportedVersion {
                    path: path.clone(),
                    found,
                    supported,
                });
            }
        }
        let mut removed = Vec::new();
        for path in files {
            match fs::remove_file(&path) {
                Ok(()) => removed.push(path),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(StoreError::Io { path, source }),
            }
        }
        Ok(removed)
    }

    /// Path of the secrets file next to the state file.
    pub fn secrets_path(&self) -> PathBuf {
        self.path.with_extension("secrets.json")
    }

    /// Reads the state file of a [`FileKind::V2`] store. Same recovery rules as
    /// [`StationStore::load`]: a corrupt file or one of another charge point is moved to
    /// `<file>.bak`; a newer version is an error and the file is left alone.
    pub fn load2(&self) -> Result<LoadOutcome2, StoreError> {
        match self.read_state2()? {
            Inspection2::Missing => Ok(LoadOutcome2::Missing),
            Inspection2::Loaded(state) => Ok(LoadOutcome2::Loaded(state)),
            Inspection2::Unusable { reason } => {
                let backup = self.move_aside(&self.path, &self.secrets_path())?;
                Ok(LoadOutcome2::Quarantined { backup, reason })
            }
        }
    }

    /// Like [`StationStore::load2`], but nothing is moved aside.
    pub fn inspect2(&self) -> Result<Inspection2, StoreError> {
        self.read_state2()
    }

    fn read_state2(&self) -> Result<Inspection2, StoreError> {
        check_file_alias(&self.path)?;
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Inspection2::Missing),
            Err(source) => return Err(self.io_error(source)),
        };
        let unusable = |reason: String| Ok(Inspection2::Unusable { reason });
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(e) => return unusable(format!("not valid JSON: {e}")),
        };
        self.check_case_collision(&self.path, &value)?;
        match value.get("version").and_then(Value::as_u64) {
            Some(found) if found > u64::from(STORE2_VERSION) => {
                return Err(StoreError::UnsupportedVersion {
                    path: self.path.clone(),
                    found,
                    supported: STORE2_VERSION,
                });
            }
            Some(_) => {}
            None => return unusable("missing format version".to_owned()),
        }
        let state: PersistedStation2 = match serde_json::from_value(value) {
            Ok(state) => state,
            Err(e) => return unusable(format!("unexpected structure: {e}")),
        };
        if state.charge_point_id != self.charge_point_id {
            return unusable(format!(
                "belongs to charge point `{}`, expected `{}`",
                crate::text::escape_unsafe(&state.charge_point_id),
                crate::text::escape_unsafe(&self.charge_point_id),
            ));
        }
        Ok(Inspection2::Loaded(Box::new(state)))
    }

    /// Atomically replaces the state file of a [`FileKind::V2`] store. The version and the
    /// charge point id are always the store's own.
    pub fn save2(&self, state: &PersistedStation2) -> Result<(), StoreError> {
        let mut copy = state.clone();
        copy.version = STORE2_VERSION;
        copy.charge_point_id.clone_from(&self.charge_point_id);
        let json = serde_json::to_vec_pretty(&copy).map_err(StoreError::Serialize)?;
        Self::write_atomic(&self.path, &json)
    }

    /// Reads the secrets file of a [`FileKind::V2`] store; a missing one is "no secrets".
    /// A corrupt file is moved aside (reported as `InvalidData`); a newer one is left untouched
    /// ([`StoreError::UnsupportedVersion`]).
    pub fn load_secrets2(&self) -> Result<StationSecrets2, StoreError> {
        self.load_secrets_file()
    }

    fn load_secrets_file<T: serde::de::DeserializeOwned + Default>(&self) -> Result<T, StoreError> {
        let path = self.secrets_path();
        check_file_alias(&path)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(T::default()),
            Err(source) => return Err(StoreError::Io { path, source }),
        };
        let supported = self.kind.version();
        let parsed = match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => {
                self.check_case_collision(&path, &value)?;
                let version = value.get("version").and_then(Value::as_u64).unwrap_or(0);
                if version > u64::from(supported) {
                    return Err(StoreError::UnsupportedVersion {
                        path,
                        found: version,
                        supported,
                    });
                }
                if let Some(found) = value
                    .get("charge_point_id")
                    .and_then(Value::as_str)
                    .filter(|found| *found != self.charge_point_id)
                {
                    Err(format!(
                        "belongs to charge point `{}`, expected `{}`",
                        crate::text::escape_unsafe(found),
                        crate::text::escape_unsafe(&self.charge_point_id),
                    ))
                } else {
                    serde_json::from_value(value).map_err(|e| e.to_string())
                }
            }
            Err(e) => Err(e.to_string()),
        };
        match parsed {
            Ok(secrets) => Ok(secrets),
            Err(e) => {
                let backup = self.move_aside(&path, &self.path)?;
                Err(StoreError::Io {
                    path,
                    source: io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{e}; the file was moved to {}", backup.display()),
                    ),
                })
            }
        }
    }

    /// Atomically replaces the secrets file of a [`FileKind::V2`] store (removes it when
    /// there is no secret to keep).
    pub fn save_secrets2(&self, secrets: &StationSecrets2) -> Result<(), StoreError> {
        let path = self.secrets_path();
        if secrets.basic_auth_password.is_none() {
            return self.remove_secrets_file(path);
        }
        let mut copy = secrets.clone();
        copy.version = STORE2_VERSION;
        let json = serde_json::to_vec_pretty(&copy).map_err(StoreError::Serialize)?;
        Self::write_atomic(&path, &json)
    }

    /// Reads the secrets file. A missing file is "no secrets".
    pub fn load_secrets(&self) -> Result<StationSecrets, StoreError> {
        self.load_secrets_file()
    }

    fn check_case_collision(&self, path: &Path, value: &Value) -> Result<(), StoreError> {
        if let Some(found) = value.get("charge_point_id").and_then(Value::as_str)
            && found != self.charge_point_id
            && found.to_lowercase() == self.charge_point_id.to_lowercase()
        {
            return Err(case_collision(path.file_name().unwrap_or(path.as_os_str())));
        }
        Ok(())
    }

    /// Before moving an unusable file aside, check whether the other file identifies a
    /// station with a case-colliding ID.
    fn check_companion_case_collision(&self, path: &Path) -> Result<(), StoreError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(StoreError::Io {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        if let Ok(value) = serde_json::from_slice(&bytes) {
            self.check_case_collision(path, &value)?;
        }
        Ok(())
    }

    /// Atomically replaces the secrets file (removes it when `secrets` is empty).
    pub fn save_secrets(&self, secrets: &StationSecrets) -> Result<(), StoreError> {
        let path = self.secrets_path();
        if secrets.is_empty() {
            return self.remove_secrets_file(path);
        }
        let mut copy = secrets.clone();
        copy.version = STORE_VERSION;
        let json = serde_json::to_vec_pretty(&copy).map_err(StoreError::Serialize)?;
        Self::write_atomic(&path, &json)
    }

    fn remove_secrets_file(&self, path: PathBuf) -> Result<(), StoreError> {
        check_file_aliases(&self.owned_files())?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StoreError::Io { path, source }),
        }
    }

    /// Atomically replaces the state file (creating the directory if needed).
    pub fn save(&self, state: &PersistedStation) -> Result<(), StoreError> {
        let json = serde_json::to_vec_pretty(state).map_err(StoreError::Serialize)?;
        Self::write_atomic(&self.path, &json)
    }

    fn write_atomic(path: &Path, json: &[u8]) -> Result<(), StoreError> {
        check_file_alias(path)?;
        crate::fs_util::write_atomic(path, json).map_err(|e| StoreError::Io {
            path: e.path,
            source: e.source,
        })
    }

    fn move_aside(&self, path: &Path, companion: &Path) -> Result<PathBuf, StoreError> {
        check_file_aliases(&self.owned_files())?;
        self.check_companion_case_collision(companion)?;
        move_aside(path)
    }

    fn io_error(&self, source: io::Error) -> StoreError {
        StoreError::Io {
            path: self.path.clone(),
            source,
        }
    }
}

fn check_file_aliases(paths: &[PathBuf]) -> Result<(), StoreError> {
    paths.iter().try_for_each(|path| check_file_alias(path))
}

fn check_file_alias(path: &Path) -> Result<(), StoreError> {
    match case_alias(path) {
        Ok(Some(name)) => Err(case_collision(&name)),
        Ok(None) => Ok(()),
        Err(source) => Err(StoreError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

fn case_collision(name: &OsStr) -> StoreError {
    StoreError::CaseCollision {
        found: crate::text::escape_unsafe(&name.to_string_lossy()),
    }
}

#[cfg(test)]
thread_local! {
    static DIRECTORY_ENTRY_OVERRIDE: std::cell::RefCell<Option<(PathBuf, OsString)>> =
        const { std::cell::RefCell::new(None) };
}

fn case_alias(path: &Path) -> io::Result<Option<OsString>> {
    let (Some(parent), Some(wanted)) = (path.parent(), path.file_name()) else {
        return Ok(None);
    };
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    }
    #[cfg(test)]
    if let Some(name) = DIRECTORY_ENTRY_OVERRIDE.with(|entry| {
        entry
            .borrow()
            .as_ref()
            .and_then(|(overridden, name)| (overridden == path).then(|| name.clone()))
    }) {
        return Ok(alias_in([name], wanted));
    }
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let mut error = None;
    let names = fs::read_dir(parent)?.map_while(|entry| match entry {
        Ok(entry) => Some(entry.file_name()),
        Err(e) => {
            error = Some(e);
            None
        }
    });
    let alias = alias_in(names, wanted);
    error.map_or(Ok(alias), Err)
}

fn alias_in(names: impl IntoIterator<Item = OsString>, wanted: &OsStr) -> Option<OsString> {
    let mut alias = None;
    let bytes = wanted.as_encoded_bytes();
    for name in names {
        if name == wanted {
            return None;
        }
        if alias.is_none()
            && (name.as_encoded_bytes().eq_ignore_ascii_case(bytes)
                || ((!bytes.is_ascii() || !name.as_encoded_bytes().is_ascii())
                    && name.to_string_lossy().to_lowercase()
                        == wanted.to_string_lossy().to_lowercase()))
        {
            alias = Some(name);
        }
    }
    alias
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".bak");
    PathBuf::from(name)
}

/// Moves `path` to `<path>.bak` (replacing an older one) and makes the backup private.
fn move_aside(path: &Path) -> Result<PathBuf, StoreError> {
    let backup = backup_path(path);
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
    fs::rename(path, &backup).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&backup, fs::Permissions::from_mode(0o600));
    }
    Ok(backup)
}

/// Longest escaped id kept verbatim. With the longest suffix (`.station2.secrets.json`, 23
/// bytes), the `.bak` suffix and the `~<16 hex>` hash tail the name stays well below the
/// 255-byte file name limit, and short enough for the 260-character path limit of Windows with
/// a typical state directory. Both file kinds share the limit, so 1.6 names never change.
const MAX_ESCAPED_LEN: usize = 100;

/// File name for a charge point id: `[A-Za-z0-9_-]` and non-leading `.` are kept, every
/// other byte becomes `%XX`, so any id maps to a distinct, filesystem-safe name.
///
/// An escaped id longer than [`MAX_ESCAPED_LEN`] is cut at an escape boundary and followed by
/// `~` plus the FNV-1a hash of the whole id. `~` is always escaped in plain names, so a
/// shortened name cannot collide with an unshortened one.
fn file_name(kind: FileKind, charge_point_id: &str) -> String {
    file_name_for_platform(kind, charge_point_id, cfg!(windows))
}

fn file_name_for_platform(kind: FileKind, charge_point_id: &str, windows: bool) -> String {
    let mut name = String::with_capacity(charge_point_id.len().min(MAX_ESCAPED_LEN) + 32);
    let mut kept = 0;
    let mut truncated = false;
    for (index, byte) in charge_point_id.bytes().enumerate() {
        let mut piece = String::with_capacity(3);
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' => piece.push(char::from(byte)),
            b'.' if index > 0 => piece.push('.'),
            _ => {
                let _ = write!(piece, "%{byte:02X}");
            }
        }
        if truncated || name.len() + piece.len() > MAX_ESCAPED_LEN {
            if !truncated {
                kept = name.len();
                truncated = true;
            }
            continue;
        }
        name.push_str(&piece);
    }
    if truncated {
        name.truncate(kept);
    }
    let device = windows && is_windows_device_name(charge_point_id);
    if truncated || device {
        let tail = format!("~{:016x}", fnv1a_64(charge_point_id.as_bytes()));
        // Windows interprets the part before the first dot as a possible device name.
        let end = if device {
            name.find('.').unwrap_or(name.len())
        } else {
            name.len()
        };
        name.insert_str(end, &tail);
    }
    if name.is_empty() {
        name.push_str("%00empty");
    }
    name.push_str(kind.state_suffix());
    name
}

fn is_windows_device_name(id: &str) -> bool {
    let name = id.split('.').next().unwrap_or_default();
    ["CON", "PRN", "AUX", "NUL"]
        .iter()
        .any(|device| name.eq_ignore_ascii_case(device))
        || (name.len() == 4
            && name
                .as_bytes()
                .last()
                .is_some_and(|b| matches!(b, b'1'..=b'9'))
            && name.get(..3).is_some_and(|prefix| {
                prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT")
            }))
}

/// FNV-1a, 64 bit: a stable hash (unlike `DefaultHasher`) that needs no dependency.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::*;
    use crate::fs_util::dir_sync_unsupported;
    use crate::ids::TxId;
    use crate::station::transaction::ActiveTransaction;

    #[test]
    fn case_alias_requires_the_exact_directory_entry() {
        let wanted = OsStr::new("cp-1.station.json");
        let alias = OsString::from("CP-1.station.json");
        assert_eq!(alias_in([alias.clone(), wanted.to_owned()], wanted), None);
        assert_eq!(alias_in([alias.clone()], wanted), Some(alias));
        assert_eq!(
            alias_in([OsString::from("État.json")], OsStr::new("état.json")),
            Some(OsString::from("État.json"))
        );
    }

    #[test]
    fn file_case_aliases_block_load_reset_and_quarantine() {
        let mut missed = Vec::new();
        for kind in [FileKind::V16, FileKind::V2] {
            for index in 0..4 {
                let dir = tempfile::tempdir().unwrap();
                let store = StationStore::new_for(kind, dir.path(), "cp-1");
                let files = store.owned_files();
                let corrupt = |index| format!("{{ corrupt {index}");
                for (index, path) in files.iter().enumerate() {
                    fs::write(path, corrupt(index)).unwrap();
                }
                let path = &files[index];
                let name = path.file_name().unwrap().to_ascii_uppercase();
                DIRECTORY_ENTRY_OVERRIDE
                    .with(|entry| entry.replace(Some((path.clone(), name.clone()))));
                assert_eq!(case_alias(path).unwrap(), Some(name.clone()));
                let expected = name.to_str().unwrap();
                let error_matches = |error| matches!(error, StoreError::CaseCollision { found } if found == expected);
                let state = match kind {
                    FileKind::V16 => store.load().err(),
                    FileKind::V2 => store.load2().err(),
                };
                let secrets = match kind {
                    FileKind::V16 => store.load_secrets().err(),
                    FileKind::V2 => store.load_secrets2().err(),
                };
                let reset = store.remove().err();
                for (operation, error) in [
                    ("load state", state),
                    ("load secrets", secrets),
                    ("reset", reset),
                ] {
                    if !error.is_some_and(error_matches) {
                        missed.push(format!("{kind:?} {operation} with alias {expected}"));
                    }
                }
                DIRECTORY_ENTRY_OVERRIDE.with(|entry| entry.replace(None));
                for (index, path) in files.iter().enumerate() {
                    if fs::read_to_string(path).ok().as_deref() != Some(&corrupt(index)) {
                        missed.push(format!("{kind:?} modified {}", path.display()));
                    }
                }
            }
        }
        assert!(
            missed.is_empty(),
            "case aliases must leave all files untouched: {missed:?}"
        );
    }

    #[test]
    fn file_case_aliases_block_reading_valid_state_and_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "cp-1");
        store.save(&PersistedStation::new("cp-1")).unwrap();
        let secrets = store.secrets_path();
        fs::write(&secrets, br#"{"version":1,"authorization_key":"secret"}"#).unwrap();
        for (path, state) in [(store.path(), true), (&secrets, false)] {
            let name = path.file_name().unwrap().to_ascii_uppercase();
            DIRECTORY_ENTRY_OVERRIDE
                .with(|entry| entry.replace(Some((path.to_owned(), name.clone()))));
            let error = if state {
                store.load().err()
            } else {
                store.load_secrets().err()
            };
            DIRECTORY_ENTRY_OVERRIDE.with(|entry| entry.replace(None));
            assert!(matches!(error, Some(StoreError::CaseCollision { found })
                if Some(found.as_str()) == name.to_str()));
        }
    }

    #[test]
    fn unreadable_companion_blocks_quarantine() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "cp-1");
        let secrets = store.secrets_path();
        fs::write(&secrets, b"{ corrupt").unwrap();
        fs::create_dir(store.path()).unwrap();

        assert!(
            matches!(store.load_secrets(), Err(StoreError::Io { path, .. }) if path == store.path()),
            "expected an I/O error for the unreadable state path"
        );
        assert_eq!(fs::read(&secrets).unwrap(), b"{ corrupt");
        assert!(!secrets.with_extension("json.bak").exists());
    }

    /// The 1.6 file name; the tests below predate the file kinds.
    fn file_name(charge_point_id: &str) -> String {
        super::file_name(FileKind::V16, charge_point_id)
    }

    #[test]
    fn v2_names_fit_the_budget_with_the_longest_suffix_and_backup() {
        let long = "x".repeat(300);
        for id in ["CP-1", "", "a b", long.as_str(), "é".repeat(80).as_str()] {
            let store = StationStore::new_for(FileKind::V2, "/state", id);
            for path in store.owned_files() {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                assert!(name.len() <= 150, "{} bytes: {name}", name.len());
            }
            assert!(
                store
                    .path()
                    .to_string_lossy()
                    .ends_with(FileKind::V2.state_suffix())
            );
            assert!(
                store
                    .secrets_path()
                    .to_string_lossy()
                    .ends_with(FileKind::V2.secrets_suffix())
            );
        }
        assert_eq!(super::file_name(FileKind::V2, "CP-1"), "CP-1.station2.json");
        assert_eq!(
            StationStore::new(Path::new("/s"), "CP-1").path(),
            StationStore::new_for(FileKind::V16, "/s", "CP-1").path()
        );
    }

    #[test]
    fn unsupported_directory_sync_is_not_a_save_failure() {
        for kind in [io::ErrorKind::Unsupported, io::ErrorKind::InvalidInput] {
            assert!(dir_sync_unsupported(&io::Error::from(kind)));
        }
        assert!(!dir_sync_unsupported(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
    }

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
            pending_availability: Some(AvailabilityType::Operative),
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
    fn case_collision_does_not_quarantine_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "CP-1");
        store.save(&PersistedStation::new("CP-1")).unwrap();
        let original = fs::read(store.path()).unwrap();
        let alias = StationStore {
            charge_point_id: "cp-1".to_owned(),
            ..store
        };
        assert!(
            matches!(alias.load(), Err(StoreError::CaseCollision { found }) if found == "CP-1.station.json")
        );
        assert_eq!(fs::read(alias.path()).unwrap(), original);
        assert!(!alias.path().with_extension("json.bak").exists());
    }

    #[test]
    fn case_collision_leaves_secrets_untouched() {
        let dir = tempfile::tempdir().unwrap();
        for kind in [FileKind::V16, FileKind::V2] {
            let store = StationStore::new_for(kind, dir.path(), "cp-1");
            let text = br#"{"version":1,"charge_point_id":"CP-1"}"#;
            let path = store.secrets_path();
            fs::write(&path, text).unwrap();
            let rejected = match kind {
                FileKind::V16 => store.load_secrets().unwrap_err(),
                FileKind::V2 => store.load_secrets2().unwrap_err(),
            };
            assert!(matches!(rejected, StoreError::CaseCollision { found }
                if found == path.file_name().unwrap().to_str().unwrap()));
            assert_eq!(fs::read(&path).unwrap(), text);
            assert!(!path.with_extension("json.bak").exists());
        }
    }

    #[test]
    fn case_collision_prevents_quarantining_the_companion_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "CP-1");
        store.save(&PersistedStation::new("CP-1")).unwrap();
        let alias = StationStore {
            charge_point_id: "cp-1".to_owned(),
            ..store
        };
        let path = alias.secrets_path();
        fs::write(&path, b"{ torn").unwrap();
        assert!(matches!(
            alias.load_secrets(),
            Err(StoreError::CaseCollision { found }) if found == "CP-1.station.json"
        ));
        assert_eq!(fs::read(&path).unwrap(), b"{ torn");
        assert!(!path.with_extension("json.bak").exists());
    }

    #[cfg(unix)]
    #[test]
    fn secrets_path_keeps_the_non_utf8_state_directory() {
        use std::os::unix::ffi::OsStrExt;

        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(std::ffi::OsStr::from_bytes(b"st\xffate"));
        let store = StationStore::new(&dir, "CP-1");
        assert_eq!(store.secrets_path().parent(), store.path().parent());
    }

    #[test]
    fn windows_names_avoid_reserved_devices_with_or_without_extensions() {
        for id in ["CON", "nul", "Com7", "LPT9.log"] {
            assert!(is_windows_device_name(id));
            let name = file_name_for_platform(FileKind::V16, id, true);
            let stem = name.split('.').next().unwrap();
            assert!(
                stem.contains('~'),
                "{name} needs a hash before the first dot to avoid a Windows device name"
            );
        }
        for id in ["CONSOLE", "COM0", "COM10", "CP-1"] {
            assert!(!is_windows_device_name(id));
            assert_eq!(
                file_name_for_platform(FileKind::V16, id, true),
                format!("{id}.station.json")
            );
        }
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
        fs::write(
            store.path(),
            format!(r#"{{"version":{STORE_VERSION},"charge_point_id":"CP-1"}}"#).as_bytes(),
        )
        .unwrap();
        let state = loaded(store.load().unwrap());
        assert_eq!(state, PersistedStation::new("CP-1"));
    }

    #[test]
    fn version_7_file_without_offline_samples_loads_with_an_empty_queue() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "CP-1");
        let mut value = serde_json::to_value(sample()).unwrap();
        value["version"] = 7.into();
        value.as_object_mut().unwrap().remove("offline_samples");
        fs::write(store.path(), serde_json::to_vec(&value).unwrap()).unwrap();
        let state = loaded(store.load().unwrap());
        assert!(state.offline_samples.is_empty());
    }

    #[test]
    fn version_1_file_without_pending_availability_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "CP-1");
        let mut value = serde_json::to_value(sample()).unwrap();
        value["version"] = 1.into();
        value["connectors"][0]
            .as_object_mut()
            .unwrap()
            .remove("pending_availability");
        fs::write(store.path(), serde_json::to_vec(&value).unwrap()).unwrap();
        let state = loaded(store.load().unwrap());
        assert_eq!(state.connectors[0].pending_availability, None);
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

    #[test]
    fn long_ids_get_a_short_unique_file_name_and_round_trip() {
        let long_a = format!("{}A", "x".repeat(300));
        let long_b = format!("{}B", "x".repeat(300));
        let special = "é/ ".repeat(100);
        let dir = tempfile::tempdir().unwrap();
        for id in [&long_a, &long_b, &special] {
            let name = file_name(id);
            assert!(name.len() + ".bak".len() <= 255, "{} bytes", name.len());
            assert!(!name.contains('/') && !name.contains(' '));
            let store = StationStore::new(dir.path(), id);
            store.save(&PersistedStation::new(id.as_str())).unwrap();
            let state = loaded(store.load().unwrap());
            assert_eq!(&state.charge_point_id, id);
        }
        assert_ne!(file_name(&long_a), file_name(&long_b));
        assert_eq!(file_name(&long_a), file_name(&long_a));
        // Cut at an escape boundary: no dangling `%` or `%X`.
        let name = file_name(&special);
        let stem = name.split('~').next().unwrap();
        assert!(!stem.ends_with('%'));
        assert!(stem.len() <= MAX_ESCAPED_LEN);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);
    }

    #[test]
    fn short_ids_keep_their_plain_file_names() {
        let id = "a".repeat(MAX_ESCAPED_LEN);
        assert_eq!(file_name(&id), format!("{id}.station.json"));
        assert!(file_name(&format!("{id}a")).contains('~'));
    }

    #[test]
    fn corrupt_secrets_file_is_moved_aside_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "CP-1");
        fs::write(store.secrets_path(), b"{ torn").unwrap();
        let error = store.load_secrets().unwrap_err().to_string();
        assert!(error.contains(".bak"), "{error}");
        let mut backup = store.secrets_path().into_os_string();
        backup.push(".bak");
        assert_eq!(fs::read(&backup).unwrap(), b"{ torn");
        assert!(!store.secrets_path().exists());
        // An older backup is replaced.
        fs::write(store.secrets_path(), b"{ torn2").unwrap();
        assert!(store.load_secrets().is_err());
        assert_eq!(fs::read(&backup).unwrap(), b"{ torn2");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&backup).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(store.load_secrets().unwrap().is_empty());
    }

    #[test]
    fn secrets_file_of_a_newer_version_is_refused_and_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "CP-1");
        let text = format!(
            r#"{{"version": {}, "future_field": "kept"}}"#,
            u64::from(STORE_VERSION) + 1
        );
        fs::write(store.secrets_path(), &text).unwrap();
        assert!(matches!(
            store.load_secrets(),
            Err(StoreError::UnsupportedVersion { found, .. }) if found == u64::from(STORE_VERSION) + 1
        ));
        assert_eq!(fs::read_to_string(store.secrets_path()).unwrap(), text);
    }

    #[test]
    fn inspect_never_moves_a_broken_file_aside() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "CP-1");
        assert!(matches!(store.inspect().unwrap(), Inspection::Missing));
        fs::write(store.path(), b"not json").unwrap();
        assert!(matches!(
            store.inspect().unwrap(),
            Inspection::Unusable { .. }
        ));
        assert_eq!(fs::read(store.path()).unwrap(), b"not json");
        store.save(&sample()).unwrap();
        assert!(matches!(store.inspect().unwrap(), Inspection::Loaded(_)));
    }

    #[test]
    fn remove_deletes_only_the_files_of_that_station() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "CP-1");
        let other = StationStore::new(dir.path(), "CP-2");
        store.save(&sample()).unwrap();
        let secrets = StationSecrets {
            authorization_key: Some("k".to_owned()),
            ..StationSecrets::default()
        };
        store.save_secrets(&secrets).unwrap();
        other.save(&PersistedStation::new("CP-2")).unwrap();
        fs::write(dir.path().join("unrelated.txt"), b"x").unwrap();
        assert!(store.has_secrets());

        let removed = store.remove().unwrap();
        assert_eq!(removed.len(), 2);
        assert!(!store.path().exists());
        assert!(!store.has_secrets());
        assert!(other.path().exists());
        assert!(dir.path().join("unrelated.txt").exists());
        assert!(store.remove().unwrap().is_empty(), "nothing left to remove");
    }

    #[test]
    fn remove_refuses_files_of_a_newer_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "CP-1");
        store.save(&sample()).unwrap();
        let text = format!(r#"{{"version": {}}}"#, u64::from(STORE_VERSION) + 1);
        fs::write(store.secrets_path(), &text).unwrap();
        assert!(matches!(
            store.remove(),
            Err(StoreError::UnsupportedVersion { .. })
        ));
        assert!(store.path().exists(), "the state file stays");
        assert_eq!(fs::read_to_string(store.secrets_path()).unwrap(), text);
    }

    #[test]
    fn remove_refuses_a_backup_of_a_newer_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = StationStore::new(dir.path(), "CP-1");
        store.save(&sample()).unwrap();
        let files = store.owned_files();
        for index in [1, 3] {
            let text = format!(r#"{{"version": {}}}"#, u64::from(STORE_VERSION) + 1);
            fs::write(&files[index], &text).unwrap();
            assert!(matches!(
                store.remove(),
                Err(StoreError::UnsupportedVersion { .. })
            ));
            assert!(store.path().exists(), "the state file stays");
            assert_eq!(fs::read_to_string(&files[index]).unwrap(), text);
            fs::remove_file(&files[index]).unwrap();
        }
    }

    #[test]
    fn owned_files_stay_inside_the_directory_for_hostile_ids() {
        let dir = Path::new("/state");
        for id in ["../../etc/passwd", "a/b", "..", "", "x\0y"] {
            for path in StationStore::new(dir, id).owned_files() {
                assert_eq!(path.parent(), Some(dir), "{id:?} -> {}", path.display());
            }
        }
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
