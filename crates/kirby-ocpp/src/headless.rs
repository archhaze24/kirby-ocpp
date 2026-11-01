//! Headless output: every station [`Event`] as a human line or a JSON line.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;

use kirby_ocpp_core::StationHandle;
use kirby_ocpp_core::fleet::{FleetEvents, FleetItem};
use kirby_ocpp_core::station::{Direction, Event};
use serde_json::{Value, json};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

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
}

impl Sink {
    /// Subscribes to the station now (so nothing after this call is missed).
    pub fn spawn(handle: &StationHandle, json: bool, log_file: Option<&Path>) -> io::Result<Self> {
        let file = log_file
            .map(|path| OpenOptions::new().create(true).append(true).open(path))
            .transpose()?
            .map(BufWriter::new);
        let mut rx = handle.subscribe();
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut out = Output { json, file };
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
        });
        Ok(Self { stop, task })
    }

    /// Like [`Sink::spawn`] for a whole fleet: every line is tagged with its station id.
    pub fn spawn_fleet(
        mut events: FleetEvents,
        json: bool,
        log_file: Option<&Path>,
    ) -> io::Result<Self> {
        let file = log_file
            .map(|path| OpenOptions::new().create(true).append(true).open(path))
            .transpose()?
            .map(BufWriter::new);
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut out = Output { json, file };
            loop {
                tokio::select! {
                    item = events.recv() => match item {
                        Some(item) => out.write_item(&item),
                        None => return,
                    },
                    _ = &mut stopped => break,
                }
            }
            // Whatever is still queued (the closing frames of a shutdown).
            events.stop();
            while let Some(item) = events.recv().await {
                out.write_item(&item);
            }
        });
        Ok(Self { stop, task })
    }

    /// Writes everything that is queued, then ends the task.
    pub async fn finish(self) {
        let _ = self.stop.send(());
        let _ = self.task.await;
    }
}

struct Output {
    json: bool,
    file: Option<BufWriter<File>>,
}

impl Output {
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
        // A closed pipe must not kill the station.
        let _ = writeln!(io::stdout().lock(), "{line}");
        if let Some(file) = &mut self.file {
            let _ = writeln!(file, "{line}").and_then(|()| file.flush());
        }
    }
}
