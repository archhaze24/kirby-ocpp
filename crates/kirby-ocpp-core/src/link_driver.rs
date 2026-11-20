//! Version-neutral pieces of driving the CSMS connection, shared by the 1.6 and the 2.x
//! station actors: reconnect planning, draining and closing a link that is going away, and
//! the texts of connection log lines. The actors own the state machines; nothing here holds
//! state or runs a task of its own beyond the background close.

use std::time::Duration;

use tokio::sync::mpsc;

use crate::config::{ReconnectConfig, StationConfig};
use crate::ocpp::frame::FrameRecord;
use crate::ws::{ConnectError, ConnectOptions, Connection, ConnectionEvent};

/// Smallest delay between two boot attempts or heartbeats a station ever schedules.
pub(crate) const MIN_PERIOD: Duration = Duration::from_secs(1);

/// Most buffered events taken from a link that is being dropped. The connection task buffers
/// at most `ws::EVENT_CAPACITY` (64); the bound also covers a task that keeps emitting while
/// the actor drains.
pub(crate) const DRAIN_LIMIT: usize = 256;

/// A period of `secs` seconds, never below [`MIN_PERIOD`].
pub(crate) fn period_of_secs(secs: u32) -> Duration {
    Duration::from_secs(u64::from(secs)).max(MIN_PERIOD)
}

/// What to do after the link ended, as decided by the reconnect policy.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReconnectPlan {
    /// The attempts are used up, or automatic reconnect is off.
    Stop { reason: String },
    /// Try again as attempt number `attempt` after `delay`; `message` is the log line.
    Retry {
        attempt: u32,
        delay: Duration,
        message: String,
    },
}

/// Applies `policy` after `failed_attempts` attempts since the last reset.
pub(crate) fn plan_reconnect(policy: &ReconnectConfig, failed_attempts: u32) -> ReconnectPlan {
    let attempt = failed_attempts + 1;
    if !policy.allows_attempt(attempt) {
        let reason = if policy.enabled {
            format!("after {failed_attempts} failed attempt(s)")
        } else {
            "automatic reconnect is disabled".to_owned()
        };
        return ReconnectPlan::Stop { reason };
    }
    let delay = policy.delay_for_attempt(attempt);
    let limit = if policy.max_attempts > 0 {
        format!("/{}", policy.max_attempts)
    } else {
        String::new()
    };
    ReconnectPlan::Retry {
        attempt,
        delay,
        message: format!(
            "reconnect attempt {attempt}{limit} in {}ms",
            delay.as_millis()
        ),
    }
}

/// What a link that is being dropped still had buffered.
pub(crate) enum Drained {
    /// A frame the connection had already sent or received.
    Frame(FrameRecord),
    /// A protocol problem that did not close the socket (raw text, not yet sanitized).
    Warning(String),
}

/// Hands the events the connection task buffered before the link is dropped to `publish`, so
/// the frames already received show up in logs, recordings and the UI the way the ones
/// handled before them did. Does not wait: the close handshake is not part of it.
///
/// A buffered `CALL` is not dispatched on a link that is going away; dropping it makes the
/// connection answer `InternalError` if the socket is still writable. `Closed` ends the drain.
pub(crate) fn drain_buffered(
    events: Option<&mut mpsc::Receiver<ConnectionEvent>>,
    mut publish: impl FnMut(Drained),
) {
    let Some(events) = events else {
        return;
    };
    for _ in 0..DRAIN_LIMIT {
        match events.try_recv() {
            Ok(ConnectionEvent::Frame(record)) => publish(Drained::Frame(record)),
            Ok(ConnectionEvent::ProtocolWarning(text)) => publish(Drained::Warning(text)),
            Ok(
                ConnectionEvent::Call(_)
                | ConnectionEvent::Send { .. }
                | ConnectionEvent::CallResultError { .. },
            ) => {}
            Ok(ConnectionEvent::Closed { .. }) | Err(_) => break,
        }
    }
}

/// Closes `connection` and lets the close handshake finish in the background; its events
/// are not needed.
pub(crate) fn close_in_background(
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

/// Opens the connection described by `config` (its `url`, `tls` and subprotocol) with `options`.
pub(crate) async fn open_connection(
    config: &StationConfig,
    options: ConnectOptions,
) -> Result<(Connection, mpsc::Receiver<ConnectionEvent>), ConnectError> {
    Connection::connect(
        &config.url,
        &config.charge_point_id,
        &config.tls,
        &config.ws_subprotocol,
        options,
    )
    .await
}

/// The log line for a `Closed` event. `reason` has to be printable already.
pub(crate) fn describe_close(code: Option<u16>, reason: &str, by_us: bool) -> String {
    let shown = code.map_or_else(String::new, |c| format!(" ({c})"));
    let why = if reason.is_empty() {
        String::new()
    } else {
        format!(": {reason}")
    };
    let who = if by_us { " by the station" } else { "" };
    format!("disconnected{who}{shown}{why}")
}

/// The URL without credentials, for log lines.
pub(crate) fn redact_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.to_string()
        }
        Err(_) => "<invalid url>".to_owned(),
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

    #[test]
    fn a_period_is_at_least_a_second() {
        assert_eq!(period_of_secs(0), MIN_PERIOD);
        assert_eq!(period_of_secs(300), Duration::from_secs(300));
    }

    #[test]
    fn the_reconnect_plan_follows_the_policy() {
        let policy = ReconnectConfig {
            enabled: true,
            initial_delay_ms: 100,
            max_delay_ms: 250,
            max_attempts: 3,
        };
        assert_eq!(
            plan_reconnect(&policy, 1),
            ReconnectPlan::Retry {
                attempt: 2,
                delay: Duration::from_millis(200),
                message: "reconnect attempt 2/3 in 200ms".to_owned(),
            }
        );
        assert_eq!(
            plan_reconnect(&policy, 3),
            ReconnectPlan::Stop {
                reason: "after 3 failed attempt(s)".to_owned()
            }
        );
        let unlimited = ReconnectConfig {
            max_attempts: 0,
            ..policy.clone()
        };
        assert!(matches!(
            plan_reconnect(&unlimited, 40),
            ReconnectPlan::Retry { delay, ref message, .. }
                if delay == Duration::from_millis(250) && message == "reconnect attempt 41 in 250ms"
        ));
        let off = ReconnectConfig {
            enabled: false,
            ..policy
        };
        assert_eq!(
            plan_reconnect(&off, 0),
            ReconnectPlan::Stop {
                reason: "automatic reconnect is disabled".to_owned()
            }
        );
    }

    #[test]
    fn a_close_is_described_with_code_reason_and_origin() {
        assert_eq!(describe_close(None, "", false), "disconnected");
        assert_eq!(
            describe_close(Some(1006), "gone", true),
            "disconnected by the station (1006): gone"
        );
    }
}
