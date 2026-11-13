//! The recorder: a bounded queue in front of a writer thread.
//!
//! Recording must not lose frames silently, and it must not slow the station down. The
//! producer therefore never blocks and never drops quietly: when the queue is full, a write
//! fails or the size limit is reached, the recorder is marked incomplete with the reason,
//! stops writing (a gap in the middle would mislead a replay) and the `end` line carries the
//! reason. The caller reads it from [`RecordingGuard::finish`].
//!
//! This is deliberately not the lossy log pump of the terminal and headless outputs.

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::redact::redact_value;
use super::{
    DEFAULT_MAX_FILE_BYTES, MAX_FRAME_BYTES, QUEUE_LINES, RecordError, Role, SCHEMA, frame_parts,
};
use crate::ocpp::frame::{Direction, FrameRecord};

/// How long the writer waits for a line before it flushes and checks for the end.
const IDLE_FLUSH: Duration = Duration::from_millis(50);

/// Settings of one recording.
#[derive(Debug, Clone)]
pub struct RecordOptions {
    /// Replace an existing file instead of refusing.
    pub overwrite: bool,
    /// The recording stops (incomplete) when the file would grow past this.
    pub max_file_bytes: u64,
    /// Lines the queue holds.
    pub queue_lines: usize,
}

impl Default for RecordOptions {
    fn default() -> Self {
        Self {
            overwrite: false,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            queue_lines: QUEUE_LINES,
        }
    }
}

/// What a finished recording holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingSummary {
    pub path: PathBuf,
    pub frames: u64,
    pub commands: u64,
    pub bytes: u64,
    /// Why the recording is not complete; `None` when nothing was lost.
    pub incomplete: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Frame,
    Command,
}

#[derive(Debug)]
pub(super) struct Line {
    kind: Kind,
    text: String,
}

#[derive(Debug)]
struct Counts {
    frames: u64,
    commands: u64,
    bytes: u64,
}

#[derive(Debug)]
struct Producer {
    seq: u64,
    queue: SyncSender<Line>,
}

#[derive(Debug)]
struct Shared {
    start: Instant,
    producer: Mutex<Producer>,
    failure: Mutex<Option<String>>,
    /// The failure was already announced through [`RecordSink::take_failure`].
    announced: AtomicBool,
    halted: AtomicBool,
    stop: AtomicBool,
}

impl Shared {
    fn fail(&self, reason: impl Into<String>) {
        let mut slot = self.failure.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(reason.into());
        }
        self.halted.store(true, Ordering::SeqCst);
    }

    fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// The producer side: cheap, never blocks, safe to share between threads.
#[derive(Debug, Clone)]
pub struct RecordSink {
    shared: Arc<Shared>,
}

/// The owner side: ends the recording and reports what it holds.
#[derive(Debug)]
pub struct RecordingGuard {
    path: PathBuf,
    shared: Arc<Shared>,
    writer: Option<JoinHandle<Counts>>,
}

