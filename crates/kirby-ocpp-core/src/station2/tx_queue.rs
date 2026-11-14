//! The outbound queue of transaction events: ordering, offline delivery and retries.
//!
//! One FIFO holds every `TransactionEvent` of the station, in the order the events were
//! generated (the `seqNo` order of each transaction depends on it). At most one entry is in
//! flight and only the head is ever sent, so a stalled head blocks the events behind it.
//!
//! * A delivered event is removed; an `idTokenInfo` in the answer goes to the authorization
//!   module.
//! * A failure while the link is up (CALLERROR, timeout, bad answer) counts as an attempt: the
//!   head waits `attempts x MessageAttemptInterval` and is sent again unchanged, and it is
//!   discarded after `MessageAttempts` failures (E13).
//! * A closed link is not a failed attempt. The queue closes and the entries stay; every
//!   undelivered entry is flagged `offline` (E11.FR.07). Delivery starts again when the boot
//!   or the reconnect has been accepted ([`Station2Actor::flush_queue`]).
//! * The queue holds [`QUEUE_CAP`] entries. When it is full, every second `Updated` event is
//!   dropped (E12.FR.04/05); `Started` and `Ended` are never dropped.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::actor::{Station2Actor, TimerKey2};
use super::types::Origin2;
use crate::error::CallError;
use crate::ocpp::v2::messages::TransactionEventKind;
use crate::station::LogLevel;

/// Entries the queue holds before it starts dropping `Updated` events.
const QUEUE_CAP: usize = 4096;

/// A finished `TransactionEvent` waiting for delivery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedEvent {
    /// The request payload as it goes on the wire.
    pub payload: Value,
    pub evse: u32,
    pub transaction_id: String,
    pub kind: TransactionEventKind,
    pub seq_no: i32,
    /// Delivery attempts that failed so far.
    pub attempts: u32,
    /// The event was generated or queued while the link was down.
    pub offline: bool,
}

/// The queued events, as persisted.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QueuePersist {
    pub events: Vec<QueuedEvent>,
}

/// The queue of transaction events.
#[derive(Debug)]
pub(crate) struct TxQueue {
    events: VecDeque<QueuedEvent>,
    /// Delivery is allowed: the registration was accepted and the link has not dropped since.
    open: bool,
    /// The head has been handed to the call engine and not answered yet.
    in_flight: bool,
    cap: usize,
}

impl Default for TxQueue {
    fn default() -> Self {
        Self {
            events: VecDeque::new(),
            open: false,
            in_flight: false,
            cap: QUEUE_CAP,
        }
    }
}

/// Sets `offline: true` in a request payload.
fn set_offline(payload: &mut Value) {
    if let Some(object) = payload.as_object_mut() {
        object.insert("offline".to_owned(), Value::Bool(true));
    }
}

/// Drops every second `Updated` event after the first one, never the head (it may be in
/// flight). Returns how many were dropped.
fn thin_updated(events: &mut VecDeque<QueuedEvent>) -> usize {
    let updated: Vec<usize> = events
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, event)| event.kind == TransactionEventKind::Updated)
        .map(|(index, _)| index)
        .collect();
    let mut dropped = 0;
    // From the back, so the remaining indices stay valid.
    for index in updated.into_iter().skip(1).step_by(2).rev() {
        if events.remove(index).is_some() {
            dropped += 1;
        }
    }
    dropped
}

impl Station2Actor {
    /// Whether events of transaction `id` (any transaction for `None`) are queued.
    pub(super) fn queue_has_messages_for(&self, id: Option<&str>) -> bool {
        match id {
            Some(id) => self
                .queue
                .events
                .iter()
                .any(|event| event.transaction_id == id),
            None => !self.queue.events.is_empty(),
        }
    }

    /// The queued events, oldest first.
    #[cfg(test)]
    pub(super) fn queued_events(&self) -> Vec<QueuedEvent> {
        self.queue.events.iter().cloned().collect()
    }

    /// Lowers the capacity so a test can reach it.
    #[cfg(test)]
    pub(super) fn set_queue_cap(&mut self, cap: usize) {
        self.queue.cap = cap;
    }

