//! Non-blocking stdout for headless log output.
//!
//! A reader that stops reading (a full pipe, a FIFO with no reader) must neither stall the
//! caller (a tokio worker or the log file) nor panic it. Lines go through a bounded queue to a
//! dedicated writer thread; when the queue is full the line is dropped and counted, and the
//! next line that gets through is preceded by a note with the count.

use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// Lines that may wait for a slow reader before new ones are dropped.
const QUEUE: usize = 8192;

/// Writes `text` to stdout and ignores every error (a closed pipe must not panic).
pub fn print_lossy(text: &str) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
}

/// Queue to a stdout writer thread. See the module documentation.
#[derive(Debug)]
pub struct StdoutPump {
    tx: Mutex<Option<SyncSender<String>>>,
    dropped: Arc<AtomicU64>,
    done: Mutex<Option<Receiver<()>>>,
}

impl StdoutPump {
    /// Starts the writer thread. `json` picks the form of the "lines were dropped" note.
    pub fn new(json: bool) -> Self {
        let (tx, rx) = mpsc::sync_channel::<String>(QUEUE);
        let (done_tx, done) = mpsc::channel();
        let dropped = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&dropped);
        let spawned = std::thread::Builder::new()
            .name("stdout-pump".to_owned())
            .spawn(move || {
                let note = |missed: u64| {
                    let text = format!(
                        "{missed} output lines were dropped (stdout is not read fast enough)"
                    );
                    if json {
                        serde_json::json!({"type": "note", "text": text}).to_string()
                    } else {
                        format!("note: {text}")
                    }
                };
                for line in rx {
                    let missed = counter.swap(0, Ordering::Relaxed);
                    if missed > 0 {
                        print_lossy(&format!("{}\n", note(missed)));
                    }
                    print_lossy(&format!("{line}\n"));
                }
                let missed = counter.swap(0, Ordering::Relaxed);
                if missed > 0 {
                    print_lossy(&format!("{}\n", note(missed)));
                }
                let _ = done_tx.send(());
            });
        // Without a thread there is no stdout output at all; the log file is unaffected.
        let tx = spawned.is_ok().then_some(tx);
        Self {
            tx: Mutex::new(tx),
            dropped,
            done: Mutex::new(Some(done)),
        }
    }

    /// Queues one line (no trailing newline). Never blocks.
    pub fn send(&self, line: &str) {
        let guard = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(tx) = guard.as_ref() else { return };
        match tx.try_send(line.to_owned()) {
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
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
