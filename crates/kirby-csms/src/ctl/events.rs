//! `ctl events`: tails the event stream and prints one JSON line per event, reconnecting with
//! `Last-Event-ID` so no event is missed or repeated across a lost connection.

use std::time::Duration;

use clap::Args;

use super::client::{Client, Request};
use super::out::escape_json_text;
use super::requests;

/// The server sends a keep-alive comment every 15 s; a stream silent for this long is dead.
const IDLE: Duration = Duration::from_secs(60);
/// Pause before a reconnect.
#[cfg(not(test))]
const RETRY_PAUSE: Duration = Duration::from_secs(1);
#[cfg(test)]
const RETRY_PAUSE: Duration = Duration::from_millis(10);
/// Failed attempts in a row after which the tail gives up.
const MAX_FAILURES: u32 = 5;
/// Largest event (the server keeps at most 8 MiB of event text).
const MAX_EVENT: usize = 9 * 1024 * 1024;

#[derive(Debug, Args)]
pub struct EventsArgs {
    /// Only the events of this station (and the notes of the stream)
    #[arg(long, value_name = "ID")]
    pub station: Option<String>,
    /// Resume after this event id (the `id` of an earlier event, see --with-id)
    #[arg(long, value_name = "ID")]
    pub last_event_id: Option<u64>,
    /// Stop with exit code 0 after this many events
    #[arg(long, value_name = "COUNT", value_parser = clap::value_parser!(u64).range(1..))]
    pub count: Option<u64>,
    /// Do not reconnect: stop when the stream ends or breaks
    #[arg(long)]
    pub once: bool,
    /// Print `{"id":"...","event":"...","data":{...}}` instead of the bare event object
    #[arg(long)]
    pub with_id: bool,
}

/// One server-sent event.
#[derive(Debug, PartialEq, Eq)]
pub struct Event {
    pub id: Option<String>,
    /// The `event:` line: the event type.
    pub name: Option<String>,
    pub data: String,
}

/// Assembles events from stream bytes: LF and CRLF line ends, comment lines skipped,
/// several `data:` lines joined by a newline, an event ends at a blank line.
#[derive(Debug, Default)]
pub struct Parser {
    line: Vec<u8>,
    id: Option<String>,
    event: Option<String>,
    data: Vec<String>,
    size: usize,
}

impl Parser {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Event>, String> {
        let mut events = Vec::new();
        for &byte in bytes {
            if byte != b'\n' {
                self.line.push(byte);
                if self.line.len() > MAX_EVENT {
                    return Err("an event is larger than 9 MiB".to_owned());
                }
                continue;
            }
            if self.line.last() == Some(&b'\r') {
                self.line.pop();
            }
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.line)).into_owned();
            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push(Event {
                        id: self.id.clone(),
                        name: self.event.take(),
                        data: self.data.join("\n"),
                    });
                }
                self.data.clear();
                self.event = None;
                self.size = 0;
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = line.split_once(':').unwrap_or((&line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            self.size += value.len();
            if self.size > MAX_EVENT {
                return Err("an event is larger than 9 MiB".to_owned());
            }
            match field {
                "id" if !value.contains('\0') => self.id = Some(value.to_owned()),
                "event" => self.event = Some(value.to_owned()),
                "data" => self.data.push(value.to_owned()),
                _ => {}
            }
        }
        Ok(events)
    }
}

/// The printed line of an event.
pub fn event_line(event: &Event, with_id: bool) -> Result<String, String> {
    if serde_json::from_str::<serde_json::Value>(&event.data).is_err() {
        return Err("an event holds no JSON; is --url the control API?".to_owned());
    }
    let line = if with_id {
        let text = |value: Option<&str>| match value {
            Some(value) => serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned()),
            None => "null".to_owned(),
        };
        let data = event.data.replace(['\n', '\r'], " ");
        format!(
            "{{\"id\":{},\"event\":{},\"data\":{data}}}",
            text(event.id.as_deref()),
            text(event.name.as_deref()),
        )
    } else {
        event.data.replace(['\n', '\r'], " ")
    };
    Ok(escape_json_text(&line))
}

/// Why a stream attempt ended.
enum End {
    /// The requested number of events was printed.
    Done,
    /// The stream ended or broke; reconnect unless `--once`.
    Lost(String),
}

/// Receives the printed lines (without a line end).
pub type Emit<'a> = &'a mut dyn FnMut(&str) -> Result<(), String>;

