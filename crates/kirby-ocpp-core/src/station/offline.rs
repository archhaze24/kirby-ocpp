//! Offline transaction queue and post-reconnect synchronization.
//!
//! TS reference: `src/station/offline-transaction-sync.ts`, `transaction-message-retry.ts`.
//!
//! One [`OfflineTx`] per transaction that the CSMS has not fully seen:
//!
//! * `start_pending == true`: the transaction was started offline under a station-local
//!   [`TxId::Local`] id; its `StartTransaction` still has to be delivered.
//! * `stop == Some(..)`: its `StopTransaction` has to be delivered (the stop happened offline,
//!   or the online CALL failed or the link dropped mid-stop: nothing is lost).
//!
//! A queued stop returns the connector to `Idle` at once, so a new transaction can start while
//! the old one waits for its sync (the TS "one offline transaction per connector" limit is gone).
//!
//! After `BootNotification` is `Accepted` the actor syncs the entries in order, **before** the
//! meter timers are re-armed and the statuses re-sent: start (real id assigned, TxProfiles of the
//! local id cleared), then stop with the real id. Each entry is an independent chain, so one
//! failure never aborts the others. An entry the CSMS fails to process is tried again while the
//! link is up (`TransactionMessageRetryInterval`, at most `TransactionMessageAttempts` retry rounds);
//! then a stop-only entry is dropped and a start stays queued for the next boot. The queue is
//! bounded ([`MAX_QUEUED_TRANSACTIONS`]). A start whose idTag the CSMS rejects drops the
//! transaction and sends a `StopTransaction` with `DeAuthorized` (with
//! `StopTransactionOnInvalidId=false` it goes on instead). Retry policy for the CALLs themselves
//! lives in `calls.rs`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{ConnectorId, IdTag, ReservationId, TxId};
use crate::ocpp::messages::MeterValue;
use crate::ocpp::types::StopReason;

/// Most entries the queue keeps (a hostile or broken CSMS must not grow it without bound).
pub const MAX_QUEUED_TRANSACTIONS: usize = 1_000;

/// What a queued `StopTransaction` carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StopInfo {
    pub id_tag: Option<IdTag>,
    pub meter_stop_wh: u64,
    pub timestamp: DateTime<Utc>,
    pub reason: StopReason,
    pub transaction_data: Vec<MeterValue>,
}

/// A transaction waiting for delivery of its start and/or stop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OfflineTx {
    /// Unique key of the entry (the number inside a local id).
    pub key: u64,
    pub connector: ConnectorId,
    /// `Local` until the start was delivered, then `Csms`.
    pub tx_id: TxId,
    pub id_tag: IdTag,
    pub started_at: DateTime<Utc>,
    pub meter_start_wh: u64,
    pub reservation_id: Option<ReservationId>,
    pub start_pending: bool,
    pub stop: Option<StopInfo>,
}

/// The queue (persisted with the station state).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OfflineQueue {
    entries: Vec<OfflineTx>,
}

impl OfflineQueue {
    /// Appends `entry`. Over [`MAX_QUEUED_TRANSACTIONS`] entries the oldest one is dropped
    /// and returned: a stop-only entry first (its transaction is over), else the oldest.
    pub fn push(&mut self, entry: OfflineTx) -> Option<OfflineTx> {
        self.push_keeping(entry, |_| false)
    }

    /// Like [`push`](Self::push), but an entry for which `live` is true (its transaction still
    /// runs on a connector) is only dropped when every entry is live.
    pub fn push_keeping(
        &mut self,
        entry: OfflineTx,
        live: impl Fn(&OfflineTx) -> bool,
    ) -> Option<OfflineTx> {
        self.entries.push(entry);
        if self.entries.len() <= MAX_QUEUED_TRANSACTIONS {
            return None;
        }
        let index = self
            .entries
            .iter()
            .position(|e| !e.start_pending && !live(e))
            .or_else(|| self.entries.iter().position(|e| !live(e)))
            .unwrap_or(0);
        Some(self.entries.remove(index))
    }

    pub fn entries(&self) -> &[OfflineTx] {
        &self.entries
    }

    pub fn get(&self, key: u64) -> Option<&OfflineTx> {
        self.entries.iter().find(|e| e.key == key)
    }

    pub fn get_mut(&mut self, key: u64) -> Option<&mut OfflineTx> {
        self.entries.iter_mut().find(|e| e.key == key)
    }

