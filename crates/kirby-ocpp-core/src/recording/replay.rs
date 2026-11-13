//! Recorded emulator session -> a scenario that replays the CALLs the emulator originated.
//!
//! Only `out` CALLs are replayed, in recorded order, with fresh message ids (the station
//! assigns them). A transaction id the CSMS gave in the recording is replaced by the id of
//! the matching new StartTransaction answer. The BootNotification is not replayed: the
//! replaying station boots itself with its own configuration. A payload that holds a
//! redacted value (an `AuthorizationKey`, a URL password) is never sent.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use serde_json::{Value, json};

use super::read::{Entry, Recording};
use super::redact::holds_redacted;
use super::toml_out::{escape_placeholders, step, string};
use super::{RecordError, Role, at_text, clean, frame_parts};
use crate::ocpp::frame::Direction;
use crate::scenario::Scenario;

/// Longest single pause (the `sleep` step's limit).
const MAX_PAUSE_MS: u64 = 86_400_000;
/// Most warnings kept.
const MAX_WARNINGS: usize = 100;

/// How a replay is paced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReplayOptions {
    /// Time scale: 2.0 replays twice as fast as recorded.
    pub speed: f64,
    /// Send the CALLs back to back.
    pub no_delay: bool,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            speed: 1.0,
            no_delay: false,
        }
    }
}

/// What [`plan_replay`] decided.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayPlan {
    pub scenario: Scenario,
    /// CALLs that will be sent.
    pub replayed: usize,
    /// CALLs left out (each one has a warning, except the BootNotification).
    pub skipped: usize,
    pub warnings: Vec<String>,
}

