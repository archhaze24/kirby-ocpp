//! Connection lifecycle: connect, reconnect with backoff, boot, heartbeat.
//!
//! TS reference: `connect`/`disconnect`/`boot`/`scheduleReconnect`/`startHeartbeatTimer` in
//! `src/station.ts`.
//!
//! # Rules
//!
//! * `Connect` opens the connection in a spawned task (`Connection::connect` can take up to
//!   the call timeout); the result comes back as `Internal::Connected`. On success the
//!   station sends `BootNotification`.
//! * Anything that ends the link unexpectedly (a failed attempt, a `Closed` event) schedules
//!   a reconnect after `min(initial * 2^(n-1), max)` while `wanted` and attempts are left
//!   (`max_attempts == 0` = unlimited). A failed attempt counts like a lost connection.
//!   The attempt counter resets when `BootNotification` is `Accepted` and on a manual
//!   `Connect` / `Reconnect`.
//! * `Disconnect` and shutdown cancel reconnect, boot retry, heartbeat and debounce timers
//!   and fail calls in flight; a `Closed` event of a link we closed ourselves never reaches
//!   the actor (the link is dropped first), so it can never trigger a backoff.
//! * Every task result carries the link generation; results of an older link are ignored.
//!
//! # BootNotification
//!
//! * `Accepted`: store the CSMS time offset (logs only, the system clock is never set); take
//!   `interval` as `HeartbeatInterval` when it is positive, otherwise keep the configured one
//!   (`0` means "the charge point chooses" in the spec) - a heartbeat timer is never armed
//!   with a zero period; re-send all statuses (forced, connector 0 first).
//! * `Pending` / `Rejected`: no heartbeat; `BootNotification` is repeated after
//!   `max(1 s, interval)` (the configured heartbeat interval when `interval` is 0). While
//!   `Pending` the gate in `calls` lets only CSMS-triggered messages through, while `Rejected`
//!   nothing but `BootNotification`.
//! * A failed `BootNotification` CALL (timeout, CALLERROR) is retried after the configured
//!   heartbeat interval.

use std::time::Duration;

use super::actor::{Internal, StationActor, TimerKey};
use super::calls::{Origin, Reply};
use super::command::Outcome;
use super::commands::call_failure;
use super::event::LogLevel;
use crate::error::CallError;
use crate::ocpp::messages::{
    BootNotification, BootNotificationRequest, BootNotificationResponse, Heartbeat,
    HeartbeatRequest,
};
use crate::ocpp::types::RegistrationStatus;
use crate::ws::{ConnectError, ConnectOptions, Connection, ConnectionEvent, ConnectionState};

/// Smallest delay between two boot attempts or heartbeats we ever schedule.
const MIN_PERIOD: Duration = Duration::from_secs(1);

impl StationActor {
    pub(super) fn connect_options(&self) -> ConnectOptions {
        ConnectOptions {
            ping_interval: self.registry.web_socket_ping_interval(),
            call_timeout: self.config.call_timeout(),
            serialize_calls: true,
            show_secrets: self.config.show_secrets,
            write_timeout: crate::ws::DEFAULT_WRITE_TIMEOUT,
        }
    }

    /// `Connect` command: reset the backoff, open the connection unless one is open or
    /// being opened.
    pub(super) fn connect(&mut self) {
        self.link.wanted = true;
        self.link.attempts = 0;
        self.timers.cancel(&TimerKey::Reconnect);
        if self.link.conn.is_some() || self.link.connecting.is_some() {
            return;
        }
        self.start_connect();
    }

    /// `Disconnect` command: close the connection and stop all automatic behaviour.
    pub(super) fn disconnect(&mut self) {
        self.link.wanted = false;
        self.link.attempts = 0;
        self.state.reconnect_stopped = None;
        self.timers.cancel(&TimerKey::Reconnect);
        if let Some(handle) = self.link.connecting.take() {
            handle.abort();
        }
        let connection = self.link.conn.clone();
        let events = self.link.events.take();
        self.link_down(&CallError::Closed { code: None });
        close_in_background(connection, events);
        self.state.connection = ConnectionState::Disconnected;
        self.log(LogLevel::Info, "disconnected".to_owned());
    }

    /// Hard `Reset`: closes the connection and opens a new one right away (even with
    /// automatic reconnect disabled); the boot follows through the normal lifecycle.
    pub(super) fn reconnect_for_hard_reset(&mut self) {
        let connection = self.link.conn.clone();
        let events = self.link.events.take();
        self.link_down(&CallError::Closed { code: None });
        close_in_background(connection, events);
        self.link.wanted = true;
        self.link.attempts = 0;
        self.timers.cancel(&TimerKey::Reconnect);
        if let Some(handle) = self.link.connecting.take() {
            handle.abort();
        }
        self.start_connect();
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
        self.connect_identity = self.apply_security_transport(&mut config);
        let options = self.connect_options();
        let internal = self.internal_tx.clone();
        self.link.connecting = Some(tokio::spawn(async move {
            let result = Connection::connect(
                &config.url,
                &config.charge_point_id,
                &config.tls,
                &config.ws_subprotocol,
                options,
            )
            .await;
            let _ = internal.send(Internal::Connected { generation, result });
        }));
    }

