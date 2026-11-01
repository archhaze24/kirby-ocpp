//! Local authorization list (`SendLocalList`, `GetLocalListVersion`).
//!
//! TS reference: `src/station/local-auth-list.ts`.
//!
//! Check order (as in the TS code, with the size checks split):
//!
//! 1. feature disabled (`LocalAuthListEnabled=false`) -> `NotSupported`;
//! 2. the update has more entries than `SendLocalListMaxLength` -> `Failed`;
//! 3. the list *after* applying the update has more entries than
//!    `LocalAuthListMaxLength` -> `Failed` (duplicate idTags count once);
//! 4. differential update with `listVersion <= current` -> `VersionMismatch`;
//! 5. apply: full updates clear the list first, differential entries without
//!    `idTagInfo` delete, the rest insert or replace (the last entry for an idTag wins).
//!
//! The update is applied to a copy, so a `Failed` or `VersionMismatch` leaves the
//! list and its version untouched.
//!
//! Decisions beyond the TS code:
//!
//! * A **full** update entry without `idTagInfo` is not valid for a full list
//!   (the TS code defaulted it to `Accepted`, which would authorize tags the CSMS
//!   never approved). Such an update is `Failed` as a whole.
//! * `GetLocalListVersion` reports `-1` while the feature is disabled.
//! * Expiry is not stored here; the authorization layer checks it at read time.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::IdTag;
use crate::ocpp::messages::{AuthorizationData, IdTagInfo, SendLocalListRequest, UpdateStatus};
use crate::ocpp::types::UpdateType;
use crate::station::config_registry::ConfigRegistry;

/// Limits of the local list, read from the configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalListLimits {
    /// `LocalAuthListEnabled`.
    pub enabled: bool,
    /// `SendLocalListMaxLength`: entries allowed in one update.
    pub max_update_entries: usize,
    /// `LocalAuthListMaxLength`: entries allowed in the list after an update.
    pub max_entries: usize,
}

impl LocalListLimits {
    pub fn from_config(config: &ConfigRegistry) -> Self {
        Self {
            enabled: config.local_auth_list_enabled(),
            max_update_entries: usize::try_from(config.send_local_list_max_length())
                .unwrap_or(usize::MAX),
            max_entries: usize::try_from(config.local_auth_list_max_length()).unwrap_or(usize::MAX),
        }
    }
}

/// The local authorization list and its version.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalAuthList {
    version: i32,
    entries: BTreeMap<IdTag, IdTagInfo>,
}

