//! Recorded emulator session -> scenario TOML.
//!
//! The conversion follows the commands given to the station (they carry the intent: plug in,
//! start a session, ...) and uses the frames only to learn how the CSMS answered a rejected
//! Authorize or StartTransaction. Anything without a scenario step, and anything whose
//! meaning would have to be guessed, is left out and listed in a comment and in the warnings.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use serde_json::{Map, Value, json};

use super::read::{CommandLine, Entry, FrameLine, Recording};
use super::redact::holds_redacted;
use super::toml_out::{escape_placeholders, step, string};
use super::{RecordError, Role, at_text, clean, frame_parts};
use crate::ocpp::Protocol;
use crate::ocpp::frame::Direction;
use crate::scenario::Scenario;

/// Most warnings kept (the rest is counted).
const MAX_WARNINGS: usize = 100;

/// The result of [`to_scenario`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioConversion {
    /// The scenario file; it loads with [`Scenario::from_toml`].
    pub toml: String,
    /// Number of `[[steps]]` written.
    pub steps: usize,
    /// What was skipped or changed, one sentence each.
    pub warnings: Vec<String>,
}

/// Converts an emulator recording into a scenario.
///
/// # Errors
/// A recording from another role or OCPP version, one without any command to convert, or
/// (an internal error) a result that does not load as a scenario.
pub fn to_scenario(recording: &Recording) -> Result<ScenarioConversion, RecordError> {
    if recording.header.role != Role::Emulator {
        return Err(RecordError::invalid(
            "only a recording made by the emulator can become a scenario (this one was written by another role)",
        ));
    }
    if recording.header.ocpp_version != Protocol::V16.ocpp_version() {
        return Err(RecordError::invalid(format!(
            "this build converts OCPP 1.6 recordings only (the recording says `{}`)",
            clean(&recording.header.ocpp_version, 20)
        )));
    }
    let mut converter = Converter::new(recording);
    converter.run();
    if converter.steps == 0 {
        return Err(RecordError::invalid(
            "the recording holds no command that maps to a scenario step (nothing to convert)",
        ));
    }
    let toml = converter.finish();
    // Never hand out a file that does not load.
    Scenario::from_toml(&toml).map_err(|error| {
        RecordError::invalid(format!("the converted scenario is not valid: {error}"))
    })?;
    Ok(ScenarioConversion {
        toml,
        steps: converter.steps,
        warnings: converter.warnings,
    })
}

/// Where the CALLs a command caused can be found.
#[derive(Clone, Copy)]
struct Scope {
    seq: u64,
    session: u64,
    /// Sequence number of the next command of the same kind.
    limit: u64,
}

struct Converter<'a> {
    recording: &'a Recording,
    /// `out` CALLs by order: (seq, session, action, message id).
    calls: Vec<(u64, u64, &'a str, &'a str)>,
    /// Answers received (`in` CALLRESULT / CALLERROR) by message id: (is error, payload).
    answers: HashMap<&'a str, (bool, &'a Value)>,
    claimed: HashSet<&'a str>,
    body: String,
    steps: usize,
    warnings: Vec<String>,
    dropped_warnings: usize,
    skipped: usize,
    connect_seen: bool,
}

impl<'a> Converter<'a> {
    fn new(recording: &'a Recording) -> Self {
        let mut calls = Vec::new();
        let mut answers = HashMap::new();
        for entry in &recording.entries {
            let Entry::Frame(line) = entry else { continue };
            let Some(frame) = line.frame.as_ref() else {
                continue;
            };
            let Some((kind, id, action)) = frame_parts(frame) else {
                continue;
            };
            match (line.direction, kind) {
                (Direction::Out, 2) => {
                    if let Some(action) = action {
                        calls.push((line.seq, line.session, action, id));
                    }
                }
                (Direction::In, 3 | 4) => {
                    let payload = frame.get(if kind == 3 { 2 } else { 4 });
                    if let Some(payload) = payload {
                        answers.insert(id, (kind == 4, payload));
                    }
                }
                _ => {}
            }
        }
        Self {
            recording,
            calls,
            answers,
            claimed: HashSet::new(),
            body: String::new(),
            steps: 0,
            warnings: Vec::new(),
            dropped_warnings: 0,
            skipped: 0,
            connect_seen: false,
        }
    }

