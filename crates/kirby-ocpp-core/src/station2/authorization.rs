//! Authorization: the `Authorize` exchange, the authorization cache and the offline decision.

#![allow(
    dead_code,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    reason = "placeholder module; remove this allow when the module is implemented"
)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::actor::Station2Actor;
use super::commands::not_implemented;
use super::types::{IdTokenRef, Reply2};
use crate::ocpp::v2::types::IdTokenType;

/// How a token was decided.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthDecision {
    /// Accepted; carries the `idTokenInfo` when the CSMS sent one.
    Accepted(Option<Value>),
    /// Not accepted, with the status and the `idTokenInfo` as received.
    Rejected { status: String, info: Option<Value> },
    /// Decided locally because the CSMS could not be asked.
    Offline { accepted: bool },
}

/// One entry of the authorization cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEntry {
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub group_id: Option<String>,
}

/// The authorization cache, as persisted: `(token key, entry)` pairs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthCachePersist {
    pub entries: Vec<(String, CacheEntry)>,
}

/// The authorization cache and the tokens in use.
#[derive(Debug, Default)]
pub(crate) struct AuthState {}

impl Station2Actor {
    /// Decides whether `token` may be used; `cont` runs with the decision.
    pub(super) fn authorize(
        &mut self,
        _token: IdTokenRef,
        _evse: Option<u32>,
        _cont: impl FnOnce(&mut Self, AuthDecision) + Send + 'static,
    ) {
    }

    /// An `idTokenInfo` arrived in a `TransactionEventResponse` for the transaction
    /// `transaction_id` on `evse`: updates the cache and applies `StopTxOnInvalidId`.
    pub(super) fn on_id_token_info(&mut self, _evse: u32, _transaction_id: &str, _info: &Value) {}

    /// The authorization cache, for the state file.
    pub(super) fn auth_cache_persist(&self) -> AuthCachePersist {
        AuthCachePersist::default()
    }

    /// Takes back the persisted authorization cache at start.
    pub(super) fn auth_cache_restore(&mut self, _cache: AuthCachePersist) {}

    /// `Command2::PresentToken`.
    pub(super) fn cmd_present_token(
        &mut self,
        _evse: Option<u32>,
        _id_token: String,
        _token_type: IdTokenType,
        reply: Reply2,
    ) {
        not_implemented(reply, "present_token");
    }

    /// `Command2::Authorize`.
    pub(super) fn cmd_authorize(
        &mut self,
        _id_token: String,
        _token_type: IdTokenType,
        reply: Reply2,
    ) {
        not_implemented(reply, "authorize");
    }
}
