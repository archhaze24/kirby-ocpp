//! Events broadcast to observers of a station.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use crate::ocpp::frame::{Direction, FrameRecord};

/// Severity of a [`LogRecord`] (the TS `info|success|warn|error`; `in`/`out` are
/// now [`Event::Frame`] with a [`Direction`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Success,
    Warn,
    Error,
}

/// A human-readable station log line (never a raw frame).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogRecord {
    pub ts: DateTime<Utc>,
    pub level: LogLevel,
    pub text: String,
}

/// Something observable happened. Delivered over a `tokio::sync::broadcast`
/// channel: a slow subscriber gets `RecvError::Lagged` and loses events, which is
/// fine for logs. The authoritative state is always the latest [`super::Snapshot`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A frame crossed the wire.
    Frame(FrameRecord),
    /// A station log line.
    Log(LogRecord),
    /// The snapshot changed; read the new one from the watch channel.
    StateChanged,
}
