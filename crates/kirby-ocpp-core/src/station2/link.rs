//! The link to the CSMS: connect, reconnect, boot, registration and heartbeat.
//!
//! # Rules
//!
//! * `Connect` opens the connection in a spawned task (`Connection::connect` can take up to
//!   the call timeout); the result comes back as `Internal2::Connected`.
//! * Anything that ends the link unexpectedly (a failed attempt, a `Closed` event) schedules
//!   a reconnect after `min(initial * 2^(n-1), max)` while `wanted` and attempts are left
//!   (`max_attempts == 0` is unlimited). A failed attempt counts like a lost connection. The
//!   attempt counter resets when the link is registered and on a manual `Connect` /
//!   `Reconnect`. When the attempts end, `reconnect_stopped` says why; the next `Connect`
//!   clears it.
//! * `Disconnect` and shutdown cancel reconnect, boot retry and heartbeat and fail calls in
//!   flight. A `Closed` event of a link the station closed itself never reaches the actor
//!   (the link is dropped first), so it can never trigger a backoff. Frames the link had
//!   already buffered are published first, up to a bound. A `Disconnect` also drops a reset
//!   that is in its `Rebooting` phase, so the next boot is a power-up.
//! * Every task result carries the link generation; results of an older link are ignored.
//!
//! # Registration (B01 to B04)
//!
//! * The registration survives a lost link: a reconnect with an `Accepted` registration does
//!   **not** boot again. It re-sends the connector statuses (all of them when the offline
//!   period exceeded `OfflineThreshold`, otherwise those that changed), delivers the
//!   transaction queue and sends a `Heartbeat`. Only `Disconnect` and a simulated reset clear
//!   the registration, so the next connect boots.
//! * A new `BasicAuthPassword` (URL without a password) closes the link once the answer is
//!   queued and reconnects with it right away, on the same path: no boot, queue kept (A01).
//! * A reconnect with a `Pending`, `Rejected` or missing registration boots again.
//! * The reason of a boot is the reason of the reset in progress, else the reason of the last
//!   boot that was not accepted (the retries repeat it), else `PowerUp`. `Command2::Boot`
//!   sends `Unknown`, a boot asked for by `TriggerMessage` sends `Triggered`.
//! * `Accepted`: the interval of the answer replaces `HeartbeatInterval` when positive
//!   (B01.FR.04), the CSMS clock offset is logged (the system clock is never set), then the
//!   statuses of all connectors go out, the transaction queue is delivered and the heartbeat
//!   timer is armed, in this order. The heartbeat period is fixed; it does not restart when
//!   other messages are sent.
//! * `Pending` / `Rejected`: no heartbeat; `BootNotification` is repeated after the interval
//!   of the answer, or after `max(1 s, HeartbeatInterval)` when that is 0, never earlier
//!   unless the CSMS triggers it (B02.FR.04). The channel stays open (B02.FR.06). While
//!   `Rejected` every CALL of the CSMS is answered with `SecurityError` (B03.FR.08).
//! * A failed `BootNotification` CALL (timeout, CALLERROR) is repeated after the same period.

use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::actor::{Internal2, Station2Actor, TimerKey2};
use super::calls::call_failure;
use super::command::Outcome2;
use super::reset::ResetPhase;
use super::security_events::PendingSecurityEvent;
use super::types::{Origin2, Reply2};
use crate::config::StationConfig;
use crate::error::CallError;
use crate::ocpp::Protocol;
use crate::ocpp::frame::ErrorCode;
use crate::ocpp::responder::ReplyHandle;
use crate::ocpp::v2::messages::{
    BootNotification, BootNotificationRequest, BootNotificationResponse, BootReason, Heartbeat,
    HeartbeatRequest, RegistrationStatus,
};
use crate::ocpp::v2::types::{ChargingStation, WireEnum};
use crate::station::LogLevel;
use crate::ws::{
    ConnectError, ConnectOptions, Connection, ConnectionEvent, ConnectionState, SchemaSet,
};

/// Smallest delay between two boot attempts or heartbeats the station ever schedules.
const MIN_PERIOD: Duration = Duration::from_secs(1);

/// Most buffered events taken from a link that is being dropped. The connection task buffers
/// at most `ws::EVENT_CAPACITY` (64); the bound also covers a task that keeps emitting while
/// the actor drains.
const DRAIN_LIMIT: usize = 256;