    /// Adds an event at the end without writing the state file or sending anything.
    pub(super) fn queue_enqueue(&mut self, event: QueuedEvent) {
        if self.queue.events.len() >= self.queue.cap {
            let dropped = thin_updated(&mut self.queue.events);
            if dropped > 0 {
                self.log(
                    LogLevel::Warn,
                    format!("transaction event queue full: dropped {dropped} Updated event(s)"),
                );
            } else {
                self.log(
                    LogLevel::Warn,
                    "transaction event queue full: nothing can be dropped, keeping every event"
                        .to_owned(),
                );
            }
        }
        self.queue.events.push_back(event);
        self.sync_queue_count();
    }

    /// Adds an event, keeps it on disk and sends it when delivery is open.
    pub(super) fn queue_push(&mut self, event: QueuedEvent) {
        self.queue_enqueue(event);
        self.flush_now();
        self.queue_pump();
    }

    fn sync_queue_count(&mut self) {
        self.state.queued_events = u32::try_from(self.queue.events.len()).unwrap_or(u32::MAX);
    }

    /// Starts delivering the queue. Called after an accepted boot and after a reconnect with
    /// an accepted registration.
    pub(super) fn flush_queue(&mut self) {
        self.queue.open = true;
        self.queue_pump();
    }

    /// The link went down: marks every undelivered event as generated offline and stops
    /// delivery until the next flush.
    pub(super) fn queue_mark_offline(&mut self) {
        self.queue.open = false;
        self.timers.cancel(&TimerKey2::TxRetry);
        let mut changed = false;
        for event in &mut self.queue.events {
            if !event.offline {
                event.offline = true;
                set_offline(&mut event.payload);
                changed = true;
            }
        }
        if changed {
            self.mark_dirty();
        }
    }

    /// Sends the head of the queue if delivery is open and nothing is in flight or waiting
    /// for a retry.
    fn queue_pump(&mut self) {
        if !self.queue.open
            || self.queue.in_flight
            || self.link.conn.is_none()
            || !self.registration_accepted()
            || self.timers.is_armed(&TimerKey2::TxRetry)
        {
            return;
        }
        let Some(head) = self.queue.events.front() else {
            return;
        };
        let payload = head.payload.clone();
        // Set before the call: a refused call runs the continuation before it returns.
        self.queue.in_flight = true;
        self.call_raw(
            Origin2::Station,
            "TransactionEvent",
            payload,
            true,
            Box::new(Station2Actor::on_tx_event_result),
        );
    }

    fn on_tx_event_result(&mut self, result: Result<Value, CallError>) {
        self.queue.in_flight = false;
        match result {
            Ok(answer) => {
                let Some(done) = self.queue.events.pop_front() else {
                    return;
                };
                self.sync_queue_count();
                self.flush_now();
                // The hook may end the transaction and queue more events; the delivered one is
                // gone and nothing is in flight by now.
                if let Some(info) = answer.get("idTokenInfo") {
                    self.on_id_token_info(done.evse, &done.transaction_id, info);
                }
                self.queue_pump();
            }
            Err(
                CallError::Closed { .. }
                | CallError::NotConnected
                | CallError::NotRegistered { .. },
            ) => {
                // Not a failed attempt: the entry waits for the next flush.
                self.queue.open = false;
            }
            Err(error) => self.on_tx_event_failed(&error),
        }
    }

    /// The head was not accepted: waits and retries, or discards it after the last attempt.
    fn on_tx_event_failed(&mut self, error: &CallError) {
        let Some(head) = self.queue.events.front_mut() else {
            return;
        };
        head.attempts += 1;
        let attempts = head.attempts;
        let seq_no = head.seq_no;
        let limit = self.tx_attempts().max(1);
        if attempts >= limit {
            self.queue.events.pop_front();
            self.sync_queue_count();
            self.log(
                LogLevel::Error,
                format!(
                    "TransactionEvent seqNo {seq_no} discarded after {attempts} failed attempt(s): {error}"
                ),
            );
            self.flush_now();
            self.queue_pump();
        } else {
            let delay = self.tx_attempt_interval() * attempts;
            self.log(
                LogLevel::Warn,
                format!(
                    "TransactionEvent seqNo {seq_no} not accepted (attempt {attempts} of {limit}), retrying in {}s",
                    delay.as_secs()
                ),
            );
            self.timers.set(TimerKey2::TxRetry, delay);
            self.mark_dirty();
        }
    }

