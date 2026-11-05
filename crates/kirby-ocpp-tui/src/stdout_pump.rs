//! Non-blocking stdout (and log file) output for the headless modes.
//!
//! A reader that stops reading (a full pipe, a FIFO with no reader) must neither stall the
//! caller (a tokio worker or the log file) nor panic it. Lines go through a bounded queue to a
//! dedicated writer thread; when the queue is full the line is dropped and counted, and the
//! next line that gets through is preceded by a note with the count. The queue is bounded by
//! lines and by bytes, so big frames cannot pile up behind a stalled reader either.

use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// Lines that may wait for a slow reader before new ones are dropped.
const QUEUE: usize = 8192;

/// Bytes that may wait for a slow reader before new lines are dropped.
const QUEUE_BYTES: usize = 16 * 1024 * 1024;

/// Writes `text` to stdout and ignores every error (a closed pipe must not panic).
pub fn print_lossy(text: &str) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
}

/// Queue to a writer thread (stdout or a log file). See the module documentation.
#[derive(Debug)]
pub struct LinePump {
    tx: Mutex<Option<SyncSender<String>>>,
    dropped: Arc<AtomicU64>,
    queued: Arc<AtomicUsize>,
    max_bytes: usize,
    done: Mutex<Option<Receiver<()>>>,
}

/// The pump of the stdout lines.
pub type StdoutPump = LinePump;

impl LinePump {
    /// Starts a pump to stdout. `json` picks the form of the "lines were dropped" note.
    pub fn new(json: bool) -> Self {
        Self::with_sink(
            json,
            "stdout is not read fast enough",
            QUEUE,
            QUEUE_BYTES,
            |text| {
                print_lossy(text);
            },
        )
    }

    /// Starts a pump that appends every line (plus a newline) to `out`, flushing when the
    /// queue runs empty. Write errors are ignored (the stdout copy of the log is unaffected).
    pub fn to_writer(json: bool, mut out: impl Write + Send + 'static) -> Self {
        Self::with_sink(
            json,
            "the log file is not written fast enough",
            QUEUE,
            QUEUE_BYTES,
            move |text| {
                let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
            },
        )
    }

    /// Starts the writer thread. `sink` receives complete lines including the newline.
    fn with_sink(
        json: bool,
        reason: &'static str,
        lines: usize,
        max_bytes: usize,
        mut sink: impl FnMut(&str) + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::sync_channel::<String>(lines);
        let (done_tx, done) = mpsc::channel();
        let dropped = Arc::new(AtomicU64::new(0));
        let queued = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&dropped);
        let bytes = Arc::clone(&queued);
        let spawned = std::thread::Builder::new()
            .name("line-pump".to_owned())
            .spawn(move || {
                let note = |missed: u64| {
                    let text = format!("{missed} output lines were dropped ({reason})");
                    if json {
                        serde_json::json!({"type": "note", "text": text}).to_string()
                    } else {
                        format!("note: {text}")
                    }
                };
                for line in rx {
                    bytes.fetch_sub(line.len(), Ordering::Relaxed);
                    let missed = counter.swap(0, Ordering::Relaxed);
                    if missed > 0 {
                        sink(&format!("{}\n", note(missed)));
                    }
                    sink(&format!("{line}\n"));
                }
                let missed = counter.swap(0, Ordering::Relaxed);
                if missed > 0 {
                    sink(&format!("{}\n", note(missed)));
                }
                let _ = done_tx.send(());
            });
        // Without a thread there is no output at all; the other sink is unaffected.
        let tx = spawned.is_ok().then_some(tx);
        Self {
            tx: Mutex::new(tx),
            dropped,
            queued,
            max_bytes,
            done: Mutex::new(Some(done)),
        }
    }

    /// Queues one line (no trailing newline). Never blocks.
    pub fn send(&self, line: &str) {
        let guard = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(tx) = guard.as_ref() else { return };
        // A single line is always accepted by an empty queue, however big it is.
        let before = self.queued.fetch_add(line.len(), Ordering::Relaxed);
        if before > 0 && before.saturating_add(line.len()) > self.max_bytes {
            self.queued.fetch_sub(line.len(), Ordering::Relaxed);
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        match tx.try_send(line.to_owned()) {
            Err(TrySendError::Full(_)) => {
                self.queued.fetch_sub(line.len(), Ordering::Relaxed);
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Ok(()) => {}
            Err(TrySendError::Disconnected(_)) => {
                self.queued.fetch_sub(line.len(), Ordering::Relaxed);
            }
        }
    }

    /// Stops accepting lines and waits up to `timeout` for the queued ones to be written (a
    /// reader that never reads cannot hold the process back longer than that).
    pub fn finish(&self, timeout: Duration) {
        drop(
            self.tx
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        let done = self
            .done
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(done) = done {
            let _ = done.recv_timeout(timeout);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    struct Sink(mpsc::Sender<String>);
    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let _ = self.0.send(String::from_utf8_lossy(buf).into_owned());
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A pump whose sink blocks until `release` is sent; written lines go to `seen`.
    fn blocked_pump(
        lines: usize,
        max_bytes: usize,
    ) -> (LinePump, mpsc::Sender<()>, Arc<Mutex<Vec<String>>>) {
        let (release, gate) = channel::<()>();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let mut first = true;
        let pump = LinePump::with_sink(false, "test", lines, max_bytes, move |text| {
            if first {
                first = false;
                let _ = gate.recv();
            }
            log.lock().unwrap().push(text.to_owned());
        });
        (pump, release, seen)
    }

    #[test]
    fn byte_budget_drops_and_notes() {
        let (pump, release, seen) = blocked_pump(1000, 100);
        pump.send("first"); // taken by the writer, blocked in the sink
        for _ in 0..20 {
            pump.send(&"x".repeat(40));
        }
        release.send(()).unwrap();
        pump.finish(Duration::from_secs(10));
        let seen = seen.lock().unwrap();
        let lines = seen
            .iter()
            .filter(|l| l.starts_with(&"x".repeat(40)))
            .count();
        assert!(lines < 20, "kept {lines}");
        assert!(
            seen.iter().any(|l| l.contains("lines were dropped")),
            "{seen:?}"
        );
    }

    #[test]
    fn writer_keeps_order_and_flushes_on_finish() {
        let (tx, rx) = channel::<String>();
        let pump = LinePump::to_writer(false, Sink(tx));
        for i in 0..50 {
            pump.send(&format!("line {i}"));
        }
        pump.finish(Duration::from_secs(10));
        let got: Vec<String> = rx.try_iter().collect();
        let want: Vec<String> = (0..50).map(|i| format!("line {i}\n")).collect();
        assert_eq!(got, want);
    }
}
