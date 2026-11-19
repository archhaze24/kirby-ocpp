//! Authorization: the `Authorize` exchange, the authorization cache and the offline decision.
//!
//! This module decides whether a token is accepted (C01, C02, C05, C12, C15); it never
//! evaluates start or stop points. An accepted token goes to the transaction engine
//! (`on_authorized`), a token that stops being valid in a `TransactionEventResponse` to
//! `on_deauthorized` or `set_charging_state_internal`. There is no Local Authorization List.
//!
//! Cache entries expire at the earlier of the `cacheExpiryDateTime` of the CSMS and the
//! `AuthCacheCtrlr.LifeTime` after the entry was last updated (C10.FR.10); a `LifeTime` of 0
//! adds no limit. At most [`CACHE_LIMIT`] entries are kept: entries that are not `Accepted`
//! go first, then the oldest (C10.FR.07).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::time::Instant;

use super::actor::Station2Actor;
use super::calls::call_failure;
use super::command::Outcome2;
use super::device_model::Lookup;
use super::link::sanitize;
use super::types::{IdTokenRef, Origin2, Reply2};
use crate::error::{CallError, StationError};
use crate::ocpp::v2::messages::{Authorize, AuthorizeRequest, AuthorizeResponse, TriggerReason};
use crate::ocpp::v2::types::{
    AuthorizationStatus, ChargingState, Component, IdToken, IdTokenInfo, IdTokenType, StopReason,
    Variable, WireEnum,
};
use crate::station::LogLevel;

/// Most entries the authorization cache holds.
const CACHE_LIMIT: usize = 1000;

/// The `status` text of an accepted token.
const ACCEPTED: &str = "Accepted";

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

/// The authorization cache and the bookkeeping of a reset in progress.
#[derive(Debug, Default)]
pub(crate) struct AuthState {
    /// Cache entries, oldest first.
    cache: Vec<(String, CacheEntry)>,
    /// When the reset stops waiting for the transaction events to be delivered. It lives here
    /// because `ResetPlan` is a shared type with fixed fields; `None` until the transactions
    /// of the reset have been ended. Monotonic time: the actor clock may be a manual one.
    pub(super) reset_deadline: Option<Instant>,
    /// The transactions of an `Immediate` reset were ended already.
    pub(super) reset_ended: bool,
}

/// What a decision is for: a local presentation may use the cache instead of asking the CSMS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    /// Cached `Accepted` tokens are not sent to the CSMS first (C12.FR.02).
    CacheFirst,
    /// An online station always sends `Authorize` (F01.FR.01).
    Always,
}

/// The key of a token in the cache.
fn token_key(token: &IdTokenRef) -> String {
    format!("{}|{}", token.kind.as_str(), token.id_token)
}

/// Tokens of these types need no `Authorize` and are never cached (C02.FR.02, C05.FR.01).
fn needs_no_check(kind: IdTokenType) -> bool {
    matches!(kind, IdTokenType::NoAuthorization | IdTokenType::Central)
}

/// The `groupIdToken` of an `idTokenInfo` as received.
fn group_in(info: Option<&Value>) -> Option<&str> {
    info?.pointer("/groupIdToken/idToken")?.as_str()
}

fn wire_token(token: &IdTokenRef) -> IdToken {
    IdToken {
        additional_info: None,
        id_token: token.id_token.clone(),
        kind: token.kind,
        custom_data: None,
    }
}

impl Station2Actor {
    // ---- device-model switches ---------------------------------------------------

    /// The `Actual` value of a station-level variable, when the table has it.
    fn dm_text(&self, component: &str, variable: &str) -> Option<String> {
        let component = Component {
            evse: None,
            name: component.to_owned(),
            instance: None,
            custom_data: None,
        };
        let variable = Variable {
            name: variable.to_owned(),
            instance: None,
            custom_data: None,
        };
        match self.dm.find(&component, &variable) {
            Lookup::Found(target) => self.dm.read(&target, &|_| String::new()),
            _ => None,
        }
    }

    fn dm_enabled(&self, component: &str) -> bool {
        self.dm_text(component, "Enabled").as_deref() != Some("false")
    }