/// Creates the file (mode `0600` on unix), writes the header and starts the writer thread.
///
/// # Errors
/// [`RecordError::Exists`] for an existing file without `overwrite`,
/// [`RecordError::NotAFile`] for something that is not a regular file, [`RecordError::Io`]
/// for everything else.
pub fn start(
    path: &Path,
    ocpp_version: &str,
    role: Role,
    station: &str,
    options: &RecordOptions,
) -> Result<(RecordSink, RecordingGuard), RecordError> {
    let io = |source| RecordError::Io {
        path: path.to_path_buf(),
        source,
    };
    let file = open(path, options.overwrite)?;
    let mut out = BufWriter::new(file);
    // Written by hand so the keys stay in reading order.
    let text = |value: &str| Value::from(value).to_string();
    let header = format!(
        "{{\"type\":\"header\",\"schema\":{SCHEMA},\"ocpp_version\":{},\"tool_version\":{},\"role\":{},\"station\":{},\"started_at\":{}}}",
        text(ocpp_version),
        text(crate::VERSION),
        serde_json::to_string(&role).unwrap_or_else(|_| "\"unknown\"".to_owned()),
        text(station),
        text(&chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
    );
    out.write_all(header.as_bytes()).map_err(io)?;
    out.write_all(b"\n").map_err(io)?;
    out.flush().map_err(io)?;
    let bytes = header.len() as u64 + 1;

    let (shared, lines) = new_shared(options.queue_lines);
    let writer = {
        let shared = Arc::clone(&shared);
        let limit = options.max_file_bytes;
        std::thread::Builder::new()
            .name("recording-writer".to_owned())
            .spawn(move || write_loop(out, &lines, &shared, limit, bytes))
            .map_err(io)?
    };
    Ok((
        RecordSink {
            shared: Arc::clone(&shared),
        },
        RecordingGuard {
            path: path.to_path_buf(),
            shared,
            writer: Some(writer),
        },
    ))
}

fn new_shared(queue_lines: usize) -> (Arc<Shared>, Receiver<Line>) {
    let (queue, lines) = sync_channel(queue_lines.max(1));
    let shared = Arc::new(Shared {
        start: Instant::now(),
        producer: Mutex::new(Producer { seq: 0, queue }),
        failure: Mutex::new(None),
        announced: AtomicBool::new(false),
        halted: AtomicBool::new(false),
        stop: AtomicBool::new(false),
    });
    (shared, lines)
}

/// A sink whose queue nobody drains, so a test can fill it deterministically.
#[cfg(test)]
pub(super) fn detached(queue_lines: usize) -> (RecordSink, Receiver<Line>) {
    let (shared, lines) = new_shared(queue_lines);
    (RecordSink { shared }, lines)
}

fn open(path: &Path, overwrite: bool) -> Result<File, RecordError> {
    let io = |source| RecordError::Io {
        path: path.to_path_buf(),
        source,
    };
    if overwrite {
        match std::fs::metadata(path) {
            Ok(meta) if !meta.is_file() => {
                return Err(RecordError::NotAFile {
                    path: path.to_path_buf(),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io(error)),
        }
    }
    let mut options = OpenOptions::new();
    options.write(true);
    if overwrite {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            RecordError::Exists {
                path: path.to_path_buf(),
            }
        } else {
            io(error)
        }
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(io)?;
    }
    Ok(file)
}

fn write_loop(
    mut out: BufWriter<File>,
    lines: &Receiver<Line>,
    shared: &Shared,
    limit: u64,
    mut bytes: u64,
) -> Counts {
    let mut counts = Counts {
        frames: 0,
        commands: 0,
        bytes,
    };
    loop {
        match lines.recv_timeout(IDLE_FLUSH) {
            Ok(line) => {
                // After a failure the rest is discarded: a recording with a hole in the
                // middle is worse than one that ends early.
                if shared.halted.load(Ordering::SeqCst) {
                    continue;
                }
                let size = line.text.len() as u64 + 1;
                if bytes + size > limit {
                    shared.fail(format!("the size limit of {limit} bytes was reached"));
                    continue;
                }
                let written = out
                    .write_all(line.text.as_bytes())
                    .and_then(|()| out.write_all(b"\n"));
                if let Err(error) = written {
                    shared.fail(format!("writing failed: {}", error.kind()));
                    continue;
                }
                bytes += size;
                match line.kind {
                    Kind::Frame => counts.frames += 1,
                    Kind::Command => counts.commands += 1,
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if !shared.halted.load(Ordering::SeqCst)
                    && let Err(error) = out.flush()
                {
                    shared.fail(format!("writing failed: {}", error.kind()));
                }
                if shared.stop.load(Ordering::SeqCst) {
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let mut end = format!(
        "{{\"type\":\"end\",\"frames\":{},\"commands\":{},\"elapsed_ms\":{}",
        counts.frames,
        counts.commands,
        shared.elapsed_ms()
    );
    if let Some(reason) = shared.failure() {
        end.push_str(",\"incomplete\":");
        end.push_str(&Value::from(reason).to_string());
    }
    end.push('}');
    let finished = out
        .write_all(end.as_bytes())
        .and_then(|()| out.write_all(b"\n"))
        .and_then(|()| out.flush())
        .and_then(|()| out.get_ref().sync_all());
    match finished {
        Ok(()) => bytes += end.len() as u64 + 1,
        Err(error) => shared.fail(format!("finishing the file failed: {}", error.kind())),
    }
    counts.bytes = bytes;
    counts
}

impl RecordSink {
    /// Records a frame as it crossed the wire, `session` being the connection it belongs to.
    /// Secrets are redacted here, whatever the frame record holds.
    pub fn frame(&self, session: u64, record: &FrameRecord) {
        let direction = match record.direction {
            Direction::In => "in",
            Direction::Out => "out",
        };
        let msg_id = record.msg_id.as_ref().map(ToString::to_string);
        // The serialized frame, or what replaces it in the file.
        let (frame, marker) = match &record.frame {
            Value::Array(_) => {
                let mut value = record.frame.clone();
                redact_value(&mut value);
                let text = value.to_string();
                if text.len() > MAX_FRAME_BYTES {
                    ("null".to_owned(), Some(("truncated_bytes", text.len())))
                } else {
                    (text, None)
                }
            }
            Value::String(text) => ("null".to_owned(), Some(("invalid_bytes", text.len()))),
            other => (
                "null".to_owned(),
                Some(("invalid_bytes", other.to_string().len())),
            ),
        };
        let msg_id = msg_id.or_else(|| frame_parts(&record.frame).map(|(_, id, _)| id.to_owned()));
        let action = record.action.clone();
        self.submit(Kind::Frame, |seq, elapsed_ms| {
            let mut line = format!(
                "{{\"type\":\"frame\",\"seq\":{seq},\"elapsed_ms\":{elapsed_ms},\"session\":{session},\"direction\":\"{direction}\""
            );
            if let Some(id) = &msg_id {
                line.push_str(",\"msg_id\":");
                line.push_str(&Value::from(id.as_str()).to_string());
            }
            if let Some(action) = &action {
                line.push_str(",\"action\":");
                line.push_str(&Value::from(action.as_str()).to_string());
            }
            line.push_str(",\"frame\":");
            line.push_str(&frame);
            if let Some((key, size)) = marker {
                let _ = write!(line, ",\"{key}\":{size}");
            }
            line.push('}');
            line
        });
    }

    /// Records a command given to the emulator (any serializable command object).
    pub fn command(&self, session: u64, command: &Value) {
        let mut value = command.clone();
        redact_value(&mut value);
        let text = value.to_string();
        let (body, marker) = if text.len() > MAX_FRAME_BYTES {
            ("null".to_owned(), Some(text.len()))
        } else {
            (text, None)
        };
        self.submit(Kind::Command, |seq, elapsed_ms| {
            let mut line = format!(
                "{{\"type\":\"command\",\"seq\":{seq},\"elapsed_ms\":{elapsed_ms},\"session\":{session},\"command\":{body}"
            );
            if let Some(size) = marker {
                let _ = write!(line, ",\"truncated_bytes\":{size}");
            }
            line.push('}');
            line
        });
    }

    /// Why the recording is incomplete, once: the first call after a failure returns the
    /// reason, later calls return `None` (so a caller can log it without repeating itself).
    pub fn take_failure(&self) -> Option<String> {
        if self.shared.halted.load(Ordering::SeqCst)
            && !self.shared.announced.swap(true, Ordering::SeqCst)
        {
            return self.shared.failure();
        }
        None
    }

    fn submit(&self, kind: Kind, build: impl FnOnce(u64, u64) -> String) {
        if self.shared.halted.load(Ordering::SeqCst) {
            return;
        }
        let mut producer = self
            .shared
            .producer
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let line = Line {
            kind,
            text: build(producer.seq, self.shared.elapsed_ms()),
        };
        match producer.queue.try_send(line) {
            Ok(()) => producer.seq += 1,
            Err(TrySendError::Full(_)) => {
                drop(producer);
                self.shared
                    .fail("the recorder fell behind: its queue was full, frames were lost");
            }
            Err(TrySendError::Disconnected(_)) => {
                drop(producer);
                self.shared.fail("the recorder stopped");
            }
        }
    }
}

impl RecordingGuard {
    /// The reason the recording is incomplete so far, if it is.
    pub fn incomplete(&self) -> Option<String> {
        self.shared.failure()
    }

    /// Writes what is queued and the `end` line, closes the file and reports the result.
    /// Blocks briefly; call it after the producers have stopped.
    pub fn finish(mut self) -> RecordingSummary {
        self.shared.stop.store(true, Ordering::SeqCst);
        let counts = self.writer.take().and_then(|writer| writer.join().ok());
        if counts.is_none() {
            self.shared.fail("the recorder thread panicked");
        }
        let counts = counts.unwrap_or(Counts {
            frames: 0,
            commands: 0,
            bytes: 0,
        });
        RecordingSummary {
            path: self.path.clone(),
            frames: counts.frames,
            commands: counts.commands,
            bytes: counts.bytes,
            incomplete: self.shared.failure(),
        }
    }
}

impl Drop for RecordingGuard {
    fn drop(&mut self) {
        // Without `finish` the writer still ends (and writes its `end` line).
        self.shared.stop.store(true, Ordering::SeqCst);
    }
}