    /// The entry of the (local or real) transaction `tx`.
    pub fn find_by_tx(&mut self, tx: TxId) -> Option<&mut OfflineTx> {
        self.entries.iter_mut().find(|e| e.tx_id == tx)
    }

    /// The entry of transaction `tx` on `connector`. CSMS ids are not unique across
    /// connectors (a CSMS may hand out the same id twice), so stops are matched per connector.
    pub fn find_by_tx_on(&mut self, connector: ConnectorId, tx: TxId) -> Option<&mut OfflineTx> {
        self.entries
            .iter_mut()
            .find(|e| e.tx_id == tx && e.connector == connector)
    }

    pub fn remove(&mut self, key: u64) -> Option<OfflineTx> {
        let index = self.entries.iter().position(|e| e.key == key)?;
        Some(self.entries.remove(index))
    }

    pub fn keys(&self) -> Vec<u64> {
        self.entries.iter().map(|e| e.key).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Messages still to deliver (`Snapshot::pending_offline_messages`).
    pub fn pending_messages(&self) -> u32 {
        let count: usize = self
            .entries
            .iter()
            .map(|e| usize::from(e.start_pending) + usize::from(e.stop.is_some()))
            .sum();
        u32::try_from(count).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: u64, start: bool, stop: bool) -> OfflineTx {
        OfflineTx {
            key,
            connector: ConnectorId::new(1),
            tx_id: TxId::Local(key),
            id_tag: IdTag::new("T").unwrap(),
            started_at: DateTime::UNIX_EPOCH,
            meter_start_wh: 0,
            reservation_id: None,
            start_pending: start,
            stop: stop.then(|| StopInfo {
                id_tag: None,
                meter_stop_wh: 5,
                timestamp: DateTime::UNIX_EPOCH,
                reason: StopReason::Local,
                transaction_data: Vec::new(),
            }),
        }
    }

    #[test]
    fn pending_messages_counts_starts_and_stops() {
        let mut queue = OfflineQueue::default();
        queue.push(entry(1, true, true));
        queue.push(entry(2, false, true));
        queue.push(entry(3, true, false));
        assert_eq!(queue.pending_messages(), 4);
        assert_eq!(queue.keys(), [1, 2, 3]);
        queue.remove(2);
        assert_eq!(queue.pending_messages(), 3);
    }

    #[test]
    fn entries_are_found_by_key_and_transaction() {
        let mut queue = OfflineQueue::default();
        queue.push(entry(7, true, false));
        assert!(queue.get(7).is_some());
        queue.find_by_tx(TxId::Local(7)).unwrap().tx_id = TxId::Csms(55);
        assert!(queue.find_by_tx(TxId::Local(7)).is_none());
        assert!(queue.find_by_tx(TxId::Csms(55)).is_some());
    }

    #[test]
    fn the_queue_is_bounded_and_drops_stop_only_entries_first() {
        let mut queue = OfflineQueue::default();
        // One start-pending entry first (the oldest), then stop-only entries.
        assert!(queue.push(entry(0, true, false)).is_none());
        for key in 1..MAX_QUEUED_TRANSACTIONS as u64 {
            assert!(queue.push(entry(key, false, true)).is_none());
        }
        assert_eq!(queue.keys().len(), MAX_QUEUED_TRANSACTIONS);
        let dropped = queue
            .push(entry(5_000, false, true))
            .expect("over the bound");
        assert_eq!(dropped.key, 1, "the oldest stop-only entry goes first");
        assert_eq!(queue.keys().len(), MAX_QUEUED_TRANSACTIONS);
        assert!(queue.get(0).is_some(), "the pending start stays");
        assert!(queue.get(5_000).is_some());

        // Only start-pending entries: the oldest one goes.
        let mut starts = OfflineQueue::default();
        for key in 0..=MAX_QUEUED_TRANSACTIONS as u64 {
            let dropped = starts.push(entry(key, true, false));
            assert_eq!(dropped.is_some(), key == MAX_QUEUED_TRANSACTIONS as u64);
        }
        assert!(starts.get(0).is_none());
    }

    #[test]
    fn queue_serde_roundtrip() {
        let mut queue = OfflineQueue::default();
        queue.push(entry(1, true, true));
        let json = serde_json::to_string(&queue).unwrap();
        assert_eq!(serde_json::from_str::<OfflineQueue>(&json).unwrap(), queue);
    }
}
