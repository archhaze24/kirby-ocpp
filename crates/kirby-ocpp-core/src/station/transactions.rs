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
//! * **Sync** after boot `Accepted` runs before meter timers and status re-sends.

use super::actor::{StationActor, TimerKey};
use super::calls::{Origin, Reply};
use super::command::Outcome;
use super::commands::call_failure;
use super::event::LogLevel;
use super::metering::{SimulatedMeter, stop_transaction_data};
use super::offline::{OfflineTx, StopInfo};
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
                    if !decision.accepted {
                        actor.handle_invalid_id(&id_tag);
                    }
                    done(actor, Ok(Outcome::Authorized(decision)));
                }
                Err(error) => done(actor, Err(call_failure(error))),
            },
        );
    }

    /// `StopTransactionOnInvalidId`: stop (or cap the energy of) running transactions of `id_tag`.
    fn handle_invalid_id(&mut self, id_tag: &IdTag) {
        if !self.registry.stop_transaction_on_invalid_id() {
            return;
        }
        let max = u64::from(self.registry.max_energy_on_invalid_id());
        let affected: Vec<(ConnectorId, u64)> = self
            .state
            .connectors
            .iter()
            .filter(|c| !c.transaction.is_idle() && c.last_id_tag.as_ref() == Some(id_tag))
            .map(|c| (c.id, c.meter_wh))
            .collect();
        for (id, meter) in affected {
            if max == 0 {
                self.stop_transaction(id, StopReason::DeAuthorized, Some(id_tag.clone()), ignore());
            } else {
                self.invalid_limits.insert(id, meter.saturating_add(max));
                self.log(
                    LogLevel::Warn,
                    format!("Connector {id} will stop after {max}Wh because idTag {id_tag} is no longer authorized"),
                );
            }
        }
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
            self.queue.push(OfflineTx {
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
        if !decision.accepted || !still_ours {
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
        #[allow(
            clippy::cast_precision_loss,
            reason = "Wh counters stay far below 2^53"
        )]
        let mut meter = SimulatedMeter::with_energy_wh(meter_wh as f64);
        let data: Vec<MeterValue> =
            stop_transaction_data(&mut meter, now, connector.status, &sampled, &aligned);
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
                self.start_offline_sync();
            }
            return done(self, Ok(Outcome::Done));
        }

        if let Some(connector) = self.find_connector_mut(id) {
            connector.transaction = TransactionState::Stopping(StoppingTransaction {
                tx_id: tx,
                id_tag: stop_tag,
                reason,
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
                    actor.queue_stop(id, tx, tag, started_at, meter_start_wh, info);
                    actor.finish_stop(id, tx);
                    actor.log(
                        LogLevel::Warn,
                        format!("StopTransaction failed ({error}); queued for the next sync"),
                    );
                    done(actor, Err(call_failure(error)));
                }
            },
        );
    }

    fn queue_stop(
        &mut self,
        id: ConnectorId,
        tx: TxId,
        id_tag: IdTag,
        started_at: chrono::DateTime<chrono::Utc>,
        meter_start_wh: u64,
        info: StopInfo,
    ) {
        if let Some(entry) = self.queue.find_by_tx(tx) {
            entry.stop = Some(info);
            return;
        }
        let key = self.next_key();
        self.queue.push(OfflineTx {
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
    pub(super) fn start_offline_sync(&mut self) -> usize {
        if self.sync_outstanding > 0 {
            return self.sync_outstanding;
        }
        let keys = self.queue.keys();
        let count = keys.len();
        self.sync_outstanding = count;
        self.sync_keys = keys.into();
        // Entries are delivered one at a time so the CSMS sees start/stop in chronological order.
        if let Some(first) = self.sync_keys.pop_front() {
            self.sync_entry(first);
        }
        count
    }

    fn sync_done(&mut self) {
        self.sync_outstanding = self.sync_outstanding.saturating_sub(1);
        if self.sync_outstanding == 0 {
            self.sync_keys.clear();
            self.finish_boot_accepted();
        } else if let Some(next) = self.sync_keys.pop_front() {
            self.sync_entry(next);
        }
    }

    fn sync_entry(&mut self, key: u64) {
        let Some(entry) = self.queue.get(key).cloned() else {
            return self.sync_done();
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
        } else {
            self.sync_stop(key);
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
        if !decision.accepted {
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
        if entry.stop.is_some() {
            self.sync_stop(key);
        } else {
            self.sync_done();
        }
    }

    fn sync_stop(&mut self, key: u64) {
        let Some(entry) = self.queue.get(key).cloned() else {
            return self.sync_done();
        };
        let (Some(info), Some(wire)) = (entry.stop, entry.tx_id.wire()) else {
            return self.sync_done();
        };
        let request = stop_request(&info, wire);
        self.call::<StopTransaction>(Origin::Station, &request, move |actor, result| {
            match result {
                Ok(_) => {
                    actor.queue.remove(key);
                    actor.flush_now();
                }
                Err(error) => {
                    actor.log(
                        LogLevel::Warn,
                        format!("sync of queued StopTransaction failed: {error}"),
                    );
                }
            }
            actor.sync_done();
        });
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
    }

    /// Meter timers and status re-send, once the offline sync is complete.
    pub(super) fn finish_boot_accepted(&mut self) {
        if self.state.registration != Some(RegistrationStatus::Accepted) {
            return;
        }
        self.reschedule_metering();
        self.resend_all_statuses();
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
