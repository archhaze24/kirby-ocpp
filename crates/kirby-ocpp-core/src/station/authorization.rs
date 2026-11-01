//! Authorization: local list, authorization cache, offline rules.
//!
//! TS reference: `src/station/authorization.ts` (`authorizeLocally`), the
//! Authorize / IdTagInfo handling in `src/station.ts`.
//!
//! Decision order when offline (`authorize_offline`, TS `authorizeLocally` with
//! `requireOfflinePermission = true`):
//!
//! 1. `LocalAuthorizeOffline = false` -> rejected (`source: None`), even if the tag is listed;
//! 2. local list, if `LocalAuthListEnabled`;
//! 3. authorization cache, if `AuthorizationCacheEnabled`;
//! 4. `AllowOfflineTxForUnknownId` -> accepted (`source: OfflineUnknown`);
//! 5. rejected.
//!
//! `LocalPreAuthorize` ([`pre_authorize`]) runs steps 2 and 3 only, so an unknown
//! tag is rejected there even when `AllowOfflineTxForUnknownId` is on.
//!
//! `Expired` is decided at read time from `expiryDate`, so a cached "Accepted"
//! stops working when its expiry passes, without any timer. Both CSMS responses
//! (`Authorize.conf` and `StartTransaction.conf`) go through
//! [`apply_online_id_tag_info`], which rewrites the cache.
//!
//! The cache is bounded ([`AuthorizationCache`]). Decisions about the cache:
//!
//! * Eviction drops the oldest entry that is still `Accepted`. Blocked, invalid,
//!   concurrent and expired entries are kept longer: with
//!   `AllowOfflineTxForUnknownId = true`, forgetting a denial would turn it into
//!   an offline acceptance. An evicted accepted tag is rejected offline unless
//!   `AllowOfflineTxForUnknownId` is on.
//! * Lookups do not refresh recency; eviction is by write order.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use crate::ocpp::messages::IdTagInfo;

use crate::ids::IdTag;
use crate::ocpp::types::AuthorizationStatus;
use crate::station::config_registry::ConfigRegistry;
use crate::station::local_auth_list::LocalAuthList;

/// Entries the cache holds before the oldest accepted one is evicted.
pub const DEFAULT_CACHE_CAPACITY: usize = 1_000;

/// Where an authorization decision came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthSource {
    CentralSystem,
    LocalList,
    AuthorizationCache,
    /// Offline, tag unknown locally, `AllowOfflineTxForUnknownId` is on.
    OfflineUnknown,
    /// No decision source applied (offline and nothing allowed it).
    None,
}

/// Result of an `Authorize` flow. A *rejected* tag is a normal outcome
/// (`accepted == false`), not an error, so that scenarios can assert on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizationDecision {
    pub accepted: bool,
    pub id_tag_info: IdTagInfo,
    pub source: AuthSource,
}

/// `IdTagInfo` with `expiryDate` applied: a tag whose expiry is not in the future
/// reads as `Expired`, whatever status it carries.
pub fn normalize_id_tag_info(info: &IdTagInfo, now: DateTime<Utc>) -> IdTagInfo {
    let status = match info.expiry_date {
        Some(expiry) if expiry <= now => AuthorizationStatus::Expired,
        _ => info.status,
    };
    IdTagInfo {
        status,
        expiry_date: info.expiry_date,
        parent_id_tag: info.parent_id_tag.clone(),
    }
}

/// Whether an `IdTagInfo` authorizes a transaction (`status == Accepted`).
pub fn is_accepted(info: &IdTagInfo) -> bool {
    info.status == AuthorizationStatus::Accepted
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CacheSlot {
    info: IdTagInfo,
    /// Write order, for eviction.
    seq: u64,
}

/// Bounded cache of the CSMS's latest `IdTagInfo` per idTag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthorizationCache {
    #[serde(skip, default = "default_capacity")]
    capacity: usize,
    next_seq: u64,
    entries: BTreeMap<IdTag, CacheSlot>,
}

fn default_capacity() -> usize {
    DEFAULT_CACHE_CAPACITY
}

