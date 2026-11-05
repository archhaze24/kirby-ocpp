//! The station actor: the single task that owns all station state.
//!
//! Rules:
//!
//! * state is mutated **only** here, never behind a lock;
//! * the loop `select!`s over four sources: the command queue, the current connection's
//!   events, the actor's internal channel (call results, connect results, store-write
//!   results, log lines from call tasks) and the [`TimerWheel`];
//! * nothing in the loop awaits the network or the disk: connecting, CALLs and store
//!   writes run as spawned tasks that report back through the internal channel (see
//!   [`super::calls`]); the only awaited work is the graceful shutdown;
//! * the snapshot is published once per loop iteration, and only if it changed, which
//!   coalesces bursts of mutations.
//!
//! The behaviour is split over files: `link` (connect, reconnect, boot, heartbeat),
//! `calls` (outbound CALLs), `status` (StatusNotification discipline), `persistence`,
//! `commands` (user commands) and `handlers/` (CSMS-originated CALLs).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

use super::calls::PendingCall;
use super::command::{Command, Outcome};
use super::event::{Event, LogLevel, LogRecord};
use super::handle::Envelope;
use super::persistence::Persistence;
use super::snapshot::{ConnectorSnapshot, Snapshot};
use super::status::Trackers;
use super::timers::TimerWheel;
use crate::clock::Clock;
use crate::config::StationConfig;
use crate::error::{CallError, StationError};
use crate::ids::ConnectorId;
use crate::station::authorization::AuthorizationCache;
use crate::station::certificates::CertificateStore;
use crate::station::charging_profiles::ChargingProfileStore;
use crate::station::config_registry::ConfigRegistry;
use crate::station::local_auth_list::LocalAuthList;
use crate::station::offline::OfflineQueue;
use crate::station::security::PendingSecurityEvent;
use crate::store::{StationSecrets, StoreError};
use crate::ws::{ConnectError, Connection, ConnectionEvent, ConnectionState};

/// How long a graceful shutdown waits for the WebSocket close handshake.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// Names of the timers the actor uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TimerKey {
    /// Periodic `Heartbeat`.
    Heartbeat,
    /// Next reconnect attempt after a lost connection.
    Reconnect,
    /// Next `BootNotification` after `Pending` / `Rejected` / a failed attempt.
    BootRetry,
    /// `MinimumStatusDuration` debounce of one connector (0 = charge point).
    StatusDebounce(ConnectorId),
    /// Debounced write of the state file.
    PersistFlush,
    /// Periodic `MeterValues` of a connector with a running transaction.
    MeterPeriodic(ConnectorId),
    /// Clock-aligned `MeterValues`.
    MeterAligned,
    /// A reservation may have expired.
    ReservationExpiry(ConnectorId),
    /// Next simulated `GetDiagnostics` status.
    Diagnostics,
    /// Next simulated `UpdateFirmware` status.
    Firmware,
    /// Next simulated `GetLog` upload status.
    LogUpload,
    /// Next simulated `SignedUpdateFirmware` status.
    SignedFirmware,
    /// A hard `Reset` waits for in-flight CALLs, then reconnects.
    HardReset,
    /// Queued transaction messages the CSMS failed to process are tried again.
    SyncRetry,
}

/// What woke the actor loop.
enum Wake {
    Command(Option<Envelope>),
    Link(ConnectionEvent),
    Internal(Internal),
    Timer(TimerKey),
}

/// Messages from spawned tasks to the actor loop.
pub(crate) enum Internal {
    /// A connection attempt finished.
    Connected {
        generation: u64,
        result: Result<(Connection, mpsc::Receiver<ConnectionEvent>), ConnectError>,
    },
    /// A CALL finished (after all its attempts).
    CallDone {
        token: u64,
        generation: u64,
        result: Result<Value, CallError>,
    },
    /// A state file write finished.
    Saved(Result<(), StoreError>),
    /// A log line from a spawned task.
    Log(LogLevel, String),
}

