//! Session recordings: a JSON-lines log of the frames (and, for the emulator, the commands)
//! of one station or CSMS run.
//!
//! The format is version-neutral so the emulator and the CSMS write the same thing.
//! Line 1 is the header, every other line is a `frame` or a `command` entry, and a finished
//! file ends with an `end` line. Every line is one JSON object with a `type` key:
//!
//! ```text
//! {"type":"header","schema":1,"ocpp_version":"1.6","tool_version":"0.6.0","role":"emulator","station":"CP-001","started_at":"2026-11-13T10:00:00Z"}
//! {"type":"frame","seq":0,"elapsed_ms":12,"session":1,"direction":"out","msg_id":"a1","action":"BootNotification","frame":[2,"a1","BootNotification",{...}]}
//! {"type":"command","seq":3,"elapsed_ms":900,"session":1,"command":{"command":"plug_in","connector":1}}
//! {"type":"end","frames":42,"commands":3,"elapsed_ms":5000}
//! ```
//!
//! * `direction` is relative to the producer of the file: `out` is what it sent, `in` what it
//!   received.
//! * `seq` numbers frames and commands in one sequence, `elapsed_ms` is monotonic time since
//!   the recording started, `session` counts the connections of the run.
//! * A frame that is larger than [`MAX_FRAME_BYTES`] is written as `"frame":null` with
//!   `truncated_bytes`; text that is not an OCPP-J frame is written as `"frame":null` with
//!   `invalid_bytes` (its content is never stored).
//! * `AuthorizationKey` values and URL passwords are replaced by `***` before anything is
//!   written, also when the station runs with `--show-secrets`.
//! * Readers ignore unknown keys and unknown line types. New keys are only ever added; a file
//!   with a newer `schema` is refused.
//! * The `end` line is absent when the process died; its `incomplete` key names the reason
//!   when the recorder lost data (queue full, write error, size limit).

mod read;
mod redact;
mod replay;
mod scenario;
mod toml_out;
mod write;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub use self::read::{CommandLine, End, Entry, FrameLine, Header, Recording};
pub use self::redact::{holds_redacted, redact_value};
pub use self::replay::{ReplayOptions, ReplayPlan, plan_replay};
pub use self::scenario::{ScenarioConversion, to_scenario};
pub use self::write::{RecordOptions, RecordSink, RecordingGuard, RecordingSummary, start};

/// Format version written and understood by this build.
pub const SCHEMA: u32 = 1;
/// Largest frame (serialized JSON) that is recorded in full.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
/// Longest line a reader accepts.
pub const MAX_LINE_BYTES: usize = 256 * 1024;
/// Most entries (frames and commands) a reader loads.
pub const MAX_ENTRIES: usize = 1_000_000;
/// Default size limit of a recording file.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Largest file a reader loads: what a writer may produce plus the header and end lines.
pub const MAX_READ_BYTES: u64 = DEFAULT_MAX_FILE_BYTES + 2 * MAX_LINE_BYTES as u64;
/// Lines the writer queue holds before the recorder is marked incomplete.
pub const QUEUE_LINES: usize = 16_384;

/// Who wrote a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The charge point emulator.
    Emulator,
    /// The central system.
    Csms,
    /// A role this build does not know.
    #[serde(other)]
    Unknown,
}

/// Why a recording could not be written or read ([`Io`](Self::Io) is the write side,
/// [`Read`](Self::Read) the read side).
///
/// Messages never echo file content; text that came from the file is escaped and clipped.
#[derive(Debug, Error)]
pub enum RecordError {
    #[error("cannot write the recording {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot read the recording {}: {source}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{} already exists", path.display())]
    Exists { path: PathBuf },
    #[error("{} is not a regular file", path.display())]
    NotAFile { path: PathBuf },
    #[error("{0}")]
    Invalid(String),
}

impl RecordError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

/// Text from a file, made safe for a message: unsafe characters escaped, cut to `max`
/// characters.
pub(crate) fn clean(text: &str, max: usize) -> String {
    let escaped = crate::text::escape_unsafe(text);
    match escaped.char_indices().nth(max) {
        Some((at, _)) => format!("{}...", &escaped[..at]),
        None => escaped,
    }
}

/// A position in a recording as text, `+12.3s` (tenths of a second, rounded down).
pub(crate) fn at_text(elapsed_ms: u64) -> String {
    format!("+{}.{}s", elapsed_ms / 1000, elapsed_ms % 1000 / 100)
}

/// The `id` and, for a CALL, the action of a serialized OCPP-J frame.
pub(crate) fn frame_parts(frame: &Value) -> Option<(u64, &str, Option<&str>)> {
    let items = frame.as_array()?;
    let kind = items.first()?.as_u64()?;
    let id = items.get(1)?.as_str()?;
    let action = if kind == 2 {
        items.get(2)?.as_str()
    } else {
        None
    };
    Some((kind, id, action))
}

#[cfg(test)]
mod tests;