impl Default for AuthorizationCache {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthorizationCache {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CACHE_CAPACITY)
    }

    /// A cache holding at most `capacity` entries (`0` stores nothing).
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            next_seq: 0,
            entries: BTreeMap::new(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, id_tag: &IdTag) -> bool {
        self.entries.contains_key(id_tag)
    }

    /// Stores the CSMS's `IdTagInfo` for `id_tag`, evicting if the cache is full.
    pub fn remember(&mut self, id_tag: &IdTag, info: IdTagInfo, now: DateTime<Utc>) {
        if self.capacity == 0 {
            return;
        }
        if !self.entries.contains_key(id_tag) {
            while self.entries.len() >= self.capacity {
                self.evict_one(now);
            }
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.entries.insert(id_tag.clone(), CacheSlot { info, seq });
    }

    /// The cached `IdTagInfo` as seen at `now` (expiry applied).
    pub fn lookup(&self, id_tag: &IdTag, now: DateTime<Utc>) -> Option<IdTagInfo> {
        self.entries
            .get(id_tag)
            .map(|slot| normalize_id_tag_info(&slot.info, now))
    }

    /// `ClearCache`.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    fn evict_one(&mut self, now: DateTime<Utc>) {
        // Oldest live Accepted entry first; otherwise the oldest entry of any kind.
        let victim = self
            .entries
            .iter()
            .min_by_key(|(_, slot)| (!is_live_accept(&slot.info, now), slot.seq))
            .map(|(id_tag, _)| id_tag.clone());
        if let Some(id_tag) = victim {
            self.entries.remove(&id_tag);
        }
    }
}

fn is_live_accept(info: &IdTagInfo, now: DateTime<Utc>) -> bool {
    is_accepted(&normalize_id_tag_info(info, now))
}

fn decided(info: IdTagInfo, source: AuthSource) -> AuthorizationDecision {
    AuthorizationDecision {
        accepted: is_accepted(&info),
        id_tag_info: info,
        source,
    }
}

/// Offline authorization (TS `authorizeLocally`, `requireOfflinePermission = true`).
/// See the module documentation for the order.
pub fn authorize_offline(
    id_tag: &IdTag,
    now: DateTime<Utc>,
    list: &LocalAuthList,
    cache: &AuthorizationCache,
    config: &ConfigRegistry,
) -> AuthorizationDecision {
    authorize_locally(id_tag, now, list, cache, config, true)
}

/// `LocalPreAuthorize`: the local decision a StartTransaction must pass before it
/// is sent. `None` when the flag is off. When it is on, only the local list and
/// the cache can accept a tag.
pub fn pre_authorize(
    id_tag: &IdTag,
    now: DateTime<Utc>,
    list: &LocalAuthList,
    cache: &AuthorizationCache,
    config: &ConfigRegistry,
) -> Option<AuthorizationDecision> {
    config
        .local_pre_authorize()
        .then(|| authorize_locally(id_tag, now, list, cache, config, false))
}

fn authorize_locally(
    id_tag: &IdTag,
    now: DateTime<Utc>,
    list: &LocalAuthList,
    cache: &AuthorizationCache,
    config: &ConfigRegistry,
    require_offline_permission: bool,
) -> AuthorizationDecision {
    if require_offline_permission && !config.local_authorize_offline() {
        return decided(
            IdTagInfo::with_status(AuthorizationStatus::Invalid),
            AuthSource::None,
        );
    }
    if config.local_auth_list_enabled()
        && let Some(info) = list.get(id_tag)
    {
        return decided(normalize_id_tag_info(info, now), AuthSource::LocalList);
    }
    if config.authorization_cache_enabled()
        && let Some(info) = cache.lookup(id_tag, now)
    {
        return decided(info, AuthSource::AuthorizationCache);
    }
    if require_offline_permission && config.allow_offline_tx_for_unknown_id() {
        return decided(
            IdTagInfo::with_status(AuthorizationStatus::Accepted),
            AuthSource::OfflineUnknown,
        );
    }
    decided(
        IdTagInfo::with_status(AuthorizationStatus::Invalid),
        AuthSource::None,
    )
}

