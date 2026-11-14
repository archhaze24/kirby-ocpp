//! Reading recordings: bounded, tolerant of a cut-off tail, strict about the schema.

use std::io::{BufRead, Read};
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use super::{MAX_ENTRIES, MAX_LINE_BYTES, MAX_READ_BYTES, RecordError, Role, SCHEMA, clean};
use crate::ocpp::frame::Direction;

/// The first line of a recording.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Header {
    pub schema: u32,
    #[serde(default)]
    pub ocpp_version: String,
    #[serde(default)]
    pub tool_version: String,
    pub role: Role,
    #[serde(default)]
    pub station: String,
    #[serde(default)]
    pub started_at: String,
}

/// One recorded frame.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FrameLine {
    pub seq: u64,
    pub elapsed_ms: u64,
    pub session: u64,
    /// Station identity for a multi-station CSMS recording.
    #[serde(default)]
    pub station: Option<String>,
    /// Relative to the producer of the file.
    pub direction: Direction,
    #[serde(default)]
    pub msg_id: Option<String>,
    /// The action of the frame, or of the CALL it answers.
    #[serde(default)]
    pub action: Option<String>,
    /// The OCPP-J frame as JSON; `None` when it was too large or not a frame.
    #[serde(default)]
    pub frame: Option<Value>,
    #[serde(default)]
    pub truncated_bytes: Option<u64>,
    #[serde(default)]
    pub invalid_bytes: Option<u64>,
}

/// One recorded command (emulator recordings only).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CommandLine {
    pub seq: u64,
    pub elapsed_ms: u64,
    pub session: u64,
    /// The command as the station serializes it; `None` when it was too large.
    #[serde(default)]
    pub command: Option<Value>,
    #[serde(default)]
    pub truncated_bytes: Option<u64>,
}

/// The last line of a finished recording.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct End {
    #[serde(default)]
    pub frames: u64,
    #[serde(default)]
    pub commands: u64,
    #[serde(default)]
    pub elapsed_ms: u64,
    /// Why data is missing; absent for a complete recording.
    #[serde(default)]
    pub incomplete: Option<String>,
}

/// A frame or a command, in file order.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Frame(FrameLine),
    Command(CommandLine),
}

impl Entry {
    #[must_use]
    pub fn seq(&self) -> u64 {
        match self {
            Self::Frame(frame) => frame.seq,
            Self::Command(command) => command.seq,
        }
    }

    #[must_use]
    pub fn elapsed_ms(&self) -> u64 {
        match self {
            Self::Frame(frame) => frame.elapsed_ms,
            Self::Command(command) => command.elapsed_ms,
        }
    }
}

/// A loaded recording.
#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    pub header: Header,
    pub entries: Vec<Entry>,
    /// The `end` line; `None` when the process died before it wrote one.
    pub end: Option<End>,
    /// The last line was cut off and ignored.
    pub tail_truncated: bool,
    /// Lines of an unknown `type` that were skipped.
    pub ignored_lines: u64,
}

impl Recording {
    /// Reads a recording file.
    ///
    /// # Errors
    /// [`RecordError::Invalid`] with the line number for a malformed line, a file without a
    /// header, a newer `schema`, a line longer than [`MAX_LINE_BYTES`] or more than
    /// [`MAX_ENTRIES`] entries; [`RecordError::Read`] for read errors.
    pub fn read(path: &Path) -> Result<Self, RecordError> {
        let io = |source| RecordError::Read {
            path: path.to_path_buf(),
            source,
        };
        let meta = std::fs::metadata(path).map_err(io)?;
        if !meta.is_file() {
            return Err(RecordError::NotAFile {
                path: path.to_path_buf(),
            });
        }
        let file = std::fs::File::open(path).map_err(io)?;
        Self::parse(std::io::BufReader::new(file))
    }