/// Builds the replay of `recording`.
///
/// # Errors
/// A recording from another role or OCPP version, one without a replayable CALL, or one that
/// is too long for a scenario.
#[allow(clippy::too_many_lines)]
pub fn plan_replay(
    recording: &Recording,
    options: &ReplayOptions,
) -> Result<ReplayPlan, RecordError> {
    if recording.header.role != Role::Emulator {
        return Err(RecordError::invalid(
            "only a recording made by the emulator can be replayed against a CSMS",
        ));
    }
    if recording.header.ocpp_version != "1.6" {
        return Err(RecordError::invalid(format!(
            "this build replays OCPP 1.6 recordings only (the recording says `{}`)",
            clean(&recording.header.ocpp_version, 20)
        )));
    }
    let mut warnings = Vec::new();
    let warn = |text: String, warnings: &mut Vec<String>| {
        if warnings.len() < MAX_WARNINGS {
            warnings.push(text);
        }
    };
    if let Some(reason) = recording.incompleteness() {
        warn(
            format!("{reason}; the replay covers what was recorded"),
            &mut warnings,
        );
    }

    // Answers by message id, to know what to expect and to learn transaction ids.
    let mut answers: HashMap<&str, (bool, &Value)> = HashMap::new();
    for entry in &recording.entries {
        if let Entry::Frame(line) = entry
            && line.direction == Direction::In
            && let Some(frame) = &line.frame
            && let Some((kind @ (3 | 4), id, _)) = frame_parts(frame)
            && let Some(payload) = frame.get(if kind == 3 { 2 } else { 4 })
        {
            answers.insert(id, (kind == 4, payload));
        }
    }

    let mut body = String::new();
    let mut replayed = 0usize;
    let mut skipped = 0usize;
    let mut booted = 0usize;
    // old transaction id -> variable holding the new one; call id -> variable to capture.
    let mut tx_vars: HashMap<i64, String> = HashMap::new();
    let mut start_vars: HashMap<&str, String> = HashMap::new();
    let mut unknown_tx: HashSet<i64> = HashSet::new();
    let mut previous_ms: Option<u64> = None;
    let mut next_tx = 1usize;

    for entry in &recording.entries {
        let Entry::Frame(line) = entry else { continue };
        // Learn the new transaction id mapping in order: an answer to a start we replayed.
        if line.direction == Direction::In
            && let Some(frame) = &line.frame
            && let Some((3, id, _)) = frame_parts(frame)
            && let Some(var) = start_vars.get(id)
            && let Some(old) = frame.pointer("/2/transactionId").and_then(Value::as_i64)
        {
            tx_vars.insert(old, var.clone());
        }
        if line.direction != Direction::Out {
            continue;
        }
        let Some(frame) = &line.frame else {
            if let Some(size) = line.truncated_bytes {
                skipped += 1;
                warn(
                    format!(
                        "skipped a frame at {}: it was larger than the recorder keeps ({size} bytes)",
                        at_text(line.elapsed_ms)
                    ),
                    &mut warnings,
                );
            }
            continue;
        };
        let Some((2, id, Some(action))) = frame_parts(frame) else {
            continue;
        };
        let at = at_text(line.elapsed_ms);
        let shown = clean(action, 40);
        if action == "BootNotification" {
            booted += 1;
            continue;
        }
        let Some(&(is_error, _)) = answers.get(id) else {
            skipped += 1;
            warn(
                format!("skipped {shown} at {at}: the recording has no answer to it"),
                &mut warnings,
            );
            continue;
        };
        let Some(mut payload) = frame.get(3).cloned() else {
            skipped += 1;
            warn(
                format!("skipped {shown} at {at}: the frame has no payload"),
                &mut warnings,
            );
            continue;
        };
        if holds_redacted(&payload) {
            skipped += 1;
            warn(
                format!(
                    "skipped {shown} at {at}: the payload holds a redacted value (a credential is never replayed)"
                ),
                &mut warnings,
            );
            continue;
        }
        escape_placeholders(&mut payload);
        if let Some(old) = payload.get("transactionId").and_then(Value::as_i64) {
            if let Some(var) = tx_vars.get(&old) {
                payload["transactionId"] = Value::String(format!("${{{var}}}"));
            } else if unknown_tx.insert(old) {
                warn(
                    format!(
                        "the {shown} at {at} uses transaction id {old}, which was not started in the recording; it is sent as recorded"
                    ),
                    &mut warnings,
                );
            }
        }
        // Pacing: the recorded gap between two replayed CALLs, scaled.
        if !options.no_delay {
            if let Some(previous) = previous_ms {
                let gap = line.elapsed_ms.saturating_sub(previous);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    clippy::cast_precision_loss,
                    reason = "a pause in milliseconds, clamped below"
                )]
                let scaled = ((gap as f64) / options.speed)
                    .round()
                    .clamp(0.0, MAX_PAUSE_MS as f64) as u64;
                if scaled > 0 {
                    emit(
                        &mut body,
                        json!({"action": "sleep", "ms": scaled}),
                        &mut skipped,
                        &mut warnings,
                        &at,
                    );
                }
            }
            previous_ms = Some(line.elapsed_ms);
        }
        let mut fields = json!({
            "action": "send_raw",
            "name": format!("{shown} {}", clean(id, 40)),
            "call": action,
            "payload": payload,
            "validate": false,
        });
        if is_error {
            fields["expect"] = json!({ "error": true });
        }
        if action == "StartTransaction" {
            let var = format!("tx{next_tx}");
            next_tx += 1;
            let mut capture = serde_json::Map::new();
            capture.insert(var.clone(), json!("/transactionId"));
            fields["capture"] = Value::Object(capture);
            start_vars.insert(id, var);
        }
        if emit(&mut body, fields, &mut skipped, &mut warnings, &at) {
            replayed += 1;
        }
    }
    if replayed == 0 {
        return Err(RecordError::invalid(
            "the recording holds no CALL the emulator sent that can be replayed",
        ));
    }

    let station = &recording.header.station;
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
    let mut text = String::new();
    let _ = writeln!(text, "name = {}", string(&format!("replay-{safe}")));
    let _ = writeln!(
        text,
        "description = {}",
        string(&format!(
            "Replay of the CALLs recorded from {} ({booted} BootNotification not replayed)",
            clean(station, 60)
        ))
    );
    text.push_str("ocpp_version = \"1.6\"\nschema = 1\n\n");
    text.push_str(&body);
    let scenario = Scenario::from_toml(&text).map_err(|error| {
        RecordError::invalid(format!("the replay does not fit a scenario: {error}"))
    })?;
    Ok(ReplayPlan {
        scenario,
        replayed,
        skipped,
        warnings,
    })
}

/// Appends one step; a value TOML cannot hold skips the step with a warning.
fn emit(
    body: &mut String,
    fields: Value,
    skipped: &mut usize,
    warnings: &mut Vec<String>,
    at: &str,
) -> bool {
    let Value::Object(fields) = fields else {
        return false;
    };
    match step(&fields) {
        Ok(text) => {
            body.push_str(&text);
            body.push('\n');
            true
        }
        Err(error) => {
            *skipped += 1;
            if warnings.len() < MAX_WARNINGS {
                warnings.push(format!("skipped the step at {at}: {error}"));
            }
            false
        }
    }
}
