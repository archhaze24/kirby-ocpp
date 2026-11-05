//! Headless output: every engine event as a human line or a JSON line, plus the shared
//! output channel (stdout and the optional log file).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use kirby_csms_core::{
    CsmsEvent, CsmsHandle, Direction, DisconnectReason, Frame, StationInfo, StationState,
    TransactionStatus,
};
use kirby_ocpp_tui::stdout_pump::StdoutPump;
use kirby_ocpp_tui::text::escape_json_unsafe;
use serde_json::{Value, json};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

/// Where lines go: stdout (human text, or JSON with `--json`) and, always as JSON, the log file.
/// Shared between the event sink, the stdin commands and the script report.
#[derive(Debug)]
pub struct Output {
    json: bool,
    /// Off while the terminal UI owns the screen: lines then go to the log file only.
    stdout: AtomicBool,
    file: Mutex<Option<BufWriter<File>>>,
    /// Stdout is written by a helper thread behind a bounded queue, so a reader that does not
    /// read can never stall the log file or the server.
    pump: StdoutPump,
}

impl Output {
    pub fn open(json: bool, log_file: Option<&Path>) -> io::Result<Arc<Self>> {
        let file = log_file
            .map(|path| OpenOptions::new().create(true).append(true).open(path))
            .transpose()?
            .map(BufWriter::new);
        Ok(Arc::new(Self {
            json,
            stdout: AtomicBool::new(true),
            file: Mutex::new(file),
            pump: StdoutPump::new(json),
        }))
    }

    /// Writes out the queued stdout lines (bounded wait); call before the process exits.
    pub fn finish(&self) {
        self.pump.finish(Duration::from_secs(2));
    }

    /// Turns the stdout lines on or off (the log file is unaffected).
    pub fn set_stdout(&self, on: bool) {
        self.stdout.store(on, Ordering::Relaxed);
    }

    /// An event: `human` on stdout, or `value` with `--json`; `value` goes to the log file.
    pub fn event(&self, value: &Value, human: &str) {
        let json_line = escape_json_unsafe(&value.to_string());
        if self.json {
            self.write(&json_line, &json_line);
        } else {
            // Station ids and texts come from the network: no control characters on a
            // terminal. Callers sanitize attacker-controlled parts; the newlines of
            // multi-line texts (the script report) are the only control characters kept.
            let clean = human
                .split('\n')
                .map(sanitize)
                .collect::<Vec<_>>()
                .join("\n");
            self.write(&clean, &json_line);
        }
    }

    /// A machine line (stdin command results): JSON on stdout in both modes.
    pub fn json_line(&self, value: &Value) {
        let line = escape_json_unsafe(&value.to_string());
        self.write(&line, &line);
    }

    /// A note about the output itself.
    pub fn note(&self, text: &str) {
        self.event(
            &json!({"type": "note", "text": text}),
            &format!("note: {}", sanitize(text)),
        );
    }

    fn write(&self, stdout_line: &str, file_line: &str) {
        // The file first: it must not wait for stdout. A closed or unread pipe costs stdout
        // lines only.
        {
            let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(file) = file.as_mut() {
                let _ = writeln!(file, "{file_line}").and_then(|()| file.flush());
            }
        }
        if self.stdout.load(Ordering::Relaxed) {
            self.pump.send(stdout_line);
        }
    }
}

/// Replaces control characters, line separators and bidi overrides (which could drive the
/// terminal, or fake extra lines) with a dot.
pub fn sanitize(text: &str) -> String {
    kirby_ocpp_tui::text::sanitize(text)
}

/// Prints the `listening` line.
pub fn announce(out: &Output, handle: &CsmsHandle) {
    let addr = handle.local_addr().to_string();
    let url = handle.url();
    out.event(
        &json!({"type": "listening", "addr": addr, "url": url}),
        &format!("listening on {url}"),
    );
}