    pub(super) fn on_connect_result(
        &mut self,
        generation: u64,
        result: Result<(Connection, tokio::sync::mpsc::Receiver<ConnectionEvent>), ConnectError>,
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
                if self.config.url.to_ascii_lowercase().starts_with("wss://")
                    && !self.config.tls.skip_verify
                {
                    self.on_tls_connected();
                }
                self.on_connection_established();
                self.send_boot(None);
            }
            Err(error) => {
                self.log(
                    LogLevel::Error,
                    format!(
                        "connection failed: {}",
                        super::security::strip_control(&error.to_string())
                    ),
                );
                let fell_back = self.report_connect_failure(&error);
                self.state.connection = ConnectionState::Disconnected;
                if fell_back && self.link.wanted {
                    // A05.FR.08: the previous profile is in force again; try it at once.
                    self.link.attempts = 0;
                    self.timers.cancel(&TimerKey::Reconnect);
                    self.start_connect();
                } else {
                    self.schedule_reconnect();
                }
            }
        }
    }

    pub(super) fn on_connection_event(&mut self, event: ConnectionEvent) {
        match event {
            ConnectionEvent::Frame(record) => self.emit_frame(record),
            ConnectionEvent::Call(call) => self.dispatch_incoming(call),
            ConnectionEvent::ProtocolWarning(text) => {
                self.log(LogLevel::Warn, format!("protocol: {text}"));
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
                    format!(": {reason}")
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

    /// Everything that must stop when the connection is gone, whatever the reason.
    fn link_down(&mut self, error: &CallError) {
        self.link.generation += 1;
        self.link.conn = None;
        self.link.boot_in_flight = false;
        self.timers.cancel(&TimerKey::Heartbeat);
        self.timers.cancel(&TimerKey::BootRetry);
        self.timers.cancel(&TimerKey::HardReset);
        self.hard_reset_deadline = None;
        self.cancel_maintenance();
        self.cancel_log_upload();
        self.cancel_signed_firmware();
        self.sync_outstanding = 0;
        self.sync_keys.clear();
        self.sync_boot = false;
        self.sync_failed = false;
        self.sync_failures.clear();
        self.timers.cancel(&TimerKey::SyncRetry);
        self.reset_status_tracking();
        self.state.registration = None;
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
        self.timers.set(TimerKey::Reconnect, delay);
    }

    pub(super) fn on_reconnect_timer(&mut self) {
        if !self.link.wanted || self.link.conn.is_some() || self.link.connecting.is_some() {
            return;
        }
        self.start_connect();
    }

    // ---- BootNotification ------------------------------------------------------

    /// Sends `BootNotification`. `reply` (the `Boot` command) is answered with the CSMS's
    /// response. Without a reply a boot already in flight is not duplicated.
    pub(super) fn send_boot(&mut self, reply: Option<Reply>) {
        if reply.is_none() && self.link.boot_in_flight {
            return;
        }
        self.timers.cancel(&TimerKey::BootRetry);
        self.link.boot_in_flight = true;
        let request = BootNotificationRequest {
            charge_point_vendor: self.config.vendor.clone(),
            charge_point_model: self.config.model.clone(),
            charge_point_serial_number: self.config.serial_number.clone(),
            charge_box_serial_number: None,
            firmware_version: self.config.firmware_version.clone(),
            iccid: None,
            imsi: None,
            meter_type: None,
            meter_serial_number: None,
        };
        let generation = self.link.generation;
        self.call::<BootNotification>(Origin::Station, &request, move |actor, result| {
            if actor.link.generation == generation {
                actor.link.boot_in_flight = false;
            }
            actor.on_boot_result(result, reply);
        });
    }

    fn on_boot_result(
        &mut self,
        result: Result<BootNotificationResponse, CallError>,
        reply: Option<Reply>,
    ) {
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                if self.link.conn.is_some() && self.link.wanted {
                    let delay = self.registry_retry_period();
                    self.log(
                        LogLevel::Warn,
                        format!(
                            "BootNotification failed ({error}); retrying in {}s",
                            delay.as_secs()
                        ),
                    );
                    self.timers.set(TimerKey::BootRetry, delay);
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
                self.timers.cancel(&TimerKey::Heartbeat);
                let mut delay = u64::try_from(response.interval)
                    .ok()
                    .filter(|secs| *secs > 0)
                    .map_or_else(|| self.registry_retry_period(), Duration::from_secs)
                    .max(MIN_PERIOD);
                // After a Reset the boot is repeated quickly, `ResetRetries` times.
                match self.reset_retries {
                    Some(left) if left > 0 => {
                        self.reset_retries = Some(left - 1);
                        delay = Duration::from_millis(500);
                    }
                    _ => self.reset_retries = None,
                }
                self.log(
                    LogLevel::Warn,
                    format!(
                        "BootNotification was {}; retrying in {}s",
                        response.status,
                        delay.as_secs()
                    ),
                );
                if self.link.conn.is_some() && self.link.wanted {
                    self.timers.set(TimerKey::BootRetry, delay);
                }
            }
        }
        if let Some(reply) = reply {
            let payload = serde_json::to_value(&response).unwrap_or_default();
            let _ = reply.send(Ok(Outcome::Response { payload }));
        }
    }

    fn on_boot_accepted(&mut self, response: &BootNotificationResponse) {
        let offset = response.current_time - self.clock.now_utc();
        self.clock_offset = Some(offset);
        self.link.attempts = 0;
        if let Ok(interval) = u32::try_from(response.interval)
            && interval > 0
        {
            self.registry.change(
                crate::station::config_registry::HEARTBEAT_INTERVAL,
                &interval.to_string(),
            );
        }
        self.state.heartbeat_interval_secs = self.registry.heartbeat_interval_secs();
        self.arm_heartbeat();
        self.mark_dirty();
        self.log(
            LogLevel::Success,
            format!(
                "BootNotification accepted; heartbeat every {}s; CSMS clock is {}ms {}",
                self.state.heartbeat_interval_secs,
                offset.num_milliseconds().abs(),
                if offset.num_milliseconds() >= 0 {
                    "ahead"
                } else {
                    "behind"
                }
            ),
        );
        self.after_boot_accepted();
    }

    /// Everything that follows an accepted boot, in this order: offline transaction sync
    /// (starts, then stops), and when it has finished meter scheduling and the status re-send
    /// (`finish_boot_accepted`).
    fn after_boot_accepted(&mut self) {
        self.reset_retries = None;
        self.flush_offline_samples(None);
        if self.start_offline_sync(true) == 0 {
            self.finish_boot_accepted();
        }
    }

    pub(super) fn on_boot_retry_timer(&mut self) {
        if self.link.conn.is_some() {
            self.send_boot(None);
        }
    }

    /// Period to wait after a boot problem when the CSMS gave none: the configured
    /// heartbeat interval, at least a second.
    fn registry_retry_period(&self) -> Duration {
        Duration::from_secs(u64::from(self.registry.heartbeat_interval_secs())).max(MIN_PERIOD)
    }

    // ---- Heartbeat -------------------------------------------------------------

    /// (Re)arms the periodic heartbeat from `HeartbeatInterval` (never below one second).
    pub(super) fn arm_heartbeat(&mut self) {
        let period =
            Duration::from_secs(u64::from(self.registry.heartbeat_interval_secs())).max(MIN_PERIOD);
        self.timers.set(TimerKey::Heartbeat, period);
    }

    pub(super) fn on_heartbeat_timer(&mut self) {
        if self.state.registration != Some(RegistrationStatus::Accepted) {
            return;
        }
        self.arm_heartbeat();
        self.send_heartbeat(Origin::Station, None);
    }

    /// Sends `Heartbeat`; `reply` is answered with the CSMS's response.
    pub(super) fn send_heartbeat(&mut self, origin: Origin, reply: Option<Reply>) {
        self.call::<Heartbeat>(
            origin,
            &HeartbeatRequest {},
            move |actor, result| match result {
                Ok(response) => {
                    actor.state.last_heartbeat_at = Some(actor.clock.now_utc());
                    actor.log(
                        LogLevel::Success,
                        format!(
                            "Heartbeat accepted, Central System time: {}",
                            response.current_time
                        ),
                    );
                    if let Some(reply) = reply {
                        let payload = serde_json::to_value(&response).unwrap_or_default();
                        let _ = reply.send(Ok(Outcome::Response { payload }));
                    }
                }
                Err(error) => {
                    if let Some(reply) = reply {
                        let _ = reply.send(Err(call_failure(error)));
                    }
                }
            },
        );
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
    events: Option<tokio::sync::mpsc::Receiver<ConnectionEvent>>,
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
    use super::*;

    #[test]
    fn urls_are_logged_without_credentials() {
        assert_eq!(
            redact_url("ws://user:secret@host:9000/ocpp"),
            "ws://host:9000/ocpp"
        );
        assert_eq!(redact_url("not a url"), "<invalid url>");
    }
}
