//! The OCPP 2.0.1 station actor: the single task that owns all station state.
//!
//! The rules of the 1.6 actor apply: state is mutated only here, never behind a lock; the
//! loop waits on the command queue, the connection's events, outbound CALL futures, the
//! internal channel and the timers; the snapshot is published once per loop iteration, and
//! only if it changed. Each concern keeps its state in a sub-struct of [`Station2Actor`]
//! that is defined in the module that implements the concern.
//!
//! Nothing in the loop blocks on the network or the disk: connecting runs as a spawned task,
//! CALL futures only poll reply channels, and the only awaited work is the graceful shutdown.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, watch};

use super::authorization::AuthState;
use super::calls::Calls2;
use super::command::{Command2, Outcome2};
use super::device_model::DeviceModel;
use super::evse::Evse;
use super::handle::Envelope2;
use super::link::{Link2, sanitize, security_profile_of};
use super::persistence::Persistence2;
use super::reset::ResetPlan;
use super::snapshot::{Snapshot2, wire_protocol};
use super::transactions::TxEngine;
use super::tx_queue::TxQueue;
use crate::clock::Clock;
use crate::config::StationConfig;
use crate::error::CallError;
use crate::ocpp::Protocol;
use crate::recording::RecordSink;
use crate::station::scripted_reply::ReplyList;
use crate::station::{Event, FrameRecord, LogLevel, LogRecord};
use crate::store::StoreError;
use crate::timers::TimerWheel;
use crate::ws::{ConnectError, Connection, ConnectionEvent, ConnectionState};

/// How long a graceful shutdown waits for the WebSocket close handshake.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

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
    /// The next `TxEndedInterval` sample of an EVSE with a running transaction.
    TxEndedSample(u32),
    /// The next clock-aligned `TxEndedInterval` sample of such an EVSE.
    TxEndedAligned(u32),
    /// A queued transaction event is tried again.
    TxRetry,
    /// Check the energy and time limits of an ongoing transaction.
    TxLimit(u32),
    RunningCost(u32),
    DefaultTariffActivation,
    /// The EV did not connect in time after a remote start.
    EvConnectTimeout(u32),
    /// A reset waits for the moment to reboot.
    ResetDelay,
    /// The next `NotifyReport` chunk is due.
    ReportChunk,
    /// The earliest reservation expires.
    ReservationExpiry,
    /// The next `ReportChargingProfiles` is due.
    ProfileReport,
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
    #[allow(dead_code, reason = "handled by the loop; no task sends it yet")]
    Log(LogLevel, String),
}

/// What woke the loop.
enum Wake {
    Command(Option<Envelope2>),
    Link(ConnectionEvent),
    Internal(Internal2),
    Timer(TimerKey2),
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
    /// The Local Authorization List.
    pub(super) local_list: super::local_list::LocalListPersist,
    /// The reservations.
    pub(super) reservations: super::reservation::ReservationState,
    pub(super) tariffs: super::tariff::TariffState,
    /// The charging profiles.
    pub(super) charging: super::smart_charging::SmartCharging,
    pub(super) reset: Option<ResetPlan>,
    pub(super) persistence: Persistence2,
    /// Answers armed by scenario `reply` steps (runtime only, see `scripted`).
    pub(super) scripted_replies: ReplyList,
    /// The session recording, when `--record` is on, and the connection count it stamps.
    recording: Option<(RecordSink, u64)>,
    /// Messages from building the actor, logged when the loop starts.
    startup_logs: Vec<(LogLevel, String)>,
}

impl Station2Actor {
    /// The OCPP 2.x version of this station (2.0.1 or 2.1).
    pub(super) fn protocol(&self) -> Protocol {
        wire_protocol(&self.config)
    }

