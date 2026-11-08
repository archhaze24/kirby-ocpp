//! The station: public API contract plus the actor behind it.
//!
//! # API style (one, consistently)
//!
//! A running station is an **actor** (one tokio task owning all state). Callers
//! hold a cloneable [`StationHandle`] and use exactly three things:
//!
//! 1. `handle.send(Command) -> Result<Outcome, StationError>`: act. `Command` is
//!    plain data with **no reply channel inside**; the handle creates the
//!    one-shot reply internally. The future completes when the command has
//!    finished, including the CSMS round trip. Typed methods such as
//!    `handle.plug_in(..)` are thin sugar over `send`.
//! 2. `handle.snapshot()` / `handle.watch_snapshot()`: read the latest
//!    [`Snapshot`] (a `tokio::sync::watch` value; never blocks the actor).
//! 3. `handle.subscribe()`: a `tokio::sync::broadcast` stream of [`Event`]s
//!    (wire frames, log lines, "state changed").
//!
//! UIs and headless runners never touch internals. CSMS-originated CALLs are the
//! mirror image: the connection layer hands the actor an incoming call together
//! with a one-shot [`Responder`] (see [`crate::ocpp::responder`]) which can be
//! used at most once.
//!
//! # Submodules
//!
//! The actor is split by concern: `actor` (state + event loop), `link` (connect, reconnect,
//! boot, heartbeat), `calls` (outbound CALLs), `status` (StatusNotification discipline),
//! `persistence`, `commands` (user commands), `handlers/` (CSMS-originated CALLs) and
//! `timers`. The pure, separately tested domain modules (`config_registry`,
//! `local_auth_list`, `authorization`, ...) are public; the transaction,
//! offline-queue, metering, reservation, smart-charging and maintenance rules are pure too. Each maps to the TS files named in its docs.

mod actor;
mod calls;
mod command;
mod commands;
mod event;
mod handle;
mod handlers;
mod link;
mod local_edit;
mod persistence;
mod power;
mod reservations;
mod sampling;
mod secure;
mod snapshot;
mod status;
pub mod timers;
mod transactions;
mod views;

pub mod authorization;
pub mod certificates;
pub mod charging_profiles;
pub mod config_registry;
pub mod connector;
pub mod local_auth_list;
pub mod maintenance;
pub mod metering;
pub mod offline;
pub mod reservation;
pub mod response_rules;
pub mod scripted_reply;
pub mod security;
pub mod smart_charging;
pub mod transaction;

use std::sync::Arc;

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

pub use self::authorization::{AuthSource, AuthorizationDecision, IdTagInfo};
pub use self::command::{Command, Outcome};
pub use self::event::{Direction, Event, FrameRecord, LogLevel, LogRecord};
pub use self::handle::StationHandle;
pub use self::maintenance::{DiagnosticsOutcome, FirmwareOutcome};
pub use self::reservation::Reservation;
pub use self::response_rules::{Behavior, ResponseRule};
pub use self::snapshot::{ConnectorSnapshot, Snapshot};
pub use self::transaction::TransactionState;
pub use self::views::{
    ConfigItem, LOCAL_LIST_VIEW_MAX, LocalListEntry, LocalListView, ProfileSummary,
};
pub use crate::ocpp::responder::Responder;
pub use crate::ws::ConnectionState;

use self::actor::StationActor;
use crate::clock::Clock;
use crate::config::StationConfig;

/// Capacity of the command queue (back-pressure for senders).
const COMMAND_QUEUE: usize = 64;
/// Capacity of the event broadcast (slow subscribers lag, see [`Event`]).
const EVENT_BUFFER: usize = 1024;

/// Entry point: spawns station actors. The station does **not** connect by itself: send
/// [`Command::Connect`] (the CLI does so at startup).
#[derive(Debug, Clone, Copy)]
pub struct Station;