/// The connection to the CSMS and everything about keeping it.
pub(crate) struct Link2 {
    pub(super) conn: Option<Connection>,
    pub(super) events: Option<mpsc::Receiver<ConnectionEvent>>,
    /// Bumped whenever the link is replaced or torn down; stale task results carry an old value.
    pub(super) generation: u64,
    /// The task opening the connection, while one is being opened.
    pub(super) connecting: Option<JoinHandle<()>>,
    /// `false` after `Disconnect` and before the first `Connect`: no automatic reconnects.
    pub(super) wanted: bool,
    /// Reconnect attempts made since the last reset (registered link, manual `Connect`).
    attempts: u32,
    /// A `BootNotification` CALL is in flight.
    boot_in_flight: bool,
    /// A station-originated `Heartbeat` CALL is in flight; the timer skips its period.
    heartbeat_in_flight: bool,
    /// The 2.0.1 schemas the connection validates against; built once.
    schemas: SchemaSet,
    /// When the link was lost; cleared once the statuses were re-sent.
    offline_since: Option<DateTime<Utc>>,
    /// Reason of the last boot that was not accepted; the retry repeats it.
    retry_reason: Option<BootReason>,
    /// Security events raised by the station that the CSMS has not received yet.
    pub(super) security_outbox: Vec<PendingSecurityEvent>,
}

impl Link2 {
    /// A link that is not wanted yet, with the 2.0.1 schema set.
    pub(super) fn new() -> Self {
        Self {
            conn: None,
            events: None,
            generation: 0,
            connecting: None,
            wanted: false,
            attempts: 0,
            boot_in_flight: false,
            heartbeat_in_flight: false,
            schemas: SchemaSet::for_protocol(Protocol::V201)
                .expect("bundled OCPP 2.0.1 schemas compile"),
            offline_since: None,
            retry_reason: None,
            security_outbox: Vec::new(),
        }
    }
}

/// `SecurityProfile` of a station started with `config`: 3 with a client certificate on
/// `wss`, 2 with a password on `wss`, 1 with a password on `ws`, else 0.
pub(super) fn security_profile_of(config: &StationConfig, device_model_password: bool) -> u8 {
    let Ok(url) = url::Url::parse(&config.url) else {
        return 0;
    };
    let secure = url.scheme() == "wss";
    let has_password = url.password().is_some() || device_model_password;
    match (secure, has_password) {
        (true, _) if config.tls.cert_file.is_some() => 3,
        (true, true) => 2,
        (false, true) => 1,
        _ => 0,
    }
}

/// `url` with the Basic Auth credentials of the device model in its userinfo, or `None` when
/// the URL carries a password already (the URL wins) or cannot be parsed. Id and password
/// are percent-escaped byte by byte: the handshake decodes the userinfo, so the UTF-8 bytes
/// reach the `Authorization` header as they are (A00.FR.205).
fn with_basic_credentials(url: &str, charge_point_id: &str, password: &str) -> Option<String> {
    let mut parsed = url::Url::parse(url).ok()?;
    if parsed.password().is_some() {
        return None;
    }
    parsed
        .set_username(&userinfo_escape(charge_point_id))
        .ok()?;
    parsed.set_password(Some(&userinfo_escape(password))).ok()?;
    Some(parsed.to_string())
}

/// `text` percent-encoded so that nothing but ASCII letters and digits is left as is: the
/// `url` crate itself leaves `%` unescaped in a userinfo.
fn userinfo_escape(text: &str) -> String {
    percent_encoding::percent_encode(text.as_bytes(), percent_encoding::NON_ALPHANUMERIC)
        .to_string()
}

/// The CSMS clock against the station clock, for logs. The system clock is never set.
fn describe_offset(offset: chrono::Duration) -> String {
    let direction = if offset.num_milliseconds() >= 0 {
        "ahead"
    } else {
        "behind"
    };
    format!(
        "CSMS clock is {}ms {direction}",
        offset.num_milliseconds().abs()
    )
}

/// Text from the network or from a station id, safe to print.
pub(super) fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if crate::text::is_unsafe_char(c) {
                ' '
            } else {
                c
            }
        })
        .collect()
}

impl Station2Actor {
    fn connect_options(&self) -> ConnectOptions {
        ConnectOptions {
            ping_interval: self.ws_ping_interval(),
            call_timeout: self.config.call_timeout(),
            serialize_calls: true,
            show_secrets: self.config.show_secrets,
            write_timeout: crate::ws::DEFAULT_WRITE_TIMEOUT,
            schemas: self.link.schemas.clone(),
        }
    }