impl LocalAuthList {
    /// The version last accepted by `SendLocalList` (`0` = no list).
    pub fn version(&self) -> i32 {
        self.version
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Stored `IdTagInfo` for `id_tag` (exact, case-sensitive match).
    pub fn get(&self, id_tag: &IdTag) -> Option<&IdTagInfo> {
        self.entries.get(id_tag)
    }

    /// The `GetLocalListVersion` answer: `-1` when the feature is disabled.
    pub fn reported_version(&self, enabled: bool) -> i32 {
        if enabled { self.version } else { -1 }
    }

    /// Applies a `SendLocalList` request. See the module documentation for the order.
    pub fn apply(
        &mut self,
        request: &SendLocalListRequest,
        limits: &LocalListLimits,
    ) -> UpdateStatus {
        if !limits.enabled {
            return UpdateStatus::NotSupported;
        }
        let update: &[AuthorizationData] = request
            .local_authorization_list
            .as_deref()
            .unwrap_or_default();
        if update.len() > limits.max_update_entries {
            return UpdateStatus::Failed;
        }

        let full = request.update_type == UpdateType::Full;
        if full && update.iter().any(|item| item.id_tag_info.is_none()) {
            return UpdateStatus::Failed;
        }

        let mut next = if full {
            BTreeMap::new()
        } else {
            self.entries.clone()
        };
        for item in update {
            match &item.id_tag_info {
                Some(info) => {
                    next.insert(item.id_tag.clone(), info.clone());
                }
                None => {
                    next.remove(&item.id_tag);
                }
            }
        }
        if next.len() > limits.max_entries {
            return UpdateStatus::Failed;
        }
        if !full && request.list_version <= self.version {
            return UpdateStatus::VersionMismatch;
        }

        self.entries = next;
        self.version = request.list_version;
        UpdateStatus::Accepted
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::config::StationConfig;
    use crate::ocpp::types::AuthorizationStatus;

    fn tag(name: &str) -> IdTag {
        IdTag::new(name).unwrap()
    }

    // Option because `entry` takes an optional idTagInfo (a differential delete has none).
    #[allow(clippy::unnecessary_wraps)]
    fn accepted() -> Option<IdTagInfo> {
        Some(IdTagInfo::with_status(AuthorizationStatus::Accepted))
    }

    fn entry(name: &str, info: Option<IdTagInfo>) -> AuthorizationData {
        AuthorizationData {
            id_tag: tag(name),
            id_tag_info: info,
        }
    }

    fn request(
        version: i32,
        update_type: UpdateType,
        entries: Vec<AuthorizationData>,
    ) -> SendLocalListRequest {
        SendLocalListRequest {
            list_version: version,
            local_authorization_list: Some(entries),
            update_type,
        }
    }

    fn limits() -> LocalListLimits {
        LocalListLimits {
            enabled: true,
            max_update_entries: 1000,
            max_entries: 1000,
        }
    }

    fn differential(version: i32, entries: Vec<AuthorizationData>) -> SendLocalListRequest {
        request(version, UpdateType::Differential, entries)
    }

    #[test]
    fn differential_insert_and_delete() {
        let mut list = LocalAuthList::default();
        let add = differential(1, vec![entry("TAG1", accepted())]);
        assert_eq!(list.apply(&add, &limits()), UpdateStatus::Accepted);
        assert!(list.get(&tag("TAG1")).is_some());
        assert_eq!(list.version(), 1);

        // Differential entry without idTagInfo deletes the tag.
        let delete = differential(2, vec![entry("TAG1", None)]);
        assert_eq!(list.apply(&delete, &limits()), UpdateStatus::Accepted);
        assert!(list.get(&tag("TAG1")).is_none());
        assert_eq!(list.version(), 2);
    }

    #[test]
    fn deleting_an_absent_tag_is_a_no_op_that_still_bumps_the_version() {
        let mut list = LocalAuthList::default();
        let delete = differential(1, vec![entry("NOPE", None)]);
        assert_eq!(list.apply(&delete, &limits()), UpdateStatus::Accepted);
        assert!(list.is_empty());
        assert_eq!(list.version(), 1);
    }

    #[test]
    fn full_update_clears_first_and_may_lower_the_version() {
        let mut list = LocalAuthList::default();
        list.apply(&differential(5, vec![entry("OLD", accepted())]), &limits());
        let full = request(1, UpdateType::Full, vec![entry("NEW", accepted())]);
        assert_eq!(list.apply(&full, &limits()), UpdateStatus::Accepted);
        assert!(list.get(&tag("OLD")).is_none());
        assert!(list.get(&tag("NEW")).is_some());
        assert_eq!(list.version(), 1);
    }

    #[test]
    fn full_update_entry_without_id_tag_info_fails_the_whole_update() {
        let mut list = LocalAuthList::default();
        list.apply(&differential(1, vec![entry("KEEP", accepted())]), &limits());
        let before = list.clone();
        let full = request(
            9,
            UpdateType::Full,
            vec![entry("GOOD", accepted()), entry("NOINFO", None)],
        );
        assert_eq!(list.apply(&full, &limits()), UpdateStatus::Failed);
        assert_eq!(list, before, "failed update must not mutate");
    }

    #[test]
    fn disabled_feature_is_not_supported_and_does_not_mutate() {
        let mut list = LocalAuthList::default();
        let disabled = LocalListLimits {
            enabled: false,
            ..limits()
        };
        let before = list.clone();
        let update = differential(1, vec![entry("SHOULDNOTLOAD", accepted())]);
        assert_eq!(list.apply(&update, &disabled), UpdateStatus::NotSupported);
        assert_eq!(list, before);
        assert_eq!(list.reported_version(false), -1);
        assert_eq!(list.reported_version(true), 0);
    }

    #[test]
    fn update_over_send_local_list_max_length_fails() {
        let mut list = LocalAuthList::default();
        let small = LocalListLimits {
            max_update_entries: 2,
            ..limits()
        };
        let update = differential(
            1,
            vec![
                entry("A", accepted()),
                entry("B", accepted()),
                entry("C", accepted()),
            ],
        );
        assert_eq!(list.apply(&update, &small), UpdateStatus::Failed);
        assert!(list.is_empty());
        assert_eq!(list.version(), 0);
    }

    #[test]
    fn resulting_list_over_local_auth_list_max_length_fails_atomically() {
        let mut list = LocalAuthList::default();
        let small = LocalListLimits {
            max_entries: 2,
            ..limits()
        };
        list.apply(
            &differential(1, vec![entry("A", accepted()), entry("B", accepted())]),
            &small,
        );
        let before = list.clone();
        let more = differential(2, vec![entry("C", accepted())]);
        assert_eq!(list.apply(&more, &small), UpdateStatus::Failed);
        assert_eq!(list, before);
    }

    #[test]
    fn duplicates_count_once_towards_the_resulting_size() {
        let mut list = LocalAuthList::default();
        let small = LocalListLimits {
            max_entries: 2,
            ..limits()
        };
        let update = differential(
            1,
            vec![
                entry(
                    "DUP",
                    Some(IdTagInfo::with_status(AuthorizationStatus::Blocked)),
                ),
                entry("DUP", accepted()),
                entry("OTHER", accepted()),
            ],
        );
        assert_eq!(list.apply(&update, &small), UpdateStatus::Accepted);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn last_entry_for_an_idtag_wins() {
        let mut list = LocalAuthList::default();
        let update = differential(
            1,
            vec![
                entry(
                    "DUPLOCAL",
                    Some(IdTagInfo::with_status(AuthorizationStatus::Blocked)),
                ),
                entry("DUPLOCAL", accepted()),
            ],
        );
        list.apply(&update, &limits());
        assert_eq!(
            list.get(&tag("DUPLOCAL")).map(|info| info.status),
            Some(AuthorizationStatus::Accepted)
        );
    }

    #[test]
    fn stale_or_equal_differential_version_is_a_mismatch() {
        let mut list = LocalAuthList::default();
        list.apply(&differential(3, vec![entry("A", accepted())]), &limits());
        let before = list.clone();
        for stale in [1, 3] {
            let update = differential(stale, vec![entry("STALE", accepted())]);
            assert_eq!(
                list.apply(&update, &limits()),
                UpdateStatus::VersionMismatch
            );
        }
        assert_eq!(list, before);
    }

    #[test]
    fn full_update_ignores_the_version_check() {
        let mut list = LocalAuthList::default();
        list.apply(&differential(3, vec![]), &limits());
        let full = request(1, UpdateType::Full, vec![entry("A", accepted())]);
        assert_eq!(list.apply(&full, &limits()), UpdateStatus::Accepted);
    }

    #[test]
    fn missing_entry_array_is_an_empty_update() {
        let mut list = LocalAuthList::default();
        list.apply(&differential(1, vec![entry("A", accepted())]), &limits());
        let empty_full = SendLocalListRequest {
            list_version: 2,
            local_authorization_list: None,
            update_type: UpdateType::Full,
        };
        assert_eq!(list.apply(&empty_full, &limits()), UpdateStatus::Accepted);
        assert!(list.is_empty());
    }

    #[test]
    fn malformed_entry_without_id_tag_is_a_schema_error() {
        let json = serde_json::json!({
            "listVersion": 6,
            "updateType": "Differential",
            "localAuthorizationList": [{}]
        });
        assert!(serde_json::from_value::<SendLocalListRequest>(json).is_err());
    }

    #[test]
    fn limits_come_from_the_configuration() {
        let mut config = ConfigRegistry::new(&StationConfig::new("ws://h/p"));
        config.change("LocalAuthListEnabled", "false");
        let limits = LocalListLimits::from_config(&config);
        assert!(!limits.enabled);
        assert_eq!(limits.max_update_entries, 1000);
        assert_eq!(limits.max_entries, 1000);
    }

    #[test]
    fn list_roundtrips_through_json() {
        let mut list = LocalAuthList::default();
        list.apply(&differential(4, vec![entry("TAG1", accepted())]), &limits());
        let json = serde_json::to_string(&list).unwrap();
        let back: LocalAuthList = serde_json::from_str(&json).unwrap();
        assert_eq!(back, list);
    }

    /// The configuration-auth smoke scenario's local list steps, in order.
    #[test]
    fn scenario_sequence_matches_the_ts_smoke_run() {
        let mut list = LocalAuthList::default();
        let on = limits();
        let off = LocalListLimits {
            enabled: false,
            ..limits()
        };

        assert_eq!(
            run(&mut list, &differential(2, vec![entry("TAG1", None)]), &on),
            UpdateStatus::Accepted
        );
        assert_eq!(
            run(
                &mut list,
                &differential(3, vec![entry("TAG1", accepted())]),
                &on
            ),
            UpdateStatus::Accepted
        );
        assert_eq!(
            run(
                &mut list,
                &differential(4, vec![entry("DISABLEDLOCAL", accepted())]),
                &on
            ),
            UpdateStatus::Accepted
        );
        assert_eq!(
            run(
                &mut list,
                &differential(5, vec![entry("SHOULDNOTLOAD", accepted())]),
                &off
            ),
            UpdateStatus::NotSupported
        );
        assert!(run_check(&list, "SHOULDNOTLOAD").is_none());

        assert_eq!(
            run(
                &mut list,
                &differential(5, vec![entry("DISABLEDLOCAL", None)]),
                &on
            ),
            UpdateStatus::Accepted
        );

        // Malformed entries are rejected by the schema before reaching the list.
        assert!(
            serde_json::from_value::<SendLocalListRequest>(serde_json::json!({
                "listVersion": 6, "updateType": "Differential", "localAuthorizationList": [{}]
            }))
            .is_err()
        );

        let bulk: Vec<AuthorizationData> = (0..1001)
            .map(|i| entry(&format!("BULK{i}"), accepted()))
            .collect();
        assert_eq!(
            run(&mut list, &differential(7, bulk), &on),
            UpdateStatus::Failed
        );
        assert_eq!(list.version(), 5, "failed update must not bump the version");

        assert_eq!(
            run(
                &mut list,
                &differential(
                    6,
                    vec![
                        entry(
                            "EXPIREDLOCAL",
                            Some(IdTagInfo {
                                status: AuthorizationStatus::Accepted,
                                expiry_date: Some(
                                    chrono::Utc::now() - chrono::Duration::minutes(1)
                                ),
                                parent_id_tag: None,
                            })
                        ),
                        entry(
                            "PARENTLOCAL",
                            Some(IdTagInfo {
                                status: AuthorizationStatus::Accepted,
                                expiry_date: None,
                                parent_id_tag: Some(tag("ROOTLOCAL")),
                            })
                        ),
                        entry(
                            "DUPLOCAL",
                            Some(IdTagInfo::with_status(AuthorizationStatus::Blocked))
                        ),
                        entry("DUPLOCAL", accepted()),
                    ],
                ),
                &on
            ),
            UpdateStatus::Accepted
        );
        assert_eq!(list.version(), 6);
        assert!(run_check(&list, "PARENTLOCAL").is_some());
        assert!(run_check(&list, "DISABLEDLOCAL").is_none());
    }

    fn run(
        list: &mut LocalAuthList,
        req: &SendLocalListRequest,
        limits: &LocalListLimits,
    ) -> UpdateStatus {
        list.apply(req, limits)
    }

    fn run_check(list: &LocalAuthList, name: &str) -> Option<IdTagInfo> {
        list.get(&tag(name)).cloned()
    }

    #[test]
    fn stale_version_after_restore_is_a_mismatch() {
        // negative-csms: a differential with version 1 against a list already at 3.
        let mut list = LocalAuthList::default();
        list.apply(&differential(3, vec![]), &limits());
        assert_eq!(
            list.apply(&differential(1, vec![entry("TAG1", accepted())]), &limits()),
            UpdateStatus::VersionMismatch
        );
    }

    #[derive(Debug, Clone)]
    enum Op {
        Put(usize, bool),
        Delete(usize),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0..6_usize, any::<bool>()).prop_map(|(i, ok)| Op::Put(i, ok)),
            (0..6_usize).prop_map(Op::Delete),
        ]
    }

    proptest! {
        /// A sequence of differential updates matches a plain map model.
        #[test]
        fn differential_application_matches_a_model(ops in proptest::collection::vec(op(), 0..40)) {
            let mut list = LocalAuthList::default();
            let mut model: BTreeMap<String, AuthorizationStatus> = BTreeMap::new();
            for (step, op) in ops.iter().enumerate() {
                let version = i32::try_from(step).unwrap() + 1;
                let (name, item) = match *op {
                    Op::Put(i, ok) => {
                        let status = if ok { AuthorizationStatus::Accepted } else { AuthorizationStatus::Blocked };
                        (format!("T{i}"), Some(IdTagInfo::with_status(status)))
                    }
                    Op::Delete(i) => (format!("T{i}"), None),
                };
                match &item {
                    Some(info) => { model.insert(name.clone(), info.status); }
                    None => { model.remove(&name); }
                }
                let update = differential(version, vec![entry(&name, item)]);
                prop_assert_eq!(list.apply(&update, &limits()), UpdateStatus::Accepted);
                prop_assert_eq!(list.version(), version);
            }
            let actual: BTreeMap<String, AuthorizationStatus> = ["T0", "T1", "T2", "T3", "T4", "T5"]
                .iter()
                .filter_map(|name| list.get(&tag(name)).map(|info| ((*name).to_owned(), info.status)))
                .collect();
            prop_assert_eq!(actual, model);
        }
    }
}
