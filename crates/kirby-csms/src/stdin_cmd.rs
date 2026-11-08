//! JSON-line commands on stdin.
//!
//! * `{"station":"CP-1","action":"Reset","payload":{"type":"Soft"},"timeout_ms":5000,"id":"my-tag"}`
//!   sends the CALL (payload defaults to `{}`, `timeout_ms` to `--call-timeout`, `id` is optional
//!   and echoed back) and prints `{"type":"result","station":..,"action":..,"ok":true,"result":{..}}`
//!   or `{"type":"result",..,"ok":false,"error":"..."}`.
//! * `{"cmd":"stations"}` prints `{"type":"stations","stations":[..]}`.
//!
//! Every line gets exactly one answer line; a bad line never stops the processing.

use std::io::BufRead;
use std::sync::Arc;

use kirby_csms_core::CsmsHandle;
use serde_json::{Value, json};
use tokio::sync::{Semaphore, mpsc};

use crate::commands;
use crate::headless::Output;

/// Longest input line accepted, bytes; a longer one is answered with an error and discarded.
const MAX_LINE_BYTES: usize = 1024 * 1024;
/// Lines buffered between the reader thread and the dispatcher.
const QUEUE_LINES: usize = 256;

/// One unit of input from the reader thread.
#[derive(Debug, PartialEq, Eq)]
enum Input {
    /// A complete line, without the newline.
    Line(Vec<u8>),
    /// A line longer than the limit; its bytes were discarded.
    TooLong,
}

/// Splits `reader` into lines of at most `limit` bytes. Bytes beyond the limit are dropped
/// without being buffered. Returns when the input ends, a read fails or `send` returns false.
fn read_lines(mut reader: impl BufRead, limit: usize, mut send: impl FnMut(Input) -> bool) {
    let mut line: Vec<u8> = Vec::new();
    let mut too_long = false;
    loop {
        let chunk = match reader.fill_buf() {
            Ok(chunk) => chunk,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        if chunk.is_empty() {
            // End of input: a last line without a newline still counts.
            if too_long {
                send(Input::TooLong);
            } else if !line.is_empty() {
                send(Input::Line(line));
            }
            return;
        }
        let (part, ended) = match chunk.iter().position(|byte| *byte == b'\n') {
            Some(at) => (chunk.split_at(at).0, true),
            None => (chunk, false),
        };
        let consumed = part.len() + usize::from(ended);
        if !too_long {
            if line.len() + part.len() > limit {
                too_long = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(part);
            }
        }
        reader.consume(consumed);
        if ended {
            let item = if too_long {
                Input::TooLong
            } else {
                Input::Line(std::mem::take(&mut line))
            };
            too_long = false;
            if !send(item) {
                return;
            }
        }
    }
}

/// Starts reading stdin on a plain thread (a blocked tokio stdin read would hold up the
/// runtime shutdown) and answers every line in its own task, so one slow command does not
/// delay the others. End of input just ends the reader; the server keeps running.
pub fn spawn(handle: CsmsHandle, out: Arc<Output>, slots: Arc<Semaphore>) {
    let (tx, mut rx) = mpsc::channel::<Input>(QUEUE_LINES);
    std::thread::spawn(move || {
        read_lines(std::io::stdin().lock(), MAX_LINE_BYTES, |item| {
            tx.blocking_send(item).is_ok()
        });
    });
    tokio::spawn(async move {
        while let Some(input) = rx.recv().await {
            let bytes = match input {
                Input::Line(bytes) => bytes,
                Input::TooLong => {
                    out.json_line(&failure(
                        None,
                        None,
                        None,
                        &format!("line longer than {MAX_LINE_BYTES} bytes, discarded"),
                    ));
                    continue;
                }
            };
            let Ok(line) = String::from_utf8(bytes) else {
                out.json_line(&failure(None, None, None, "line is not valid UTF-8"));
                continue;
            };
            if line.trim().is_empty() {
                continue;
            }
            // Backpressure: the reader stalls (and so does the pipe) while all slots are busy.
            let Ok(permit) = Arc::clone(&slots).acquire_owned().await else {
                return;
            };
            let handle = handle.clone();
            let out = Arc::clone(&out);
            tokio::spawn(async move {
                let answer = handle_line(&handle, &line).await;
                out.json_line(&answer);
                drop(permit);
            });
        }
    });
}

/// Executes one input line and returns the line to print.
pub async fn handle_line(handle: &CsmsHandle, line: &str) -> Value {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(error) => return failure(None, None, None, &format!("invalid JSON: {error}")),
    };
    let Some(object) = value.as_object() else {
        return failure(None, None, None, "a command must be a JSON object");
    };
    let tag = object.get("id").cloned();
    if let Some(cmd) = object.get("cmd") {
        let mut answer = match cmd.as_str() {
            Some("stations") => json!({"type": "stations", "stations": commands::stations(handle)}),
            Some("history") => history(handle, object),
            Some("forget") => forget(handle, object),
            Some("rules") => rules(handle, object),
            Some("faults") => faults(handle, object),
            _ => failure(
                None,
                None,
                None,
                &format!(
                    "unknown cmd {cmd}; supported: \"stations\", \"history\", \"forget\", \
                     \"rules\", \"faults\""
                ),
            ),
        };
        add_tag(&mut answer, tag);
        return answer;
    }
    let text = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let (Some(station), Some(action)) = (text("station"), text("action")) else {
        return failure(
            tag,
            text("station"),
            text("action"),
            "needs string fields `station` and `action` (or `cmd`)",
        );
    };
    let payload = object.get("payload").cloned().unwrap_or_else(|| json!({}));
    let timeout = match commands::parse_timeout(object.get("timeout_ms")) {
        Ok(timeout) => timeout,
        Err(message) => return failure(tag, Some(station), Some(action), message),
    };
    let mut answer = commands::call(handle, station, action, payload, timeout)
        .await
        .value;
    add_tag(&mut answer, tag);
    answer
}

