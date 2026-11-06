//! Headless output: every station [`Event`] as a human line or a JSON line.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use kirby_ocpp_core::StationHandle;
use kirby_ocpp_core::fleet::{FleetEvents, FleetItem};
use kirby_ocpp_core::station::ConnectionState;
use kirby_ocpp_core::station::{Direction, Event};
use kirby_ocpp_tui::stdout_pump::{LinePump, StdoutPump, open_log_file};
use kirby_ocpp_tui::text::{escape_json_unsafe, sanitize};
use serde_json::{Value, json};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

/// How long the process waits at exit for queued stdout lines to be written.
const STDOUT_FLUSH: Duration = Duration::from_secs(2);

/// How long the process waits at exit for queued log file lines to be written.
const FILE_FLUSH: Duration = Duration::from_secs(5);

static STDOUT: OnceLock<StdoutPump> = OnceLock::new();

/// How much `run` and `suite` print (`-q`, default, `-v`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Errors and the final verdict line only.
    Quiet,
    /// The report only.
    Normal,
    /// The report and every event and frame line.
    Verbose,
}

/// Process-wide output settings, fixed at startup.
#[derive(Debug, Clone, Copy)]
struct Style {
    level: Level,
    color: bool,
}

static STYLE: OnceLock<Style> = OnceLock::new();

/// Fixes the verbosity and the colour switch (call once, before any output).
pub fn init_style(level: Level, color: bool) {
    let _ = STYLE.set(Style { level, color });
}

/// The verbosity of this process (`Normal` before [`init_style`]).
pub fn level() -> Level {
    STYLE.get().map_or(Level::Normal, |style| style.level)
}

/// Does `run`/`suite` show event and frame lines on stdout? Always with `--json` (as before).
pub fn events_on_stdout(json: bool) -> bool {
    json && level() != Level::Quiet || level() == Level::Verbose
}

/// Colour on or off: `always` and `never` are explicit; `auto` means a terminal and no
/// (non-empty) `NO_COLOR`.
pub fn resolve_color(choice: crate::cli::ColorChoice, is_terminal: bool, no_color: bool) -> bool {
    use crate::cli::ColorChoice;
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => is_terminal && !no_color,
    }
}

const GREEN: &str = "32";
const RED: &str = "31";
const YELLOW: &str = "33";

/// Wraps the status word of a human line (`PASS`, `FAIL`, `SKIP`, `PASSED`, `FAILED`, `WARN`,
/// or the `[Error]`/`[Warn]`/`[Success]` level of a log line) in an ANSI colour. The line is
/// already sanitized, so no control character of station text reaches the terminal.
fn colorize(line: &str) -> String {
    // A word and the rest of the line, the separating space included.
    fn word(text: &str) -> (&str, &str) {
        text.split_at(text.find(' ').unwrap_or(text.len()))
    }
    let body = line.trim_start();
    let indent = &line[..line.len() - body.len()];
    // Fleet tag `[id] ` in front.
    let after_tag = match body
        .strip_prefix('[')
        .and_then(|rest| rest.split_once("] "))
    {
        Some((_, rest)) if !rest.trim_start().is_empty() => rest.trim_start(),
        _ => body,
    };
    let tag_len = body.len() - after_tag.len();
    let (first, rest) = word(after_tag);
    // Event lines start with a timestamp: the level is the next word.
    let (skip, level, tail) = if first.starts_with(|c: char| c.is_ascii_digit()) {
        let after_space = rest.strip_prefix(' ').unwrap_or(rest);
        let (second, tail) = word(after_space);
        (after_tag.len() - after_space.len(), second, tail)
    } else {
        (0, first, rest)
    };
    let color = match level {
        "PASS" | "PASSED" | "[Success]" => GREEN,
        "FAIL" | "FAILED" | "[Error]" => RED,
        "WARN" | "SKIP" | "[Warn]" => YELLOW,
        _ => return line.to_owned(),
    };
    let head = &body[..tag_len + skip];
    format!("{indent}{head}\x1b[{color}m{level}\x1b[0m{tail}")
}

