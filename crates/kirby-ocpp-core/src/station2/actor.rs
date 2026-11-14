//! The OCPP 2.0.1 station actor: the single task that owns all station state.
//!
//! The rules of the 1.6 actor apply: state is mutated only here, never behind a lock; the
//! loop waits on the command queue, the connection's events, outbound CALL futures, the
//! internal channel and the timers; the snapshot is published once per loop iteration, and
//! only if it changed. Each concern keeps its state in a sub-struct of [`Station2Actor`]
//! that is defined in the module that implements the concern.

#![allow(
    dead_code,
    clippy::unused_async,
    clippy::unused_self,
    reason = "placeholder module; remove this allow when the module is implemented"
)]

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::{broadcast, mpsc, watch};

use super::authorization::AuthState;
use super::calls::Calls2;
use super::device_model::DeviceModel;
use super::evse::Evse;
use super::handle::Envelope2;
use super::link::Link2;
use super::persistence::Persistence2;
use super::reset::ResetPlan;
use super::snapshot::Snapshot2;
use super::transactions::TxEngine;
use super::tx_queue::TxQueue;
use crate::clock::Clock;
use crate::config::StationConfig;
use crate::error::CallError;
use crate::recording::RecordSink;
use crate::station::{Event, LogLevel};
use crate::store::StoreError;
use crate::timers::TimerWheel;
use crate::ws::{ConnectError, Connection, ConnectionEvent};

/// Names of the timers the actor uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TimerKey2 {
    /// Periodic `Heartbeat`.
    Heartbeat,
    /// Next reconnect attempt after a lost connection.
    Reconnect,
    /// Next `BootNotification` after `Pending`, `Rejected` or a failed attempt.
    BootRetry,
    /// Debounced write of the state file.
    PersistFlush,
    /// Periodic sample of an EVSE with a running transaction.
    MeterPeriodic(u32),
    /// Clock-aligned samples.
    MeterAligned,
    /// A queued transaction event is tried again.
    TxRetry,
    /// The EV did not connect in time after a remote start.
    EvConnectTimeout(u32),
    /// A reset waits for the moment to reboot.
    ResetDelay,
    /// The next `NotifyReport` chunk is due.
    ReportChunk,
}

/// Completions and log messages delivered to the actor loop.
pub(crate) enum Internal2 {
    /// A connection attempt finished.
    Connected {
        generation: u64,
        result: Result<(Connection, mpsc::Receiver<ConnectionEvent>), ConnectError>,
    },
    /// A CALL finished.
    CallDone {
        token: u64,
        generation: u64,
        result: Result<Value, CallError>,
    },
    /// A state file write finished.
    Saved(Result<(), StoreError>),
    /// A log line from asynchronous work.
    Log(LogLevel, String),
}

pub(crate) struct Station2Actor {
    pub(super) config: StationConfig,
    pub(super) clock: Arc<dyn Clock>,
    commands: mpsc::Receiver<Envelope2>,
    snapshot_tx: watch::Sender<Snapshot2>,
    events: broadcast::Sender<Event>,
    /// The live model; published as the snapshot.
    pub(super) state: Snapshot2,
    pub(super) timers: TimerWheel<TimerKey2>,
    pub(super) internal_tx: mpsc::UnboundedSender<Internal2>,
    internal_rx: mpsc::UnboundedReceiver<Internal2>,
    pub(super) link: Link2,
    pub(super) calls: Calls2,
    pub(super) dm: DeviceModel,
    pub(super) evses: Vec<Evse>,
    pub(super) tx: TxEngine,
    pub(super) queue: TxQueue,
    pub(super) auth: AuthState,
    pub(super) reset: Option<ResetPlan>,
    pub(super) persistence: Persistence2,
    /// The session recording, when `--record` is on, and the connection count it stamps.
    recording: Option<(RecordSink, u64)>,
}

impl Station2Actor {
    /// Builds the actor with its initial state.
    pub(crate) fn new(
        config: StationConfig,
        clock: Arc<dyn Clock>,
        commands: mpsc::Receiver<Envelope2>,
        events: broadcast::Sender<Event>,
        recording: Option<RecordSink>,
    ) -> (Self, watch::Receiver<Snapshot2>) {
        let (internal_tx, internal_rx) = mpsc::unbounded_channel();
        let state = Snapshot2::initial(&config);
        let (snapshot_tx, snapshot_rx) = watch::channel(state.clone());
        let actor = Self {
            config,
            clock,
            commands,
            snapshot_tx,
            events,
            state,
            timers: TimerWheel::new(),
            internal_tx,
            internal_rx,
            link: Link2::default(),
            calls: Calls2::default(),
            dm: DeviceModel::default(),
            evses: Vec::new(),
            tx: TxEngine::default(),
            queue: TxQueue::default(),
            auth: AuthState::default(),
            reset: None,
            persistence: Persistence2::default(),
            recording: recording.map(|sink| (sink, 0)),
        };
        (actor, snapshot_rx)
    }

    /// The event loop. Ends on `Shutdown` or when every handle has been dropped.
    pub(crate) async fn run(self) {}

    /// Emits a log line as an [`Event::Log`].
    pub(super) fn log(&self, _level: LogLevel, _text: String) {}

    /// Publishes the live model as the snapshot if it changed.
    pub(super) fn publish(&mut self) {}

    /// `true` once the CSMS accepted a `BootNotification`.
    pub(super) fn registration_accepted(&self) -> bool {
        false
    }
}