/// The connection to the CSMS and everything about keeping it.
#[derive(Default)]
pub(crate) struct Link {
    pub(crate) conn: Option<Connection>,
    pub(crate) events: Option<mpsc::Receiver<ConnectionEvent>>,
    /// Bumped whenever the link is replaced or torn down; stale task results carry an old value.
    pub(crate) generation: u64,
    /// The task opening the connection, while one is being opened.
    pub(crate) connecting: Option<JoinHandle<()>>,
    /// `false` after `Disconnect` / before the first `Connect`: no automatic reconnects.
    pub(crate) wanted: bool,
    /// Reconnect attempts made since the last reset (boot `Accepted`, manual `Connect`).
    pub(crate) attempts: u32,
    /// A `BootNotification` CALL is in flight.
    pub(crate) boot_in_flight: bool,
}

/// State that persistence restores besides the snapshot.
pub(super) struct RestoredParts {
    pub(super) registry: ConfigRegistry,
    pub(super) local_list: LocalAuthList,
    pub(super) auth_cache: AuthorizationCache,
    pub(super) profiles: ChargingProfileStore,
    pub(super) queue: OfflineQueue,
    pub(super) certs: CertificateStore,
    pub(super) secrets: StationSecrets,
    pub(super) security_events: Vec<PendingSecurityEvent>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent dirty/pending flags of one actor"
)]
pub(crate) struct StationActor {
    pub(super) config: StationConfig,
    pub(super) clock: Arc<dyn Clock>,
    commands: mpsc::Receiver<Envelope>,
    snapshot_tx: watch::Sender<Snapshot>,
    events: broadcast::Sender<Event>,
    /// The live model; published as the snapshot.
    pub(super) state: Snapshot,
    pub(super) registry: ConfigRegistry,
    pub(super) local_list: LocalAuthList,
    pub(super) auth_cache: AuthorizationCache,
    pub(super) profiles: ChargingProfileStore,
    pub(super) queue: OfflineQueue,
    /// Installed root certificates and the station's own chain.
    pub(super) certs: CertificateStore,
    /// Secrets kept in their own file: authorization key, private keys.
    pub(super) secrets: StationSecrets,
    /// Security events that could not be sent yet (bounded).
    pub(super) security_events: Vec<PendingSecurityEvent>,
    /// The certificate summaries of the snapshot are out of date.
    pub(super) certs_dirty: bool,
    /// The next connection trusts the fallback Central System root (runtime only).
    pub(super) use_fallback_root: bool,
    /// `StartupOfTheDevice` is sent once per process, after the first accepted boot.
    pub(super) startup_event_pending: bool,
    /// The local list changed since `Snapshot::local_list` was built.
    pub(super) local_list_dirty: bool,
    /// Counter for station-local transaction ids and queue keys.
    pub(super) next_local: u64,
    /// Energy (Wh) after which a transaction with an invalidated idTag is stopped.
    pub(super) invalid_limits: HashMap<ConnectorId, u64>,
    /// Energy integration state per connector (see `power.rs`).
    pub(super) meters: HashMap<ConnectorId, super::power::MeterRuntime>,
    /// Offline syncs of queue entries still running.
    pub(super) sync_outstanding: usize,
    /// Queue keys not yet started by the running offline sync (entries run one at a time, in order).
    pub(super) sync_keys: VecDeque<u64>,
    /// The running sync follows an accepted boot (it ends in `finish_boot_accepted`).
    pub(super) sync_boot: bool,
    /// A queued message failed in the running sync: arm a retry when it ends.
    pub(super) sync_failed: bool,
    /// Rounds each queued message failed in this connection (not persisted).
    pub(super) sync_failures: HashMap<u64, u32>,
    /// Boot attempts left after a `Reset` (`ResetRetries`).
    pub(super) reset_retries: Option<u32>,
    /// A hard `Reset` reconnects at the latest at this instant (see `handlers/remote.rs`).
    pub(super) hard_reset_deadline: Option<tokio::time::Instant>,
    /// Simulated diagnostics/firmware steps still to report.
    pub(super) diagnostics_plan: VecDeque<(Duration, super::maintenance::Step)>,
    pub(super) firmware_plan: VecDeque<(Duration, super::maintenance::Step)>,
    /// Simulated `GetLog` upload statuses still to report (own queue: a log upload runs next to
    /// a firmware update), with the request id they belong to.
    pub(super) log_upload: VecDeque<(Duration, crate::ocpp::messages::UploadLogStatus)>,
    pub(super) log_request_id: Option<i32>,
    /// Simulated `SignedUpdateFirmware` statuses still to report, and their request id.
    pub(super) signed_fw: VecDeque<(Duration, crate::ocpp::messages::SignedFirmwareStatus)>,
    pub(super) signed_fw_request_id: Option<i32>,
    /// The last firmware update was a `SignedUpdateFirmware` (decides which notification an
    /// `ExtendedTriggerMessage` for firmware status sends).
    pub(super) last_firmware_signed: bool,
    pub(super) timers: TimerWheel<TimerKey>,
    pub(super) internal_tx: mpsc::UnboundedSender<Internal>,
    internal_rx: mpsc::UnboundedReceiver<Internal>,
    pub(super) link: Link,
    pub(super) calls: HashMap<u64, PendingCall>,
    pub(super) next_call_token: u64,
    /// Tokens of the timer-driven `MeterValues` in flight, oldest first (see `sampling.rs`).
    pub(super) sample_calls: std::collections::VecDeque<u64>,
    /// A sample was dropped since the last one that got through (one warning per burst).
    pub(super) sample_overflow_warned: bool,
    pub(super) trackers: Trackers,
    pub(super) persistence: Persistence,
    /// CSMS time minus our clock at the last accepted `BootNotification`; for logs only.
    pub(super) clock_offset: Option<chrono::Duration>,
    /// Log lines produced before `run` started (store loading).
    startup_logs: Vec<(LogLevel, String)>,
}