/// What the connection hints need to know about the configuration.
#[derive(Debug, Clone)]
struct ConnectContext {
    /// The URL is `wss://`.
    secure: bool,
    /// The WebSocket subprotocol the station offers.
    subprotocol: String,
}

static CONNECT: OnceLock<ConnectContext> = OnceLock::new();

/// Remembers the URL scheme and the offered subprotocol for [`hint_for`] (call once).
pub fn init_connect_context(url: &str, subprotocol: &str) {
    let _ = CONNECT.set(ConnectContext {
        secure: url.to_ascii_lowercase().starts_with("wss://"),
        subprotocol: subprotocol.to_owned(),
    });
}

/// One line telling what to check for a connection error, by error class; `None` for errors
/// without a known fix. Version-neutral: the subprotocol comes from the configuration.
pub fn hint_for(error: &str) -> Option<String> {
    let context = CONNECT.get_or_init(|| ConnectContext {
        secure: false,
        subprotocol: String::new(),
    });
    hint_with(error, context)
}

fn hint_with(error: &str, context: &ConnectContext) -> Option<String> {
    let text = error.to_ascii_lowercase();
    let offered = &context.subprotocol;
    if text.contains("unknownissuer")
        || text.contains("unknown issuer")
        || text.contains("unknown ca")
        || text.contains("unknownca")
        || text.contains("unknown certificate authority")
    {
        return Some("use --tls-ca or --tls-skip-verify".to_owned());
    }
    if context.secure
        && text.contains("tls handshake failed")
        && (text.contains("eof") || text.contains("corrupt") || text.contains("invalid message"))
    {
        return Some("the server may speak plain ws://".to_owned());
    }
    if text.contains("cannot reach the central system") && text.contains("refused") {
        return Some("check the host and port and that the CSMS is running".to_owned());
    }
    if let Some(status) = http_status(&text)
        && matches!(status, 400 | 401 | 403 | 404)
    {
        return Some(format!(
            "HTTP status {status}, offered subprotocol `{offered}`: check the URL path and --ws-subprotocol"
        ));
    }
    if text.contains("did not negotiate required websocket subprotocol") {
        return Some(format!(
            "offered subprotocol `{offered}`: check the URL path and --ws-subprotocol"
        ));
    }
    None
}

/// The status code of "answered `HTTP/1.1 404 Not Found`" in a lowercased handshake error.
fn http_status(text: &str) -> Option<u16> {
    let (_, after) = text.split_once("answered `http/")?;
    let (_, code) = after.split_once(' ')?;
    code.get(..3)?.parse().ok()
}

/// `error` plus its hint, as one line: `error. Hint: ...`.
pub fn with_hint(error: &str) -> String {
    match hint_for(error) {
        Some(hint) => format!("{error}. Hint: {hint}"),
        None => error.to_owned(),
    }
}

/// Watches a station's log for the end of the reconnect loop, so a headless run can fail
/// instead of idling forever, and remembers the last connection error for messages.
#[derive(Debug)]
pub struct LinkWatch {
    handle: StationHandle,
    rx: broadcast::Receiver<Event>,
    last_error: Option<String>,
}

impl LinkWatch {
    /// Subscribes now: create it before the station connects, or the first failure is missed.
    pub fn new(handle: &StationHandle) -> Self {
        Self {
            handle: handle.clone(),
            rx: handle.subscribe(),
            last_error: None,
        }
    }

    /// The last connection failure or disconnect seen, with its hint.
    pub fn last_error(&self) -> Option<String> {
        self.last_error.as_deref().map(with_hint)
    }