    /// Parses a recording from a reader (see [`Recording::read`] for the errors).
    pub fn parse(mut reader: impl BufRead) -> Result<Self, RecordError> {
        let mut header: Option<Header> = None;
        let mut entries = Vec::new();
        let mut end = None;
        let mut ignored_lines = 0;
        let mut tail_truncated = false;
        let mut buffer = Vec::new();
        let mut number = 0usize;
        let mut total = 0u64;
        loop {
            buffer.clear();
            let read = (&mut reader)
                .take(MAX_LINE_BYTES as u64 + 1)
                .read_until(b'\n', &mut buffer)
                .map_err(|source| {
                    RecordError::invalid(format!(
                        "line {}: cannot read: {}",
                        number + 1,
                        source.kind()
                    ))
                })?;
            if read == 0 {
                break;
            }
            number += 1;
            total += read as u64;
            if total > MAX_READ_BYTES {
                return Err(RecordError::invalid(format!(
                    "line {number}: the recording is larger than {MAX_READ_BYTES} bytes, more than a recorder writes"
                )));
            }
            let complete = buffer.last() == Some(&b'\n');
            if !complete && buffer.len() > MAX_LINE_BYTES {
                return Err(RecordError::invalid(format!(
                    "line {number}: longer than {MAX_LINE_BYTES} bytes"
                )));
            }
            let text = String::from_utf8_lossy(&buffer);
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let value: Value = match serde_json::from_str(text) {
                Ok(value) => value,
                // A line the writer did not finish: the process was killed mid-write.
                Err(_) if !complete => {
                    tail_truncated = true;
                    break;
                }
                Err(error) => {
                    return Err(RecordError::invalid(format!(
                        "line {number}: not valid JSON ({})",
                        clean(&error.to_string(), 100)
                    )));
                }
            };
            let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
            if header.is_none() {
                if kind != "header" {
                    return Err(RecordError::invalid(format!(
                        "line {number}: not a recording (the first line must be the header)"
                    )));
                }
                header = Some(parse_header(value, number)?);
                continue;
            }
            if end.is_some() && matches!(kind, "frame" | "command" | "end") {
                return Err(RecordError::invalid(format!(
                    "line {number}: `{kind}` after the `end` line"
                )));
            }
            match kind {
                "frame" => {
                    entries.push(Entry::Frame(parse_line(value, number, "frame")?));
                }
                "command" => {
                    entries.push(Entry::Command(parse_line(value, number, "command")?));
                }
                "end" => end = Some(parse_line(value, number, "end")?),
                _ => ignored_lines += 1,
            }
            if entries.len() > MAX_ENTRIES {
                return Err(RecordError::invalid(format!(
                    "line {number}: more than {MAX_ENTRIES} entries"
                )));
            }
        }
        let Some(header) = header else {
            return Err(RecordError::invalid(if tail_truncated {
                "the recording has no complete header line".to_owned()
            } else {
                "the recording is empty".to_owned()
            }));
        };
        Ok(Self {
            header,
            entries,
            end,
            tail_truncated,
            ignored_lines,
        })
    }

    /// Whether nothing is known to be missing: an `end` line without `incomplete`, no cut-off
    /// tail.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.tail_truncated
            && self
                .end
                .as_ref()
                .is_some_and(|end| end.incomplete.is_none())
    }

    /// A sentence that says why the recording may be missing data, `None` when it is complete.
    #[must_use]
    pub fn incompleteness(&self) -> Option<String> {
        if let Some(reason) = self.end.as_ref().and_then(|end| end.incomplete.as_deref()) {
            return Some(format!("the recorder lost data: {}", clean(reason, 200)));
        }
        if self.end.is_none() {
            return Some(if self.tail_truncated {
                "the recording was cut off (no end line, the last line is partial)".to_owned()
            } else {
                "the recording has no end line".to_owned()
            });
        }
        None
    }
}

fn parse_header(value: Value, number: usize) -> Result<Header, RecordError> {
    let schema = value.get("schema").and_then(Value::as_u64);
    match schema {
        Some(found) if found > u64::from(SCHEMA) => {
            return Err(RecordError::invalid(format!(
                "line {number}: the recording uses schema {found}, this build reads schema {SCHEMA} (written by a newer version)"
            )));
        }
        Some(found) if found >= 1 => {}
        _ => {
            return Err(RecordError::invalid(format!(
                "line {number}: the header has no valid `schema`"
            )));
        }
    }
    parse_line(value, number, "header")
}

fn parse_line<T: for<'de> Deserialize<'de>>(
    value: Value,
    number: usize,
    what: &str,
) -> Result<T, RecordError> {
    serde_json::from_value(value).map_err(|error| {
        RecordError::invalid(format!(
            "line {number}: invalid {what} line ({})",
            clean(&error.to_string(), 100)
        ))
    })
}