impl StationActor {
    /// Builds the actor, restoring persisted state synchronously (one small file read), so
    /// that the very first snapshot is already the restored one.
    pub(crate) fn new(
        config: StationConfig,
        clock: Arc<dyn Clock>,
        commands: mpsc::Receiver<Envelope>,
        events: broadcast::Sender<Event>,
    ) -> (Self, watch::Receiver<Snapshot>) {
        let (internal_tx, internal_rx) = mpsc::unbounded_channel();
        let clock_seed = clock.now_utc().timestamp_millis().max(1);
        let mut startup_logs = Vec::new();
        let mut state = Snapshot::initial(&config);
        let mut persistence = Persistence::open(&config, &mut startup_logs);
        let mut parts = RestoredParts {
            registry: ConfigRegistry::new(&config),
            local_list: LocalAuthList::default(),
            auth_cache: AuthorizationCache::default(),
            profiles: ChargingProfileStore::default(),
            queue: OfflineQueue::default(),
            certs: CertificateStore::default(),
            secrets: StationSecrets::default(),
            security_events: Vec::new(),
        };
        persistence.restore(&mut state, &mut parts, &mut startup_logs, clock.now_utc());
        // Profiles saved before store version 3 have no install time: count it from now.
        parts.profiles.backfill_installed_at(clock.now_utc());
        state.heartbeat_interval_secs = parts.registry.heartbeat_interval_secs();
        state.pending_offline_messages = parts.queue.pending_messages();
        let mut local_list_dirty = true;
        super::views::refresh(
            &mut state,
            &parts.registry,
            &parts.local_list,
            &parts.profiles,
            &mut local_list_dirty,
        );

        state.certificates = parts.certs.summaries();
        state.security_profile = parts.registry.security_profile();
        state.pending_security_events =
            u32::try_from(parts.security_events.len()).unwrap_or(u32::MAX);
        state.certificate_request_pending = parts.secrets.pending_key_pem.is_some();
        let queue_high_water = parts.queue.keys().into_iter().max().unwrap_or(0);
        let (snapshot_tx, snapshot_rx) = watch::channel(state.clone());
        let actor = Self {
            config,
            clock,
            commands,
            snapshot_tx,
            events,
            state,
            registry: parts.registry,
            local_list: parts.local_list,
            auth_cache: parts.auth_cache,
            profiles: parts.profiles,
            queue: parts.queue,
            certs: parts.certs,
            secrets: parts.secrets,
            security_events: parts.security_events,
            certs_dirty: false,
            use_fallback_root: false,
            startup_event_pending: true,
            local_list_dirty,
            // Never reuse a key still present in the restored queue (the wall clock may jump back).
            next_local: u64::try_from(clock_seed).unwrap_or(1).max(queue_high_water),
            invalid_limits: HashMap::new(),
            meters: HashMap::new(),
            sync_outstanding: 0,
            sync_keys: VecDeque::new(),
            sync_boot: false,
            sync_failed: false,
            sync_failures: HashMap::new(),
            reset_retries: None,
            hard_reset_deadline: None,
            diagnostics_plan: VecDeque::new(),
            firmware_plan: VecDeque::new(),
            log_upload: VecDeque::new(),
            log_request_id: None,
            signed_fw: VecDeque::new(),
            signed_fw_request_id: None,
            last_firmware_signed: false,
            timers: TimerWheel::new(),
            internal_tx,
            internal_rx,
            link: Link::default(),
            calls: HashMap::new(),
            next_call_token: 0,
            sample_calls: std::collections::VecDeque::new(),
            sample_overflow_warned: false,
            trackers: BTreeMap::new(),
            persistence,
            clock_offset: None,
            startup_logs,
        };
        (actor, snapshot_rx)
    }

