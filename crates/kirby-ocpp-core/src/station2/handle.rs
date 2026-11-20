//! The cloneable client side of a running OCPP 2.0.1 station.

use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, oneshot, watch};

use super::command::{Command2, Outcome2};
use super::snapshot::Snapshot2;
use super::types::Reply2;
use crate::config::StationConfig;
use crate::error::StationError;
use crate::station::Event;

/// A command on its way to the actor, with the channel for its answer.
#[derive(Debug)]
pub(crate) struct Envelope2 {
    pub(crate) command: Command2,
    pub(crate) reply: Reply2,
}

/// Cloneable handle to a running station. All interaction goes through it.
///
/// * **Act:** [`Station2Handle::send`].
/// * **Observe state:** [`Station2Handle::snapshot`] / [`Station2Handle::watch_snapshot`].
/// * **Observe activity:** [`Station2Handle::subscribe`].
///
/// When the last handle is dropped the actor shuts down.
#[derive(Debug, Clone)]
pub struct Station2Handle {
    config: Arc<StationConfig>,
    commands: mpsc::Sender<Envelope2>,
    snapshot: watch::Receiver<Snapshot2>,
    events: broadcast::Sender<Event>,
}

impl Station2Handle {
    pub(crate) fn new(
        config: Arc<StationConfig>,
        commands: mpsc::Sender<Envelope2>,
        snapshot: watch::Receiver<Snapshot2>,
        events: broadcast::Sender<Event>,
    ) -> Self {
        Self {
            config,
            commands,
            snapshot,
            events,
        }
    }

    /// The launch configuration (after `normalize`).
    pub fn config(&self) -> &StationConfig {
        &self.config
    }

    /// Submits `command` and waits until it has completed.
    ///
    /// **Cancellation:** the command is queued before the future first yields, so dropping
    /// the future afterwards does **not** cancel the command; it still runs and its outcome
    /// is discarded. To abandon a long operation, send a command that undoes it (for
    /// example `Disconnect`).
    ///
    /// # Errors
    /// [`StationError::Stopped`] if the station is no longer running; otherwise whatever
    /// error the command itself produced.
    pub async fn send(&self, command: Command2) -> Result<Outcome2, StationError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Envelope2 { command, reply })
            .await
            .map_err(|_| StationError::Stopped)?;
        answer.await.map_err(|_| StationError::Stopped)?
    }

    /// The latest snapshot (a cheap copy).
    pub fn snapshot(&self) -> Snapshot2 {
        self.snapshot.borrow().clone()
    }

    /// A receiver that wakes on every snapshot change.
    pub fn watch_snapshot(&self) -> watch::Receiver<Snapshot2> {
        self.snapshot.clone()
    }

    /// A new subscription to station events. Only events after this call are seen.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub async fn connect(&self) -> Result<(), StationError> {
        self.send(Command2::Connect).await.map(drop)
    }

    pub async fn disconnect(&self) -> Result<(), StationError> {
        self.send(Command2::Disconnect).await.map(drop)
    }

    /// An external system limits `evse` (0 is the whole station); see
    /// [`Command2::SetExternalLimit`]. Returns the answer of the CSMS to `NotifyChargingLimit`.
    pub async fn set_external_limit(
        &self,
        evse: u32,
        limit: serde_json::Value,
    ) -> Result<Outcome2, StationError> {
        self.send(Command2::SetExternalLimit { evse, limit }).await
    }

    /// The external system releases its limits on `evse`; see [`Command2::ClearExternalLimit`].
    pub async fn clear_external_limit(&self, evse: u32) -> Result<Outcome2, StationError> {
        self.send(Command2::ClearExternalLimit { evse }).await
    }

    /// Asks the station to stop. `Err(Stopped)` if it already has.
    pub async fn shutdown(&self) -> Result<(), StationError> {
        self.send(Command2::Shutdown).await.map(drop)
    }
}
