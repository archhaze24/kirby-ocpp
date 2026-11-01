//! The cloneable client side of a running station.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use super::command::{Command, Outcome};
use super::event::Event;
use super::snapshot::Snapshot;
use crate::config::StationConfig;
use crate::error::StationError;
use crate::ids::{ConnectorId, IdTag};
use crate::ocpp::types::StopReason;

/// A command on its way to the actor, with the channel for its answer.
#[derive(Debug)]
pub(crate) struct Envelope {
    pub(crate) command: Command,
    pub(crate) reply: oneshot::Sender<Result<Outcome, StationError>>,
}

/// Cloneable handle to a running station. All interaction goes through it.
///
/// * **Act:** [`StationHandle::send`] (or one of the thin typed wrappers below).
/// * **Observe state:** [`StationHandle::snapshot`] / [`StationHandle::watch_snapshot`].
/// * **Observe activity:** [`StationHandle::subscribe`].
///
/// When the last handle is dropped the actor shuts down.
#[derive(Debug, Clone)]
pub struct StationHandle {
    config: Arc<StationConfig>,
    commands: mpsc::Sender<Envelope>,
    snapshot: watch::Receiver<Snapshot>,
    events: broadcast::Sender<Event>,
}

impl StationHandle {
    pub(crate) fn new(
        config: Arc<StationConfig>,
        commands: mpsc::Sender<Envelope>,
        snapshot: watch::Receiver<Snapshot>,
        events: broadcast::Sender<Event>,
    ) -> Self {
        Self {
            config,
            commands,
            snapshot,
            events,
        }
    }

    /// The launch configuration (after `normalize`). Static for the station's lifetime:
    /// UIs read the URL, vendor/model and `initial_connector` from here, not from the snapshot.
    pub fn config(&self) -> &StationConfig {
        &self.config
    }

    /// Submits `command` and waits until it has completed (for CALL commands: until the CSMS
    /// answered; for state commands: until the state changed, see [`Command`]).
    ///
    /// **Cancellation:** the command is queued before the future first yields, so dropping
    /// the future afterwards does **not** cancel the command; it still runs, its
    /// `Outcome` is discarded. To abandon a long operation, send a command that undoes
    /// it (e.g. `Disconnect`) instead.
    ///
    /// # Errors
    /// [`StationError::Stopped`] if the station is no longer running; otherwise
    /// whatever error the command itself produced. A *rejection by the CSMS* of an
    /// idTag is **not** an error: it is `Ok(Outcome::TransactionRejected{..})` or an
    /// `Authorized` decision with `accepted == false`.
    pub async fn send(&self, command: Command) -> Result<Outcome, StationError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Envelope { command, reply })
            .await
            .map_err(|_| StationError::Stopped)?;
        answer.await.map_err(|_| StationError::Stopped)?
    }

    /// The latest snapshot (a cheap copy).
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.borrow().clone()
    }

    /// A receiver that wakes on every snapshot change (`changed().await`).
    pub fn watch_snapshot(&self) -> watch::Receiver<Snapshot> {
        self.snapshot.clone()
    }

    /// A new subscription to station events. Only events after this call are seen.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    // Thin sugar over `send`; the `Command` enum stays the single source of truth.

    pub async fn connect(&self) -> Result<(), StationError> {
        self.send(Command::Connect).await.map(drop)
    }

    pub async fn disconnect(&self) -> Result<(), StationError> {
        self.send(Command::Disconnect).await.map(drop)
    }

    pub async fn boot(&self) -> Result<Outcome, StationError> {
        self.send(Command::Boot).await
    }

    pub async fn plug_in(&self, connector: ConnectorId) -> Result<(), StationError> {
        self.send(Command::PlugIn { connector }).await.map(drop)
    }

    pub async fn authorize(&self, id_tag: IdTag) -> Result<Outcome, StationError> {
        self.send(Command::Authorize { id_tag }).await
    }

    pub async fn start_transaction(
        &self,
        connector: ConnectorId,
        id_tag: IdTag,
    ) -> Result<Outcome, StationError> {
        self.send(Command::StartTransaction { connector, id_tag })
            .await
    }

    pub async fn stop_transaction(
        &self,
        connector: ConnectorId,
        reason: StopReason,
    ) -> Result<(), StationError> {
        self.send(Command::StopTransaction {
            connector,
            reason,
            id_tag: None,
        })
        .await
        .map(drop)
    }

    /// Sends a validated raw CALL and returns its CALLRESULT payload.
    pub async fn send_raw(
        &self,
        action: impl Into<String>,
        payload: Value,
    ) -> Result<Value, StationError> {
        match self
            .send(Command::SendRaw {
                action: action.into(),
                payload,
                validate: true,
            })
            .await?
        {
            Outcome::Response { payload } => Ok(payload),
            other => Err(StationError::InvalidState {
                connector: None,
                reason: format!("SendRaw produced an unexpected outcome: {other:?}"),
            }),
        }
    }

    /// Asks the station to stop. `Err(Stopped)` if it already has.
    pub async fn shutdown(&self) -> Result<(), StationError> {
        self.send(Command::Shutdown).await.map(drop)
    }
}
