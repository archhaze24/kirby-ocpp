//! Per-connector transaction state machine.
//!
//! TS reference: `startTransaction`/`stopTransaction` in `src/station.ts`,
//! `src/station/transaction-rules.ts`, `src/station/offline-transaction-sync.ts`.
//!
//! ```text
//!            start requested                    CSMS accepts (id assigned)
//!   Idle ───────────────────────▶ PendingStart ───────────────────────────▶ Active
//!    ▲                              │   │ CSMS rejects idTag                  │
//!    │◀─────────────────────────────┘   │ (no local state is kept)            │ stop requested
//!    │                                                                         ▼
//!    └──────────────────────── StopTransaction confirmed ◀──────────────── Stopping
//! ```
//!
//! * `PendingStart`: a `StartTransaction` exists but has no CSMS id yet - either
//!   the CALL is in flight, or we are offline and the transaction runs on a
//!   station-local [`TxId::Local`] id (connector already `Charging`, the start is
//!   queued in [`crate::station::offline`]). It is the guard against a second
//!   start while the first is unresolved.
//! * `Active`: has a CSMS-assigned id; meter values and stop are allowed.
//! * `Stopping`: a `StopTransaction` is in flight. This is the guard against the
//!   double stop of the TS station (remote stop racing an unplug): a second stop
//!   request while `Stopping` is a no-op/`InvalidState`.
//! * A stop issued while **offline** does not linger in `Stopping`: the message is
//!   queued in the offline queue (keyed by the transaction's id, local or real)
//!   and the connector returns to `Idle`, so a new session can start right away.
//!   This removes the "one offline transaction per connector" limitation.
//!
//! The enum is serde-serializable: it is part of [`crate::station::Snapshot`]
//! and of the persisted state.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{IdTag, ReservationId, TxId};
use crate::ocpp::types::StopReason;

/// State of the transaction on one connector.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TransactionState {
    /// No transaction.
    #[default]
    Idle,
    /// Started but not yet acknowledged by the CSMS (in flight or queued offline).
    PendingStart(PendingStart),
    /// Running, CSMS id known.
    Active(ActiveTransaction),
    /// `StopTransaction` in flight.
    Stopping(StoppingTransaction),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingStart {
    /// Station-local id while unacknowledged ([`TxId::Local`]).
    pub tx_id: TxId,
    pub id_tag: IdTag,
    pub started_at: DateTime<Utc>,
    pub meter_start_wh: u64,
    pub reservation_id: Option<ReservationId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveTransaction {
    pub tx_id: TxId,
    pub id_tag: IdTag,
    pub started_at: DateTime<Utc>,
    pub meter_start_wh: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoppingTransaction {
    pub tx_id: TxId,
    pub id_tag: Option<IdTag>,
    pub reason: StopReason,
}

impl TransactionState {
    pub fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    /// The transaction id, whatever the phase (`None` when idle).
    pub fn tx_id(&self) -> Option<TxId> {
        match self {
            Self::Idle => None,
            Self::PendingStart(t) => Some(t.tx_id),
            Self::Active(t) => Some(t.tx_id),
            Self::Stopping(t) => Some(t.tx_id),
        }
    }

    /// The idTag that owns the transaction.
    pub fn id_tag(&self) -> Option<&IdTag> {
        match self {
            Self::Idle => None,
            Self::PendingStart(t) => Some(&t.id_tag),
            Self::Active(t) => Some(&t.id_tag),
            Self::Stopping(t) => t.id_tag.as_ref(),
        }
    }

    /// `true` if a new transaction may not start on this connector.
    pub fn blocks_new_transaction(&self) -> bool {
        !self.is_idle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag() -> IdTag {
        IdTag::new("TAG1").unwrap()
    }

    #[test]
    fn idle_has_no_id_and_does_not_block() {
        let idle = TransactionState::default();
        assert!(idle.is_idle());
        assert_eq!(idle.tx_id(), None);
        assert_eq!(idle.id_tag(), None);
        assert!(!idle.blocks_new_transaction());
    }

    #[test]
    fn transaction_id_zero_is_a_running_transaction() {
        let active = TransactionState::Active(ActiveTransaction {
            tx_id: TxId::Csms(0),
            id_tag: tag(),
            started_at: DateTime::<Utc>::UNIX_EPOCH,
            meter_start_wh: 0,
        });
        assert_eq!(active.tx_id(), Some(TxId::Csms(0)));
        assert!(active.blocks_new_transaction());
    }

    #[test]
    fn every_non_idle_phase_blocks_and_exposes_its_id() {
        let pending = TransactionState::PendingStart(PendingStart {
            tx_id: TxId::Local(1),
            id_tag: tag(),
            started_at: DateTime::<Utc>::UNIX_EPOCH,
            meter_start_wh: 5,
            reservation_id: None,
        });
        let stopping = TransactionState::Stopping(StoppingTransaction {
            tx_id: TxId::Csms(9),
            id_tag: None,
            reason: StopReason::Local,
        });
        assert!(pending.blocks_new_transaction() && stopping.blocks_new_transaction());
        assert_eq!(pending.tx_id(), Some(TxId::Local(1)));
        assert_eq!(pending.id_tag(), Some(&tag()));
        assert_eq!(stopping.tx_id(), Some(TxId::Csms(9)));
        assert_eq!(stopping.id_tag(), None);
    }

    #[test]
    fn serde_uses_an_internal_state_tag() {
        let json = serde_json::to_value(TransactionState::Idle).unwrap();
        assert_eq!(json, serde_json::json!({"state": "idle"}));
        let stopping = TransactionState::Stopping(StoppingTransaction {
            tx_id: TxId::Csms(3),
            id_tag: Some(tag()),
            reason: StopReason::Remote,
        });
        let json = serde_json::to_string(&stopping).unwrap();
        assert_eq!(
            serde_json::from_str::<TransactionState>(&json).unwrap(),
            stopping
        );
    }
}