/// Runs the tail. `Ok(())` is a clean stop.
pub async fn tail(client: &Client, args: &EventsArgs, emit: Emit<'_>) -> Result<(), super::Fail> {
    let path = requests::events(args.station.as_deref());
    let mut last_id = args.last_event_id;
    let mut printed = 0_u64;
    let mut failures = 0_u32;
    let mut connected_once = false;
    loop {
        let attempt = attempt(client, &path, args, &mut last_id, &mut printed, &mut *emit).await;
        match attempt {
            Ok(End::Done) => return Ok(()),
            Ok(End::Lost(reason)) => {
                connected_once = true;
                failures = 0;
                if args.once {
                    return if reason.is_empty() {
                        Ok(())
                    } else {
                        Err(super::Fail::Transport(reason))
                    };
                }
            }
            Err(super::Fail::Transport(reason)) => {
                failures += 1;
                if args.once || !connected_once || failures >= MAX_FAILURES {
                    return Err(super::Fail::Transport(reason));
                }
            }
            Err(other) => return Err(other),
        }
        tokio::time::sleep(RETRY_PAUSE).await;
    }
}

async fn attempt(
    client: &Client,
    path: &str,
    args: &EventsArgs,
    last_id: &mut Option<u64>,
    printed: &mut u64,
    emit: Emit<'_>,
) -> Result<End, super::Fail> {
    let resume = last_id.map(|id| id.to_string());
    let headers: Vec<(&str, &str)> = resume
        .as_deref()
        .map(|id| ("Last-Event-ID", id))
        .into_iter()
        .collect();
    let request = Request {
        method: "GET",
        path,
        body: None,
        headers: &headers,
    };
    let opened = tokio::time::timeout(client.timeout, client.open(&request))
        .await
        .map_err(|_| {
            super::Fail::Transport(format!(
                "no answer from {} within {} s (raise --timeout)",
                client.endpoint().display(),
                client.timeout.as_secs()
            ))
        })?
        .map_err(super::Fail::Transport)?;
    let (head, mut body) = opened;
    if head.status != 200 {
        let bytes = body
            .collect(1024 * 1024)
            .await
            .map_err(super::Fail::Transport)?;
        return Err(super::Fail::Api(head.status, bytes));
    }
    let mut parser = Parser::default();
    loop {
        let piece = match tokio::time::timeout(IDLE, body.next()).await {
            Err(_) => return Ok(End::Lost("the event stream went silent".to_owned())),
            Ok(Err(error)) => return Ok(End::Lost(error)),
            Ok(Ok(None)) => return Ok(End::Lost(String::new())),
            Ok(Ok(Some(piece))) => piece,
        };
        let events = parser.feed(&piece).map_err(super::Fail::Transport)?;
        for event in events {
            let line = event_line(&event, args.with_id).map_err(super::Fail::Transport)?;
            emit(&line).map_err(super::Fail::output)?;
            if let Some(id) = event.id.as_deref().and_then(|id| id.parse::<u64>().ok()) {
                *last_id = Some(id);
            }
            *printed += 1;
            if args.count.is_some_and(|count| *printed >= count) {
                return Ok(End::Done);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(chunks: &[&[u8]]) -> Vec<Event> {
        let mut parser = Parser::default();
        let mut all = Vec::new();
        for chunk in chunks {
            all.extend(parser.feed(chunk).unwrap());
        }
        all
    }

    #[test]
    fn events_are_assembled_across_pieces_and_line_ends() {
        let events = feed_all(&[
            b": keep-alive\n\nid: 7\nevent: fra",
            b"me\ndata: {\"a\":1}\n\nid: 8\r\ndata: x\r\ndata:y\r\n\r\n",
        ]);
        assert_eq!(
            events,
            vec![
                Event {
                    id: Some("7".to_owned()),
                    name: Some("frame".to_owned()),
                    data: "{\"a\":1}".to_owned(),
                },
                Event {
                    id: Some("8".to_owned()),
                    name: None,
                    data: "x\ny".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn an_event_without_data_is_not_dispatched() {
        assert_eq!(feed_all(&[b"id: 3\nevent: x\n\n"]), vec![]);
    }

    #[test]
    fn lines_are_escaped_and_must_hold_json() {
        let event = Event {
            id: Some("17".to_owned()),
            name: Some("frame".to_owned()),
            data: "{\"t\":\"a\u{202e}b\"}".to_owned(),
        };
        assert_eq!(event_line(&event, false).unwrap(), "{\"t\":\"a\\u202eb\"}");
        let line = event_line(&event, true).unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["id"], "17");
        assert_eq!(value["event"], "frame");
        assert_eq!(value["data"]["t"], "a\u{202e}b");
        let plain = Event {
            id: None,
            name: None,
            data: "not json".to_owned(),
        };
        assert!(event_line(&plain, false).is_err());
    }

    /// A server that answers each connection with the next entry of `streams` (chunked, then
    /// closed) and records the request heads.
    async fn fake_server(
        streams: Vec<&'static str>,
    ) -> (Client, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let heads = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = heads.clone();
        tokio::spawn(async move {
            let mut listener = Some(listener);
            let count = streams.len();
            for (index, stream) in streams.into_iter().enumerate() {
                let (mut socket, _) = listener.as_ref().unwrap().accept().await.unwrap();
                if index + 1 == count {
                    // Nothing listens any more: later connections are refused.
                    listener = None;
                }
                let mut head = Vec::new();
                let mut byte = [0_u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut byte).await.unwrap();
                    head.push(byte[0]);
                }
                seen.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&head).into_owned());
                let chunk = format!("{:x}\r\n{stream}\r\n0\r\n\r\n", stream.len());
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                     Transfer-Encoding: chunked\r\n\r\n{chunk}"
                );
                socket.write_all(answer.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        let endpoint =
            super::super::client::Endpoint::parse(&format!("http://127.0.0.1:{port}")).unwrap();
        let client = Client::new(
            endpoint,
            None,
            &super::super::client::TlsFiles::default(),
            Duration::from_secs(10),
        )
        .unwrap();
        (client, heads)
    }

    fn args(count: Option<u64>, once: bool, with_id: bool) -> EventsArgs {
        EventsArgs {
            station: None,
            last_event_id: None,
            count,
            once,
            with_id,
        }
    }

    #[tokio::test]
    async fn a_lost_stream_is_resumed_after_the_last_event_id() {
        let (client, heads) = fake_server(vec![
            "id: 10\nevent: a\ndata: {\"n\":1}\n\nid: 11\nevent: a\ndata: {\"n\":2}\n\n",
            "id: 12\nevent: a\ndata: {\"n\":3}\n\n",
        ])
        .await;
        let mut lines = Vec::new();
        let mut emit = |line: &str| {
            lines.push(line.to_owned());
            Ok(())
        };
        let result = tail(&client, &args(Some(3), false, true), &mut emit).await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(lines.len(), 3);
        assert!(lines[2].contains("\"id\":\"12\""), "{lines:?}");
        let heads = heads.lock().unwrap();
        assert_eq!(heads.len(), 2);
        assert!(
            !heads[0].to_lowercase().contains("last-event-id"),
            "{}",
            heads[0]
        );
        assert!(heads[1].contains("Last-Event-ID: 11\r\n"), "{}", heads[1]);
    }

    #[tokio::test]
    async fn a_dead_server_ends_the_tail_after_a_few_attempts_and_once_does_not_retry() {
        // One stream, then the listener task ends and the port refuses connections.
        let (client, heads) = fake_server(vec!["id: 1\nevent: a\ndata: {}\n\n"]).await;
        let mut lines = Vec::new();
        let mut emit = |line: &str| {
            lines.push(line.to_owned());
            Ok(())
        };
        let result = tail(&client, &args(None, false, false), &mut emit).await;
        assert!(
            matches!(result, Err(crate::ctl::Fail::Transport(_))),
            "{result:?}"
        );
        assert_eq!(lines, vec!["{}".to_owned()]);
        assert_eq!(heads.lock().unwrap().len(), 1);
        // A server that is not there at the first attempt fails at once.
        let mut emit = |_: &str| Ok(());
        let result = tail(&client, &args(None, false, false), &mut emit).await;
        assert!(
            matches!(result, Err(crate::ctl::Fail::Transport(_))),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn once_stops_at_the_end_of_the_stream() {
        let (client, _) = fake_server(vec!["id: 1\nevent: a\ndata: {}\n\n"]).await;
        let mut lines = Vec::new();
        let mut emit = |line: &str| {
            lines.push(line.to_owned());
            Ok(())
        };
        let result = tail(&client, &args(None, true, false), &mut emit).await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(lines.len(), 1);
    }

    #[tokio::test]
    async fn a_closed_output_stops_the_tail() {
        let (client, _) = fake_server(vec!["id: 1\nevent: a\ndata: {}\n\n"]).await;
        let mut emit = |_: &str| Err(crate::ctl::out::BROKEN_PIPE.to_owned());
        let result = tail(&client, &args(None, false, false), &mut emit).await;
        assert!(matches!(result, Err(crate::ctl::Fail::Pipe)), "{result:?}");
    }

    #[test]
    fn oversized_events_are_refused() {
        let mut parser = Parser::default();
        let big = vec![b'a'; MAX_EVENT + 1];
        assert!(parser.feed(&big).is_err());
    }
}