type Object = serde_json::Map<String, Value>;

/// An optional text field; a field of another type or an empty text is an error.
fn opt_text<'a>(object: &'a Object, key: &str) -> Result<Option<&'a str>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if !text.is_empty() => Ok(Some(text)),
        Some(_) => Err(format!("`{key}` must be a non-empty string")),
    }
}

fn req_text<'a>(object: &'a Object, key: &str) -> Result<&'a str, String> {
    opt_text(object, key)?.ok_or_else(|| format!("needs a string field `{key}`"))
}

/// The `result`-shaped failure answer of a `cmd` that went wrong.
fn cmd_failure(object: &Object, error: &str) -> Value {
    let station = object.get("station").and_then(Value::as_str);
    failure(None, station, None, &kirby_ocpp_tui::text::sanitize(error))
}

fn history(handle: &CsmsHandle, object: &Object) -> Value {
    let run = || -> Result<Value, String> {
        let station = req_text(object, "station")?;
        let limit = match object.get("limit") {
            None | Some(Value::Null) => usize::MAX,
            Some(value) => value
                .as_u64()
                .and_then(|limit| usize::try_from(limit).ok())
                .ok_or("`limit` must be a non-negative integer")?,
        };
        let entries = handle
            .history_tail(station, limit)
            .ok_or("unknown station")?;
        Ok(json!({
            "type": "history", "station": station,
            "frames": commands::history_frames(station, &entries),
        }))
    };
    run().unwrap_or_else(|error| cmd_failure(object, &error))
}

fn forget(handle: &CsmsHandle, object: &Object) -> Value {
    let run = || -> Result<Value, String> {
        let station = req_text(object, "station")?;
        match handle.forget_if_disconnected(station) {
            None => Err("unknown station".to_owned()),
            Some(false) => {
                Err("station is connected; only a disconnected station can be forgotten".to_owned())
            }
            Some(true) => Ok(json!({"type": "forgotten", "station": station, "forgotten": true})),
        }
    };
    run().unwrap_or_else(|error| cmd_failure(object, &error))
}

/// `{"cmd":"rules","op":"list"|"set"|"clear", ...}`; `op` defaults to `list`.
fn rules(handle: &CsmsHandle, object: &Object) -> Value {
    let run = || -> Result<Value, String> {
        let station = opt_text(object, "station")?;
        let protocol = opt_text(object, "protocol")?;
        let scope = commands::scope_of(station, protocol)?;
        match opt_text(object, "op")?.unwrap_or("list") {
            "list" => {
                let scoped: Vec<Value> = handle
                    .scoped_rules(station)
                    .iter()
                    .map(commands::rule_json)
                    .collect();
                Ok(json!({
                    "type": "rules", "global": handle.responder_actions(), "scoped": scoped,
                }))
            }
            op @ ("set" | "clear") => {
                let action = req_text(object, "action")?;
                if !commands::is_rule_action(action, protocol) {
                    return Err(format!(
                        "{action} is not a known charge-point action for protocol {}",
                        protocol.unwrap_or(kirby_csms_core::ACTIONS_PROTOCOL)
                    ));
                }
                if op == "set" {
                    let payload = object
                        .get("payload")
                        .filter(|payload| payload.is_object())
                        .ok_or("`payload` must be a JSON object (the CALLRESULT payload)")?;
                    commands::set_rule(handle, &scope, action, payload.clone())?;
                    Ok(json!({
                        "type": "rule", "station": station, "protocol": protocol,
                        "action": action, "set": true,
                    }))
                } else {
                    let cleared = commands::clear_rule(handle, &scope, action);
                    Ok(json!({
                        "type": "rule", "station": station, "protocol": protocol,
                        "action": action, "cleared": cleared,
                    }))
                }
            }
            _ => Err("`op` must be one of: list, set, clear".to_owned()),
        }
    };
    run().unwrap_or_else(|error| cmd_failure(object, &error))
}