    /// Builds the actor with its initial state.
    pub(crate) fn new(
        config: StationConfig,
        clock: Arc<dyn Clock>,
        commands: mpsc::Receiver<Envelope2>,
        events: broadcast::Sender<Event>,
        recording: Option<RecordSink>,
    ) -> (Self, watch::Receiver<Snapshot2>) {
        let (internal_tx, internal_rx) = mpsc::unbounded_channel();
        let mut startup_logs = Vec::new();
        let persistence = Persistence2::open(&config, &mut startup_logs);
        let state = Snapshot2::initial(&config);
        let (snapshot_tx, snapshot_rx) = watch::channel(state.clone());
        let link = Link2::new(wire_protocol(&config));
        let mut actor = Self {
            dm: DeviceModel::new(&config),
            config,
            clock,
            commands,
            snapshot_tx,
            events,
            state,
            timers: TimerWheel::new(),
            internal_tx,
            internal_rx,
            link,
            calls: Calls2::default(),
            evses: Vec::new(),
            tx: TxEngine::default(),
            queue: TxQueue::default(),
            auth: AuthState::default(),
            local_list: super::local_list::LocalListPersist::default(),
            reservations: super::reservation::ReservationState::default(),
            tariffs: super::tariff::TariffState::default(),
            charging: super::smart_charging::SmartCharging::default(),
            reset: None,
            persistence,
            scripted_replies: ReplyList::default(),
            recording: recording.map(|sink| (sink, 0)),
            startup_logs,
        };
        // The first snapshot is already the restored one.
        actor.init_evses();
        actor.restore_persisted();
        actor.state.heartbeat_interval_secs = actor.heartbeat_interval_secs();
        actor.state.security_profile =
            security_profile_of(&actor.config, actor.basic_auth_password().is_some());
        actor.dm.security_profile = actor.state.security_profile;
        actor.refresh_variables();
        // A04.FR.01: the start of the station is its boot; reported once the CSMS accepts it.
        actor.raise_security_event("StartupOfTheDevice");
        let _ = actor.snapshot_tx.send_replace(actor.state.clone());
        (actor, snapshot_rx)
    }

    /// The event loop. Ends on `Shutdown` or when every handle has been dropped.
    pub(crate) async fn run(mut self) {
        self.log(
            LogLevel::Info,
            format!(
                "station {} started ({} EVSE(s))",
                sanitize(&self.config.charge_point_id),
                self.state.evses.len()
            ),
        );
        for (level, text) in std::mem::take(&mut self.startup_logs) {
            self.log(level, text);
        }
        self.publish();

        self.settle_meters();
        loop {
            let wake = tokio::select! {
                envelope = self.commands.recv() => Wake::Command(envelope),
                event = next_link_event(&mut self.link.events) => Wake::Link(event),
                Some(internal) = self.internal_rx.recv() => Wake::Internal(internal),
                Some(result) = self.calls.results.next() => Wake::Internal(result),
                key = self.timers.next() => Wake::Timer(key),
            };
            // Before anything changes the state: energy of the time that just passed.
            // The limit timer settles the meters itself so it can detect a cap crossing.
            if !matches!(wake, Wake::Timer(TimerKey2::TxLimit(_))) {
                self.settle_meters();
            }
            match wake {
                Wake::Command(Some(Envelope2 {
                    command: Command2::Shutdown,
                    reply,
                })) => {
                    self.shutdown().await;
                    // The requester may have gone away; nothing to do about that.
                    let _ = reply.send(Ok(Outcome2::Done));
                    break;
                }
                Wake::Command(Some(Envelope2 { command, reply })) => {
                    self.record_command(&command);
                    self.execute(command, reply);
                }
                Wake::Command(None) => {
                    self.shutdown().await;
                    break;
                }
                Wake::Link(event) => self.on_connection_event(event),
                Wake::Internal(internal) => self.on_internal(internal),
                Wake::Timer(key) => self.on_timer(key),
            }
            // A handler may have changed the state without settling afterwards: record it
            // now, so the next span is integrated with the new state and not with the stale
            // one.
            self.settle_meters();
            self.publish();
        }
        self.log(LogLevel::Info, "station stopped".to_owned());
        self.publish();
    }

    fn on_internal(&mut self, internal: Internal2) {
        match internal {
            Internal2::Connected { generation, result } => {
                self.on_connect_result(generation, result);
            }
            Internal2::CallDone {
                token,
                generation,
                result,
            } => self.on_call_done(token, generation, result),
            Internal2::Saved(result) => self.on_saved(result),
            Internal2::Log(level, text) => self.log(level, text),
        }
    }

