//! Transactions: Authorize, StartTransaction, StopTransaction and the offline sync.
//!
//! TS reference: `startTransaction`/`stopTransaction`/`authorize` in `src/station.ts`,
//! `transaction-rules.ts`, `offline-transaction-sync.ts`. State machine: see
//! [`super::transaction`]; the offline queue: see [`super::offline`].
//!
//! * **Start** validates *before* any side effect (connector state, reservation owner incl.
//!   parentIdTag, `LocalPreAuthorize`, offline authorization). Online, the connector holds a
//!   `PendingStart` (guard against a second start) while the CALL is in flight; failure of any
//!   kind (CALLERROR, timeout, link lost) leaves no transaction behind. A CSMS answer that
//!   refuses the idTag creates no local state, and the transaction the CSMS opened is closed
//!   with `StopTransaction(DeAuthorized)`.
//! * **Offline start** (not connected or not registered) needs an offline authorization
//!   decision; the connector goes `Charging` at once on a typed `TxId::Local` and the start is queued.
//! * **Stop** has an in-flight guard (`Stopping`): a second stop is `InvalidState`. If the
//!   CALL fails or the link drops, the stop is queued (nothing is lost) and the connector is
//!   idle again. Stops while offline are queued at once.
//! * **Sync** after boot `Accepted` runs before meter timers and status re-sends. A queued
//!   message the CSMS fails to process (CALLERROR, timeout; each CALL already tried
//!   `TransactionMessageAttempts` times, `TransactionMessageRetryInterval` apart) is tried
//!   again while the link is up, after `TransactionMessageRetryInterval`, for at most
//!   `TransactionMessageAttempts` retry rounds. Then a queued `StopTransaction` is dropped with a
//!   warning (the Charge Point gives up after its configured attempts); a queued
//!   `StartTransaction` stays queued until the next connection. The queue holds at most
//!   [`MAX_QUEUED_TRANSACTIONS`] entries; beyond that the oldest one is dropped.

use super::actor::{StationActor, TimerKey};
use super::calls::{Origin, Reply};
use super::command::Outcome;
use super::commands::call_failure;
use super::event::LogLevel;
use super::metering::stop_transaction_data;
use super::offline::{MAX_QUEUED_TRANSACTIONS, OfflineTx, StopInfo};
use super::transaction::{
    ActiveTransaction, PendingStart, StoppingTransaction, TransactionState, state_after_stop,
};
use crate::error::{CallError, StationError};
use crate::ids::{ConnectorId, IdTag, TxId};
use crate::ocpp::messages::{
    Authorize, AuthorizeRequest, MeterValue, StartTransaction, StartTransactionRequest,
    StartTransactionResponse, StopTransaction, StopTransactionRequest,
};
use crate::ocpp::types::{
    AuthorizationStatus, AvailabilityType, ConnectorStatus, RegistrationStatus, StopReason,
};
use crate::station::authorization::{
    apply_online_id_tag_info, authorize_offline, known_parent, pre_authorize,
};
use crate::station::config_registry::{STOP_TXN_ALIGNED_DATA, STOP_TXN_SAMPLED_DATA};
use crate::station::reservation::{clear_reservation, reserved_for_other};

/// Shortest pause before a queued message is tried again (a zero
/// `TransactionMessageRetryInterval` must not spin).
const MIN_SYNC_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

/// Completion of an operation that may span a CSMS round trip.
pub(super) type Done = Box<dyn FnOnce(&mut StationActor, Result<Outcome, StationError>) + Send>;

/// A `Done` that answers a command's reply channel.
pub(super) fn reply_to(reply: Reply) -> Done {
    Box::new(move |_, result| {
        let _ = reply.send(result);
    })
}

/// A `Done` that ignores the result.
pub(super) fn ignore() -> Done {
    Box::new(|_, _| {})
}

fn invalid(connector: ConnectorId, reason: impl Into<String>) -> StationError {
    StationError::InvalidState {
        connector: Some(connector),
        reason: format!("connector {connector}: {}", reason.into()),
    }
}

impl StationActor {
    /// Connected and `BootNotification` accepted: the CSMS can be talked to.
    pub(super) fn can_talk(&self) -> bool {
        self.link.conn.is_some() && self.state.registration == Some(RegistrationStatus::Accepted)
    }