    /// Resolves with a description when the station gives up reconnecting (`--no-reconnect` or
    /// `--reconnect-max-attempts` used up), or when the station ends.
    pub async fn dead(&mut self) -> String {
        loop {
            match self.rx.recv().await {
                Ok(Event::Log(record)) => {
                    if let Some(error) = record.text.strip_prefix("connection failed: ") {
                        self.last_error = Some(error.to_owned());
                    } else if record.text.starts_with("disconnected") {
                        self.last_error = Some(record.text.clone());
                    } else if let Some(reason) = record.text.strip_prefix("reconnect stopped: ") {
                        return self.describe(reason);
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Events were lost: the state still tells whether the station gave up.
                    if self.handle.snapshot().connection == ConnectionState::Disconnected {
                        return self.describe("the connection is down");
                    }
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return "the station stopped".to_owned();
                }
            }
        }
    }

    fn describe(&self, reason: &str) -> String {
        match self.last_error() {
            Some(last) => format!("reconnect stopped ({reason}); last error: {last}"),
            None => format!("reconnect stopped ({reason})"),
        }
    }
}

/// Sets up the stdout queue; `json` picks the form of its "lines dropped" note.
pub fn init_stdout(json: bool) {
    let _ = STDOUT.set(StdoutPump::new(json));
}

/// Queues one stdout line, made safe for a terminal: control characters, line separators and
/// bidi overrides are replaced (human) or escaped (`json`). Never blocks, never panics.
pub fn stdout_line(line: &str, json: bool) {
    let line = if json {
        escape_json_unsafe(line)
    } else if STYLE.get().is_some_and(|style| style.color) {
        colorize(&sanitize(line))
    } else {
        sanitize(line)
    };
    STDOUT.get_or_init(|| StdoutPump::new(json)).send(&line);
}

/// Writes out the queued stdout lines (bounded wait); call before the process exits.
pub fn flush_stdout() {
    if let Some(pump) = STDOUT.get() {
        pump.finish(STDOUT_FLUSH);
    }
}

/// One event as a JSON object: the serde form plus the extracted `payload` of frames.
pub fn event_json(event: &Event) -> Value {
    let mut value = serde_json::to_value(event).unwrap_or(Value::Null);
    if let (Event::Frame(record), Some(object)) = (event, value.as_object_mut()) {
        let index = match record.frame.get(0).and_then(Value::as_u64) {
            Some(2) => 3,
            Some(3) => 2,
            _ => 99,
        };
        object.insert(
            "payload".to_owned(),
            record.frame.get(index).cloned().unwrap_or(Value::Null),
        );
    }
    value
}

/// One event as a log line; `None` for events that are not worth a line.
pub fn event_human(event: &Event) -> Option<String> {
    match event {
        Event::Frame(record) => {
            let arrow = match record.direction {
                Direction::In => "<-",
                Direction::Out => "->",
            };
            Some(format!(
                "{} {arrow} {}",
                record.ts.format("%H:%M:%S%.3f"),
                record.frame
            ))
        }
        Event::Log(record) => Some(format!(
            "{} [{:?}] {}",
            record.ts.format("%H:%M:%S%.3f"),
            record.level,
            record.text
        )),
        Event::StateChanged => None,
    }
}

/// One line for `event`; with `station` (fleet mode) the line says which station it is from:
/// `[id] ` in front of the human form, a `"station"` field in the JSON form.
fn format_event(event: &Event, json: bool, station: Option<&str>) -> Option<String> {
    if json {
        let mut value = event_json(event);
        if let (Some(station), Some(object)) = (station, value.as_object_mut()) {
            object.insert("station".to_owned(), station.into());
        }
        Some(value.to_string())
    } else {
        let line = event_human(event)?;
        Some(tagged(&line, station))
    }
}

/// `line` with the human fleet tag in front, if there is a station.
pub fn tagged(line: &str, station: Option<&str>) -> String {
    match station {
        Some(station) => format!("[{station}] {line}"),
        None => line.to_owned(),
    }
}

/// Background task printing events to stdout (and a log file) until [`Sink::finish`].
#[derive(Debug)]
pub struct Sink {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
    stdout: Arc<AtomicBool>,
}

impl Sink {
    /// Subscribes to the station now (so nothing after this call is missed).
    pub fn spawn(
        handle: &StationHandle,
        json: bool,
        log_file: Option<&Path>,
        stdout: bool,
    ) -> io::Result<Self> {
        let file = log_file
            .map(open_log_file)
            .transpose()?
            .map(|file| LinePump::to_writer(json, file));
        let stdout_on = Arc::new(AtomicBool::new(stdout));
        let mut rx = handle.subscribe();
        let (stop, mut stopped) = oneshot::channel();
        let flag = Arc::clone(&stdout_on);
        let task = tokio::spawn(async move {
            let mut out = Output {
                json,
                file,
                stdout: flag,
            };
            loop {
                tokio::select! {
                    event = rx.recv() => match event {
                        Ok(event) => out.write(&event, None),
                        Err(broadcast::error::RecvError::Lagged(missed)) => {
                            out.write_note(&format!("{missed} events were dropped"), None);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    _ = &mut stopped => break,
                }
            }
            // Whatever is still queued (the closing frames of a shutdown).
            while let Ok(event) = rx.try_recv() {
                out.write(&event, None);
            }
            out.finish_file().await;
        });
        Ok(Self {
            stop,
            task,
            stdout: stdout_on,
        })
    }

    /// Like [`Sink::spawn`] for a whole fleet: every line is tagged with its station id.
    pub fn spawn_fleet(
        mut events: FleetEvents,
        json: bool,
        log_file: Option<&Path>,
        stdout: bool,
    ) -> io::Result<Self> {
        let file = log_file
            .map(open_log_file)
            .transpose()?
            .map(|file| LinePump::to_writer(json, file));
        let stdout_on = Arc::new(AtomicBool::new(stdout));
        let (stop, mut stopped) = oneshot::channel();
        let flag = Arc::clone(&stdout_on);
        let task = tokio::spawn(async move {
            let mut out = Output {
                json,
                file,
                stdout: flag,
            };
            loop {
                tokio::select! {
                    item = events.recv() => match item {
                        Some(item) => out.write_item(&item),
                        None => break,
                    },
                    _ = &mut stopped => break,
                }
            }
            // Whatever is still queued (the closing frames of a shutdown).
            events.stop();
            while let Some(item) = events.recv().await {
                out.write_item(&item);
            }
            out.finish_file().await;
        });
        Ok(Self {
            stop,
            task,
            stdout: stdout_on,
        })
    }

    /// Starts printing to stdout too (the terminal UI could not be set up).
    pub fn enable_stdout(&self) {
        self.stdout.store(true, Ordering::Relaxed);
    }

    /// Writes everything that is queued, then ends the task.
    pub async fn finish(self) {
        let _ = self.stop.send(());
        let _ = self.task.await;
    }
}

struct Output {
    json: bool,
    /// Written by a helper thread: a stalled file cannot block the runtime.
    file: Option<LinePump>,
    stdout: Arc<AtomicBool>,
}

impl Output {
    /// Waits (off the runtime threads, bounded) until the file thread has written every line.
    async fn finish_file(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = tokio::task::spawn_blocking(move || file.finish(FILE_FLUSH)).await;
        }
    }

    fn write(&mut self, event: &Event, station: Option<&str>) {
        if let Some(line) = format_event(event, self.json, station) {
            self.line(&line);
        }
    }

    fn write_item(&mut self, item: &FleetItem) {
        match item {
            FleetItem::Event(event) => self.write(&event.event, Some(&event.station_id)),
            FleetItem::Lagged { station_id, missed } => {
                self.write_note(&format!("{missed} events were dropped"), Some(station_id));
            }
        }
    }

    fn write_note(&mut self, text: &str, station: Option<&str>) {
        let line = if self.json {
            let mut value = json!({"type": "note", "text": text});
            if let (Some(station), Some(object)) = (station, value.as_object_mut()) {
                object.insert("station".to_owned(), station.into());
            }
            value.to_string()
        } else {
            tagged(&format!("note: {text}"), station)
        };
        self.line(&line);
    }

    fn line(&mut self, line: &str) {
        // Station-supplied text must not forge lines or drive the terminal, on stdout or in
        // the file. The file goes first and stdout never blocks it: a closed or unread pipe
        // costs stdout lines only.
        let line = if self.json {
            escape_json_unsafe(line)
        } else {
            sanitize(line)
        };
        if let Some(file) = &self.file {
            file.send(&line);
        }
        if self.stdout.load(Ordering::Relaxed) {
            stdout_line(&line, self.json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ColorChoice;

    #[test]
    fn colorize_marks_status_words_only() {
        let green = "\x1b[32m";
        let reset = "\x1b[0m";
        assert_eq!(
            colorize("  PASS  1 hello (3 ms)"),
            format!("  {green}PASS{reset}  1 hello (3 ms)")
        );
        assert_eq!(
            colorize("[CP-1]   FAIL  2 x (1 ms): boom"),
            format!("[CP-1]   \x1b[31mFAIL{reset}  2 x (1 ms): boom")
        );
        assert_eq!(
            colorize("PASSED in 5 ms"),
            format!("{green}PASSED{reset} in 5 ms")
        );
        assert_eq!(
            colorize("12:00:01.300 [Warn] slow"),
            format!("12:00:01.300 \x1b[33m[Warn]{reset} slow")
        );
        assert_eq!(
            colorize("[CP-1] 12:00:01.300 [Error]"),
            format!("[CP-1] 12:00:01.300 \x1b[31m[Error]{reset}")
        );
        // Frames, ordinary words and a status word later in the line stay plain.
        for plain in [
            "12:00:01.234 -> [2,\"1\",\"Heartbeat\",{}]",
            "12:00:01.234 [Info] FAIL inside text",
            "suite: 1/2 scenarios passed, FAILED: x",
            "scenario charge-session",
            "",
        ] {
            assert_eq!(colorize(plain), plain);
        }
    }

    #[test]
    fn color_choice_resolution() {
        assert!(resolve_color(ColorChoice::Auto, true, false));
        assert!(!resolve_color(ColorChoice::Auto, true, true));
        assert!(!resolve_color(ColorChoice::Auto, false, false));
        assert!(resolve_color(ColorChoice::Always, false, true));
        assert!(!resolve_color(ColorChoice::Never, true, false));
    }

    #[test]
    fn hints_by_error_class() {
        let plain = ConnectContext {
            secure: false,
            subprotocol: "ocpp1.6".to_owned(),
        };
        let secure = ConnectContext {
            secure: true,
            ..plain.clone()
        };
        let hint = |error: &str, context: &ConnectContext| hint_with(error, context);
        assert!(
            hint(
                "cannot reach the central system: 127.0.0.1:1: Connection refused (os error 111)",
                &plain
            )
            .unwrap()
            .contains("check the host and port")
        );
        assert!(
            hint("TLS handshake failed: peer closed connection without sending TLS close_notify: https://docs.rs/rustls/latest/rustls/manual/_03_howto/index.html#unexpected-eof (eof)", &secure)
                .unwrap()
                .contains("plain ws://")
        );
        // Not a TLS URL: no plain-ws advice.
        assert_eq!(hint("TLS handshake failed: unexpected eof", &plain), None);
        assert_eq!(
            hint(
                "TLS handshake failed: invalid peer certificate: UnknownIssuer",
                &secure
            )
            .unwrap(),
            "use --tls-ca or --tls-skip-verify"
        );
        let status = hint(
            "WebSocket handshake failed: the central system answered `HTTP/1.1 404 Not Found` instead of 101 Switching Protocols",
            &plain,
        )
        .unwrap();
        assert!(
            status.contains("404")
                && status.contains("`ocpp1.6`")
                && status.contains("--ws-subprotocol"),
            "{status}"
        );
        assert!(
            hint(
                "central system did not negotiate required WebSocket subprotocol ocpp1.6",
                &plain
            )
            .unwrap()
            .contains("--ws-subprotocol")
        );
        // Other statuses and unknown errors get no hint.
        assert_eq!(
            hint(
                "WebSocket handshake failed: the central system answered `HTTP/1.1 500 Oops` instead of 101",
                &plain
            ),
            None
        );
        assert_eq!(hint("connection attempt timed out", &plain), None);
    }
}
