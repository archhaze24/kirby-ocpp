//! The station actor: the single task that owns all station state.
//!
//! Stub: it publishes the initial snapshot, answers [`Command::Shutdown`] and
//! rejects every other command with [`StationError::NotImplemented`] (so callers
//! never hang). Later stages add: the connection (`ws`), timers, per-connector
//! state machines and the CSMS command handlers. Rules for whoever fills it in:
//!
//! * state is mutated **only** here, never behind a lock;
//! * after each mutation call `publish()` (snapshot + `StateChanged`);
//! * never `await` a CSMS answer inline in the command loop in a way that blocks
//!   other commands from being rejected/queued: use the outbound CALL queue.

use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, watch};

use super::command::{Command, Outcome};
use super::event::{Event, LogLevel, LogRecord};
use super::handle::Envelope;
use super::snapshot::Snapshot;
use crate::clock::Clock;
use crate::config::StationConfig;
use crate::error::StationError;
use crate::ws::ConnectionState;

pub(crate) struct StationActor {
    config: StationConfig,
    clock: Arc<dyn Clock>,
    commands: mpsc::Receiver<Envelope>,
    snapshot_tx: watch::Sender<Snapshot>,
    events: broadcast::Sender<Event>,
    state: Snapshot,
}

impl StationActor {
    pub(crate) fn new(
        config: StationConfig,
        clock: Arc<dyn Clock>,
        commands: mpsc::Receiver<Envelope>,
        snapshot_tx: watch::Sender<Snapshot>,
        events: broadcast::Sender<Event>,
    ) -> Self {
        let state = Snapshot::initial(&config);
        Self {
            config,
            clock,
            commands,
            snapshot_tx,
            events,
            state,
        }
    }

    pub(crate) async fn run(mut self) {
        self.log(
            LogLevel::Info,
            format!(
                "station {} started ({} connector(s))",
                self.config.charge_point_id,
                self.state.connectors.len()
            ),
        );
        while let Some(Envelope { command, reply }) = self.commands.recv().await {
            let name = command.name();
            tracing::debug!(command = name, "station command");
            let result = match command {
                Command::Shutdown => {
                    // The requester may have gone away; nothing to do about that.
                    let _ = reply.send(Ok(Outcome::Done));
                    break;
                }
                _ => Err(StationError::NotImplemented { command: name }),
            };
            let _ = reply.send(result);
        }
        self.state.connection = ConnectionState::Disconnected;
        self.publish();
        self.log(LogLevel::Info, "station stopped".to_owned());
    }

    fn publish(&self) {
        self.snapshot_tx.send_replace(self.state.clone());
        // `Err` only means nobody is subscribed.
        let _ = self.events.send(Event::StateChanged);
    }

    fn log(&self, level: LogLevel, text: String) {
        let _ = self.events.send(Event::Log(LogRecord {
            ts: self.clock.now_utc(),
            level,
            text,
        }));
    }
}