/// `{"cmd":"faults","op":"list"|"inject"|"remove"|"clear", ...}`; `op` defaults to `list`.
fn faults(handle: &CsmsHandle, object: &Object) -> Value {
    let run = || -> Result<Value, String> {
        let station = opt_text(object, "station")?;
        match opt_text(object, "op")?.unwrap_or("list") {
            "list" => Ok(json!({
                "type": "faults",
                "remaining": handle.faults_remaining(),
                "faults": commands::faults_list(handle, station),
            })),
            "inject" => {
                let scope = commands::scope_of(station, opt_text(object, "protocol")?)?;
                let action = req_text(object, "action")?;
                let kind: commands::FaultKind =
                    serde_json::from_value(object.get("kind").cloned().unwrap_or(Value::Null))
                        .map_err(|_| {
                            "`kind` must be one of: call_error, silence, close, invalid_payload"
                                .to_owned()
                        })?;
                let times = match object.get("times") {
                    None | Some(Value::Null) => 1,
                    Some(value) => value
                        .as_u64()
                        .and_then(|times| usize::try_from(times).ok())
                        .ok_or("`times` must be a positive integer")?,
                };
                let payload = object.get("payload").cloned();
                commands::check_fault(action, kind, payload.as_ref(), times)?;
                let id = handle.try_inject_scoped(
                    commands::build_fault(action, kind, payload, times),
                    scope,
                )?;
                Ok(json!({
                    "type": "fault", "injected": true, "fault_id": id,
                    "remaining": handle.faults_remaining(),
                }))
            }
            "remove" => {
                let id = req_text(object, "fault_id")?;
                Ok(json!({"type": "fault", "fault_id": id, "removed": handle.remove_fault(id)}))
            }
            "clear" => {
                handle.clear_faults();
                Ok(json!({"type": "faults", "remaining": 0, "faults": []}))
            }
            _ => Err("`op` must be one of: list, inject, remove, clear".to_owned()),
        }
    };
    run().unwrap_or_else(|error| cmd_failure(object, &error))
}

fn failure(tag: Option<Value>, station: Option<&str>, action: Option<&str>, error: &str) -> Value {
    let mut answer = commands::failure(station, action, error);
    add_tag(&mut answer, tag);
    answer
}

fn add_tag(answer: &mut Value, tag: Option<Value>) {
    if let (Some(tag), Some(object)) = (tag, answer.as_object_mut()) {
        object.insert("id".to_owned(), tag);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn lines(input: &[u8], limit: usize) -> Vec<Input> {
        let mut out = Vec::new();
        read_lines(Cursor::new(input.to_vec()), limit, |item| {
            out.push(item);
            true
        });
        out
    }

    #[test]
    fn splits_lines_and_keeps_an_unterminated_last_one() {
        assert_eq!(
            lines(b"ab\n\ncd", 16),
            [
                Input::Line(b"ab".to_vec()),
                Input::Line(Vec::new()),
                Input::Line(b"cd".to_vec())
            ]
        );
    }

    #[test]
    fn long_lines_are_reported_once_and_the_rest_continues() {
        let mut input = vec![b'x'; 40];
        input.extend_from_slice(b"\nok\n");
        input.extend_from_slice(&[b'y'; 40]); // no newline at the end
        assert_eq!(
            lines(&input, 16),
            [Input::TooLong, Input::Line(b"ok".to_vec()), Input::TooLong]
        );
    }

    #[test]
    fn invalid_utf8_passes_through_as_bytes() {
        assert_eq!(lines(b"\xff\xfe\n", 16), [Input::Line(vec![0xff, 0xfe])]);
    }
}