    /// `Command2::Connect`.
    pub(super) fn cmd_connect(&mut self, reply: Reply2) {
        self.connect();
        let _ = reply.send(Ok(Outcome2::Done));
    }

    /// `Command2::Disconnect`.
    pub(super) fn cmd_disconnect(&mut self, reply: Reply2) {
        // A reboot that was cut short: the next connect is a power-up, not the end of the reset.
        if self
            .reset
            .as_ref()
            .is_some_and(|plan| plan.phase == ResetPhase::Rebooting)
        {
            self.reset = None;
        }
        self.disconnect();
        let _ = reply.send(Ok(Outcome2::Done));
    }

    /// `Command2::Reconnect`.
    pub(super) fn cmd_reconnect(&mut self, reply: Reply2) {
        self.disconnect();
        self.connect();
        let _ = reply.send(Ok(Outcome2::Done));
    }

    /// `Command2::Boot`.
    pub(super) fn cmd_boot(&mut self, reply: Reply2) {
        if self.link.conn.is_none() {
            let _ = reply.send(Err(crate::error::StationError::NotConnected));
        } else {
            self.send_boot(BootReason::Unknown, Some(reply));
        }
    }

    /// `Command2::Heartbeat`.
    pub(super) fn cmd_heartbeat(&mut self, reply: Reply2) {
        self.send_heartbeat(Origin2::Station, Some(reply));
    }

    /// Opens the connection unless one is open or being opened; resets the backoff.
    fn connect(&mut self) {
        self.link.wanted = true;
        self.link.attempts = 0;
        self.timers.cancel(&TimerKey2::Reconnect);
        if self.link.conn.is_some() || self.link.connecting.is_some() {
            return;
        }
        self.start_connect();
    }

    /// Closes the connection and stops all automatic behavior; the next connect boots.
    fn disconnect(&mut self) {
        self.link.wanted = false;
        self.link.attempts = 0;
        self.state.reconnect_stopped = None;
        self.timers.cancel(&TimerKey2::Reconnect);
        if let Some(handle) = self.link.connecting.take() {
            handle.abort();
        }
        let connection = self.link.conn.clone();
        let mut events = self.link.events.take();
        self.drain_buffered_events(events.as_mut());
        self.link_down(&CallError::Closed { code: None });
        close_in_background(connection, events);
        self.state.connection = ConnectionState::Disconnected;
        self.state.registration = None;
        self.link.offline_since = None;
        self.log(LogLevel::Info, "disconnected".to_owned());
    }

    /// A simulated reboot: closes the connection and opens a new one right away (even with
    /// automatic reconnect disabled). The registration is cleared, so the new link boots.
    pub(super) fn reconnect_for_reset(&mut self) {
        let connection = self.link.conn.clone();
        let mut events = self.link.events.take();
        self.drain_buffered_events(events.as_mut());
        self.link_down(&CallError::Closed { code: None });
        close_in_background(connection, events);
        self.state.registration = None;
        self.link.offline_since = None;
        self.link.wanted = true;
        self.link.attempts = 0;
        self.timers.cancel(&TimerKey2::Reconnect);
        if let Some(handle) = self.link.connecting.take() {
            handle.abort();
        }
        self.start_connect();
    }

    /// `BasicAuthPassword` changed while the link is up and the URL carries no password (the
    /// URL wins): the answer to the CSMS is queued, so the next step is to drop the link and
    /// open it again with the new password. The registration stays, so there is no boot.
    pub(super) fn schedule_password_reconnect(&mut self) {
        let url_has_password =
            url::Url::parse(&self.config.url).map_or(true, |url| url.password().is_some());
        if url_has_password || self.link.conn.is_none() {
            return;
        }
        // The reconnect timer is otherwise armed only while no link is up, so a timer that
        // fires with a link up is this one. It fires after the current handler returned, so
        // the answer is queued before the close.
        self.timers.set(TimerKey2::Reconnect, Duration::ZERO);
    }

    fn reconnect_with_new_password(&mut self) {
        self.log(
            LogLevel::Info,
            "BasicAuthPassword changed; reconnecting with it".to_owned(),
        );
        let connection = self.link.conn.clone();
        let mut events = self.link.events.take();
        self.drain_buffered_events(events.as_mut());
        self.finish_answered_calls();
        self.link_down(&CallError::Closed { code: None });
        close_in_background(connection, events);
        self.link.attempts = 0;
        if self.link.wanted && self.link.connecting.is_none() {
            self.start_connect();
        }
    }