    fn next_key(&mut self) -> u64 {
        self.next_local += 1;
        self.next_local
    }

    // ---- Authorize ---------------------------------------------------------------

    pub(super) fn authorize(&mut self, id_tag: IdTag, done: Done) {
        if !self.can_talk() {
            let now = self.clock.now_utc();
            let decision = authorize_offline(
                &id_tag,
                now,
                &self.local_list,
                &self.auth_cache,
                &self.registry,
            );
            self.log(
                if decision.accepted {
                    LogLevel::Success
                } else {
                    LogLevel::Warn
                },
                format!(
                    "Offline authorization status: {} ({id_tag}, {:?})",
                    decision.id_tag_info.status, decision.source
                ),
            );
            done(self, Ok(Outcome::Authorized(decision)));
            return;
        }
        let request = AuthorizeRequest {
            id_tag: id_tag.clone(),
        };
        self.call::<Authorize>(
            Origin::Station,
            &request,
            move |actor, result| match result {
                Ok(response) => {
                    let now = actor.clock.now_utc();
                    let decision = apply_online_id_tag_info(
                        &id_tag,
                        &response.id_tag_info,
                        now,
                        &mut actor.auth_cache,
                        &actor.registry,
                    );
                    actor.mark_dirty();
                    actor.log(
                        if decision.accepted {
                            LogLevel::Success
                        } else {
                            LogLevel::Warn
                        },
                        format!(
                            "Authorization status: {} ({id_tag})",
                            decision.id_tag_info.status
                        ),
                    );
                    done(actor, Ok(Outcome::Authorized(decision)));
                }
                Err(error) => done(actor, Err(call_failure(error))),
            },
        );
    }

    /// The Central System refused the idTag of a started transaction (non-`Accepted`
    /// `StartTransaction.conf`) while `StopTransactionOnInvalidId` is `false`: the transaction
    /// goes on, limited to `MaxEnergyOnInvalidId` Wh when that is set (`0` = no limit).
    fn limit_energy_on_invalid_id(
        &mut self,
        id: ConnectorId,
        id_tag: &IdTag,
        status: AuthorizationStatus,
    ) {
        self.flush_meters();
        let max = u64::from(self.registry.max_energy_on_invalid_id());
        let Some(meter) = self.find_connector(id).map(|c| c.meter_wh) else {
            return;
        };
        if max == 0 {
            self.log(
                LogLevel::Warn,
                format!("idTag {id_tag} was refused ({status}); the transaction on connector {id} continues (StopTransactionOnInvalidId=false, no MaxEnergyOnInvalidId)"),
            );
            return;
        }
        self.invalid_limits.insert(id, meter.saturating_add(max));
        self.log(
            LogLevel::Warn,
            format!("idTag {id_tag} was refused ({status}); connector {id} will stop after {max}Wh (MaxEnergyOnInvalidId)"),
        );
    }

    /// After the meter advanced: stop a transaction that used up its `MaxEnergyOnInvalidId`.
    pub(super) fn check_energy_limit(&mut self, id: ConnectorId) {
        let Some(limit) = self.invalid_limits.get(&id).copied() else {
            return;
        };
        let Some(connector) = self.find_connector(id) else {
            return;
        };
        if connector.meter_wh >= limit && !connector.transaction.is_idle() {
            let tag = connector.last_id_tag.clone();
            self.invalid_limits.remove(&id);
            self.stop_transaction(id, StopReason::DeAuthorized, tag, ignore());
        }
    }

    // ---- start ---------------------------------------------------------------------

