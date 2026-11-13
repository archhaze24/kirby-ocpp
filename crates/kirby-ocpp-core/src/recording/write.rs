//! The recorder: a bounded queue in front of a writer thread.
//!
//! Recording must not lose frames silently, and it must not slow the station down. The
//! producer therefore never blocks and never drops quietly: when the queue is full, a write
//! fails or the size limit is reached, the recorder is marked incomplete with the reason,
//! stops accepting entries and the `end` line carries the reason. On queue overflow, entries
//! already queued are still written as an unbroken prefix. The caller reads the result from
//! [`RecordingGuard::finish`].
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
    /// Producers stopped queueing (the failure is set); the writer may still be draining.
    halted: AtomicBool,
    /// The queue overflowed: what is already queued is valid and is still written.
    overflowed: AtomicBool,
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
    /// With `overwrite`, the file being written; it replaces `path` when the recording is
    /// finished, so an existing recording survives anything that ends before that.
    temp: Option<PathBuf>,
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
    let (file, temp) = open(path, options.overwrite)?;
    let written = temp.clone().unwrap_or_else(|| path.to_path_buf());
    let started = (|| {
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
                temp,
                shared,
                writer: Some(writer),
            },
        ))
    })();
    if started.is_err() {
        let _ = std::fs::remove_file(&written);
    }
    started
}

fn new_shared(queue_lines: usize) -> (Arc<Shared>, Receiver<Line>) {
    let (queue, lines) = sync_channel(queue_lines.max(1));
    let shared = Arc::new(Shared {
        start: Instant::now(),
        producer: Mutex::new(Producer { seq: 0, queue }),
        failure: Mutex::new(None),
        announced: AtomicBool::new(false),
        halted: AtomicBool::new(false),
        overflowed: AtomicBool::new(false),
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

/// Creates the file to write: `path` itself, which must not exist, or with `overwrite` a new
/// file beside it (returned as the second value) that replaces `path` when the recording is
/// finished. A symlink is never followed or replaced. The file is private (`0600`) on unix.
fn open(path: &Path, overwrite: bool) -> Result<(File, Option<PathBuf>), RecordError> {
    let io = |source| RecordError::Io {
        path: path.to_path_buf(),
        source,
    };
    let not_a_file = || RecordError::NotAFile {
        path: path.to_path_buf(),
    };
    let target = if overwrite {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if !meta.is_file() => return Err(not_a_file()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io(error)),
        }
        let name = path.file_name().ok_or_else(not_a_file)?;
        let mut temp_name = std::ffi::OsString::from(".");
        temp_name.push(name);
        temp_name.push(format!(".{}.tmp", std::process::id()));
        path.with_file_name(temp_name)
    } else {
        path.to_path_buf()
    };
    // Never truncates: a name that exists (a file, a link, a dangling link) is refused.
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&target).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists && !overwrite {
            RecordError::Exists {
                path: path.to_path_buf(),
            }
        } else {
            io(error)
        }
    })?;
    Ok((file, overwrite.then_some(target)))
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
    // The writer itself gave up (size limit, write error).
    let mut stopped = false;
    loop {
        match lines.recv_timeout(IDLE_FLUSH) {
            Ok(line) => {
                // After a failure the rest is discarded: a recording with a hole in the
                // middle is worse than one that ends early. A queue overflow is different:
                // the lines already queued form an unbroken prefix, so they are kept and the
                // producers (halted at the first lost line) add nothing after them.
                if stopped
                    || (shared.halted.load(Ordering::SeqCst)
                        && !shared.overflowed.load(Ordering::SeqCst))
                {
                    continue;
                }
                let size = line.text.len() as u64 + 1;
                if bytes + size > limit {
                    shared.fail(format!("the size limit of {limit} bytes was reached"));
                    stopped = true;
                    continue;
                }
                let written = out
                    .write_all(line.text.as_bytes())
                    .and_then(|()| out.write_all(b"\n"));
                if let Err(error) = written {
                    shared.fail(format!("writing failed: {}", error.kind()));
                    stopped = true;
                    continue;
                }
                bytes += size;
                match line.kind {
                    Kind::Frame => counts.frames += 1,
                    Kind::Command => counts.commands += 1,
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if !stopped && let Err(error) = out.flush() {
                    shared.fail(format!("writing failed: {}", error.kind()));
                    stopped = true;
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
        // Another producer may have filled the queue while this one waited for the lock.
        if self.shared.halted.load(Ordering::SeqCst) {
            return;
        }
        let line = Line {
            kind,
            text: build(producer.seq, self.shared.elapsed_ms()),
        };
        match producer.queue.try_send(line) {
            Ok(()) => producer.seq += 1,
            Err(TrySendError::Full(_)) => {
                self.shared.overflowed.store(true, Ordering::SeqCst);
                self.shared
                    .fail("the recorder fell behind: its queue was full, frames were lost");
            }
            Err(TrySendError::Disconnected(_)) => {
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
    pub fn finish(self) -> RecordingSummary {
        self.close(true)
    }

    /// Like [`RecordingGuard::finish`], but a recording of nothing (no frame, no command, no
    /// failure) is not kept, and an existing file at the path stays as it was.
    pub fn finish_unless_empty(self) -> RecordingSummary {
        self.close(false)
    }

    fn close(mut self, keep_empty: bool) -> RecordingSummary {
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
        let empty = counts.frames == 0 && counts.commands == 0 && self.shared.failure().is_none();
        let written = self.temp.take();
        if empty && !keep_empty {
            let _ = std::fs::remove_file(written.as_ref().unwrap_or(&self.path));
        } else if let Some(temp) = written
            && let Err(error) = std::fs::rename(&temp, &self.path)
        {
            let _ = std::fs::remove_file(&temp);
            self.shared.fail(format!(
                "moving the recording into place failed: {}",
                error.kind()
            ));
        }
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
        // A replacement that was never finished is discarded; the old file stays.
        if let Some(temp) = self.temp.take() {
            if let Some(writer) = self.writer.take() {
                let _ = writer.join();
            }
            let _ = std::fs::remove_file(temp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_queue_still_writes_what_was_queued() {
        let (shared, lines) = new_shared(4);
        let sink = RecordSink {
            shared: Arc::clone(&shared),
        };
        for n in 0..6 {
            sink.command(1, &serde_json::json!({"command": "heartbeat", "n": n}));
        }
        let reason = shared.failure().expect("the fifth did not fit");
        assert!(reason.contains("fell behind"), "{reason}");
        shared.stop.store(true, Ordering::SeqCst);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queued.jsonl");
        let file = File::create(&path).unwrap();
        let counts = write_loop(BufWriter::new(file), &lines, &shared, u64::MAX, 0);
        assert_eq!(counts.commands, 4);
        let text = std::fs::read_to_string(&path).unwrap();
        let written: Vec<&str> = text.lines().collect();
        assert_eq!(written.len(), 5, "{text}");
        assert!(written[3].contains("\"n\":3"), "{text}");
        assert!(
            written[4].contains("\"type\":\"end\"")
                && written[4].contains("\"commands\":4")
                && written[4].contains("fell behind"),
            "{text}"
        );
    }
}