    fn start_connect(&mut self) {
        self.state.reconnect_stopped = None;
        self.link.generation += 1;
        let generation = self.link.generation;
        self.state.connection = ConnectionState::Connecting;
        self.log(
            LogLevel::Info,
            format!("connecting to {}", redact_url(&self.config.url)),
        );
        let mut config = self.config.clone();
        // The URL wins; else the password of the device model, applied at the next connect.
        if let Some(password) = self.basic_auth_password()
            && let Some(url) =
                with_basic_credentials(&config.url, &config.charge_point_id, password)
        {
            config.url = url;
        }
        self.state.security_profile =
            security_profile_of(&self.config, self.basic_auth_password().is_some());
        self.dm.security_profile = self.state.security_profile;
        let internal = self.internal_tx.clone();
        let options = self.connect_options();
        self.link.connecting = Some(tokio::spawn(async move {
            let result = Connection::connect(
                &config.url,
                &config.charge_point_id,
                &config.tls,
                &config.ws_subprotocol,
                options,
            )
            .await;
            let _ = internal.send(Internal2::Connected { generation, result });
        }));
    }

    pub(super) fn on_connect_result(
        &mut self,
        generation: u64,
        result: Result<(Connection, mpsc::Receiver<ConnectionEvent>), ConnectError>,
    ) {
        if generation != self.link.generation {
            // A superseded attempt: dropping the connection closes it.
            return;
        }
        self.link.connecting = None;
        match result {
            Ok((connection, events)) => {
                self.link.conn = Some(connection);
                self.link.events = Some(events);
                self.recording_new_session();
                self.state.connection = ConnectionState::Connected;
                self.log(LogLevel::Success, "connected".to_owned());
                self.on_connection_established();
            }
            Err(error) => {
                self.log(
                    LogLevel::Error,
                    format!("connection failed: {}", sanitize(&error.to_string())),
                );
                self.state.connection = ConnectionState::Disconnected;
                self.schedule_reconnect();
            }
        }
    }

    /// The link is up: boots, or resumes with the registration of before the loss (B04).
    fn on_connection_established(&mut self) {
        let rebooting = self
            .reset
            .as_ref()
            .is_some_and(|plan| plan.phase == ResetPhase::Rebooting);
        if self.registration_accepted() && !rebooting {
            self.link.attempts = 0;
            self.resume_after_reconnect();
            return;
        }
        if rebooting && let Some(plan) = self.reset.take() {
            // The reboot is over once its boot goes out; retries keep the reason.
            self.link.retry_reason = Some(plan.reason);
            self.raise_security_event("ResetOrReboot");
            self.log(LogLevel::Info, "reset finished".to_owned());
        }
        let reason = self.link.retry_reason.unwrap_or(BootReason::PowerUp);
        self.send_boot(reason, None);
    }

    /// B04: statuses (all after a long outage, else the changed ones), the transaction
    /// queue, then the heartbeat.
    fn resume_after_reconnect(&mut self) {
        let since = self.link.offline_since.take().filter(|start| {
            let threshold = chrono::Duration::from_std(self.offline_threshold())
                .unwrap_or(chrono::Duration::MAX);
            self.clock.now_utc() - *start <= threshold
        });
        self.send_all_statuses(since);
        self.flush_queue();
        self.flush_security_events();
        self.send_heartbeat(Origin2::Station, None);
        self.arm_heartbeat();
    }

    pub(super) fn on_connection_event(&mut self, event: ConnectionEvent) {
        match event {
            ConnectionEvent::Frame(record) => self.emit_frame(record),
            ConnectionEvent::Call(call) => self.dispatch_incoming(call),
            ConnectionEvent::ProtocolWarning(text) => {
                self.log(LogLevel::Warn, format!("protocol: {}", sanitize(&text)));
            }
            ConnectionEvent::Closed {
                code,
                reason,
                by_us,
            } => {
                self.finish_answered_calls();
                let shown = code.map_or_else(String::new, |c| format!(" ({c})"));
                let why = if reason.is_empty() {
                    String::new()
                } else {
                    format!(": {}", sanitize(&reason))
                };
                let who = if by_us { " by the station" } else { "" };
                self.log(LogLevel::Warn, format!("disconnected{who}{shown}{why}"));
                self.link.events = None;
                self.link_down(&CallError::Closed { code });
                self.state.connection = ConnectionState::Disconnected;
                // Every close that reaches this point is unexpected (our own closes drop the
                // link first); a watchdog termination reports `by_us` as well.
                self.schedule_reconnect();
            }
        }
    }

