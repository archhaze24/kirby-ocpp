//! Validation and correlation of CSMS recordings before replay.
//!
//! The whole file is checked before anything binds or sends. The plan keeps the CSMS CALLs in
//! recorded order and, per station, the recorded `StartTransaction` identities that a
//! `RemoteStopTransaction` is later tied to.

use std::collections::{BTreeMap, HashMap};

use kirby_ocpp_core::ocpp::frame::{Direction, Frame};
use kirby_ocpp_core::recording::{Entry, Recording, Role, holds_redacted};
use serde_json::Value;

/// Longest station id and `idTag` the plan accepts.
const MAX_STATION_ID: usize = 128;
const MAX_ID_TAG: usize = kirby_csms_core::MAX_OBSERVED_ID_TAG;

/// A CALL the CSMS sent in the recording.
#[derive(Debug, Clone)]
pub(super) struct Step {
    pub station: String,
    pub elapsed_ms: u64,
    pub action: String,
    /// The recorded payload, unchanged; placeholders such as `${x}` are literal text.
    pub payload: Value,
    /// For `RemoteStopTransaction`: the position of the recorded `StartTransaction` of this
    /// station that owns the recorded transaction id.
    pub start: Option<usize>,
    /// The recording holds a CALLERROR as the answer.
    pub expect_error: bool,
    /// The station was booted (answer `Accepted`) before this CALL in the recording.
    pub needs_boot: bool,
}

/// A recorded `StartTransaction` of one station, in order of arrival.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RecordedStart {
    pub connector: u32,
    pub id_tag: String,
}

#[derive(Debug, Default)]
pub(super) struct StationPlan {
    pub starts: Vec<RecordedStart>,
}

#[derive(Debug)]
pub(super) struct Plan {
    pub steps: Vec<Step>,
    pub stations: BTreeMap<String, StationPlan>,
    /// Recorded time from the first to the last replayed CALL.
    pub span_ms: u64,
}

struct Pending {
    outbound: bool,
    action: String,
    /// Index into `steps` (outbound) or into the station's starts (a StartTransaction).
    index: Option<usize>,
}

#[derive(Default)]
struct StationScratch {
    session: u64,
    /// `starts` index, recorded transaction id and whether the transaction is still running.
    started: Vec<(Option<i32>, bool)>,
    boot_answered: bool,
}

fn transaction_id(value: &Value) -> Option<i32> {
    value.as_i64().and_then(|id| i32::try_from(id).ok())
}

fn holds_key(value: &Value, key: &str) -> bool {
    match value {
        Value::Array(items) => items.iter().any(|item| holds_key(item, key)),
        Value::Object(fields) => {
            fields.contains_key(key) || fields.values().any(|item| holds_key(item, key))
        }
        _ => false,
    }
}