/// A compact JSON view of one station: connection, boot info, connector statuses, transactions.
pub fn station_summary(info: &StationInfo, state: Option<&StationState>) -> Value {
    let mut value = json!({
        "id": info.id,
        "connected": info.connected,
        "received_calls": info.received_calls,
    });
    let Some(object) = value.as_object_mut() else {
        return value;
    };
    if let Some(addr) = info.remote_addr {
        object.insert("remote_addr".to_owned(), addr.to_string().into());
    }
    if let Some(state) = state {
        if let Some(boot) = &state.boot {
            object.insert("vendor".to_owned(), boot.vendor.clone().into());
            object.insert("model".to_owned(), boot.model.clone().into());
        }
        object.insert("connectors".to_owned(), connector_statuses(state));
        object.insert("active_transactions".to_owned(), active_transactions(state));
        object.insert(
            "security_events".to_owned(),
            state.security_events_total.into(),
        );
    }
    value
}

fn connector_statuses(state: &StationState) -> Value {
    let map = state
        .connectors
        .iter()
        .map(|(id, connector)| {
            let status = serde_json::to_value(connector.status).unwrap_or(Value::Null);
            (id.to_string(), status)
        })
        .collect::<serde_json::Map<_, _>>();
    Value::Object(map)
}

fn active_transactions(state: &StationState) -> Value {
    state
        .transactions
        .iter()
        .filter(|tx| tx.status == TransactionStatus::Active)
        .map(|tx| json!({"id": tx.id, "connector": tx.connector}))
        .collect()
}

/// Background task printing engine events until [`Sink::finish`].
#[derive(Debug)]
pub struct Sink {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Sink {
    /// Prints the events of `rx` (a receiver subscribed by the caller, so none is missed).
    pub fn spawn(
        handle: &CsmsHandle,
        mut rx: broadcast::Receiver<CsmsEvent>,
        out: Arc<Output>,
    ) -> Self {
        let handle = handle.clone();
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut printer = Printer {
                handle,
                out,
                last_state: HashMap::new(),
            };
            loop {
                tokio::select! {
                    event = rx.recv() => match event {
                        Ok(event) => printer.print(&event),
                        Err(broadcast::error::RecvError::Lagged(missed)) => {
                            printer.out.note(&format!("{missed} events were dropped"));
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    _ = &mut stopped => break,
                }
            }
            // Whatever is still queued (the closing frames of a shutdown).
            while let Ok(event) = rx.try_recv() {
                printer.print(&event);
            }
        });
        Self { stop, task }
    }

    /// Writes everything that is queued, then ends the task.
    pub async fn finish(self) {
        let _ = self.stop.send(());
        let _ = self.task.await;
    }
}

struct Printer {
    handle: CsmsHandle,
    out: Arc<Output>,
    /// Last printed state view per station, to print only changes.
    last_state: HashMap<String, Value>,
}

impl Printer {
    fn print(&mut self, event: &CsmsEvent) {
        let ts = Utc::now();
        let clock = ts.format("%H:%M:%S%.3f");
        let stamp = ts.to_rfc3339_opts(SecondsFormat::Millis, true);
        match event {
            CsmsEvent::Connected { station } => {
                self.out.event(
                    &json!({"type": "connected", "ts": stamp, "station": station}),
                    &format!("{clock} {} connected", sanitize(station)),
                );
            }
            CsmsEvent::Disconnected { station, reason } => {
                let reason = reason_name(*reason);
                self.out.event(
                    &json!({"type": "disconnected", "ts": stamp, "station": station, "reason": reason}),
                    &format!("{clock} {} disconnected ({reason})", sanitize(station)),
                );
            }
            CsmsEvent::Frame {
                station,
                direction,
                frame,
            } => {
                let (name, arrow) = match direction {
                    Direction::In => ("in", "<-"),
                    Direction::Out => ("out", "->"),
                };
                let mut value = frame_json(frame);
                if let Some(object) = value.as_object_mut() {
                    object.insert("type".to_owned(), "frame".into());
                    object.insert("ts".to_owned(), stamp.into());
                    object.insert("station".to_owned(), station.clone().into());
                    object.insert("direction".to_owned(), name.into());
                }
                self.out.event(
                    &value,
                    &format!(
                        "{clock} {} {arrow} {}",
                        sanitize(station),
                        sanitize(&frame.to_json_string())
                    ),
                );
            }
            CsmsEvent::Malformed { station, text } => {
                self.out.event(
                    &json!({"type": "malformed", "ts": stamp, "station": station, "text": text}),
                    &format!(
                        "{clock} {} sent malformed text: {}",
                        sanitize(station),
                        sanitize(text)
                    ),
                );
            }
            CsmsEvent::Forgotten { station } => {
                self.last_state.remove(station);
                self.out.event(
                    &json!({"type": "forgotten", "ts": stamp, "station": station}),
                    &format!("{clock} {} forgotten", sanitize(station)),
                );
            }
            CsmsEvent::StateChanged { station } => {
                self.print_state(station, &stamp, &clock.to_string());
            }
            CsmsEvent::AuthFailed {
                station,
                remote_addr,
                reason,
            } => {
                let who = station
                    .as_deref()
                    .map_or_else(|| "unknown station".to_owned(), sanitize);
                self.out.event(
                    &json!({
                        "type": "auth_failed", "ts": stamp, "station": station,
                        "remote_addr": remote_addr.to_string(), "reason": reason
                    }),
                    &format!(
                        "{clock} {who} refused ({}) from {remote_addr}",
                        sanitize(reason)
                    ),
                );
            }
        }
    }

