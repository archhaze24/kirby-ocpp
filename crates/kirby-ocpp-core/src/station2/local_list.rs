//! The Local Authorization List (D01, D02, C13, C14).
//!
//! The list maps a token (kind and text) to the status and expiry the CSMS sent. Only those
//! two facts are kept: they are all the authorization decision reads.
//!
//! `SendLocalList` is checked in this order, and the list is replaced only when every check
//! passes, so a refused update leaves the list and its version as they were:
//!
//! 1. the version is not greater than 0 (D01.FR.18), or the request is over the size caps
//!    (`ItemsPerMessage`, `BytesPerMessage`) -> `Failed`;
//! 2. a full update has an entry without `idTokenInfo` -> `Failed`;
//! 3. the list after the update would hold more than [`MAX_ENTRIES`] tokens -> `Failed`;
//! 4. a differential update with a version not above the current one -> `VersionMismatch`
//!    (D01.FR.19).
//!
//! A full update replaces the list, a differential one adds, replaces and removes entries; the
//! last entry for a token wins. The caps match the ones of the 1.6 station.
//!
//! `LocalAuthListCtrlr.Enabled` only switches the use of the list: updates are applied and the
//! list is persisted either way, but an authorization ignores a disabled list.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::actor::Station2Actor;
use super::types::IdTokenRef;
use crate::ocpp::responder::Responder;
use crate::ocpp::v2::messages::{
    GetLocalListVersion, GetLocalListVersionRequest, GetLocalListVersionResponse, SendLocalList,
    SendLocalListRequest, SendLocalListResponse, SendLocalListStatus, UpdateKind,
};
use crate::ocpp::v2::types::{AuthorizationStatus, IdToken, WireEnum};
use crate::station::LogLevel;

/// `LocalAuthListCtrlr` as named in the device model.
pub(super) const COMPONENT: &str = "LocalAuthListCtrlr";

/// Most tokens the list holds (the `maxLimit` of `Entries`).
pub(super) const MAX_ENTRIES: usize = 1_000;
/// Most entries one `SendLocalList` may carry (`ItemsPerMessage`).
pub(super) const ITEMS_PER_MESSAGE: usize = 1_000;
/// Largest `SendLocalList` request, serialized as JSON (`BytesPerMessage`).
pub(super) const BYTES_PER_MESSAGE: usize = 262_144;

/// What the list knows about a token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListEntry {
    /// The `AuthorizationStatusEnumType` text.
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
}

impl ListEntry {
    pub(super) fn is_accepted(&self) -> bool {
        self.status == AuthorizationStatus::Accepted.as_str()
    }
}

/// The local list as persisted and held: the version and the entries by token key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalListPersist {
    /// The version of the last accepted update; `0` while no list was ever sent.
    pub version: i32,
    pub entries: BTreeMap<String, ListEntry>,
}

/// The key of a token in the list: the same text as in the authorization cache.
pub(super) fn key_of(token: &IdTokenRef) -> String {
    format!("{}|{}", token.kind.as_str(), token.id_token)
}

fn key_of_wire(token: &IdToken) -> String {
    format!("{}|{}", token.kind.as_str(), token.id_token)
}

impl LocalListPersist {
    /// Applies an update; see the module documentation for the checks.
    pub(super) fn apply(&mut self, request: &SendLocalListRequest) -> SendLocalListStatus {
        let update = request.local_authorization_list.as_deref().unwrap_or(&[]);
        let too_large = update.len() > ITEMS_PER_MESSAGE
            || serde_json::to_vec(request).map_or(true, |bytes| bytes.len() > BYTES_PER_MESSAGE);
        if request.version_number <= 0 || too_large {
            return SendLocalListStatus::Failed;
        }
        let full = request.update_type == UpdateKind::Full;
        if full && update.iter().any(|item| item.id_token_info.is_none()) {
            return SendLocalListStatus::Failed;
        }
        let mut next = if full {
            BTreeMap::new()
        } else {
            self.entries.clone()
        };
        for item in update {
            let key = key_of_wire(&item.id_token);
            match &item.id_token_info {
                Some(info) => {
                    next.insert(
                        key,
                        ListEntry {
                            status: info.status.as_str().to_owned(),
                            expires_at: info.cache_expiry_date_time,
                        },
                    );
                }
                None => {
                    next.remove(&key);
                }
            }
        }
        if next.len() > MAX_ENTRIES {
            return SendLocalListStatus::Failed;
        }
        if !full && request.version_number <= self.version {
            return SendLocalListStatus::VersionMismatch;
        }
        self.entries = next;
        self.version = request.version_number;
        SendLocalListStatus::Accepted
    }