/// Applies the CSMS's `IdTagInfo` (from `Authorize.conf` or `StartTransaction.conf`):
/// stores it in the cache when `AuthorizationCacheEnabled`, and returns the
/// decision with expiry applied (`source: CentralSystem`).
pub fn apply_online_id_tag_info(
    id_tag: &IdTag,
    info: &IdTagInfo,
    now: DateTime<Utc>,
    cache: &mut AuthorizationCache,
    config: &ConfigRegistry,
) -> AuthorizationDecision {
    if config.authorization_cache_enabled() {
        cache.remember(id_tag, info.clone(), now);
    }
    decided(normalize_id_tag_info(info, now), AuthSource::CentralSystem)
}

/// The parentIdTag of `id_tag` as the station already knows it (local list first,
/// then cache), without asking the CSMS. RemoteStart uses it to match a
/// reservation's parentIdTag when no Authorize has run.
pub fn known_parent(
    id_tag: &IdTag,
    now: DateTime<Utc>,
    list: &LocalAuthList,
    cache: &AuthorizationCache,
) -> Option<IdTag> {
    list.get(id_tag)
        .and_then(|info| info.parent_id_tag.clone())
        .or_else(|| {
            cache
                .lookup(id_tag, now)
                .and_then(|info| info.parent_id_tag)
        })
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};

    #[test]
    fn known_parent_prefers_the_list_and_falls_back_to_the_cache() {
        let parent = |name: &str| IdTagInfo {
            status: AuthorizationStatus::Accepted,
            expiry_date: None,
            parent_id_tag: Some(tag(name)),
        };
        let list = list_with(vec![entry("LISTED", parent("LISTPARENT"))]);
        let mut cache = AuthorizationCache::new();
        cache.remember(&tag("LISTED"), parent("CACHEPARENT"), now());
        cache.remember(&tag("CACHED"), parent("CACHEPARENT"), now());
        assert_eq!(
            known_parent(&tag("LISTED"), now(), &list, &cache),
            Some(tag("LISTPARENT"))
        );
        assert_eq!(
            known_parent(&tag("CACHED"), now(), &list, &cache),
            Some(tag("CACHEPARENT"))
        );
        assert_eq!(known_parent(&tag("UNKNOWN"), now(), &list, &cache), None);
    }

    use super::*;
    use crate::config::StationConfig;
    use crate::ocpp::messages::AuthorizationData;
    use crate::ocpp::messages::{SendLocalListRequest, UpdateStatus};
    use crate::ocpp::types::UpdateType;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
    }

    fn tag(name: &str) -> IdTag {
        IdTag::new(name).unwrap()
    }

    fn info(status: AuthorizationStatus) -> IdTagInfo {
        IdTagInfo::with_status(status)
    }

    fn config() -> ConfigRegistry {
        ConfigRegistry::new(&StationConfig::new("ws://h/p"))
    }

    fn with(mut config: ConfigRegistry, changes: &[(&str, &str)]) -> ConfigRegistry {
        for (name, value) in changes {
            config.change(name, value);
        }
        config
    }

    /// A local list with the given entries, loaded through the real update path.
    fn list_with(entries: Vec<AuthorizationData>) -> LocalAuthList {
        let mut list = LocalAuthList::default();
        let request = SendLocalListRequest {
            list_version: 1,
            local_authorization_list: Some(entries),
            update_type: UpdateType::Differential,
        };
        let limits = crate::station::local_auth_list::LocalListLimits {
            enabled: true,
            max_update_entries: 1000,
            max_entries: 1000,
        };
        assert_eq!(list.apply(&request, &limits), UpdateStatus::Accepted);
        list
    }

    fn entry(name: &str, info: IdTagInfo) -> AuthorizationData {
        AuthorizationData {
            id_tag: tag(name),
            id_tag_info: Some(info),
        }
    }

    fn offline_on() -> ConfigRegistry {
        with(config(), &[("LocalAuthorizeOffline", "true")])
    }

    #[test]
    fn offline_gate_rejects_everything_when_local_authorize_offline_is_off() {
        let list = list_with(vec![entry("TAG", info(AuthorizationStatus::Accepted))]);
        let mut cache = AuthorizationCache::new();
        cache.remember(&tag("CACHED"), info(AuthorizationStatus::Accepted), now());
        let config = config();
        let listed = authorize_offline(&tag("TAG"), now(), &list, &cache, &config);
        assert!(!listed.accepted);
        assert_eq!(listed.source, AuthSource::None);
        let cached = authorize_offline(&tag("CACHED"), now(), &list, &cache, &config);
        assert!(!cached.accepted);
        assert_eq!(cached.source, AuthSource::None);
    }

    #[test]
    fn local_list_hit_is_accepted_offline() {
        let list = list_with(vec![entry("TAG", info(AuthorizationStatus::Accepted))]);
        let decision = authorize_offline(
            &tag("TAG"),
            now(),
            &list,
            &AuthorizationCache::new(),
            &offline_on(),
        );
        assert!(decision.accepted);
        assert_eq!(decision.source, AuthSource::LocalList);
    }

    #[test]
    fn disabled_local_list_is_skipped() {
        let list = list_with(vec![entry("TAG", info(AuthorizationStatus::Accepted))]);
        let config = with(offline_on(), &[("LocalAuthListEnabled", "false")]);
        let decision = authorize_offline(
            &tag("TAG"),
            now(),
            &list,
            &AuthorizationCache::new(),
            &config,
        );
        assert!(!decision.accepted);
        assert_eq!(decision.source, AuthSource::None);
    }

    #[test]
    fn cache_hit_is_accepted_offline_and_disabled_cache_is_skipped() {
        let list = LocalAuthList::default();
        let mut cache = AuthorizationCache::new();
        cache.remember(&tag("CACHED"), info(AuthorizationStatus::Accepted), now());
        let decision = authorize_offline(&tag("CACHED"), now(), &list, &cache, &offline_on());
        assert!(decision.accepted);
        assert_eq!(decision.source, AuthSource::AuthorizationCache);

        let disabled = with(offline_on(), &[("AuthorizationCacheEnabled", "false")]);
        let skipped = authorize_offline(&tag("CACHED"), now(), &list, &cache, &disabled);
        assert!(!skipped.accepted);
        assert_eq!(skipped.source, AuthSource::None);
    }

    #[test]
    fn blocked_cache_entry_denies_offline() {
        let mut cache = AuthorizationCache::new();
        cache.remember(&tag("BLOCKED"), info(AuthorizationStatus::Blocked), now());
        let config = with(offline_on(), &[("AllowOfflineTxForUnknownId", "true")]);
        let decision = authorize_offline(
            &tag("BLOCKED"),
            now(),
            &LocalAuthList::default(),
            &cache,
            &config,
        );
        assert!(!decision.accepted);
        assert_eq!(decision.id_tag_info.status, AuthorizationStatus::Blocked);
        assert_eq!(decision.source, AuthSource::AuthorizationCache);
    }

    #[test]
    fn unknown_tag_offline_depends_on_allow_offline_tx_for_unknown_id() {
        let list = LocalAuthList::default();
        let cache = AuthorizationCache::new();
        let denied = authorize_offline(&tag("NEW"), now(), &list, &cache, &offline_on());
        assert!(!denied.accepted);
        assert_eq!(denied.source, AuthSource::None);

        let allowed_config = with(offline_on(), &[("AllowOfflineTxForUnknownId", "true")]);
        let allowed = authorize_offline(&tag("NEW"), now(), &list, &cache, &allowed_config);
        assert!(allowed.accepted);
        assert_eq!(allowed.source, AuthSource::OfflineUnknown);
    }

    #[test]
    fn expired_local_entry_is_expired_not_accepted_and_keeps_the_parent() {
        let expired = IdTagInfo {
            status: AuthorizationStatus::Accepted,
            expiry_date: Some(now() - Duration::minutes(1)),
            parent_id_tag: None,
        };
        let parent = IdTagInfo {
            status: AuthorizationStatus::Accepted,
            expiry_date: None,
            parent_id_tag: Some(tag("ROOTLOCAL")),
        };
        let list = list_with(vec![
            entry("EXPIREDLOCAL", expired),
            entry("PARENTLOCAL", parent),
        ]);
        let config = config();
        let cache = AuthorizationCache::new();

        let expired_decision =
            authorize_locally_for_test(&tag("EXPIREDLOCAL"), &list, &cache, &config);
        assert!(!expired_decision.accepted);
        assert_eq!(
            expired_decision.id_tag_info.status,
            AuthorizationStatus::Expired
        );

        let parent_decision =
            authorize_locally_for_test(&tag("PARENTLOCAL"), &list, &cache, &config);
        assert!(parent_decision.accepted);
        assert_eq!(
            parent_decision.id_tag_info.parent_id_tag,
            Some(tag("ROOTLOCAL"))
        );
    }

    fn authorize_locally_for_test(
        id_tag: &IdTag,
        list: &LocalAuthList,
        cache: &AuthorizationCache,
        config: &ConfigRegistry,
    ) -> AuthorizationDecision {
        pre_authorize(
            id_tag,
            now(),
            list,
            cache,
            &with(config.clone(), &[("LocalPreAuthorize", "true")]),
        )
        .expect("flag on")
    }

    #[test]
    fn cached_accept_expires_at_read_time_without_any_timer() {
        let mut cache = AuthorizationCache::new();
        let until = now() + Duration::minutes(10);
        cache.remember(
            &tag("SHORT"),
            IdTagInfo {
                status: AuthorizationStatus::Accepted,
                expiry_date: Some(until),
                parent_id_tag: None,
            },
            now(),
        );
        let config = with(offline_on(), &[("AllowOfflineTxForUnknownId", "true")]);
        let list = LocalAuthList::default();
        let before = authorize_offline(&tag("SHORT"), now(), &list, &cache, &config);
        assert!(before.accepted);
        let after = authorize_offline(&tag("SHORT"), until, &list, &cache, &config);
        assert!(
            !after.accepted,
            "expired cache entry must not fall through to unknown-tag allowance"
        );
        assert_eq!(after.id_tag_info.status, AuthorizationStatus::Expired);
        assert_eq!(after.source, AuthSource::AuthorizationCache);
    }

    #[test]
    fn pre_authorize_is_none_when_disabled() {
        assert!(
            pre_authorize(
                &tag("X"),
                now(),
                &LocalAuthList::default(),
                &AuthorizationCache::new(),
                &config(),
            )
            .is_none()
        );
    }

    #[test]
    fn pre_authorize_rejects_unknown_tags_even_when_unknown_tags_are_allowed_offline() {
        let config = with(
            config(),
            &[
                ("LocalPreAuthorize", "true"),
                ("AllowOfflineTxForUnknownId", "true"),
                ("LocalAuthorizeOffline", "false"),
            ],
        );
        let decision = pre_authorize(
            &tag("UNKNOWNLOCAL"),
            now(),
            &LocalAuthList::default(),
            &AuthorizationCache::new(),
            &config,
        )
        .expect("flag on");
        assert!(!decision.accepted);
        assert_eq!(decision.source, AuthSource::None);
    }

    #[test]
    fn pre_authorize_uses_the_list_even_when_offline_authorization_is_off() {
        let list = list_with(vec![entry("LISTED", info(AuthorizationStatus::Accepted))]);
        let config = with(config(), &[("LocalPreAuthorize", "true")]);
        let decision = pre_authorize(
            &tag("LISTED"),
            now(),
            &list,
            &AuthorizationCache::new(),
            &config,
        )
        .expect("flag on");
        assert!(decision.accepted);
        assert_eq!(decision.source, AuthSource::LocalList);
    }

    #[test]
    fn pre_authorize_with_disabled_list_rejects_a_listed_tag() {
        let list = list_with(vec![entry(
            "DISABLEDLOCAL",
            info(AuthorizationStatus::Accepted),
        )]);
        let config = with(
            config(),
            &[
                ("LocalPreAuthorize", "true"),
                ("LocalAuthListEnabled", "false"),
            ],
        );
        let decision = pre_authorize(
            &tag("DISABLEDLOCAL"),
            now(),
            &list,
            &AuthorizationCache::new(),
            &config,
        )
        .expect("flag on");
        assert!(!decision.accepted);
    }

    #[test]
    fn online_response_fills_the_cache_when_enabled() {
        let mut cache = AuthorizationCache::new();
        let decision = apply_online_id_tag_info(
            &tag("NOCACHE"),
            &info(AuthorizationStatus::Accepted),
            now(),
            &mut cache,
            &config(),
        );
        assert!(decision.accepted);
        assert_eq!(decision.source, AuthSource::CentralSystem);
        assert!(cache.contains(&tag("NOCACHE")));
    }

    #[test]
    fn online_response_does_not_touch_the_cache_when_disabled() {
        let mut cache = AuthorizationCache::new();
        let config = with(config(), &[("AuthorizationCacheEnabled", "false")]);
        apply_online_id_tag_info(
            &tag("NOCACHE"),
            &info(AuthorizationStatus::Accepted),
            now(),
            &mut cache,
            &config,
        );
        assert!(cache.is_empty());
    }

    #[test]
    fn online_expired_response_is_rejected_locally_after_normalization() {
        let mut cache = AuthorizationCache::new();
        let expired = IdTagInfo {
            status: AuthorizationStatus::Accepted,
            expiry_date: Some(now() - Duration::minutes(1)),
            parent_id_tag: None,
        };
        let decision =
            apply_online_id_tag_info(&tag("EXPIREDAUTH"), &expired, now(), &mut cache, &config());
        assert!(!decision.accepted);
        assert_eq!(decision.id_tag_info.status, AuthorizationStatus::Expired);
    }

    #[test]
    fn online_response_keeps_the_parent_and_concurrent_tx_is_not_accepted() {
        let mut cache = AuthorizationCache::new();
        let parent = IdTagInfo {
            status: AuthorizationStatus::Accepted,
            expiry_date: None,
            parent_id_tag: Some(tag("PARENTTAG")),
        };
        let accepted =
            apply_online_id_tag_info(&tag("PARENTAUTH"), &parent, now(), &mut cache, &config());
        assert!(accepted.accepted);
        assert_eq!(accepted.id_tag_info.parent_id_tag, Some(tag("PARENTTAG")));

        let concurrent = apply_online_id_tag_info(
            &tag("CONCURRENTAUTH"),
            &info(AuthorizationStatus::ConcurrentTx),
            now(),
            &mut cache,
            &config(),
        );
        assert!(!concurrent.accepted);
        assert_eq!(
            concurrent.id_tag_info.status,
            AuthorizationStatus::ConcurrentTx
        );
    }

    #[test]
    fn cache_evicts_the_oldest_accepted_entry_first() {
        let mut cache = AuthorizationCache::with_capacity(2);
        cache.remember(&tag("OLD"), info(AuthorizationStatus::Accepted), now());
        cache.remember(&tag("MID"), info(AuthorizationStatus::Accepted), now());
        cache.remember(&tag("NEW"), info(AuthorizationStatus::Accepted), now());
        assert_eq!(cache.len(), 2);
        assert!(!cache.contains(&tag("OLD")));
        assert!(cache.contains(&tag("MID")));
        assert!(cache.contains(&tag("NEW")));
    }

    #[test]
    fn cache_keeps_denials_while_an_accepted_entry_can_go() {
        let mut cache = AuthorizationCache::with_capacity(2);
        cache.remember(&tag("BLOCKED"), info(AuthorizationStatus::Blocked), now());
        cache.remember(&tag("ACCEPTED"), info(AuthorizationStatus::Accepted), now());
        cache.remember(&tag("NEW"), info(AuthorizationStatus::Accepted), now());
        assert!(
            cache.contains(&tag("BLOCKED")),
            "a denial must outlive accepted entries"
        );
        assert!(!cache.contains(&tag("ACCEPTED")));
    }

    #[test]
    fn cache_keeps_expired_entries_as_denials() {
        let mut cache = AuthorizationCache::with_capacity(2);
        let expired = IdTagInfo {
            status: AuthorizationStatus::Accepted,
            expiry_date: Some(now() - Duration::minutes(5)),
            parent_id_tag: None,
        };
        cache.remember(&tag("EXPIRED"), expired, now());
        cache.remember(&tag("LIVE"), info(AuthorizationStatus::Accepted), now());
        cache.remember(&tag("NEW"), info(AuthorizationStatus::Accepted), now());
        assert!(
            cache.contains(&tag("EXPIRED")),
            "an expired entry is a denial"
        );
        assert!(
            !cache.contains(&tag("LIVE")),
            "a live accept is the eviction victim"
        );
        assert!(cache.contains(&tag("NEW")));
    }

    #[test]
    fn cache_is_bounded_and_zero_capacity_stores_nothing() {
        let mut cache = AuthorizationCache::with_capacity(3);
        for i in 0..50 {
            cache.remember(
                &tag(&format!("T{i}")),
                info(AuthorizationStatus::Accepted),
                now(),
            );
        }
        assert_eq!(cache.len(), 3);

        let mut none = AuthorizationCache::with_capacity(0);
        none.remember(&tag("X"), info(AuthorizationStatus::Accepted), now());
        assert!(none.is_empty());
    }

    #[test]
    fn remembering_an_existing_tag_replaces_it_without_evicting() {
        let mut cache = AuthorizationCache::with_capacity(2);
        cache.remember(&tag("A"), info(AuthorizationStatus::Accepted), now());
        cache.remember(&tag("B"), info(AuthorizationStatus::Accepted), now());
        cache.remember(&tag("A"), info(AuthorizationStatus::Blocked), now());
        assert_eq!(cache.len(), 2);
        assert_eq!(
            cache.lookup(&tag("A"), now()).map(|i| i.status),
            Some(AuthorizationStatus::Blocked)
        );
    }

    #[test]
    fn clear_empties_the_cache() {
        let mut cache = AuthorizationCache::new();
        cache.remember(&tag("A"), info(AuthorizationStatus::Accepted), now());
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn cache_roundtrips_and_restores_the_default_capacity() {
        let mut cache = AuthorizationCache::with_capacity(5);
        cache.remember(&tag("A"), info(AuthorizationStatus::Accepted), now());
        let json = serde_json::to_string(&cache).unwrap();
        let back: AuthorizationCache = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back.capacity(), DEFAULT_CACHE_CAPACITY);
    }

    #[test]
    fn online_denials_are_never_accepted() {
        for status in [
            AuthorizationStatus::Blocked,
            AuthorizationStatus::Invalid,
            AuthorizationStatus::ConcurrentTx,
        ] {
            let mut cache = AuthorizationCache::new();
            let decision =
                apply_online_id_tag_info(&tag("X"), &info(status), now(), &mut cache, &config());
            assert!(!decision.accepted, "{status:?}");
            assert_eq!(decision.id_tag_info.status, status);
        }
    }

    #[test]
    fn deleted_local_entry_no_longer_authorizes_from_the_list() {
        let mut list = list_with(vec![entry("TAG1", info(AuthorizationStatus::Accepted))]);
        let delete = SendLocalListRequest {
            list_version: 2,
            local_authorization_list: Some(vec![AuthorizationData {
                id_tag: tag("TAG1"),
                id_tag_info: None,
            }]),
            update_type: UpdateType::Differential,
        };
        let limits = crate::station::local_auth_list::LocalListLimits {
            enabled: true,
            max_update_entries: 1000,
            max_entries: 1000,
        };
        assert_eq!(list.apply(&delete, &limits), UpdateStatus::Accepted);
        let decision = authorize_offline(
            &tag("TAG1"),
            now(),
            &list,
            &AuthorizationCache::new(),
            &offline_on(),
        );
        assert_ne!(decision.source, AuthSource::LocalList);
        assert!(!decision.accepted);
    }

    #[test]
    fn decision_serializes_with_the_source_name() {
        let decision = decided(
            info(AuthorizationStatus::Accepted),
            AuthSource::OfflineUnknown,
        );
        let json = serde_json::to_value(&decision).unwrap();
        assert_eq!(json["source"], "offline_unknown");
        assert_eq!(json["id_tag_info"]["status"], "Accepted");
    }
}