    pub(crate) async fn run(mut self) {
        let startup_logs = std::mem::take(&mut self.startup_logs);
        self.log(
            LogLevel::Info,
            format!(
                "station {} started ({} connector(s))",
                self.config.charge_point_id,
                self.state.connectors.len()
            ),
        );
        for (level, text) in startup_logs {
            self.log(level, text);
        }
        self.after_restore();
        self.publish();

        self.settle_meters();
        loop {
            let wake = tokio::select! {
                envelope = self.commands.recv() => Wake::Command(envelope),
                event = next_link_event(&mut self.link.events) => Wake::Link(event),
                Some(internal) = self.internal_rx.recv() => Wake::Internal(internal),
                key = self.timers.next() => Wake::Timer(key),
            };
            // Before anything changes the state: energy of the time that just passed.
            self.settle_meters();
            match wake {
                Wake::Command(Some(Envelope {
                    command: Command::Shutdown,
                    reply,
                })) => {
                    self.shutdown().await;
                    // The requester may have gone away; nothing to do about that.
                    let _ = reply.send(Ok(Outcome::Done));
                    break;
                }
                Wake::Command(Some(Envelope { command, reply })) => self.execute(command, reply),
                Wake::Command(None) => {
                    self.shutdown().await;
                    break;
                }
                Wake::Link(event) => self.on_connection_event(event),
                Wake::Internal(internal) => self.on_internal(internal),
                Wake::Timer(key) => self.on_timer(key),
            }
            // A handler may have changed a connector's status without settling afterwards
            // (unplug, plug-in resume, manual status, fault clear): record the new status now,
            // so the next span is integrated with it and not with the stale one.
            self.settle_meters();
            self.publish();
        }
        self.log(LogLevel::Info, "station stopped".to_owned());
        self.publish();
    }