    #[allow(
        clippy::too_many_lines,
        reason = "one linear validate-then-act sequence"
    )]
    pub(super) fn start_transaction(&mut self, id: ConnectorId, id_tag: IdTag, done: Done) {
        self.flush_meters();
        let now = self.clock.now_utc();
        let connector = match self.connector(id) {
            Ok(connector) => connector.clone(),
            Err(error) => return done(self, Err(error)),
        };
        let connector = &connector;
        // Validate everything before touching state.
        let parent = known_parent(&id_tag, now, &self.local_list, &self.auth_cache);
        let rejection = if connector.transaction.blocks_new_transaction() {
            Some("already has a transaction")
        } else if connector.availability == AvailabilityType::Inoperative
            || connector.status == ConnectorStatus::Unavailable
        {
            Some("is unavailable")
        } else if connector.status == ConnectorStatus::Faulted {
            Some("is faulted")
        } else if connector.status == ConnectorStatus::Finishing {
            Some("is finishing; unplug before starting a new transaction")
        } else if reserved_for_other(connector, &id_tag, parent.as_ref(), now) {
            Some("is reserved for another idTag")
        } else {
            None
        };
        if let Some(reason) = rejection {
            return done(self, Err(invalid(id, reason)));
        }
        if let Some(decision) = pre_authorize(
            &id_tag,
            now,
            &self.local_list,
            &self.auth_cache,
            &self.registry,
        ) && !decision.accepted
        {
            self.log(
                LogLevel::Warn,
                format!(
                    "StartTransaction blocked by local authorization: {} ({id_tag})",
                    decision.id_tag_info.status
                ),
            );
            return done(
                self,
                Ok(Outcome::TransactionRejected {
                    status: decision.id_tag_info.status,
                }),
            );
        }
        let online = self.can_talk();
        if !online {
            let decision = authorize_offline(
                &id_tag,
                now,
                &self.local_list,
                &self.auth_cache,
                &self.registry,
            );
            if !decision.accepted {
                self.log(
                    LogLevel::Warn,
                    format!(
                        "Offline StartTransaction blocked: {} ({id_tag}, {:?})",
                        decision.id_tag_info.status, decision.source
                    ),
                );
                return done(
                    self,
                    Ok(Outcome::TransactionRejected {
                        status: decision.id_tag_info.status,
                    }),
                );
            }
        }

        let key = self.next_key();
        let local = TxId::Local(key);
        let reservation_id = connector.reservation.as_ref().map(|r| r.id);
        let meter_start_wh = connector.meter_wh;
        let pending = PendingStart {
            tx_id: local,
            id_tag: id_tag.clone(),
            started_at: now,
            meter_start_wh,
            reservation_id,
        };
        // Side effects: auto-plug, Preparing, guard state.
        if let Some(connector) = self.find_connector_mut(id) {
            connector.ev_connected = true;
            if connector.status == ConnectorStatus::Available {
                connector.status = ConnectorStatus::Preparing;
            }
            connector.transaction = TransactionState::PendingStart(pending.clone());
        }
        self.invalid_limits.remove(&id);

        if !online {
            self.begin_charging(id, &id_tag);
            self.push_queue(OfflineTx {
                key,
                connector: id,
                tx_id: local,
                id_tag,
                started_at: now,
                meter_start_wh,
                reservation_id,
                start_pending: true,
                stop: None,
            });
            self.flush_now();
            self.log(
                LogLevel::Warn,
                format!("Started local offline transaction {local} on connector {id}; will sync after reconnect"),
            );
            return done(
                self,
                Ok(Outcome::TransactionStarted {
                    connector: id,
                    tx_id: local,
                }),
            );
        }

        self.status_changed(id);
        let request = StartTransactionRequest {
            connector_id: id,
            id_tag: id_tag.clone(),
            meter_start: i64::try_from(meter_start_wh).unwrap_or(i64::MAX),
            reservation_id,
            timestamp: now,
        };
        self.call::<StartTransaction>(Origin::Station, &request, move |actor, result| {
            actor.on_start_result(id, local, pending, result, done);
        });
    }

    fn on_start_result(
        &mut self,
        id: ConnectorId,
        local: TxId,
        pending: PendingStart,
        result: Result<StartTransactionResponse, CallError>,
        done: Done,
    ) {
        let still_ours = self
            .find_connector(id)
            .is_some_and(|c| c.transaction.tx_id() == Some(local));
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                if still_ours {
                    self.abandon_start(id);
                }
                return done(self, Err(call_failure(error)));
            }
        };
        let now = self.clock.now_utc();
        let decision = apply_online_id_tag_info(
            &pending.id_tag,
            &response.id_tag_info,
            now,
            &mut self.auth_cache,
            &self.registry,
        );
        self.mark_dirty();
        // `StopTransactionOnInvalidId=false`: a refused idTag does not end the transaction
        // (OCPP 1.6 configuration keys); `MaxEnergyOnInvalidId` caps the energy instead.
        let lenient =
            !decision.accepted && still_ours && !self.registry.stop_transaction_on_invalid_id();
        if (!decision.accepted && !lenient) || !still_ours {
            let status = if decision.accepted {
                AuthorizationStatus::Invalid
            } else {
                decision.id_tag_info.status
            };
            self.log(
                LogLevel::Warn,
                format!(
                    "StartTransaction rejected by Central System: {status} ({})",
                    pending.id_tag
                ),
            );
            if still_ours {
                self.abandon_start(id);
            }
            // The CSMS opened transaction N for a tag it refused: close it.
            self.send_stop_for(
                response.transaction_id,
                pending.meter_start_wh,
                Some(pending.id_tag),
                StopReason::DeAuthorized,
            );
            return done(self, Ok(Outcome::TransactionRejected { status }));
        }
        // A fault or an unavailable status that landed while the start was in flight must
        // not be overwritten with `Charging`: the transaction the CSMS opened is closed.
        let blocked = self.find_connector(id).is_some_and(|c| {
            matches!(
                c.status,
                ConnectorStatus::Faulted | ConnectorStatus::Unavailable
            )
        });
        if blocked {
            self.log(
                LogLevel::Warn,
                format!("connector {id} became unavailable while the start was in flight; closing transaction {}", response.transaction_id),
            );
            self.abandon_start(id);
            self.send_stop_for(
                response.transaction_id,
                pending.meter_start_wh,
                Some(pending.id_tag),
                StopReason::Other,
            );
            return done(
                self,
                Err(invalid(
                    id,
                    "became unavailable while the start was in flight",
                )),
            );
        }
        let tx_id = TxId::Csms(response.transaction_id);
        if let Some(connector) = self.find_connector_mut(id) {
            connector.transaction = TransactionState::Active(ActiveTransaction {
                tx_id,
                id_tag: pending.id_tag.clone(),
                started_at: pending.started_at,
                meter_start_wh: pending.meter_start_wh,
            });
        }
        self.begin_charging(id, &pending.id_tag);
        if lenient {
            self.limit_energy_on_invalid_id(id, &pending.id_tag, decision.id_tag_info.status);
        }
        done(
            self,
            Ok(Outcome::TransactionStarted {
                connector: id,
                tx_id,
            }),
        );
    }

    /// Connector goes `Charging`; reservation consumed; meters armed.
    fn begin_charging(&mut self, id: ConnectorId, id_tag: &IdTag) {
        if let Some(connector) = self.find_connector_mut(id) {
            clear_reservation(connector);
            connector.status = ConnectorStatus::Charging;
            connector.last_id_tag = Some(id_tag.clone());
        }
        self.timers.cancel(&TimerKey::ReservationExpiry(id));
        self.flush_now();
        self.reschedule_metering();
        self.status_changed(id);
    }

    /// A start that did not happen: no transaction, EV still connected.
    fn abandon_start(&mut self, id: ConnectorId) {
        if let Some(connector) = self.find_connector_mut(id) {
            connector.transaction = TransactionState::Idle;
            if connector.status == ConnectorStatus::Charging {
                connector.status = if connector.ev_connected {
                    ConnectorStatus::Preparing
                } else {
                    ConnectorStatus::Available
                };
            }
        }
        // A ChangeAvailability that waited for this (never started) transaction.
        self.apply_pending_availability(id);
        self.status_changed(id);
    }

    /// Fire-and-forget `StopTransaction` for a transaction the CSMS opened but we do not keep.
    fn send_stop_for(
        &mut self,
        tx: i32,
        meter_stop_wh: u64,
        id_tag: Option<IdTag>,
        reason: StopReason,
    ) {
        let request = StopTransactionRequest {
            id_tag,
            meter_stop: i64::try_from(meter_stop_wh).unwrap_or(i64::MAX),
            timestamp: self.clock.now_utc(),
            transaction_id: tx,
            reason: Some(reason),
            transaction_data: None,
        };
        self.call::<StopTransaction>(Origin::Station, &request, |_, _| {});
    }

    // ---- stop ----------------------------------------------------------------------

    #[allow(
        clippy::too_many_lines,
        reason = "one linear validate-then-act sequence"
    )]
    pub(super) fn stop_transaction(
        &mut self,
        id: ConnectorId,
        reason: StopReason,
        id_tag: Option<IdTag>,
        done: Done,
    ) {
        self.flush_meters();
        let connector = match self.connector(id) {
            Ok(connector) => connector.clone(),
            Err(error) => return done(self, Err(error)),
        };
        let connector = &connector;
        let (tx, started_at, meter_start_wh, tag) = match &connector.transaction {
            TransactionState::Idle => return done(self, Err(invalid(id, "has no transaction"))),
            TransactionState::Stopping(_) => {
                return done(self, Err(invalid(id, "transaction is already stopping")));
            }
            TransactionState::PendingStart(p) => {
                if !self.queue.entries().iter().any(|e| e.tx_id == p.tx_id) {
                    return done(
                        self,
                        Err(invalid(id, "transaction start is still in progress")),
                    );
                }
                (p.tx_id, p.started_at, p.meter_start_wh, p.id_tag.clone())
            }
            TransactionState::Active(a) => {
                (a.tx_id, a.started_at, a.meter_start_wh, a.id_tag.clone())
            }
        };
        let now = self.clock.now_utc();
        let (ev_connected, status) =
            state_after_stop(reason, connector.ev_connected, connector.availability);
        let meter_wh = connector.meter_wh;
        let sampled = self.registry.measurands(STOP_TXN_SAMPLED_DATA);
        let aligned = self.registry.measurands(STOP_TXN_ALIGNED_DATA);
        let reading = self.meter_reading(id, false);
        let data: Vec<MeterValue> =
            stop_transaction_data(&self.power_model(), &reading, now, &sampled, &aligned);
        let stop_tag = id_tag.or_else(|| connector.last_id_tag.clone());
        let info = StopInfo {
            id_tag: stop_tag.clone(),
            meter_stop_wh: meter_wh,
            timestamp: now,
            reason,
            transaction_data: data,
        };

        if let Some(connector) = self.find_connector_mut(id) {
            connector.ev_connected = ev_connected;
            connector.status = status;
        }
        self.timers.cancel(&TimerKey::MeterPeriodic(id));
        self.invalid_limits.remove(&id);

        let queue_it = tx.is_local() || !self.can_talk();
        if queue_it {
            self.queue_stop(id, tx, tag, started_at, meter_start_wh, info);
            self.finish_stop(id, tx);
            self.log(
                LogLevel::Warn,
                format!("Queued StopTransaction for connector {id}; will sync after reconnect"),
            );
            if self.can_talk() {
                self.start_offline_sync(false);
            }
            return done(self, Ok(Outcome::Done));
        }

        if let Some(connector) = self.find_connector_mut(id) {
            connector.transaction = TransactionState::Stopping(StoppingTransaction {
                tx_id: tx,
                id_tag: stop_tag,
                reason,
                stopped_at: Some(now),
                meter_stop_wh: Some(meter_wh),
            });
        }
        self.status_changed(id);
        let Some(wire) = tx.wire() else {
            return done(self, Ok(Outcome::Done));
        };
        let request = stop_request(&info, wire);
        self.call::<StopTransaction>(
            Origin::Station,
            &request,
            move |actor, result| match result {
                Ok(_) => {
                    actor.finish_stop(id, tx);
                    done(actor, Ok(Outcome::Done));
                }
                Err(error) => {
                    let key = actor.queue_stop(id, tx, tag, started_at, meter_start_wh, info);
                    actor.finish_stop(id, tx);
                    actor.log(
                        LogLevel::Warn,
                        format!("StopTransaction failed ({error}); queued, will be retried"),
                    );
                    actor.record_sync_failure(key, &error);
                    actor.arm_sync_retry_if_idle();
                    done(actor, Err(call_failure(error)));
                }
            },
        );
    }

    /// Adds an entry to the offline queue; when the queue is over its bound the oldest
    /// entry is dropped (non-start entries first) with a warning.
    fn push_queue(&mut self, entry: OfflineTx) {
        if let Some(dropped) = self.queue.push(entry) {
            self.sync_failures.remove(&dropped.key);
            self.log(
                LogLevel::Warn,
                format!(
                    "offline queue is full ({MAX_QUEUED_TRANSACTIONS} entries): dropped the oldest entry (transaction {}, connector {})",
                    dropped.tx_id, dropped.connector
                ),
            );
        }
    }

    /// Queues the stop of `tx`; returns the key of its queue entry.
    fn queue_stop(
        &mut self,
        id: ConnectorId,
        tx: TxId,
        id_tag: IdTag,
        started_at: chrono::DateTime<chrono::Utc>,
        meter_start_wh: u64,
        info: StopInfo,
    ) -> u64 {
        if let Some(entry) = self.queue.find_by_tx_on(id, tx) {
            entry.stop = Some(info);
            return entry.key;
        }
        let key = self.next_key();
        self.push_queue(OfflineTx {
            key,
            connector: id,
            tx_id: tx,
            id_tag,
            started_at,
            meter_start_wh,
            reservation_id: None,
            start_pending: false,
            stop: Some(info),
        });
        key
    }

    /// The transaction on `id` is over (delivered or queued): connector idle, profiles cleaned,
    /// pending availability applied.
    fn finish_stop(&mut self, id: ConnectorId, tx: TxId) {
        if let Some(connector) = self.find_connector_mut(id)
            && connector.transaction.tx_id() == Some(tx)
        {
            connector.transaction = TransactionState::Idle;
        }
        if self.profiles.on_transaction_ended(tx) > 0 {
            self.mark_dirty();
        }
        self.apply_pending_availability(id);
        self.flush_now();
        self.status_changed(id);
    }

    // ---- offline sync ----------------------------------------------------------------

    /// Delivers the queued transactions. Returns the number of entries being synced.
    /// `boot` marks the sync that follows an accepted `BootNotification`: only it ends in
    /// `finish_boot_accepted` (meter timers, status re-send); a sync triggered later (a stop
    /// queued while online, a retry round) just delivers the entries.
    ///
    /// Entries are delivered one at a time so the CSMS sees start/stop in chronological order.
    /// An entry that failed `TransactionMessageAttempts` rounds in this connection is left
    /// alone until the next connection (see [`Self::arm_sync_retry`]).
    pub(super) fn start_offline_sync(&mut self, boot: bool) -> usize {
        if self.sync_outstanding > 0 {
            return self.sync_outstanding;
        }
        self.timers.cancel(&TimerKey::SyncRetry);
        let rounds = self.sync_round_limit();
        let keys: Vec<u64> = self
            .queue
            .keys()
            .into_iter()
            .filter(|key| self.sync_failures.get(key).copied().unwrap_or(0) <= rounds)
            .collect();
        let count = keys.len();
        if count == 0 {
            return 0;
        }
        self.sync_outstanding = count;
        self.sync_boot = boot;
        self.sync_keys = keys.into();
        self.run_sync();
        count
    }

    /// Starts the next entry that needs a CALL. Iterative: entries that finish without a
    /// CALL (gone from the queue, nothing to send) never nest.
    fn run_sync(&mut self) {
        while let Some(key) = self.sync_keys.pop_front() {
            if self.sync_entry(key) {
                return;
            }
            self.sync_outstanding = self.sync_outstanding.saturating_sub(1);
        }
        self.sync_finished();
    }

    /// The CALL of one entry has completed.
    fn sync_done(&mut self) {
        self.sync_outstanding = self.sync_outstanding.saturating_sub(1);
        self.run_sync();
    }

    fn sync_finished(&mut self) {
        self.sync_outstanding = 0;
        if std::mem::take(&mut self.sync_boot) {
            self.finish_boot_accepted();
        }
        self.arm_sync_retry_if_idle();
    }

    /// Retry rounds an entry gets after its first failed round (`TransactionMessageAttempts`,
    /// at least one). Each round is itself a CALL with up to `TransactionMessageAttempts`
    /// attempts, `TransactionMessageRetryInterval` apart.
    fn sync_round_limit(&self) -> u32 {
        self.registry.transaction_message_attempts().max(1)
    }

    /// Remembers that the CSMS did not process the queued message `key`. A lost link is not
    /// the CSMS's fault: the sync starts over after the reconnect.
    fn record_sync_failure(&mut self, key: u64, error: &CallError) {
        if matches!(
            error,
            CallError::NotConnected | CallError::Closed { .. } | CallError::NotRegistered { .. }
        ) {
            return;
        }
        *self.sync_failures.entry(key).or_insert(0) += 1;
        self.sync_failed = true;
    }

    fn arm_sync_retry_if_idle(&mut self) {
        if self.sync_outstanding == 0 && std::mem::take(&mut self.sync_failed) {
            self.arm_sync_retry();
        }
    }

    /// While the link is up, queued messages the CSMS failed to process are tried again after
    /// `TransactionMessageRetryInterval`, for at most `TransactionMessageAttempts` retry rounds
    /// per entry. Then a queued `StopTransaction` is dropped (OCPP 1.6: the Charge Point gives up
    /// after the configured attempts; this also bounds what a hostile CSMS can leave queued);
    /// a queued `StartTransaction` stays for the next connection, its transaction is running.
    fn arm_sync_retry(&mut self) {
        if !self.can_talk() {
            return;
        }
        let rounds = self.sync_round_limit();
        let exhausted: Vec<u64> = self
            .queue
            .entries()
            .iter()
            .filter(|e| {
                !e.start_pending && self.sync_failures.get(&e.key).copied().unwrap_or(0) > rounds
            })
            .map(|e| e.key)
            .collect();
        for key in exhausted {
            if let Some(entry) = self.queue.remove(key) {
                self.log(
                    LogLevel::Warn,
                    format!(
                        "giving up on the queued StopTransaction of {} (connector {}) after {rounds} retry round(s)",
                        entry.tx_id, entry.connector
                    ),
                );
            }
            self.sync_failures.remove(&key);
            self.flush_now();
        }
        let retryable = self.queue.entries().iter().any(|e| {
            let failures = self.sync_failures.get(&e.key).copied().unwrap_or(0);
            failures > 0 && failures <= rounds
        });
        if retryable {
            let interval = self
                .registry
                .transaction_message_retry_interval()
                .max(MIN_SYNC_RETRY);
            self.timers.set(TimerKey::SyncRetry, interval);
        }
    }

    pub(super) fn on_sync_retry_timer(&mut self) {
        if self.can_talk() {
            self.start_offline_sync(false);
        }
    }

    /// Starts the CALL for one entry. `false`: nothing to send, the entry is done.
    fn sync_entry(&mut self, key: u64) -> bool {
        let Some(entry) = self.queue.get(key).cloned() else {
            return false;
        };
        if entry.start_pending {
            let request = StartTransactionRequest {
                connector_id: entry.connector,
                id_tag: entry.id_tag.clone(),
                meter_start: i64::try_from(entry.meter_start_wh).unwrap_or(i64::MAX),
                reservation_id: entry.reservation_id,
                timestamp: entry.started_at,
            };
            self.call::<StartTransaction>(Origin::Station, &request, move |actor, result| {
                actor.on_sync_start(key, result);
            });
            true
        } else {
            self.sync_stop(key)
        }
    }

    fn on_sync_start(&mut self, key: u64, result: Result<StartTransactionResponse, CallError>) {
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                self.log(
                    LogLevel::Warn,
                    format!("sync of queued StartTransaction failed: {error}"),
                );
                self.record_sync_failure(key, &error);
                return self.sync_done();
            }
        };
        let Some(entry) = self.queue.get(key).cloned() else {
            return self.sync_done();
        };
        let now = self.clock.now_utc();
        let decision = apply_online_id_tag_info(
            &entry.id_tag,
            &response.id_tag_info,
            now,
            &mut self.auth_cache,
            &self.registry,
        );
        self.mark_dirty();
        let local = entry.tx_id;
        self.sync_failures.remove(&key);
        // `StopTransactionOnInvalidId=false`: the refused transaction goes on (see `on_start_result`).
        let lenient = !decision.accepted && !self.registry.stop_transaction_on_invalid_id();
        if !decision.accepted && !lenient {
            self.log(
                LogLevel::Warn,
                format!(
                    "queued transaction {local} rejected by the Central System: {}",
                    decision.id_tag_info.status
                ),
            );
            self.queue.remove(key);
            self.drop_local_transaction(entry.connector, local);
            let meter = entry
                .stop
                .as_ref()
                .map_or(entry.meter_start_wh, |s| s.meter_stop_wh);
            self.send_stop_for(
                response.transaction_id,
                meter,
                Some(entry.id_tag),
                StopReason::DeAuthorized,
            );
            self.flush_now();
            return self.sync_done();
        }
        let real = TxId::Csms(response.transaction_id);
        if let Some(queued) = self.queue.get_mut(key) {
            queued.tx_id = real;
            queued.start_pending = false;
        }
        if let Some(connector) = self.find_connector_mut(entry.connector)
            && connector.transaction.tx_id() == Some(local)
        {
            connector.transaction = TransactionState::Active(ActiveTransaction {
                tx_id: real,
                id_tag: entry.id_tag.clone(),
                started_at: entry.started_at,
                meter_start_wh: entry.meter_start_wh,
            });
        }
        // The local id is gone: TxProfiles bound to it are no longer valid.
        if self.profiles.on_transaction_ended(local) > 0 {
            self.mark_dirty();
        }
        self.flush_now();
        if lenient && entry.stop.is_none() {
            self.limit_energy_on_invalid_id(
                entry.connector,
                &entry.id_tag,
                decision.id_tag_info.status,
            );
        }
        if entry.stop.is_some() && self.sync_stop(key) {
            return;
        }
        self.sync_done();
    }

    /// Starts the `StopTransaction` CALL of an entry; `false` if there is nothing to send.
    fn sync_stop(&mut self, key: u64) -> bool {
        let Some(entry) = self.queue.get(key).cloned() else {
            return false;
        };
        let (Some(info), Some(wire)) = (entry.stop, entry.tx_id.wire()) else {
            return false;
        };
        let request = stop_request(&info, wire);
        self.call::<StopTransaction>(Origin::Station, &request, move |actor, result| {
            match result {
                Ok(_) => {
                    actor.queue.remove(key);
                    actor.sync_failures.remove(&key);
                    actor.flush_now();
                }
                Err(error) => {
                    actor.log(
                        LogLevel::Warn,
                        format!("sync of queued StopTransaction failed: {error}"),
                    );
                    actor.record_sync_failure(key, &error);
                }
            }
            actor.sync_done();
        });
        true
    }

    /// A local transaction the CSMS refused: the connector forgets it.
    fn drop_local_transaction(&mut self, id: ConnectorId, local: TxId) {
        if let Some(connector) = self.find_connector_mut(id)
            && connector.transaction.tx_id() == Some(local)
        {
            connector.transaction = TransactionState::Idle;
            connector.status = if connector.availability == AvailabilityType::Inoperative {
                ConnectorStatus::Unavailable
            } else if connector.ev_connected {
                ConnectorStatus::Preparing
            } else {
                ConnectorStatus::Available
            };
        }
        self.timers.cancel(&TimerKey::MeterPeriodic(id));
        self.profiles.on_transaction_ended(local);
        self.apply_pending_availability(id);
        self.status_changed(id);
    }

    /// Meter timers and status re-send, once the offline sync is complete.
    pub(super) fn finish_boot_accepted(&mut self) {
        if self.state.registration != Some(RegistrationStatus::Accepted) {
            return;
        }
        self.reschedule_metering();
        self.resend_all_statuses();
        if std::mem::take(&mut self.startup_event_pending) {
            self.security_event("StartupOfTheDevice", None);
        } else {
            self.flush_security_events();
        }
    }
}

fn stop_request(info: &StopInfo, wire: i32) -> StopTransactionRequest {
    StopTransactionRequest {
        id_tag: info.id_tag.clone(),
        meter_stop: i64::try_from(info.meter_stop_wh).unwrap_or(i64::MAX),
        timestamp: info.timestamp,
        transaction_id: wire,
        reason: Some(info.reason),
        transaction_data: (!info.transaction_data.is_empty())
            .then(|| info.transaction_data.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    #[test]
    fn stop_request_omits_empty_transaction_data() {
        let info = StopInfo {
            id_tag: None,
            meter_stop_wh: 12,
            timestamp: DateTime::UNIX_EPOCH,
            reason: StopReason::Local,
            transaction_data: Vec::new(),
        };
        let request = stop_request(&info, 7);
        assert_eq!(request.transaction_id, 7);
        assert_eq!(request.meter_stop, 12);
        assert!(request.transaction_data.is_none());
    }
}
