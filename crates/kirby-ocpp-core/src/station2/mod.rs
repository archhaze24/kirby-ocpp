//! The OCPP 2.0.1 charge station.
//!
//! It follows the API style of the 1.6 [`station`](crate::station) module: a running station
//! is an actor behind a cloneable [`Station2Handle`]. Callers send typed [`Command2`]s, read
//! [`Snapshot2`]s and subscribe to [`Event`](crate::station::Event)s. The 1.6 and 2.0.1
//! stations share the connection layer, the timers, the recording and the event types, and
//! nothing else: the commands, outcomes and snapshots are separate types because the
//! protocols model EVSEs, connectors, transactions and configuration differently.
//!
//! # Submodules
//!
//! The actor is split by concern: `actor` (state and event loop), `link` (connect, reconnect,
//! boot, heartbeat), `calls` (outbound CALLs and the registration gate), `commands` (routing
//! of user commands), `hooks` (timer and loop hooks), `handlers/` (CSMS-originated CALLs),
//! `device_model`, `evse` and `status` (EVSEs, connectors, `StatusNotification`),
//! `transactions` and `tx_queue` (transactions and the outbound event queue),
//! `authorization`, `local_list` (the Local Authorization List), `remote`, `reservation` (reservations), `reset`, `security_events` (startup and reboot events), `scripted`
//! (scenario replies), `smart_charging` (charging profiles and external limits) and
//! `persistence`.

mod actor;
mod authorization;
mod calls;
mod command;
mod commands;
mod device_model;
mod evse;
mod handle;
mod handlers;
mod hooks;
mod link;
mod local_list;
mod persistence;
mod remote;
mod reservation;
mod reset;
mod scripted;
mod security_events;
mod smart_charging;
mod snapshot;
mod status;
mod tariff;
mod transactions;
mod tx_queue;
mod types;

use std::sync::Arc;

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

pub use self::authorization::{AuthCachePersist, CacheEntry};
pub use self::command::{Command2, Outcome2};
pub use self::device_model::DeviceModelPersist;
pub use self::evse::EvseAvail;
pub use self::handle::Station2Handle;
pub use self::local_list::{ListEntry, LocalListPersist};
pub use self::reservation::{Reservation, ReservationPersist};
pub use self::smart_charging::{SmartChargingPersist, StoredProfile2};
pub use self::snapshot::{
    ConnectorSnapshot2, EvseSnapshot, ReservationView2, Snapshot2, TransactionSnapshot,
    VariableView,
};
pub use self::transactions::{
    ActiveTx, LimitKind, TransactionLimit2, TransactionTariff, TxPersist, TxRuntime,
};
pub use self::tx_queue::{QueuePersist, QueuedEvent};
pub use self::types::{IdTokenRef, StartStopPoint, TxId2};

use self::actor::Station2Actor;
use crate::clock::Clock;
use crate::config::StationConfig;
use crate::recording::RecordSink;

/// Capacity of the command queue (back-pressure for senders).
const COMMAND_QUEUE: usize = 64;
/// Capacity of the event broadcast (slow subscribers lag).
const EVENT_BUFFER: usize = 1024;

/// Entry point: spawns OCPP 2.0.1 station actors. The station does **not** connect by itself:
/// send [`Command2::Connect`].
#[derive(Debug, Clone, Copy)]
pub struct Station2;

impl Station2 {
    /// Starts a station and returns its handle and the actor's `JoinHandle`.
    ///
    /// Must be called inside a tokio runtime. The config is **not** validated here: call
    /// [`StationConfig::validate`] first. The actor ends on [`Command2::Shutdown`] or when
    /// every handle has been dropped; await the `JoinHandle` after a shutdown to be sure it
    /// is gone.
    pub fn spawn(config: StationConfig, clock: Arc<dyn Clock>) -> (Station2Handle, JoinHandle<()>) {
        Self::spawn_recorded(config, clock, None)
    }

    /// [`Station2::spawn`] with a session recording: every frame and every command goes to
    /// `recording` (see [`crate::recording`]) before anyone else sees it.
    pub fn spawn_recorded(
        mut config: StationConfig,
        clock: Arc<dyn Clock>,
        recording: Option<RecordSink>,
    ) -> (Station2Handle, JoinHandle<()>) {
        config.normalize();
        let (command_tx, command_rx) = mpsc::channel(COMMAND_QUEUE);
        let (event_tx, _) = broadcast::channel(EVENT_BUFFER);

        let (actor, snapshot_rx) = Station2Actor::new(
            config.clone(),
            clock,
            command_rx,
            event_tx.clone(),
            recording,
        );
        let handle = Station2Handle::new(Arc::new(config), command_tx, snapshot_rx, event_tx);
        (handle, tokio::spawn(actor.run()))
    }
}