    /// Graceful stop: tear the link down (closing the WebSocket and waiting briefly for the
    /// handshake), fail what is in flight, write the state file.
    async fn shutdown(&mut self) {
        self.log(LogLevel::Info, "station shutting down".to_owned());
        self.link.wanted = false;
        self.timers.cancel_all();
        self.fail_pending_calls(|| CallError::Closed { code: None });
        if let Some(handle) = self.link.connecting.take() {
            handle.abort();
        }
        self.link.generation += 1;
        let connection = self.link.conn.take();
        let events = self.link.events.take();
        if let Some(connection) = connection {
            connection.close();
            if let Some(mut events) = events {
                let mut frames = Vec::new();
                let _ = tokio::time::timeout(CLOSE_GRACE, async {
                    while let Some(event) = events.recv().await {
                        match event {
                            ConnectionEvent::Frame(record) => frames.push(record),
                            ConnectionEvent::Closed { .. } => break,
                            _ => {}
                        }
                    }
                })
                .await;
                for record in frames {
                    self.emit_frame(record);
                }
            }
        }
        self.state.connection = ConnectionState::Disconnected;
        self.publish();
        self.finish_persistence().await;
    }

    /// Publishes the live model as the snapshot if it changed.
    pub(super) fn publish(&mut self) {
        self.refresh_variables();
        let state = &self.state;
        let changed = self.snapshot_tx.send_if_modified(|published| {
            if published == state {
                false
            } else {
                published.clone_from(state);
                true
            }
        });
        if changed {
            // `Err` only means nobody is subscribed.
            let _ = self.events.send(Event::StateChanged);
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    /// Publishes a frame; the session recording (when on) gets it first, redacted and never
    /// through the lossy event channel.
    pub(super) fn emit_frame(&mut self, record: FrameRecord) {
        let failure = self.recording.as_ref().and_then(|(sink, session)| {
            sink.frame(*session, &record);
            sink.take_failure()
        });
        self.emit(Event::Frame(record));
        self.log_recording_failure(failure);
    }

    /// Counts a new connection for the recording.
    pub(super) fn recording_new_session(&mut self) {
        if let Some((_, session)) = &mut self.recording {
            *session += 1;
        }
    }

    /// Records a command given to the station (secrets redacted by the recorder).
    fn record_command(&mut self, command: &Command2) {
        let failure = self.recording.as_ref().and_then(|(sink, session)| {
            if let Ok(value) = serde_json::to_value(command) {
                sink.command(*session, &value);
            }
            sink.take_failure()
        });
        self.log_recording_failure(failure);
    }

    fn log_recording_failure(&self, failure: Option<String>) {
        if let Some(reason) = failure {
            self.log(
                LogLevel::Error,
                format!("recording stopped, the file is incomplete: {reason}"),
            );
        }
    }

    /// Emits a log line as an [`Event::Log`].
    pub(super) fn log(&self, level: LogLevel, text: String) {
        match level {
            LogLevel::Error => tracing::error!("{text}"),
            LogLevel::Warn => tracing::warn!("{text}"),
            LogLevel::Info | LogLevel::Success => tracing::debug!("{text}"),
        }
        self.emit(Event::Log(LogRecord {
            ts: self.clock.now_utc(),
            level,
            text,
        }));
    }

    /// `true` once the CSMS accepted a `BootNotification` (and no disconnect or reset cleared
    /// the registration since).
    pub(super) fn registration_accepted(&self) -> bool {
        self.state.booted()
    }
}

/// Next event of the current connection; pending while there is none. A connection task
/// that vanished without a `Closed` event counts as closed.
async fn next_link_event(events: &mut Option<mpsc::Receiver<ConnectionEvent>>) -> ConnectionEvent {
    match events {
        Some(receiver) => receiver
            .recv()
            .await
            .unwrap_or_else(|| ConnectionEvent::Closed {
                code: None,
                reason: "connection task ended".to_owned(),
                by_us: false,
            }),
        None => std::future::pending().await,
    }
}