    fn warn(&mut self, text: String) {
        if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push(text);
        } else {
            self.dropped_warnings += 1;
        }
    }

    fn run(&mut self) {
        if let Some(reason) = self.recording.incompleteness() {
            self.warn(format!(
                "{reason}; the scenario may stop before the end of the session"
            ));
        }
        let recording = self.recording;
        let commands: Vec<&CommandLine> = recording
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Command(line) => Some(line),
                Entry::Frame(_) => None,
            })
            .collect();
        // For each command, where the next command of the same kind starts: the CALLs of this
        // command are searched before that point only, so it never claims the CALL of a later
        // one (an Authorize decided offline sends nothing).
        let mut limits = vec![u64::MAX; commands.len()];
        let mut next_of_kind: HashMap<&str, u64> = HashMap::new();
        for (index, line) in commands.iter().enumerate().rev() {
            let kind = line
                .command
                .as_ref()
                .and_then(|command| command.get("command"))
                .and_then(Value::as_str)
                .unwrap_or("");
            limits[index] = next_of_kind.get(kind).copied().unwrap_or(u64::MAX);
            next_of_kind.insert(kind, line.seq);
        }
        for (line, limit) in commands.into_iter().zip(limits) {
            let at = at_text(line.elapsed_ms);
            match &line.command {
                Some(Value::Object(command)) => {
                    let scope = Scope {
                        seq: line.seq,
                        session: line.session,
                        limit,
                    };
                    self.command(command, scope, &at);
                }
                _ => self.skip("a command that was too large to record", &at),
            }
        }
    }

    fn skip(&mut self, what: &str, at: &str) {
        self.skipped += 1;
        if self.skipped <= MAX_WARNINGS {
            let _ = writeln!(self.body, "# skipped at {at}: {what}");
        }
        self.warn(format!("skipped {what} at {at}"));
    }

    fn push(&mut self, mut fields: Value, at: &str) {
        // Recorded text is data: a `${` in it must not become a placeholder.
        escape_placeholders(&mut fields);
        let Value::Object(fields) = fields else {
            return;
        };
        match step(&fields) {
            Ok(text) => {
                self.body.push_str(&text);
                self.body.push('\n');
                self.steps += 1;
            }
            Err(error) => {
                let action = fields
                    .get("action")
                    .and_then(Value::as_str)
                    .unwrap_or("step");
                self.skip(&format!("a `{action}` step ({error})"), at);
            }
        }
    }

    /// The answer to the first unclaimed CALL of `action` that the command (`scope`) caused:
    /// same session, after it and before the next command of its kind. No match, no answer.
    fn answer_after(&mut self, scope: Scope, action: &str) -> Option<(bool, &'a Value)> {
        let (_, _, _, id) = *self.calls.iter().find(|(call_seq, session, name, id)| {
            *call_seq > scope.seq
                && *call_seq < scope.limit
                && *session == scope.session
                && *name == action
                && !self.claimed.contains(id)
        })?;
        self.claimed.insert(id);
        self.answers.get(id).copied()
    }

    /// `expect = { status = ... }` for a rejected Authorize / StartTransaction.
    fn rejected_status(&mut self, scope: Scope, action: &str) -> Option<String> {
        let (is_error, payload) = self.answer_after(scope, action)?;
        if is_error {
            return None;
        }
        let status = payload.pointer("/idTagInfo/status")?.as_str()?;
        (status != "Accepted").then(|| status.to_owned())
    }

    #[allow(clippy::too_many_lines)]
    fn command(&mut self, command: &Map<String, Value>, scope: Scope, at: &str) {
        let tag = command.get("command").and_then(Value::as_str).unwrap_or("");
        let get = |key: &str| command.get(key).filter(|value| !value.is_null()).cloned();
        let mut fields = Map::new();
        let action = |fields: &mut Map<String, Value>, name: &str| {
            fields.insert("action".to_owned(), json!(name));
        };
        let copy = |fields: &mut Map<String, Value>, keys: &[&str]| {
            for key in keys {
                if let Some(value) = get(key) {
                    fields.insert((*key).to_owned(), value);
                }
            }
        };
        match tag {
            "connect" => {
                // The run connects by itself before the first step.
                if !std::mem::replace(&mut self.connect_seen, true) {
                    return;
                }
                action(&mut fields, "connect");
                self.push(Value::Object(fields), at);
                self.wait_registered(at);
            }
            "reconnect" => {
                action(&mut fields, "reconnect");
                self.push(Value::Object(fields), at);
                self.wait_registered(at);
            }
            "disconnect" => {
                action(&mut fields, "disconnect");
                self.push(Value::Object(fields), at);
                self.push(
                    json!({"action": "wait_for", "condition": {"kind": "disconnected"}}),
                    at,
                );
            }
            "boot" | "heartbeat" | "request_certificate" => {
                action(&mut fields, tag);
                self.push(Value::Object(fields), at);
            }
            "plug_in" | "unplug" | "clear_fault" => {
                action(
                    &mut fields,
                    match tag {
                        "plug_in" => "plug",
                        other => other,
                    },
                );
                copy(&mut fields, &["connector"]);
                self.push(Value::Object(fields), at);
            }
            "set_connector_status" => {
                action(&mut fields, "status");
                copy(&mut fields, &["connector", "status"]);
                self.push(Value::Object(fields), at);
            }
            "fault" => {
                action(&mut fields, "fault");
                copy(&mut fields, &["connector", "error_code", "info"]);
                if get("vendor_error_code").is_some() {
                    self.warn(format!(
                        "the vendor error code of the fault at {at} is not part of the `fault` step and was left out"
                    ));
                }
                self.push(Value::Object(fields), at);
            }
            "authorize" => {
                action(&mut fields, "authorize");
                copy(&mut fields, &["id_tag"]);
                if let Some(status) = self.rejected_status(scope, "Authorize") {
                    fields.insert("expect".to_owned(), json!({ "status": status }));
                }
                self.push(Value::Object(fields), at);
            }
            "start_transaction" => {
                action(&mut fields, "start_session");
                copy(&mut fields, &["connector", "id_tag"]);
                if let Some(status) = self.rejected_status(scope, "StartTransaction") {
                    fields.insert("expect".to_owned(), json!({ "status": status }));
                }
                self.push(Value::Object(fields), at);
            }
            "stop_transaction" => {
                action(&mut fields, "stop_session");
                copy(&mut fields, &["connector", "reason", "id_tag"]);
                self.push(Value::Object(fields), at);
            }
            "meter_values" => {
                action(&mut fields, "meter");
                copy(&mut fields, &["connector", "wh_delta"]);
                if get("measurands").is_some() {
                    self.warn(format!(
                        "the measurands of the MeterValues at {at} are not part of the `meter` step; the station's configured ones are used"
                    ));
                }
                self.push(Value::Object(fields), at);
            }
            "data_transfer" => {
                if get("data").as_ref().is_some_and(holds_redacted) {
                    self.skip("a data_transfer command whose data was redacted", at);
                    return;
                }
                action(&mut fields, "data_transfer");
                copy(&mut fields, &["vendor_id", "message_id", "data"]);
                self.push(Value::Object(fields), at);
            }
            "send_security_event" => {
                action(&mut fields, "security_event");
                copy(&mut fields, &["event_type", "tech_info"]);
                self.push(Value::Object(fields), at);
            }
            "send_raw" => {
                let call = get("action").and_then(|v| v.as_str().map(str::to_owned));
                let Some(call) = call else {
                    self.skip("a send_raw command without an action", at);
                    return;
                };
                let payload = get("payload");
                if payload.as_ref().is_some_and(holds_redacted) {
                    self.skip("a send_raw command whose payload was redacted", at);
                    return;
                }
                action(&mut fields, "send_raw");
                fields.insert("call".to_owned(), json!(call));
                if let Some(payload) = payload {
                    fields.insert("payload".to_owned(), payload);
                }
                if get("validate") == Some(json!(false)) {
                    fields.insert("validate".to_owned(), json!(false));
                }
                if let Some((true, _)) = self.answer_after(scope, &call) {
                    fields.insert("expect".to_owned(), json!({ "error": true }));
                }
                self.push(Value::Object(fields), at);
            }
            "set_response_rule" => {
                let rule = get("rule").and_then(|rule| {
                    serde_json::from_value::<crate::station::ResponseRule>(rule).ok()
                });
                if let Some(rule) = rule {
                    action(&mut fields, "set_rule");
                    fields.insert("rule".to_owned(), json!(rule.to_string()));
                    self.push(Value::Object(fields), at);
                } else {
                    self.skip("a response rule that could not be read", at);
                }
            }
            "clear_response_rules" => {
                action(&mut fields, "clear_rules");
                if let Some(call) = get("action") {
                    fields.insert("call".to_owned(), call);
                }
                self.push(Value::Object(fields), at);
            }
            // Bookkeeping of scenario runs and the shutdown: nothing to reproduce.
            "shutdown" | "clear_scripted_replies" => {}
            other => {
                let label = format!("`{}` (no scenario step for it)", clean(other, 40));
                self.skip(&label, at);
            }
        }
    }

    fn wait_registered(&mut self, at: &str) {
        self.push(
            json!({"action": "wait_for", "condition": {"kind": "registration", "status": "Accepted"}}),
            at,
        );
    }

    fn finish(&self) -> String {
        let station = &self.recording.header.station;
        let safe: String = station
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .take(48)
            .collect();
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# Converted from a recording of station {} (recorded {}, kirby-ocpp {}).",
            clean(station, 60),
            clean(&self.recording.header.started_at, 40),
            clean(&self.recording.header.tool_version, 30)
        );
        out.push_str(
            "# The steps follow the commands given to the station, in order. The scenario connects\n# and waits for the BootNotification itself, so the recorded first connect is not a step.\n",
        );
        let mut csms: BTreeMap<&str, u64> = BTreeMap::new();
        for entry in &self.recording.entries {
            if let Entry::Frame(FrameLine {
                direction: Direction::In,
                frame: Some(frame),
                ..
            }) = entry
                && let Some((2, _, Some(action))) = frame_parts(frame)
            {
                *csms.entry(action).or_default() += 1;
            }
        }
        if !csms.is_empty() {
            let list: Vec<String> = csms
                .iter()
                .take(20)
                .map(|(action, count)| format!("{} x{count}", clean(action, 40)))
                .collect();
            let _ = writeln!(
                out,
                "# The CSMS sent these commands during the recording; a scenario cannot make the CSMS send\n# them, so they are not steps: {}.",
                list.join(", ")
            );
        }
        if !self.warnings.is_empty() {
            out.push_str("# Warnings:\n");
            for warning in &self.warnings {
                let _ = writeln!(out, "#   {}", warning.replace('\n', " "));
            }
            if self.dropped_warnings > 0 {
                let _ = writeln!(out, "#   ... and {} more", self.dropped_warnings);
            }
        }
        let _ = writeln!(out, "name = {}", string(&format!("recorded-{safe}")));
        let _ = writeln!(
            out,
            "description = {}",
            string(&format!("Recorded session of {}", clean(station, 60)))
        );
        out.push_str("ocpp_version = \"1.6\"\nschema = 1\n\n");
        out.push_str(&self.body);
        out
    }
}