    /// Everything that must stop when the connection is gone, whatever the reason. The
    /// registration stays: it decides whether the next link boots.
    pub(super) fn link_down(&mut self, error: &CallError) {
        if self.link.conn.is_some() && self.link.offline_since.is_none() {
            self.link.offline_since = Some(self.clock.now_utc());
        }
        self.link.generation += 1;
        self.link.conn = None;
        self.link.boot_in_flight = false;
        self.link.heartbeat_in_flight = false;
        self.timers.cancel(&TimerKey2::Heartbeat);
        self.timers.cancel(&TimerKey2::BootRetry);
        self.queue_mark_offline();
        let code = match error {
            CallError::Closed { code } => *code,
            _ => None,
        };
        self.fail_pending_calls(|| CallError::Closed { code });
        self.state.connection = ConnectionState::Disconnected;
    }

    /// Arms the next reconnect attempt, or gives up when the attempts are used up.
    fn schedule_reconnect(&mut self) {
        if !self.link.wanted {
            return;
        }
        let attempt = self.link.attempts + 1;
        let policy = self.config.reconnect.clone();
        if !policy.allows_attempt(attempt) {
            let reason = if policy.enabled {
                format!("after {} failed attempt(s)", self.link.attempts)
            } else {
                "automatic reconnect is disabled".to_owned()
            };
            self.log(LogLevel::Error, format!("reconnect stopped: {reason}"));
            self.state.connection = ConnectionState::Disconnected;
            self.state.reconnect_stopped = Some(reason);
            return;
        }
        self.link.attempts = attempt;
        let delay = policy.delay_for_attempt(attempt);
        let limit = if policy.max_attempts > 0 {
            format!("/{}", policy.max_attempts)
        } else {
            String::new()
        };
        self.log(
            LogLevel::Warn,
            format!(
                "reconnect attempt {attempt}{limit} in {}ms",
                delay.as_millis()
            ),
        );
        self.state.connection = ConnectionState::Backoff { attempt };
        self.timers.set(TimerKey2::Reconnect, delay);
    }

    /// The reconnect delay elapsed.
    pub(super) fn on_reconnect_timer(&mut self) {
        if self.link.conn.is_some() {
            self.reconnect_with_new_password();
            return;
        }
        if !self.link.wanted || self.link.conn.is_some() || self.link.connecting.is_some() {
            return;
        }
        self.start_connect();
    }

    // ---- BootNotification ------------------------------------------------------

    /// Sends a `BootNotification` with `reason`; the answer goes to `reply` when given.
    /// Without a reply a boot already in flight is not duplicated.
    pub(super) fn send_boot(&mut self, reason: BootReason, reply: Option<Reply2>) {
        if reply.is_none() && self.link.boot_in_flight {
            return;
        }
        self.timers.cancel(&TimerKey2::BootRetry);
        self.link.boot_in_flight = true;
        self.state.boot_reason = Some(reason);
        let request = BootNotificationRequest {
            charging_station: ChargingStation {
                serial_number: self.config.serial_number.clone(),
                model: self.config.model.clone(),
                modem: None,
                vendor_name: self.config.vendor.clone(),
                firmware_version: self.config.firmware_version.clone(),
                custom_data: None,
            },
            reason,
            custom_data: None,
        };
        let generation = self.link.generation;
        self.call::<BootNotification>(Origin2::Station, &request, move |actor, result| {
            if actor.link.generation == generation {
                actor.link.boot_in_flight = false;
            }
            actor.on_boot_result(result, reason, reply);
        });
    }