    /// The retry delay of the event at the head of the queue elapsed.
    pub(super) fn on_tx_retry_timer(&mut self) {
        self.queue_pump();
    }

    /// The queued events, for the state file.
    pub(super) fn queue_persist(&self) -> QueuePersist {
        QueuePersist {
            events: self.queue.events.iter().cloned().collect(),
        }
    }

    /// Takes back the persisted queue at start. The restored events go in front of anything
    /// queued before (an ended transaction), and every one of them counts as offline: the
    /// process was down.
    pub(super) fn queue_restore(&mut self, queue: QueuePersist) {
        for mut event in queue.events.into_iter().rev() {
            event.offline = true;
            set_offline(&mut event.payload);
            self.queue.events.push_front(event);
        }
        self.sync_queue_count();
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn event(kind: TransactionEventKind, seq_no: i32) -> QueuedEvent {
        QueuedEvent {
            payload: json!({"seqNo": seq_no}),
            evse: 1,
            transaction_id: "tx".to_owned(),
            kind,
            seq_no,
            attempts: 0,
            offline: false,
        }
    }

    fn seqs(events: &VecDeque<QueuedEvent>) -> Vec<i32> {
        events.iter().map(|e| e.seq_no).collect()
    }

    #[test]
    fn thinning_drops_every_second_updated_and_keeps_the_head() {
        let mut events: VecDeque<_> = [
            event(TransactionEventKind::Started, 0),
            event(TransactionEventKind::Updated, 1),
            event(TransactionEventKind::Updated, 2),
            event(TransactionEventKind::Updated, 3),
            event(TransactionEventKind::Updated, 4),
            event(TransactionEventKind::Updated, 5),
            event(TransactionEventKind::Ended, 6),
        ]
        .into();
        // Updated after the head: 1, 2, 3, 4, 5; the second, fourth go: 2 and 4.
        assert_eq!(thin_updated(&mut events), 2);
        assert_eq!(seqs(&events), [0, 1, 3, 5, 6]);
    }

    #[test]
    fn thinning_never_drops_the_head_even_when_it_is_updated() {
        let mut events: VecDeque<_> = [
            event(TransactionEventKind::Updated, 1),
            event(TransactionEventKind::Updated, 2),
            event(TransactionEventKind::Updated, 3),
        ]
        .into();
        // The head is excluded: the candidates are 2 and 3; 3 is the second.
        assert_eq!(thin_updated(&mut events), 1);
        assert_eq!(seqs(&events), [1, 2]);
    }

    #[test]
    fn thinning_never_drops_started_or_ended() {
        let mut events: VecDeque<_> = [
            event(TransactionEventKind::Started, 0),
            event(TransactionEventKind::Ended, 1),
            event(TransactionEventKind::Started, 0),
            event(TransactionEventKind::Ended, 1),
        ]
        .into();
        assert_eq!(thin_updated(&mut events), 0);
        assert_eq!(events.len(), 4);
    }

    #[test]
    fn a_single_updated_event_is_kept() {
        let mut events: VecDeque<_> = [
            event(TransactionEventKind::Started, 0),
            event(TransactionEventKind::Updated, 1),
        ]
        .into();
        assert_eq!(thin_updated(&mut events), 0);
    }

    #[test]
    fn offline_flag_is_written_into_the_payload() {
        let mut payload = json!({"seqNo": 3});
        set_offline(&mut payload);
        assert_eq!(payload, json!({"seqNo": 3, "offline": true}));
        // A payload that is not an object is left alone.
        let mut odd = json!(7);
        set_offline(&mut odd);
        assert_eq!(odd, json!(7));
    }
}