    /// `AuthCtrlr.Enabled`: without it energy is offered without authorization (C01.FR.01).
    fn auth_enabled(&self) -> bool {
        self.dm_enabled("AuthCtrlr")
    }

    /// `AuthCacheCtrlr.Enabled`.
    pub(super) fn cache_enabled(&self) -> bool {
        self.dm_enabled("AuthCacheCtrlr")
    }

    /// `AuthCacheCtrlr.LifeTime`; `None` for 0.
    fn cache_life_time(&self) -> Option<chrono::Duration> {
        let secs: i64 = self.dm_text("AuthCacheCtrlr", "LifeTime")?.parse().ok()?;
        (secs > 0).then(|| chrono::Duration::seconds(secs))
    }

    // ---- the cache ---------------------------------------------------------------

    /// A valid entry; an expired one is dropped.
    fn cache_lookup(&mut self, key: &str) -> Option<CacheEntry> {
        let now = self.clock.now_utc();
        let position = self.auth.cache.iter().position(|(k, _)| k == key)?;
        if self.auth.cache[position]
            .1
            .expires_at
            .is_some_and(|at| at <= now)
        {
            self.auth.cache.remove(position);
            self.mark_dirty();
            return None;
        }
        Some(self.auth.cache[position].1.clone())
    }