    /// A compact line when connector statuses or the active transactions changed.
    fn print_state(&mut self, station: &str, stamp: &str, clock: &str) {
        let Some(state) = self.handle.state(station) else {
            return;
        };
        let view = json!({
            "connectors": connector_statuses(&state),
            "active_transactions": active_transactions(&state),
        });
        if self.last_state.get(station) == Some(&view) {
            return;
        }
        let connectors = view["connectors"]
            .as_object()
            .map(|map| {
                map.iter()
                    .map(|(id, status)| format!("{id}={}", status.as_str().unwrap_or("?")))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let active = state
            .transactions
            .iter()
            .filter(|tx| tx.status == TransactionStatus::Active)
            .map(|tx| tx.id.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let human = format!(
            "{clock} {} state: connectors [{connectors}] active transactions [{active}]",
            sanitize(station)
        );
        let mut value = view.clone();
        if let Some(object) = value.as_object_mut() {
            object.insert("type".to_owned(), "state".into());
            object.insert("ts".to_owned(), stamp.into());
            object.insert("station".to_owned(), station.into());
        }
        self.out.event(&value, &human);
        self.last_state.insert(station.to_owned(), view);
    }
}

fn reason_name(reason: DisconnectReason) -> &'static str {
    match reason {
        DisconnectReason::Closed => "closed",
        DisconnectReason::Replaced => "replaced",
        DisconnectReason::Shutdown => "shutdown",
        DisconnectReason::Injected => "injected",
        DisconnectReason::Forgotten => "forgotten",
        DisconnectReason::PingTimeout => "ping_timeout",
    }
}

/// The frame's fields as a JSON object: `kind`, `id`, and `action`/`payload` or the error parts.
pub fn frame_json(frame: &Frame) -> Value {
    match frame {
        Frame::Call {
            id,
            action,
            payload,
        } => json!({"kind": "call", "id": id.clone(), "action": action, "payload": payload}),
        Frame::CallResult { id, payload } => {
            json!({"kind": "result", "id": id.clone(), "payload": payload})
        }
        Frame::CallError {
            id,
            code,
            description,
            details,
        } => json!({
            "kind": "error", "id": id.clone(), "code": code.as_str(),
            "description": description, "details": details
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_are_neutralised() {
        let hostile = "CP\u{1b}]0;x\u{7}\nnext\u{9b}31m";
        let clean = sanitize(hostile);
        assert!(!clean.chars().any(char::is_control), "{clean:?}");
        assert!(clean.starts_with("CP"));
    }
}
