//! The outbound queue of transaction events: ordering, offline delivery and retries.

#![allow(
    dead_code,
    clippy::unused_self,
    reason = "placeholder module; remove this allow when the module is implemented"
)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::actor::Station2Actor;
use crate::ocpp::v2::messages::TransactionEventKind;

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
#[derive(Debug, Default)]
pub(crate) struct TxQueue {}

impl Station2Actor {
    /// Whether events of transaction `id` (any transaction for `None`) are queued.
    pub(super) fn queue_has_messages_for(&self, _id: Option<&str>) -> bool {
        false
    }

    /// Starts delivering the queue. Called after an accepted boot and after a reconnect with
    /// an accepted registration.
    pub(super) fn flush_queue(&mut self) {}

    /// The link went down: marks every undelivered event as generated offline.
    pub(super) fn queue_mark_offline(&mut self) {}

    /// The retry delay of the event at the head of the queue elapsed.
    pub(super) fn on_tx_retry_timer(&mut self) {}
}