    /// A valid entry, without touching the cache.
    fn cache_peek(&self, key: &str) -> Option<&CacheEntry> {
        let now = self.clock.now_utc();
        self.auth
            .cache
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, entry)| entry)
            .filter(|entry| entry.expires_at.is_none_or(|at| at > now))
    }

    /// Stores what the CSMS said about `token` (C12.FR.06); `Central` and `NoAuthorization`
    /// tokens are never stored (C05.FR.02). An `idTokenInfo` that already expired removes the
    /// entry instead (C10).
    fn cache_store(&mut self, token: &IdTokenRef, info: &IdTokenInfo) {
        if needs_no_check(token.kind) || !self.cache_enabled() {
            return;
        }
        let key = token_key(token);
        let now = self.clock.now_utc();
        let limit = self.cache_life_time().map(|life| now + life);
        let expires_at = match (info.cache_expiry_date_time, limit) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.auth.cache.retain(|(k, _)| *k != key);
        if expires_at.is_some_and(|at| at <= now) {
            self.mark_dirty();
            return;
        }
        if self.auth.cache.len() >= CACHE_LIMIT {
            let victim = self
                .auth
                .cache
                .iter()
                .position(|(_, entry)| entry.status != ACCEPTED)
                .unwrap_or(0);
            self.auth.cache.remove(victim);
        }
        self.auth.cache.push((
            key,
            CacheEntry {
                status: info.status.as_str().to_owned(),
                expires_at,
                group_id: info.group_id_token.as_ref().map(|g| g.id_token.clone()),
            },
        ));
        self.mark_dirty();
    }

    /// Empties the cache (`ClearCache`).
    pub(super) fn cache_clear(&mut self) {
        self.auth.cache.clear();
        self.mark_dirty();
    }

    /// The authorization cache, for the state file.
    pub(super) fn auth_cache_persist(&self) -> AuthCachePersist {
        AuthCachePersist {
            entries: self.auth.cache.clone(),
        }
    }

    /// Takes back the persisted authorization cache at start: expired entries are dropped, at
    /// most [`CACHE_LIMIT`] of the newest are kept.
    pub(super) fn auth_cache_restore(&mut self, cache: AuthCachePersist) {
        let now = self.clock.now_utc();
        let mut entries: Vec<_> = cache
            .entries
            .into_iter()
            .filter(|(_, entry)| entry.expires_at.is_none_or(|at| at > now))
            .collect();
        let excess = entries.len().saturating_sub(CACHE_LIMIT);
        entries.drain(..excess);
        self.auth.cache = entries;
    }

    // ---- the decision ------------------------------------------------------------

    /// Decides whether `token` may be used; `cont` runs with the decision.
    pub(super) fn authorize(
        &mut self,
        token: IdTokenRef,
        evse: Option<u32>,
        cont: impl FnOnce(&mut Self, AuthDecision) + Send + 'static,
    ) {
        self.authorize_with(token, evse, Ask::CacheFirst, cont);
    }

    /// Like [`Self::authorize`], but an online station asks the CSMS even for a cached token
    /// (`RequestStartTransaction` with `AuthorizeRemoteStart`, F01.FR.01).
    pub(super) fn authorize_asking_csms(
        &mut self,
        token: IdTokenRef,
        evse: Option<u32>,
        cont: impl FnOnce(&mut Self, AuthDecision) + Send + 'static,
    ) {
        self.authorize_with(token, evse, Ask::Always, cont);
    }

    fn authorize_with(
        &mut self,
        token: IdTokenRef,
        evse: Option<u32>,
        ask: Ask,
        cont: impl FnOnce(&mut Self, AuthDecision) + Send + 'static,
    ) {
        if !self.auth_enabled() || needs_no_check(token.kind) {
            cont(self, AuthDecision::Accepted(None));
            return;
        }
        let key = token_key(&token);
        let cached = if self.cache_enabled() {
            self.cache_lookup(&key)
        } else {
            None
        };
        let online =
            self.link.conn.is_some() && self.may_send("Authorize", Origin2::Station).is_ok();
        if !online {
            let decision = self.offline_decision(cached.as_ref());
            cont(self, decision);
            return;
        }
        if ask == Ask::CacheFirst && cached.as_ref().is_some_and(|e| e.status == ACCEPTED) {
            if self.local_pre_authorize() {
                self.refresh_cached(token, evse);
            }
            cont(self, AuthDecision::Accepted(None));
            return;
        }
        let request = AuthorizeRequest {
            id_token: wire_token(&token),
            certificate: None,
            iso15118_certificate_hash_data: None,
            custom_data: None,
        };
        self.call::<Authorize>(Origin2::Station, &request, move |actor, result| {
            let decision = actor.decision_from_answer(&token, result);
            cont(actor, decision);
        });
    }

    /// The decision for the answer of an `Authorize` CALL; a failed CALL counts as offline.
    fn decision_from_answer(
        &mut self,
        token: &IdTokenRef,
        result: Result<AuthorizeResponse, CallError>,
    ) -> AuthDecision {
        match result {
            Ok(response) => {
                self.cache_store(token, &response.id_token_info);
                let info = serde_json::to_value(&response.id_token_info).ok();
                if response.id_token_info.status == AuthorizationStatus::Accepted {
                    AuthDecision::Accepted(info)
                } else {
                    AuthDecision::Rejected {
                        status: response.id_token_info.status.as_str().to_owned(),
                        info,
                    }
                }
            }
            Err(error) => {
                self.log(
                    LogLevel::Warn,
                    format!(
                        "Authorize failed ({}); deciding locally",
                        sanitize(&error.to_string())
                    ),
                );
                let cached = self.cache_lookup(&token_key(token));
                self.offline_decision(cached.as_ref())
            }
        }
    }

    /// C12/C15: a cached `Accepted` token is accepted with `LocalAuthorizeOffline`, an unknown
    /// one with `OfflineTxForUnknownIdEnabled`; neither is added to the cache (C15.FR.01).
    fn offline_decision(&self, cached: Option<&CacheEntry>) -> AuthDecision {
        let accepted = match cached {
            Some(entry) => entry.status == ACCEPTED && self.local_authorize_offline(),
            None => self.offline_tx_for_unknown_id(),
        };
        AuthDecision::Offline { accepted }
    }

    /// `LocalPreAuthorize`: the transaction starts from the cache, and the CSMS is asked all
    /// the same; an answer other than `Accepted` is handled like one in a
    /// `TransactionEventResponse`.
    fn refresh_cached(&mut self, token: IdTokenRef, evse: Option<u32>) {
        let request = AuthorizeRequest {
            id_token: wire_token(&token),
            certificate: None,
            iso15118_certificate_hash_data: None,
            custom_data: None,
        };
        self.call::<Authorize>(Origin2::Station, &request, move |actor, result| {
            let Ok(response) = result else {
                return;
            };
            actor.cache_store(&token, &response.id_token_info);
            if response.id_token_info.status != AuthorizationStatus::Accepted
                && let Some(evse) = evse
                && actor.authorized_token(evse) == Some(&token)
            {
                actor.apply_invalid_id(evse);
            }
        });
    }

    // ---- idTokenInfo of a TransactionEventResponse (E05, C15) ---------------------

    /// An `idTokenInfo` arrived in a `TransactionEventResponse` for the transaction
    /// `transaction_id` on `evse`: updates the cache and applies `StopTxOnInvalidId`.
    ///
    /// The answer belongs to the token of the event it answers (`event_token`, when that event
    /// carried one) or else to the token of the transaction `transaction_id`, when that
    /// transaction is the one running on `evse`. With neither the cache is left alone: the
    /// running transaction may be another one with another token.
    pub(super) fn on_id_token_info(
        &mut self,
        evse: u32,
        transaction_id: &str,
        event_token: Option<IdTokenRef>,
        info: &Value,
    ) {
        let parsed = match serde_json::from_value::<IdTokenInfo>(info.clone()) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.log(
                    LogLevel::Warn,
                    format!(
                        "idTokenInfo of a TransactionEventResponse cannot be read: {}",
                        sanitize(&error.to_string())
                    ),
                );
                return;
            }
        };
        let (current, active_token) = self
            .active_tx(evse)
            .map_or((None, None), |tx| (Some(tx.id.0.clone()), tx.token.clone()));
        let of_transaction = if current.as_deref() == Some(transaction_id) {
            active_token
        } else {
            None
        };
        if let Some(token) = event_token.or(of_transaction) {
            self.cache_store(&token, &parsed);
            if current.as_deref() == Some(transaction_id)
                && parsed.status == AuthorizationStatus::Accepted
            {
                let group = parsed.group_id_token.as_ref().map(|g| g.id_token.clone());
                self.set_authorization_group(evse, &token, group);
            }
        }
        let ongoing = self.tx_by_id(transaction_id).is_some_and(|tx| tx.ongoing);
        if parsed.status != AuthorizationStatus::Accepted
            && ongoing
            && current.as_deref() == Some(transaction_id)
        {
            self.log(
                LogLevel::Warn,
                format!(
                    "the CSMS reports the token of transaction {transaction_id} as {}",
                    parsed.status.as_str()
                ),
            );
            self.apply_invalid_id(evse);
        }
    }

    /// `StopTxOnInvalidId` (C15.FR.03/04/06). With it set the transaction engine ends the
    /// transaction or only stops the energy, depending on the stop points; without it the
    /// energy stops and the transaction stays open.
    fn apply_invalid_id(&mut self, evse: u32) {
        if self.stop_tx_on_invalid_id() {
            self.on_deauthorized(evse, TriggerReason::Deauthorized, StopReason::DeAuthorized);
        } else {
            self.set_charging_state_internal(
                evse,
                ChargingState::SuspendedEvse,
                TriggerReason::ChargingStateChanged,
            );
        }
    }

    // ---- commands ----------------------------------------------------------------

    /// `Command2::PresentToken`.
    pub(super) fn cmd_present_token(
        &mut self,
        evse: Option<u32>,
        id_token: String,
        token_type: IdTokenType,
        reply: Reply2,
    ) {
        let token = IdTokenRef {
            id_token,
            kind: token_type,
        };
        let evse = match self.pick_evse(evse, &token) {
            Ok(evse) => evse,
            Err(error) => {
                // The requester may have gone away; nothing to do about that.
                let _ = reply.send(Err(error));
                return;
            }
        };
        // C01.FR.03: the token that started the authorization ends it, without `Authorize`.
        if self.token_ends_authorization(evse, &token, None) {
            self.on_stopped_by_token(evse, token);
            let _ = reply.send(Ok(Outcome2::Done));
            return;
        }
        let presented = token.clone();
        self.authorize(token, Some(evse), move |actor, decision| {
            actor.finish_presentation(evse, presented, decision, reply);
        });
    }

    /// The EVSE a token is presented to: the given one; else the one this token authorized,
    /// the first with an EV connected and no authorization, or the first free operative one.
    fn pick_evse(&self, evse: Option<u32>, token: &IdTokenRef) -> Result<u32, StationError> {
        if let Some(id) = evse {
            return if self.evse(id).is_some() {
                Ok(id)
            } else {
                Err(StationError::InvalidArgument(format!(
                    "EVSE {id} does not exist"
                )))
            };
        }
        let ids: Vec<u32> = self.evses.iter().map(|e| e.id).collect();
        let free = |id: &u32| self.authorized_token(*id).is_none() && self.active_tx(*id).is_none();
        let connected = |id: &u32| {
            self.evse(*id)
                .is_some_and(|e| e.connectors.iter().any(|c| c.ev_connected))
        };
        ids.iter()
            .copied()
            .find(|id| self.token_ends_authorization(*id, token, None))
            .or_else(|| ids.iter().copied().find(|id| free(id) && connected(id)))
            .or_else(|| {
                ids.iter()
                    .copied()
                    .find(|id| free(id) && self.evse_operative(*id))
            })
            .ok_or_else(|| StationError::InvalidState {
                connector: None,
                reason: "no EVSE is free for this token".to_owned(),
            })
    }

    /// The group of an accepted `token`: the one the fresh answer named, else the cache's.
    fn group_of_token(&self, token: &IdTokenRef, fresh: Option<&str>) -> Option<String> {
        fresh.map(str::to_owned).or_else(|| {
            self.cache_peek(&token_key(token))
                .filter(|entry| entry.status == ACCEPTED)
                .and_then(|entry| entry.group_id.clone())
        })
    }

    /// Keeps the group of `token` with the authorization it just started on `evse`.
    pub(super) fn note_authorization_group(
        &mut self,
        evse: u32,
        token: &IdTokenRef,
        info: Option<&Value>,
    ) {
        let group = self.group_of_token(token, group_in(info));
        self.set_authorization_group(evse, token, group);
    }

    /// Whether `token` is the one that authorized `evse`, or accepted with the same group
    /// (C01.FR.03). `fresh` is the group of the `Authorize` answer for `token`, when there is
    /// one; the group of the authorization is the one kept with it, else the cache's.
    fn token_ends_authorization(&self, evse: u32, token: &IdTokenRef, fresh: Option<&str>) -> bool {
        let Some(started) = self.authorized_token(evse) else {
            return false;
        };
        if started == token {
            return true;
        }
        let started_group = self
            .authorization_group(evse)
            .map(str::to_owned)
            .or_else(|| self.group_of_token(started, None));
        match (self.group_of_token(token, fresh), started_group) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    /// Answers `PresentToken` once the token has been decided.
    fn finish_presentation(
        &mut self,
        evse: u32,
        token: IdTokenRef,
        decision: AuthDecision,
        reply: Reply2,
    ) {
        let (accepted, status, info) = match decision {
            AuthDecision::Accepted(info) => (true, ACCEPTED.to_owned(), info),
            AuthDecision::Rejected { status, info } => (false, status, info),
            AuthDecision::Offline { accepted } => (
                accepted,
                if accepted { ACCEPTED } else { "Unknown" }.to_owned(),
                None,
            ),
        };
        let outcome = if !accepted {
            self.log(
                LogLevel::Warn,
                format!("token was not accepted on EVSE {evse}: {status}"),
            );
            Ok(Outcome2::Authorized {
                status,
                id_token_info: info,
            })
        } else if self.token_ends_authorization(evse, &token, group_in(info.as_ref())) {
            // The answer showed the token shares the group of the one that started.
            self.on_stopped_by_token(evse, token);
            Ok(Outcome2::Done)
        } else if self.authorized_token(evse).is_some() {
            // C01.FR.23: a second, unrelated token does not take over a running authorization.
            Ok(Outcome2::Authorized {
                status: AuthorizationStatus::ConcurrentTx.as_str().to_owned(),
                id_token_info: info,
            })
        } else {
            let had_transaction = self.active_tx(evse).is_some();
            self.on_authorized(evse, token.clone(), None, TriggerReason::Authorized)
                .map(|()| {
                    self.note_authorization_group(evse, &token, info.as_ref());
                    match self.active_tx(evse) {
                        Some(tx) if !had_transaction => Outcome2::TransactionStarted {
                            evse,
                            transaction_id: tx.id.0.clone(),
                        },
                        _ => Outcome2::Authorized {
                            status,
                            id_token_info: info,
                        },
                    }
                })
        };
        // The requester may have gone away; nothing to do about that.
        let _ = reply.send(outcome);
    }

    /// `Command2::Authorize`: a bare `Authorize` CALL; nothing else happens with the answer
    /// except that the cache learns it.
    pub(super) fn cmd_authorize(
        &mut self,
        id_token: String,
        token_type: IdTokenType,
        reply: Reply2,
    ) {
        let token = IdTokenRef {
            id_token,
            kind: token_type,
        };
        let request = AuthorizeRequest {
            id_token: wire_token(&token),
            certificate: None,
            iso15118_certificate_hash_data: None,
            custom_data: None,
        };
        self.call::<Authorize>(Origin2::Station, &request, move |actor, result| {
            let outcome = match result {
                Ok(response) => {
                    actor.cache_store(&token, &response.id_token_info);
                    Ok(Outcome2::Authorized {
                        status: response.id_token_info.status.as_str().to_owned(),
                        id_token_info: serde_json::to_value(&response.id_token_info).ok(),
                    })
                }
                Err(error) => Err(call_failure(error)),
            };
            // The requester may have gone away; nothing to do about that.
            let _ = reply.send(outcome);
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{Duration, TimeZone};
    use tokio::sync::{broadcast, mpsc};

    use super::*;
    use crate::clock::{Clock, ManualClock};
    use crate::config::StationConfig;
    use crate::ocpp::v2::types::IdToken as WireIdToken;

    fn actor(clock: &ManualClock) -> Station2Actor {
        let (_commands_tx, commands) = mpsc::channel(1);
        let (events, _) = broadcast::channel(16);
        let config = StationConfig::new("ws://127.0.0.1:1/").without_persistence();
        Station2Actor::new(config, Arc::new(clock.clone()), commands, events, None).0
    }

    fn token(id: &str) -> IdTokenRef {
        IdTokenRef {
            id_token: id.to_owned(),
            kind: IdTokenType::KeyCode,
        }
    }

    fn info(status: AuthorizationStatus, group: Option<&str>) -> IdTokenInfo {
        IdTokenInfo {
            status,
            cache_expiry_date_time: None,
            charging_priority: None,
            language1: None,
            evse_id: None,
            group_id_token: group.map(|g| WireIdToken {
                additional_info: None,
                id_token: g.to_owned(),
                kind: IdTokenType::KeyCode,
                custom_data: None,
            }),
            language2: None,
            personal_message: None,
            custom_data: None,
        }
    }

    fn clock() -> ManualClock {
        ManualClock::new(Utc.with_ymd_and_hms(2026, 3, 1, 12, 0, 0).single().unwrap())
    }

    #[tokio::test]
    async fn a_late_answer_for_another_transaction_does_not_cache_the_current_token() {
        let clock = clock();
        let mut actor = actor(&clock);
        let (reply, _answer) = tokio::sync::oneshot::channel();
        actor.cmd_plug_in(1, reply);
        actor
            .on_authorized(1, token("B"), None, TriggerReason::Authorized)
            .expect("authorized");
        assert!(actor.active_tx(1).is_some_and(|tx| tx.token.is_some()));
        let blocked = serde_json::json!({"status": "Blocked"});
        actor.on_id_token_info(1, "an-earlier-transaction", None, &blocked);
        assert!(actor.cache_lookup(&token_key(&token("B"))).is_none());
    }

    #[test]
    fn an_entry_expires_with_its_own_date() {
        let clock = clock();
        let mut actor = actor(&clock);
        let mut with_expiry = info(AuthorizationStatus::Accepted, None);
        with_expiry.cache_expiry_date_time = Some(clock.now_utc() + Duration::seconds(60));
        actor.cache_store(&token("A"), &with_expiry);
        assert!(actor.cache_lookup(&token_key(&token("A"))).is_some());
        clock.advance(Duration::seconds(61));
        assert!(actor.cache_lookup(&token_key(&token("A"))).is_none());
        assert_eq!(actor.auth_cache_persist().entries.len(), 0);
    }

    #[test]
    fn the_life_time_limits_an_entry_without_a_date() {
        let clock = clock();
        let mut actor = actor(&clock);
        // Default `LifeTime` is 86400 s.
        actor.cache_store(&token("A"), &info(AuthorizationStatus::Accepted, None));
        clock.advance(Duration::seconds(86_399));
        assert!(actor.cache_lookup(&token_key(&token("A"))).is_some());
        clock.advance(Duration::seconds(2));
        assert!(actor.cache_lookup(&token_key(&token("A"))).is_none());
    }

    #[test]
    fn an_expired_info_removes_the_entry_and_central_is_never_stored() {
        let clock = clock();
        let mut actor = actor(&clock);
        actor.cache_store(&token("A"), &info(AuthorizationStatus::Accepted, None));
        let mut expired = info(AuthorizationStatus::Accepted, None);
        expired.cache_expiry_date_time = Some(clock.now_utc() - Duration::seconds(1));
        actor.cache_store(&token("A"), &expired);
        assert_eq!(actor.auth_cache_persist().entries.len(), 0);
        let central = IdTokenRef {
            id_token: "C".to_owned(),
            kind: IdTokenType::Central,
        };
        actor.cache_store(&central, &info(AuthorizationStatus::Accepted, None));
        assert_eq!(actor.auth_cache_persist().entries.len(), 0);
    }

    #[test]
    fn a_full_cache_drops_rejected_entries_first_then_the_oldest() {
        let mut actor = actor(&clock());
        actor.cache_store(&token("blocked"), &info(AuthorizationStatus::Blocked, None));
        for n in 0..CACHE_LIMIT - 1 {
            actor.cache_store(
                &token(&format!("T{n}")),
                &info(AuthorizationStatus::Accepted, None),
            );
        }
        assert_eq!(actor.auth_cache_persist().entries.len(), CACHE_LIMIT);
        actor.cache_store(&token("new"), &info(AuthorizationStatus::Accepted, None));
        let keys: Vec<String> = actor
            .auth_cache_persist()
            .entries
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(keys.len(), CACHE_LIMIT);
        assert!(!keys.contains(&token_key(&token("blocked"))));
        assert!(keys.contains(&token_key(&token("T0"))));
        actor.cache_store(&token("newer"), &info(AuthorizationStatus::Accepted, None));
        let keys: Vec<String> = actor
            .auth_cache_persist()
            .entries
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(keys.len(), CACHE_LIMIT);
        assert!(!keys.contains(&token_key(&token("T0"))));
    }

    #[test]
    fn restore_drops_expired_entries_and_keeps_the_newest() {
        let clock = clock();
        let mut actor = actor(&clock);
        let now = clock.now_utc();
        let entry = |expires_at| CacheEntry {
            status: ACCEPTED.to_owned(),
            expires_at,
            group_id: None,
        };
        actor.auth_cache_restore(AuthCachePersist {
            entries: vec![
                ("old".to_owned(), entry(Some(now - Duration::seconds(1)))),
                ("keep".to_owned(), entry(Some(now + Duration::seconds(5)))),
                ("forever".to_owned(), entry(None)),
            ],
        });
        let keys: Vec<String> = actor
            .auth_cache_persist()
            .entries
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(keys, ["keep", "forever"]);
    }

    #[test]
    fn the_group_of_a_cached_token_is_remembered() {
        let mut actor = actor(&clock());
        actor.cache_store(
            &token("A"),
            &info(AuthorizationStatus::Accepted, Some("GROUP")),
        );
        let entry = actor.cache_peek(&token_key(&token("A"))).unwrap();
        assert_eq!(entry.group_id.as_deref(), Some("GROUP"));
    }
}