    fn on_internal(&mut self, internal: Internal) {
        match internal {
            Internal::Connected { generation, result } => {
                self.on_connect_result(generation, result);
            }
            Internal::CallDone {
                token,
                generation,
                result,
            } => self.on_call_done(token, generation, result),
            Internal::Saved(result) => self.on_saved(result),
            Internal::Log(level, text) => self.log(level, text),
        }
    }

    fn on_timer(&mut self, key: TimerKey) {
        match key {
            TimerKey::Heartbeat => self.on_heartbeat_timer(),
            TimerKey::Reconnect => self.on_reconnect_timer(),
            TimerKey::BootRetry => self.on_boot_retry_timer(),
            TimerKey::StatusDebounce(id) => self.status_timer_fired(id),
            TimerKey::PersistFlush => self.flush_persistence(),
            TimerKey::MeterPeriodic(id) => self.on_meter_periodic(id),
            TimerKey::MeterAligned => self.on_meter_aligned(),
            TimerKey::ReservationExpiry(id) => self.on_reservation_timer(id),
            TimerKey::Diagnostics => self.on_diagnostics_timer(),
            TimerKey::Firmware => self.on_firmware_timer(),
            TimerKey::LogUpload => self.on_log_timer(),
            TimerKey::SignedFirmware => self.on_signed_firmware_timer(),
            TimerKey::HardReset => self.on_hard_reset_timer(),
            TimerKey::SyncRetry => self.on_sync_retry_timer(),
        }
    }

    /// Graceful stop: tear the link down (closing the WebSocket and waiting briefly for the
    /// handshake), fail what is in flight, write the state file.
    async fn shutdown(&mut self) {
        self.log(LogLevel::Info, "station shutting down".to_owned());
        self.flush_meters();
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
                    self.emit(Event::Frame(record));
                }
            }
        }
        self.state.connection = ConnectionState::Disconnected;
        self.state.registration = None;
        self.publish();
        self.finish_persistence().await;
    }

    /// Publishes the snapshot if it changed since the last publication.
    pub(super) fn publish(&mut self) {
        self.state.pending_offline_messages = self.queue.pending_messages();
        self.state.security_profile = self.registry.security_profile();
        self.state.pending_security_events =
            u32::try_from(self.security_events.len()).unwrap_or(u32::MAX);
        self.state.certificate_request_pending = self.secrets.pending_key_pem.is_some();
        if std::mem::take(&mut self.certs_dirty) {
            self.state.certificates = self.certs.summaries();
        }
        super::views::refresh(
            &mut self.state,
            &self.registry,
            &self.local_list,
            &self.profiles,
            &mut self.local_list_dirty,
        );
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

    pub(super) fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

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

    // ---- connector lookup ------------------------------------------------------

    /// A connector by id; `0` is the charge point.
    pub(super) fn find_connector(&self, id: ConnectorId) -> Option<&ConnectorSnapshot> {
        self.state.connector(id)
    }

    pub(super) fn find_connector_mut(&mut self, id: ConnectorId) -> Option<&mut ConnectorSnapshot> {
        if id.is_station() {
            return Some(&mut self.state.charge_point);
        }
        self.state.connectors.iter_mut().find(|c| c.id == id)
    }

    /// A physical connector (`1..=n`).
    pub(super) fn connector(&self, id: ConnectorId) -> Result<&ConnectorSnapshot, StationError> {
        if id.is_station() {
            return Err(StationError::UnknownConnector(id));
        }
        self.state
            .connector(id)
            .ok_or(StationError::UnknownConnector(id))
    }

    /// Connector `id` or the charge point (`0`).
    pub(super) fn connector_or_station_mut(
        &mut self,
        id: ConnectorId,
    ) -> Result<&mut ConnectorSnapshot, StationError> {
        self.find_connector_mut(id)
            .ok_or(StationError::UnknownConnector(id))
    }
}

/// Next event of the current connection; pending while there is none. A connection
/// task that vanished without a `Closed` event counts as closed.
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