    fn on_boot_result(
        &mut self,
        result: Result<BootNotificationResponse, CallError>,
        reason: BootReason,
        reply: Option<Reply2>,
    ) {
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                if self.link.conn.is_some() && self.link.wanted && !self.registration_accepted() {
                    let delay = self.retry_period();
                    self.log(
                        LogLevel::Warn,
                        format!(
                            "BootNotification failed ({error}); retrying in {}s",
                            delay.as_secs()
                        ),
                    );
                    self.link.retry_reason = Some(reason);
                    self.timers.set(TimerKey2::BootRetry, delay);
                }
                if let Some(reply) = reply {
                    let _ = reply.send(Err(call_failure(error)));
                }
                return;
            }
        };

        self.state.registration = Some(response.status);
        match response.status {
            RegistrationStatus::Accepted => self.on_boot_accepted(&response),
            RegistrationStatus::Pending | RegistrationStatus::Rejected => {
                self.timers.cancel(&TimerKey2::Heartbeat);
                self.link.retry_reason = Some(reason);
                let delay = u64::try_from(response.interval)
                    .ok()
                    .filter(|secs| *secs > 0)
                    .map_or_else(|| self.retry_period(), Duration::from_secs)
                    .max(MIN_PERIOD);
                self.log(
                    LogLevel::Warn,
                    format!(
                        "BootNotification was {}; retrying in {}s",
                        response.status.as_str(),
                        delay.as_secs()
                    ),
                );
                if self.link.conn.is_some() && self.link.wanted {
                    self.timers.set(TimerKey2::BootRetry, delay);
                }
            }
        }
        if let Some(reply) = reply {
            let payload = serde_json::to_value(&response).unwrap_or_default();
            let _ = reply.send(Ok(Outcome2::Response { payload }));
        }
    }

    fn on_boot_accepted(&mut self, response: &BootNotificationResponse) {
        let offset = response.current_time - self.clock.now_utc();
        self.link.attempts = 0;
        self.link.retry_reason = None;
        self.link.offline_since = None;
        self.timers.cancel(&TimerKey2::BootRetry);
        if let Ok(interval) = u32::try_from(response.interval)
            && interval > 0
        {
            // B01.FR.04: the interval of the answer becomes `HeartbeatInterval`.
            self.adopt_heartbeat_interval(interval);
        }
        self.state.heartbeat_interval_secs = self.heartbeat_secs();
        self.log(
            LogLevel::Success,
            format!(
                "BootNotification accepted; heartbeat every {}s; {}",
                self.state.heartbeat_interval_secs,
                describe_offset(offset)
            ),
        );
        // B01.FR.05: the statuses first, then the queued transaction events, then the
        // heartbeat timer.
        self.send_all_statuses(None);
        self.flush_queue();
        // A04.FR.01: the startup or reboot of the station is a critical security event.
        self.flush_security_events();
        self.arm_heartbeat();
    }

    /// The boot retry delay elapsed.
    pub(super) fn on_boot_retry_timer(&mut self) {
        if self.link.conn.is_some() && !self.registration_accepted() {
            let reason = self.link.retry_reason.unwrap_or(BootReason::PowerUp);
            self.send_boot(reason, None);
        }
    }

    /// Period to wait after a boot problem when the CSMS gave none: `HeartbeatInterval`, at
    /// least a second.
    fn retry_period(&self) -> Duration {
        Duration::from_secs(u64::from(self.heartbeat_secs())).max(MIN_PERIOD)
    }

    // ---- Heartbeat -------------------------------------------------------------

    /// The effective `HeartbeatInterval`: the device model's, else the configured fallback.
    fn heartbeat_secs(&self) -> u32 {
        match self.heartbeat_interval_secs() {
            0 => self.config.heartbeat_interval_secs,
            secs => secs,
        }
    }

    /// (Re)arms the periodic heartbeat (never below one second).
    fn arm_heartbeat(&mut self) {
        let secs = self.heartbeat_secs();
        self.state.heartbeat_interval_secs = secs;
        self.timers.set(
            TimerKey2::Heartbeat,
            Duration::from_secs(u64::from(secs)).max(MIN_PERIOD),
        );
    }

    /// The heartbeat period elapsed.
    pub(super) fn on_heartbeat_timer(&mut self) {
        if !self.registration_accepted() {
            return;
        }
        self.arm_heartbeat();
        // The connection sends one CALL at a time: another heartbeat would only queue up
        // behind the unanswered one.
        if self.link.heartbeat_in_flight {
            return;
        }
        self.send_heartbeat(Origin2::Station, None);
    }

    /// Sends `Heartbeat`; `reply` is answered with the CSMS's response.
    pub(super) fn send_heartbeat(&mut self, origin: Origin2, reply: Option<Reply2>) {
        let tracked = origin == Origin2::Station && reply.is_none();
        if tracked {
            self.link.heartbeat_in_flight = true;
        }
        self.call::<Heartbeat>(
            origin,
            &HeartbeatRequest { custom_data: None },
            move |actor, result| {
                if tracked {
                    actor.link.heartbeat_in_flight = false;
                }
                match result {
                    Ok(response) => {
                        let now = actor.clock.now_utc();
                        actor.state.last_heartbeat_at = Some(now);
                        actor.log(
                            LogLevel::Success,
                            format!(
                                "Heartbeat accepted, CSMS time: {}; {}",
                                response.current_time,
                                describe_offset(response.current_time - now)
                            ),
                        );
                        if let Some(reply) = reply {
                            let payload = serde_json::to_value(&response).unwrap_or_default();
                            let _ = reply.send(Ok(Outcome2::Response { payload }));
                        }
                    }
                    Err(error) => {
                        if let Some(reply) = reply {
                            let _ = reply.send(Err(call_failure(error)));
                        }
                    }
                }
            },
        );
    }

    /// Registration gate for an incoming CALL: hands the reply back when the call may be
    /// dispatched, or answers it and returns `None`. While the CSMS has rejected the boot it
    /// may not send anything (B03.FR.08).
    pub(super) fn gate_incoming(
        &mut self,
        action: &str,
        reply: ReplyHandle,
    ) -> Option<ReplyHandle> {
        if self.state.registration != Some(RegistrationStatus::Rejected) {
            return Some(reply);
        }
        self.log(
            LogLevel::Warn,
            format!("{action} refused: the registration was rejected"),
        );
        let _ = reply.reject(
            ErrorCode::SecurityError,
            "the BootNotification was rejected",
        );
        None
    }

    // ---- draining ----------------------------------------------------------------

    /// Publishes the events the connection task buffered before the link is dropped, so the
    /// frames already received show up in logs, recordings and the UI the way the ones
    /// handled before them did. Does not wait: the close handshake is not part of it.
    ///
    /// A buffered `CALL` is not dispatched on a link that is going away; dropping it makes
    /// the connection answer `InternalError` if the socket is still writable. `Closed` ends
    /// the drain.
    fn drain_buffered_events(&mut self, events: Option<&mut mpsc::Receiver<ConnectionEvent>>) {
        let Some(events) = events else {
            return;
        };
        for _ in 0..DRAIN_LIMIT {
            match events.try_recv() {
                Ok(ConnectionEvent::Frame(record)) => self.emit_frame(record),
                Ok(ConnectionEvent::ProtocolWarning(text)) => {
                    self.log(LogLevel::Warn, format!("protocol: {}", sanitize(&text)));
                }
                Ok(ConnectionEvent::Call(_)) => {}
                Ok(ConnectionEvent::Closed { .. }) | Err(_) => break,
            }
        }
    }
}