/// Validate the entire file before the runner sends any CALL.
#[allow(clippy::too_many_lines)]
pub(super) fn plan(recording: &Recording) -> Result<Plan, String> {
    if recording.header.role != Role::Csms || recording.header.ocpp_version != "1.6" {
        return Err("expected an OCPP 1.6 CSMS recording".into());
    }
    if !recording.is_complete() || recording.ignored_lines != 0 {
        return Err("the recording is incomplete or contains unknown lines".into());
    }
    let Some(end) = &recording.end else {
        return Err("the recording has no end line".into());
    };
    if end.commands != 0 || end.frames != recording.entries.len() as u64 {
        return Err("the recording frame count does not match its end line".into());
    }
    let mut pending = HashMap::<(String, bool, String), Pending>::new();
    let mut steps = Vec::<Step>::new();
    let mut stations = BTreeMap::<String, StationPlan>::new();
    let mut scratch = HashMap::<String, StationScratch>::new();
    let mut previous_ms = 0;
    for (index, entry) in recording.entries.iter().enumerate() {
        let Entry::Frame(line) = entry else {
            return Err("a CSMS recording cannot contain commands".into());
        };
        if line.seq != index as u64
            || line.elapsed_ms < previous_ms
            || line.elapsed_ms > end.elapsed_ms
        {
            return Err("the recording sequence or elapsed time is invalid".into());
        }
        previous_ms = line.elapsed_ms;
        let Some(station) = line.station.as_ref().filter(|s| {
            !s.is_empty()
                && s.len() <= MAX_STATION_ID
                && !s.chars().any(kirby_ocpp_core::text::is_unsafe_char)
        }) else {
            return Err("a frame has an invalid station id".into());
        };
        if line.session == 0 {
            return Err("a frame has an invalid session".into());
        }
        let scratch_entry = scratch.entry(station.clone()).or_default();
        if scratch_entry.session == 0 {
            scratch_entry.session = line.session;
        } else if scratch_entry.session != line.session {
            return Err(
                "a station has more than one session in the recording; record one connection per station"
                    .into(),
            );
        }
        let Some(value) = &line.frame else {
            return Err("a recording frame is missing or truncated".into());
        };
        if line.truncated_bytes.is_some() || line.invalid_bytes.is_some() {
            return Err("a recording frame is truncated or invalid".into());
        }
        let frame =
            Frame::parse(&value.to_string()).map_err(|_| "a recording frame is malformed")?;
        if frame.id().is_empty() {
            return Err("a recording frame has an empty message id".into());
        }
        let outbound = line.direction == Direction::Out;
        match frame {
            Frame::Call {
                id,
                action,
                payload,
            } => {
                if action.is_empty() {
                    return Err("a recording CALL has an empty action".into());
                }
                let mut slot = None;
                if outbound {
                    if holds_redacted(&payload) {
                        return Err(
                            "a recorded CALL holds a redacted value (a credential is never replayed)"
                                .into(),
                        );
                    }
                    let mut start = None;
                    if action == "RemoteStopTransaction" {
                        let id = payload
                            .get("transactionId")
                            .and_then(transaction_id)
                            .ok_or(
                                "a recorded RemoteStopTransaction has no valid transaction id",
                            )?;
                        let mut owners = scratch_entry
                            .started
                            .iter()
                            .enumerate()
                            .filter(|(_, (tx, running))| *running && *tx == Some(id));
                        let owner = owners.next().map(|(position, _)| position);
                        if owners.next().is_some() {
                            return Err("a recorded transaction id is active more than once".into());
                        }
                        start = Some(owner.ok_or(
                            "a recorded RemoteStopTransaction names a transaction that was not started and answered earlier in the recording",
                        )?);
                    } else if holds_key(&payload, "transactionId") {
                        return Err(format!(
                            "a recorded {} carries a transaction id; only RemoteStopTransaction is supported",
                            kirby_ocpp_core::text::escape_unsafe(&action)
                        ));
                    }
                    slot = Some(steps.len());
                    steps.push(Step {
                        station: station.clone(),
                        elapsed_ms: line.elapsed_ms,
                        action: action.clone(),
                        payload,
                        start,
                        expect_error: false,
                        needs_boot: scratch_entry.boot_answered,
                    });
                } else if action == "StartTransaction" {
                    let connector = payload
                        .get("connectorId")
                        .and_then(Value::as_u64)
                        .and_then(|n| u32::try_from(n).ok());
                    let id_tag = payload
                        .get("idTag")
                        .and_then(Value::as_str)
                        .filter(|tag| tag.len() <= MAX_ID_TAG);
                    let (Some(connector), Some(id_tag)) = (connector, id_tag) else {
                        return Err("a recorded StartTransaction has no valid identity".into());
                    };
                    let plan = stations.entry(station.clone()).or_default();
                    slot = Some(plan.starts.len());
                    plan.starts.push(RecordedStart {
                        connector,
                        id_tag: id_tag.to_owned(),
                    });
                    scratch_entry.started.push((None, false));
                } else if action == "StopTransaction" {
                    let id = payload
                        .get("transactionId")
                        .and_then(transaction_id)
                        .ok_or("a recorded StopTransaction has no valid transaction id")?;
                    for (tx, running) in &mut scratch_entry.started {
                        if *tx == Some(id) {
                            *running = false;
                        }
                    }
                }
                let key = (station.clone(), outbound, id);
                if pending
                    .insert(
                        key,
                        Pending {
                            outbound,
                            action,
                            index: slot,
                        },
                    )
                    .is_some()
                {
                    return Err("a recording contains ambiguous concurrent CALL ids".into());
                }
            }
            reply => {
                let key = (station.clone(), !outbound, reply.id().to_owned());
                let Some(call) = pending.remove(&key) else {
                    return Err("a recording reply has no matching CALL".into());
                };
                if call.outbound {
                    if let Some(step) = call.index.and_then(|index| steps.get_mut(index)) {
                        step.expect_error = !matches!(reply, Frame::CallResult { .. });
                    }
                } else if let Frame::CallResult { payload, .. } = &reply {
                    match call.action.as_str() {
                        "StartTransaction" => {
                            let id = payload.get("transactionId").and_then(transaction_id);
                            if id.is_none()
                                || payload
                                    .pointer("/idTagInfo/status")
                                    .and_then(Value::as_str)
                                    .is_none()
                            {
                                return Err(
                                    "a recorded StartTransaction reply is missing transaction identity"
                                        .into(),
                                );
                            }
                            if let Some(slot) = call
                                .index
                                .and_then(|index| scratch_entry.started.get_mut(index))
                            {
                                *slot = (id, true);
                            }
                        }
                        "BootNotification"
                            if payload.get("status").and_then(Value::as_str)
                                == Some("Accepted") =>
                        {
                            scratch_entry.boot_answered = true;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    // An unanswered station CALL (a fault that stayed silent) changes nothing for the replay,
    // except a start, whose live counterpart could not be tied to a recorded answer.
    if pending
        .values()
        .any(|call| call.outbound || (call.action == "StartTransaction"))
    {
        return Err("a recorded CSMS CALL or StartTransaction has no matching reply".into());
    }
    let (Some(first), Some(last)) = (steps.first(), steps.last()) else {
        return Err("the recording contains no CSMS CALLs".into());
    };
    let span_ms = last.elapsed_ms - first.elapsed_ms;
    for step in &steps {
        stations.entry(step.station.clone()).or_default();
    }
    // Only stations that receive a CALL are waited for.
    stations.retain(|station, _| steps.iter().any(|step| &step.station == station));
    Ok(Plan {
        steps,
        stations,
        span_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn recording(frames: Vec<Value>) -> Recording {
        let count = frames.len();
        let mut lines = vec![
            json!({"type":"header", "schema":1, "ocpp_version":"1.6", "role":"csms"}).to_string(),
        ];
        for (seq, mut frame) in frames.into_iter().enumerate() {
            frame["type"] = json!("frame");
            frame["seq"] = json!(seq);
            frame["elapsed_ms"] = json!(seq);
            lines.push(frame.to_string());
        }
        lines.push(
            json!({"type":"end", "frames":count, "commands":0, "elapsed_ms":count}).to_string(),
        );
        Recording::parse(format!("{}\n", lines.join("\n")).as_bytes()).unwrap()
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "call sites pass json! literals"
    )]
    fn frame(direction: &str, session: u64, value: Value) -> Value {
        json!({"station":"CP-1", "session":session, "direction":direction, "frame":value})
    }

    fn sample() -> Recording {
        recording(vec![
            frame(
                "in",
                1,
                json!([2,"s","StartTransaction",{"connectorId":1,"idTag":"${literal}"}]),
            ),
            frame(
                "out",
                1,
                json!([3,"s",{"transactionId":7,"idTagInfo":{"status":"Accepted"}}]),
            ),
            frame(
                "out",
                1,
                json!([2,"x","RemoteStopTransaction",{"transactionId":7}]),
            ),
            frame("in", 1, json!([3,"x",{"status":"Accepted"}])),
        ])
    }

    fn renumber(record: &mut Recording) {
        for (seq, entry) in record.entries.iter_mut().enumerate() {
            if let Entry::Frame(line) = entry {
                line.seq = seq as u64;
                line.elapsed_ms = seq as u64;
            }
        }
        let end = record.end.as_mut().unwrap();
        end.frames = record.entries.len() as u64;
        end.elapsed_ms = record.entries.len() as u64;
    }

    #[test]
    fn ties_a_stop_to_its_start_and_keeps_payloads_literal() {
        let result = plan(&sample()).unwrap();
        assert_eq!(result.steps.len(), 1);
        assert_eq!(result.steps[0].start, Some(0));
        assert_eq!(result.steps[0].payload["transactionId"], 7);
        assert_eq!(result.stations["CP-1"].starts[0].id_tag, "${literal}");
        assert!(!result.steps[0].expect_error);
    }

    #[test]
    fn rejects_gaps_missing_frames_and_incomplete_files() {
        let mut record = sample();
        if let Entry::Frame(line) = &mut record.entries[1] {
            line.seq = 4;
        }
        assert!(plan(&record).is_err());
        let mut record = sample();
        if let Entry::Frame(line) = &mut record.entries[1] {
            line.frame = None;
        }
        assert!(plan(&record).is_err());
        let mut record = sample();
        record.ignored_lines = 1;
        assert!(plan(&record).is_err());
        record.ignored_lines = 0;
        record.end.as_mut().unwrap().incomplete = Some("overflow".into());
        assert!(plan(&record).is_err());
    }

    #[test]
    fn redaction_only_matters_for_replayed_calls() {
        let mut record = sample();
        if let Entry::Frame(line) = &mut record.entries[0] {
            line.frame = Some(json!([2,"s","StartTransaction",{"connectorId":1,"idTag":"***"}]));
        }
        assert!(
            plan(&record).is_ok(),
            "an inbound masked value is never sent"
        );
        let masked = recording(vec![
            frame(
                "out",
                1,
                json!([2,"c","ChangeConfiguration",{"key":"AuthorizationKey","value":"***"}]),
            ),
            frame("in", 1, json!([3,"c",{"status":"Accepted"}])),
        ]);
        assert!(plan(&masked).unwrap_err().contains("redacted"));
    }

    #[test]
    fn rejects_ambiguous_ids_and_several_sessions_of_a_station() {
        let mut record = sample();
        record.entries.insert(1, record.entries[0].clone());
        renumber(&mut record);
        assert!(plan(&record).is_err());
        let mut record = sample();
        if let Entry::Frame(line) = &mut record.entries[2] {
            line.session = 2;
        }
        assert!(plan(&record).unwrap_err().contains("more than one session"));
    }

    #[test]
    fn rejects_unknown_stopped_and_duplicate_transactions() {
        let mut record = sample();
        if let Entry::Frame(line) = &mut record.entries[2] {
            line.frame = Some(json!([2,"x","RemoteStopTransaction",{"transactionId":8}]));
        }
        assert!(plan(&record).is_err());
        let mut record = sample();
        if let Entry::Frame(line) = &mut record.entries[2] {
            line.frame = Some(json!([2, "x", "RemoteStopTransaction", {}]));
        }
        assert!(plan(&record).is_err());
        // The same id twice in a row: ambiguous, whichever start a stop means.
        let mut record = sample();
        let again: Vec<_> = vec![
            frame(
                "in",
                1,
                json!([2,"t","StartTransaction",{"connectorId":2,"idTag":"B"}]),
            ),
            frame(
                "out",
                1,
                json!([3,"t",{"transactionId":7,"idTagInfo":{"status":"Accepted"}}]),
            ),
        ];
        for (offset, value) in again.into_iter().enumerate() {
            let mut line = record.entries[0].clone();
            if let Entry::Frame(frame_line) = &mut line {
                frame_line.frame = Some(value["frame"].clone());
                frame_line.direction = if offset == 0 {
                    Direction::In
                } else {
                    Direction::Out
                };
            }
            record.entries.insert(2 + offset, line);
        }
        renumber(&mut record);
        assert!(plan(&record).unwrap_err().contains("more than once"));
    }

    #[test]
    fn rejects_foreign_transaction_ids_in_other_calls() {
        let record = recording(vec![
            frame(
                "out",
                1,
                json!([2,"p","SetChargingProfile",{"connectorId":1,"csChargingProfiles":{"transactionId":3}}]),
            ),
            frame("in", 1, json!([3,"p",{"status":"Accepted"}])),
        ]);
        assert!(plan(&record).unwrap_err().contains("transaction id"));
    }

    #[test]
    fn unanswered_outbound_calls_are_refused_but_unanswered_inbound_ones_are_not() {
        let silent = recording(vec![frame(
            "out",
            1,
            json!([2,"r","Reset",{"type":"Soft"}]),
        )]);
        assert!(plan(&silent).is_err());
        let heartbeat = recording(vec![
            frame("in", 1, json!([2, "h", "Heartbeat", {}])),
            frame("out", 1, json!([2,"r","Reset",{"type":"Soft"}])),
            frame("in", 1, json!([4, "r", "NotSupported", "", {}])),
        ]);
        let planned = plan(&heartbeat).unwrap();
        assert!(planned.steps[0].expect_error);
    }

    #[test]
    fn a_boot_before_the_call_becomes_a_prerequisite() {
        let record = recording(vec![
            frame("out", 1, json!([2,"a","Reset",{"type":"Soft"}])),
            frame("in", 1, json!([3,"a",{"status":"Accepted"}])),
            frame(
                "in",
                1,
                json!([2,"b","BootNotification",{"chargePointVendor":"V","chargePointModel":"M"}]),
            ),
            frame(
                "out",
                1,
                json!([3,"b",{"status":"Accepted","currentTime":"2026-01-01T00:00:00Z","interval":60}]),
            ),
            frame("out", 1, json!([2,"c","Reset",{"type":"Hard"}])),
            frame("in", 1, json!([3,"c",{"status":"Accepted"}])),
        ]);
        let planned = plan(&record).unwrap();
        assert_eq!(
            planned
                .steps
                .iter()
                .map(|s| s.needs_boot)
                .collect::<Vec<_>>(),
            [false, true]
        );
        assert_eq!(planned.span_ms, 4);
    }
}