    /// Drops what a file may hold beyond the caps; keeps the first tokens in key order.
    pub(super) fn bounded(mut self) -> Self {
        if self.entries.len() > MAX_ENTRIES {
            self.entries = std::mem::take(&mut self.entries)
                .into_iter()
                .take(MAX_ENTRIES)
                .collect();
        }
        self
    }
}

impl Station2Actor {
    /// `LocalAuthListCtrlr.Enabled`.
    pub(super) fn local_list_enabled(&self) -> bool {
        self.dm_enabled(COMPONENT)
    }

    /// `LocalAuthListCtrlr.DisablePostAuthorize` (C14.FR.03).
    pub(super) fn local_list_disable_post_authorize(&self) -> bool {
        self.dm_text(COMPONENT, "DisablePostAuthorize").as_deref() == Some("true")
    }

    /// Tokens in the list, for `Entries`.
    pub(super) fn local_list_len(&self) -> usize {
        self.local_list.entries.len()
    }

    /// The entry of `token` while the list is enabled. An entry past its expiry reads as
    /// absent (C14.FR.05).
    pub(super) fn local_list_entry(&self, token: &IdTokenRef) -> Option<ListEntry> {
        if !self.local_list_enabled() {
            return None;
        }
        let now = self.clock.now_utc();
        self.local_list
            .entries
            .get(&key_of(token))
            .filter(|entry| entry.expires_at.is_none_or(|at| at > now))
            .cloned()
    }

    /// The list for the state file.
    pub(super) fn local_list_persist(&self) -> LocalListPersist {
        self.local_list.clone()
    }

    /// Takes back the persisted list at start.
    pub(super) fn local_list_restore(&mut self, list: LocalListPersist) {
        self.local_list = list.bounded();
    }
}

// ---- CALL handlers ---------------------------------------------------------------------

#[allow(
    clippy::needless_pass_by_value,
    reason = "handlers share the signature `fn(&mut Station2Actor, Responder<A>, A::Req)`"
)]
pub(super) fn send_local_list(
    actor: &mut Station2Actor,
    responder: Responder<SendLocalList>,
    request: SendLocalListRequest,
) {
    let status = actor.local_list.apply(&request);
    actor.log(
        LogLevel::Info,
        format!(
            "SendLocalList {} version {}: {}",
            request.update_type.as_str(),
            request.version_number,
            status.as_str()
        ),
    );
    actor.respond(
        responder,
        SendLocalListResponse {
            status,
            status_info: None,
            custom_data: None,
        },
    );
    if status == SendLocalListStatus::Accepted {
        actor.flush_now();
        actor.refresh_variable_views();
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "handlers share the signature `fn(&mut Station2Actor, Responder<A>, A::Req)`"
)]
pub(super) fn get_local_list_version(
    actor: &mut Station2Actor,
    responder: Responder<GetLocalListVersion>,
    _request: GetLocalListVersionRequest,
) {
    // D02.FR.03: a disabled list reports 0 like a missing one.
    let version_number = if actor.local_list_enabled() {
        actor.local_list.version
    } else {
        0
    };
    actor.respond(
        responder,
        GetLocalListVersionResponse {
            version_number,
            custom_data: None,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::station2::device_model::{Dflt, TABLE};

    #[test]
    fn the_table_reports_the_caps() {
        let spec = |name: &str| {
            TABLE
                .iter()
                .find(|spec| spec.component == COMPONENT && spec.variable == name)
                .unwrap_or_else(|| panic!("{name} is in the table"))
        };
        assert_eq!(
            spec("ItemsPerMessage").default,
            Dflt::Fixed("1000"),
            "ITEMS_PER_MESSAGE = {ITEMS_PER_MESSAGE}"
        );
        assert_eq!(ITEMS_PER_MESSAGE, 1_000);
        assert_eq!(spec("BytesPerMessage").default, Dflt::Fixed("262144"));
        assert_eq!(BYTES_PER_MESSAGE, 262_144);
        assert_eq!(spec("Entries").max, i32::try_from(MAX_ENTRIES).ok());
    }
}