impl Station {
    /// Starts a station and returns its handle and the actor's `JoinHandle`.
    ///
    /// Must be called inside a tokio runtime. The config is **not** validated
    /// here: call [`StationConfig::validate`] first (the CLI does). The actor
    /// ends on [`Command::Shutdown`] or when every handle has been dropped; await the
    /// `JoinHandle` after a shutdown to be sure it is gone.
    pub fn spawn(
        mut config: StationConfig,
        clock: Arc<dyn Clock>,
    ) -> (StationHandle, JoinHandle<()>) {
        config.normalize();
        let (command_tx, command_rx) = mpsc::channel(COMMAND_QUEUE);
        let (event_tx, _) = broadcast::channel(EVENT_BUFFER);

        let (actor, snapshot_rx) =
            StationActor::new(config.clone(), clock, command_rx, event_tx.clone());
        let handle = StationHandle::new(Arc::new(config), command_tx, snapshot_rx, event_tx);
        (handle, tokio::spawn(actor.run()))
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::clock::ManualClock;
    use crate::error::StationError;
    use crate::ids::ConnectorId;

    fn spawn() -> (StationHandle, JoinHandle<()>) {
        let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
        let config = StationConfig::new("ws://localhost:9000/ocpp")
            .with_connectors(2)
            .without_persistence();
        Station::spawn(config, Arc::new(clock))
    }

    #[tokio::test]
    async fn snapshot_is_available_immediately() {
        let (handle, join) = spawn();
        let snapshot = handle.snapshot();
        assert_eq!(snapshot.charge_point_id, "CP-001");
        assert_eq!(handle.config().url, "ws://localhost:9000/ocpp");
        assert_eq!(snapshot.connectors.len(), 2);
        handle.shutdown().await.unwrap();
        join.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_commands_error_instead_of_hanging() {
        let (handle, join) = spawn();
        // An unknown connector must be answered, not ignored.
        let err = handle
            .send(Command::MeterValues {
                connector: ConnectorId::new(9),
                wh_delta: 1,
                measurands: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, StationError::UnknownConnector(_)));
        handle.shutdown().await.unwrap();
        join.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_ends_the_actor_and_later_sends_fail() {
        let (handle, join) = spawn();
        let mut events = handle.subscribe();
        assert!(matches!(
            handle.send(Command::Shutdown).await,
            Ok(Outcome::Done)
        ));
        join.await.unwrap();
        assert!(matches!(
            handle.send(Command::Heartbeat).await,
            Err(StationError::Stopped)
        ));
        // The actor logged its stop through the injected clock.
        let mut saw_stopped = false;
        while let Ok(event) = events.try_recv() {
            if let Event::Log(record) = event
                && record.text == "station stopped"
            {
                assert_eq!(
                    record.ts,
                    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
                );
                saw_stopped = true;
            }
        }
        assert!(saw_stopped);
    }

    #[tokio::test]
    async fn actor_exits_when_all_handles_are_dropped() {
        let (handle, join) = spawn();
        drop(handle);
        join.await.unwrap();
    }

    #[tokio::test]
    async fn clones_share_one_station() {
        let (handle, join) = spawn();
        let other = handle.clone();
        let mut watcher = other.watch_snapshot();
        handle.shutdown().await.unwrap();
        join.await.unwrap();
        // The receiver outlives the actor and still holds the last published snapshot.
        assert_eq!(
            watcher.borrow_and_update().connection,
            ConnectionState::Disconnected
        );
        assert!(matches!(other.boot().await, Err(StationError::Stopped)));
    }

    #[test]
    fn command_names_match_the_serde_tag() {
        let commands = [
            Command::Connect,
            Command::PlugIn {
                connector: ConnectorId::new(1),
            },
            Command::SendRaw {
                action: "Heartbeat".into(),
                payload: serde_json::json!({}),
                validate: true,
            },
            Command::Shutdown,
        ];
        for command in commands {
            let json = serde_json::to_value(&command).unwrap();
            assert_eq!(json["command"], command.name());
            assert_eq!(serde_json::from_value::<Command>(json).unwrap(), command);
        }
    }
}