/// The URL without credentials, for log lines.
fn redact_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.to_string()
        }
        Err(_) => "<invalid url>".to_owned(),
    }
}

/// Closes `connection` and lets the close handshake finish in the background; its events
/// are not needed.
fn close_in_background(
    connection: Option<Connection>,
    events: Option<mpsc::Receiver<ConnectionEvent>>,
) {
    let Some(connection) = connection else {
        return;
    };
    connection.close();
    if let Some(mut events) = events {
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if matches!(event, ConnectionEvent::Closed { .. }) {
                    break;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::broadcast;

    use super::*;
    use crate::ocpp::frame::{Direction, FrameRecord};
    use crate::station::Event;

    /// An actor whose link holds `frames` buffered inbound frames (and a warning among them);
    /// `observed` sees what the actor publishes.
    fn actor_with_buffered_events(
        frames: usize,
    ) -> (Station2Actor, broadcast::Receiver<Event>, Vec<String>) {
        let (_, commands) = mpsc::channel(1);
        let (events, observed) = broadcast::channel(1024);
        let mut actor = Station2Actor::new(
            StationConfig::new("ws://127.0.0.1:1/ocpp").without_persistence(),
            Arc::new(crate::clock::SystemClock),
            commands,
            events,
            None,
        )
        .0;
        let (connection, _reply) = Connection::test_reply_channel();
        let (sender, receiver) = mpsc::channel(frames + 2);
        let mut ids = Vec::new();
        for number in 0..frames {
            let id = format!("buffered-{number}");
            sender
                .try_send(ConnectionEvent::Frame(FrameRecord {
                    ts: chrono::Utc::now(),
                    direction: Direction::In,
                    msg_id: None,
                    action: Some("Heartbeat".to_owned()),
                    frame: serde_json::json!([3, id, {}]),
                }))
                .expect("room in the channel");
            ids.push(id);
            if number == 0 {
                sender
                    .try_send(ConnectionEvent::ProtocolWarning("odd frame".to_owned()))
                    .expect("room in the channel");
            }
        }
        actor.link.conn = Some(connection);
        actor.link.events = Some(receiver);
        (actor, observed, ids)
    }

    fn observed_frame_ids(observed: &mut broadcast::Receiver<Event>) -> Vec<String> {
        let mut ids = Vec::new();
        while let Ok(event) = observed.try_recv() {
            if let Event::Frame(record) = event
                && let Some(id) = record.frame.get(1).and_then(serde_json::Value::as_str)
            {
                ids.push(id.to_owned());
            }
        }
        ids
    }

    #[tokio::test]
    async fn a_manual_disconnect_ends_a_reboot_in_progress() {
        use crate::ocpp::v2::messages::ResetKind;
        use crate::station2::reset::ResetPlan;

        let (mut actor, _observed, _ids) = actor_with_buffered_events(0);
        actor.reset = Some(ResetPlan {
            kind: ResetKind::Immediate,
            phase: ResetPhase::Rebooting,
            reason: BootReason::RemoteReset,
        });
        let (reply, _answer) = tokio::sync::oneshot::channel();
        actor.cmd_disconnect(reply);
        assert!(actor.reset.is_none(), "the next connect is a power-up");
    }

    #[tokio::test]
    async fn disconnect_publishes_frames_already_received() {
        let (mut actor, mut observed, ids) = actor_with_buffered_events(5);
        actor.disconnect();
        assert_eq!(observed_frame_ids(&mut observed), ids);
    }

    #[tokio::test]
    async fn reset_reconnect_publishes_frames_already_received() {
        let (mut actor, mut observed, ids) = actor_with_buffered_events(5);
        actor.reconnect_for_reset();
        assert_eq!(observed_frame_ids(&mut observed), ids);
    }

    #[tokio::test]
    async fn draining_on_disconnect_stops_at_its_bound() {
        let (mut actor, mut observed, ids) = actor_with_buffered_events(DRAIN_LIMIT + 10);
        actor.disconnect();
        let seen = observed_frame_ids(&mut observed);
        // The warning in the buffer uses one of the steps.
        assert_eq!(seen.len(), DRAIN_LIMIT - 1);
        assert_eq!(seen, ids[..seen.len()]);
    }

    #[test]
    fn urls_are_logged_without_credentials() {
        assert_eq!(
            redact_url("ws://user:secret@host:9000/ocpp"),
            "ws://host:9000/ocpp"
        );
        assert_eq!(redact_url("not a url"), "<invalid url>");
    }

    #[test]
    fn device_model_password_goes_into_the_userinfo_byte_for_byte() {
        let password = "p@ss:wörd%41 /?#";
        let url = with_basic_credentials("ws://host:9000/ocpp", "CP 1", password)
            .expect("a URL without a password takes one");
        let parsed = url::Url::parse(&url).unwrap();
        let decode = |text: &str| {
            percent_encoding::percent_decode_str(text)
                .decode_utf8()
                .unwrap()
                .into_owned()
        };
        assert_eq!(decode(parsed.username()), "CP 1");
        assert_eq!(decode(parsed.password().unwrap()), password);
        assert_eq!(parsed.host_str(), Some("host"));
    }

    #[test]
    fn a_password_in_the_url_wins() {
        assert_eq!(
            with_basic_credentials("ws://CP:fromurl@host/ocpp", "CP", "fromdm"),
            None
        );
        assert_eq!(with_basic_credentials("not a url", "CP", "pw"), None);
    }

    #[test]
    fn the_security_profile_follows_scheme_password_and_certificate() {
        let config = |url: &str| StationConfig::new(url).without_persistence();
        assert_eq!(security_profile_of(&config("ws://h/ocpp"), false), 0);
        assert_eq!(security_profile_of(&config("wss://h/ocpp"), false), 0);
        assert_eq!(security_profile_of(&config("ws://h/ocpp"), true), 1);
        assert_eq!(security_profile_of(&config("ws://u:p@h/ocpp"), false), 1);
        assert_eq!(security_profile_of(&config("wss://h/ocpp"), true), 2);
        let mut with_cert = config("wss://h/ocpp");
        with_cert.tls.cert_file = Some("client.pem".into());
        assert_eq!(security_profile_of(&with_cert, false), 3);
        // A client certificate means nothing on a plain connection.
        let mut plain = config("ws://h/ocpp");
        plain.tls.cert_file = Some("client.pem".into());
        assert_eq!(security_profile_of(&plain, false), 0);
    }

    #[test]
    fn network_text_is_made_printable() {
        assert_eq!(sanitize("a\nb\u{1b}[31mc"), "a b [31mc");
    }
}
