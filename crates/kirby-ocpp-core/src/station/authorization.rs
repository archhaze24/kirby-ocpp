//! Authorization: local list, authorization cache, offline rules.
//!
//! TS reference: `src/station/authorization.ts` (`authorizeLocally`), the
//! Authorize / IdTagInfo handling in `src/station.ts` and
//! `src/station/local-auth-list.ts`.
//!
//! Order to preserve (`core-analysis.md`): offline permission gate
//! (`LocalAuthorizeOffline`), local list (if enabled), cache (if enabled), then
//! `AllowOfflineTxForUnknownId`. `Expired` is computed at read time from
//! `expiryDate`; both CSMS responses rewrite the cache. Full `SendLocalList`
//! entries without `idTagInfo` must not default to `Accepted` (known TS bug).
//!
//! Only the data types shared with the public API live here for now.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::IdTag;
use crate::ocpp::types::AuthorizationStatus;

/// OCPP `IdTagInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdTagInfo {
    pub status: AuthorizationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry_date: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id_tag: Option<IdTag>,
}

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
