//! Unit tests of the recording format, redaction, reader and converters.

use std::io::{Cursor, Read as _};

use chrono::Utc;
use serde_json::{Value, json};

use super::write::detached;
use super::*;
use crate::ocpp::frame::{Direction, FrameRecord};
use crate::scenario::{Action, Scenario};

fn record(direction: Direction, frame: Value) -> FrameRecord {
    let msg_id = frame.get(1).and_then(Value::as_str).map(str::to_owned);
    let action = frame.get(2).and_then(Value::as_str).map(str::to_owned);
    FrameRecord {
        ts: Utc::now(),
        direction,
        msg_id,
        action,
        frame,
    }
}

fn started(
    dir: &tempfile::TempDir,
    options: &RecordOptions,
) -> (RecordSink, RecordingGuard, std::path::PathBuf) {
    let path = dir.path().join("rec.jsonl");
    let (sink, guard) = start(&path, "1.6", Role::Emulator, "CP-1", options).unwrap();
    (sink, guard, path)
}

#[test]
fn round_trip_keeps_order_counts_and_header() {
    let dir = tempfile::tempdir().unwrap();
    let (sink, guard, path) = started(&dir, &RecordOptions::default());
    sink.command(1, &json!({"command": "heartbeat"}));
    sink.frame(
        1,
        &record(Direction::Out, json!([2, "a1", "Heartbeat", {}])),
    );
    sink.frame(
        1,
        &record(
            Direction::In,
            json!([3, "a1", {"currentTime": "2026-01-01T00:00:00Z"}]),
        ),
    );
    let summary = guard.finish();
    assert_eq!((summary.frames, summary.commands), (2, 1));
    assert_eq!(summary.incomplete, None);

    let read = Recording::read(&path).unwrap();
    assert_eq!(read.header.schema, SCHEMA);
    assert_eq!(read.header.role, Role::Emulator);
    assert_eq!(read.header.station, "CP-1");
    assert_eq!(read.header.ocpp_version, "1.6");
    assert_eq!(read.entries.len(), 3);
    let seqs: Vec<u64> = read.entries.iter().map(Entry::seq).collect();
    assert_eq!(seqs, [0, 1, 2]);
    let times: Vec<u64> = read.entries.iter().map(Entry::elapsed_ms).collect();
    assert!(times.windows(2).all(|pair| pair[0] <= pair[1]));
    let Entry::Frame(frame) = &read.entries[2] else {
        panic!("a frame");
    };
    assert_eq!(frame.direction, Direction::In);
    assert_eq!(frame.action.as_deref(), None);
    assert_eq!(frame.msg_id.as_deref(), Some("a1"));
    assert_eq!(frame.frame.as_ref().unwrap()[0], 3);
    let end = read.end.as_ref().unwrap();
    assert_eq!((end.frames, end.commands), (2, 1));
    assert!(read.is_complete());
    assert_eq!(read.incompleteness(), None);
    // The first line is the header, the keys in reading order.
    let first = std::fs::read_to_string(&path).unwrap();
    assert!(
        first.starts_with("{\"type\":\"header\",\"schema\":1,"),
        "{first}"
    );
}

#[test]
fn csms_frames_include_station_and_stop_at_oversized_frame() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("csms.jsonl");
    let (sink, guard) = start(&path, "1.6", Role::Csms, "", &RecordOptions::default()).unwrap();
    sink.frame_for_station(
        "CP-1",
        1,
        &record(Direction::In, json!([2, "1", "Heartbeat", {}])),
    );
    sink.frame_for_station("CP-2", 2, &record(Direction::Out, json!([3, "1", {}])));
    sink.frame_for_station(
        "CP-1",
        1,
        &record(
            Direction::In,
            json!([2, "2", "DataTransfer", {"data": "x".repeat(MAX_FRAME_BYTES)}]),
        ),
    );
    let summary = guard.finish();
    assert_eq!(summary.frames, 2);
    assert!(summary.incomplete.is_some());
    let recording = Recording::read(&path).unwrap();
    assert_eq!(recording.header.role, Role::Csms);
    let stations: Vec<_> = recording
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Frame(frame) => Some((frame.station.as_deref(), frame.session)),
            Entry::Command(_) => None,
        })
        .collect();
    assert_eq!(stations, [(Some("CP-1"), 1), (Some("CP-2"), 2)]);
}

#[cfg(unix)]
#[test]
fn the_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let (_sink, guard, path) = started(&dir, &RecordOptions::default());
    guard.finish();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    // An overwritten file is private as well.
    let options = RecordOptions {
        overwrite: true,
        ..RecordOptions::default()
    };
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let (_sink, guard) = start(&path, "1.6", Role::Emulator, "CP-1", &options).unwrap();
    guard.finish();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn an_existing_file_is_refused_unless_overwrite_is_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let (_sink, guard, path) = started(&dir, &RecordOptions::default());
    guard.finish();
    let error = start(
        &path,
        "1.6",
        Role::Emulator,
        "CP-1",
        &RecordOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(error, RecordError::Exists { .. }), "{error}");
    // The refused attempt left the file alone.
    assert!(Recording::read(&path).is_ok());
    let options = RecordOptions {
        overwrite: true,
        ..RecordOptions::default()
    };
    let (_sink, guard) = start(&path, "1.6", Role::Emulator, "CP-2", &options).unwrap();
    guard.finish();
    assert_eq!(Recording::read(&path).unwrap().header.station, "CP-2");
}

#[test]
fn a_directory_is_not_a_recording_target() {
    let dir = tempfile::tempdir().unwrap();
    let options = RecordOptions {
        overwrite: true,
        ..RecordOptions::default()
    };
    let error = start(dir.path(), "1.6", Role::Emulator, "CP-1", &options).unwrap_err();
    assert!(matches!(error, RecordError::NotAFile { .. }), "{error}");
}

#[test]
fn secrets_never_reach_the_file_whatever_the_frame_record_holds() {
    let dir = tempfile::tempdir().unwrap();
    let (sink, guard, path) = started(&dir, &RecordOptions::default());
    // Frame records are redacted by the station unless `--show-secrets`; the recorder does
    // not rely on that.
    let frames = [
        json!([2, "c1", "ChangeConfiguration", {"key": "AuthorizationKey", "value": "S3CR3T-ONE"}]),
        json!([3, "g1", {"configurationKey": [{"key": "authorizationkey", "readonly": false, "value": "S3CR3T-TWO"}]}]),
        json!([4, "e1", "InternalError", "x", {"echo": {"key": "AuthorizationKey", "value": "S3CR3T-THREE"}}]),
        json!([2, "u1", "UpdateFirmware", {"location": "ftp://user:S3CR3T-FOUR@host/fw.bin?x=1", "retrieveDate": "2026-01-01T00:00:00Z"}]),
        json!([2, "d1", "DataTransfer", {"vendorId": "v", "data": "see https://a:S3CR3T-FIVE@h.example/p now"}]),
    ];
    for frame in frames {
        sink.frame(1, &record(Direction::In, frame));
    }
    sink.command(
        1,
        &json!({"command": "set_configuration", "key": "AuthorizationKey", "value": "S3CR3T-SIX"}),
    );
    sink.command(
        1,
        &json!({"command": "send_raw", "action": "ChangeConfiguration", "payload": {"key": "AuthorizationKey", "value": "S3CR3T-SEVEN"}, "validate": true}),
    );
    guard.finish();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("S3CR3T"), "{text}");
    assert!(text.contains("***"));
    // Everything else survives: user names, hosts, paths and the other fields.
    assert!(text.contains("ftp://user:***@host/fw.bin?x=1"), "{text}");
    assert!(text.contains("https://a:***@h.example/p now"), "{text}");
    assert!(text.contains("\"readonly\":false"));
}

#[test]
fn a_big_frame_and_a_non_frame_are_marked_not_stored() {
    let dir = tempfile::tempdir().unwrap();
    let (sink, guard, path) = started(&dir, &RecordOptions::default());
    let big = "x".repeat(MAX_FRAME_BYTES + 10);
    sink.frame(
        1,
        &record(
            Direction::Out,
            json!([2, "b1", "DataTransfer", {"vendorId": "v", "data": big}]),
        ),
    );
    let mut text = record(
        Direction::In,
        Value::String("not json \u{1b}[31m SECRET-ish".to_owned()),
    );
    text.msg_id = Some("t1".to_owned());
    sink.frame(1, &text);
    guard.finish();
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(!raw.contains("SECRET-ish"), "free text is never stored");
    let read = Recording::read(&path).unwrap();
    let Entry::Frame(first) = &read.entries[0] else {
        panic!()
    };
    assert!(first.frame.is_none());
    assert!(first.truncated_bytes.unwrap() > MAX_FRAME_BYTES as u64);
    assert_eq!(first.action.as_deref(), Some("DataTransfer"));
    let Entry::Frame(second) = &read.entries[1] else {
        panic!()
    };
    assert!(second.frame.is_none());
    assert!(second.invalid_bytes.unwrap() > 0);
    assert_eq!(second.msg_id.as_deref(), Some("t1"));
}

#[test]
fn a_full_queue_marks_the_recording_incomplete_once_and_stops() {
    let (sink, lines) = detached(2);
    let frame = record(Direction::Out, json!([2, "a", "Heartbeat", {}]));
    sink.frame(1, &frame);
    sink.frame(1, &frame);
    assert_eq!(sink.take_failure(), None, "two fit");
    sink.frame(1, &frame);
    let reason = sink.take_failure().expect("the third did not fit");
    assert!(reason.contains("fell behind"), "{reason}");
    assert_eq!(sink.take_failure(), None, "announced once");
    // Nothing after the gap is queued: the recording ends before the hole.
    sink.frame(1, &frame);
    assert_eq!(lines.try_iter().count(), 2);
}

#[test]
fn the_size_limit_ends_the_recording_with_the_reason_in_the_end_line() {
    let dir = tempfile::tempdir().unwrap();
    let options = RecordOptions {
        max_file_bytes: 700,
        ..RecordOptions::default()
    };
    let (sink, guard, path) = started(&dir, &options);
    for index in 0..50 {
        sink.frame(
            1,
            &record(
                Direction::Out,
                json!([2, format!("m{index}"), "Heartbeat", {}]),
            ),
        );
    }
    let summary = guard.finish();
    let reason = summary.incomplete.expect("limit reached");
    assert!(reason.contains("size limit"), "{reason}");
    assert!(
        summary.frames > 0 && summary.frames < 50,
        "{}",
        summary.frames
    );
    let read = Recording::read(&path).unwrap();
    assert_eq!(read.entries.len() as u64, summary.frames);
    assert!(!read.is_complete());
    assert!(read.incompleteness().unwrap().contains("size limit"));
}

fn parse(text: &str) -> Result<Recording, RecordError> {
    Recording::parse(Cursor::new(text.as_bytes().to_vec()))
}

const HEADER: &str = r#"{"type":"header","schema":1,"ocpp_version":"1.6","tool_version":"9.9.9","role":"emulator","station":"CP-1","started_at":"2026-01-01T00:00:00Z"}"#;

#[test]
fn unknown_keys_line_types_and_roles_are_ignored() {
    let text = format!(
        "{}\n{}\n{}\n{}\n",
        HEADER.replace(
            "\"role\":\"emulator\"",
            "\"role\":\"proxy\",\"future\":[1,2]"
        ),
        r#"{"type":"frame","seq":0,"elapsed_ms":1,"session":1,"direction":"out","frame":[2,"a","Heartbeat",{}],"extra":{"a":1}}"#,
        r#"{"type":"annotation","text":"later schema addition"}"#,
        r#"{"type":"end","frames":1,"commands":0,"elapsed_ms":2,"more":true}"#,
    );
    let read = parse(&text).unwrap();
    assert_eq!(read.header.role, Role::Unknown);
    assert_eq!(read.entries.len(), 1);
    assert_eq!(read.ignored_lines, 1);
    assert!(read.is_complete());
}

#[test]
fn a_second_end_line_or_entries_after_it_are_refused() {
    let frame = r#"{"type":"frame","seq":0,"elapsed_ms":1,"session":1,"direction":"out","frame":[2,"a","Heartbeat",{}]}"#;
    let end = r#"{"type":"end","frames":1,"commands":0,"elapsed_ms":2}"#;
    assert!(parse(&format!("{HEADER}\n{frame}\n{end}\n")).is_ok());
    assert!(parse(&format!("{HEADER}\n{frame}\n")).is_ok());
    assert!(parse(&format!("{HEADER}\n{frame}\n{end}\n{{\"type\":\"end\"")).is_ok());
    for extra in [end, frame] {
        let error = parse(&format!("{HEADER}\n{frame}\n{end}\n{extra}\n"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("after the `end` line"), "{error}");
    }
}

#[test]
fn a_newer_or_missing_schema_is_refused() {
    let newer = HEADER.replace("\"schema\":1", "\"schema\":2");
    let error = parse(&format!("{newer}\n")).unwrap_err().to_string();
    assert!(
        error.contains("schema 2") && error.contains("line 1"),
        "{error}"
    );
    for broken in [
        HEADER.replace("\"schema\":1,", ""),
        HEADER.replace("\"schema\":1", "\"schema\":0"),
        HEADER.replace("\"schema\":1", "\"schema\":\"1\""),
    ] {
        let error = parse(&format!("{broken}\n")).unwrap_err().to_string();
        assert!(error.contains("schema"), "{error}");
    }
}

#[test]
fn a_file_without_a_header_is_not_a_recording() {
    assert!(parse("").unwrap_err().to_string().contains("empty"));
    let error = parse("{\"type\":\"frame\"}\n").unwrap_err().to_string();
    assert!(
        error.contains("line 1") && error.contains("header"),
        "{error}"
    );
    let error = parse("[1,2,3]\n").unwrap_err().to_string();
    assert!(error.contains("header"), "{error}");
}

#[test]
fn a_cut_off_last_line_is_tolerated_and_reported() {
    let frame = r#"{"type":"frame","seq":0,"elapsed_ms":1,"session":1,"direction":"out","frame":[2,"a","Heartbeat",{}]}"#;
    let text = format!("{HEADER}\n{frame}\n{{\"type\":\"frame\",\"seq\":1,\"elap");
    let read = parse(&text).unwrap();
    assert_eq!(read.entries.len(), 1);
    assert!(read.tail_truncated);
    assert!(!read.is_complete());
    assert!(read.incompleteness().unwrap().contains("cut off"));
    // A header that was cut off is no recording at all.
    assert!(
        parse(&HEADER[..40])
            .unwrap_err()
            .to_string()
            .contains("header")
    );
}

#[test]
fn a_bad_line_in_the_middle_names_its_line_and_never_echoes_content() {
    let text = format!(
        "{HEADER}\n{}\n{{\"type\":\"frame\",\"seq\":1}}\n",
        r#"{"type":"frame","seq":0,"elapsed_ms":1,"session":1,"direction":"\u001b[31mevil","frame":null}"#
    );
    let error = parse(&text).unwrap_err().to_string();
    assert!(error.starts_with("line 2:"), "{error}");
    assert!(!error.contains('\u{1b}'), "{error:?}");
    let error = parse(&format!("{HEADER}\nnot json at all\n{HEADER}\n"))
        .unwrap_err()
        .to_string();
    assert!(error.starts_with("line 2:"), "{error}");
}

#[test]
fn a_line_longer_than_the_limit_is_refused() {
    let long = format!(
        "{HEADER}\n{{\"type\":\"frame\",\"pad\":\"{}\"}}\n",
        "x".repeat(MAX_LINE_BYTES)
    );
    let error = parse(&long).unwrap_err().to_string();
    assert!(
        error.contains("line 2") && error.contains("longer"),
        "{error}"
    );
}

/// An endless recording: the header, then the same small line for ever.
struct Endless {
    pending: Vec<u8>,
    sent: u64,
}

impl std::io::Read for Endless {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pending.is_empty() {
            self.pending = br#"{"type":"annotation"}"#.to_vec();
            self.pending.push(b'\n');
        }
        let count = out.len().min(self.pending.len());
        out[..count].copy_from_slice(&self.pending[..count]);
        self.pending.drain(..count);
        self.sent += count as u64;
        Ok(count)
    }
}

#[test]
fn a_file_larger_than_a_recorder_writes_is_refused_before_it_fills_memory() {
    let mut head = HEADER.as_bytes().to_vec();
    head.push(b'\n');
    let reader = std::io::BufReader::new(Cursor::new(head).chain(Endless {
        pending: Vec::new(),
        sent: 0,
    }));
    let error = Recording::parse(reader).unwrap_err().to_string();
    assert!(error.contains("larger than"), "{error}");
}

#[test]
fn text_in_messages_is_escaped_and_clipped() {
    let text = clean("a\u{1b}[2Jb\u{202e}c", 100);
    assert!(
        !text.contains('\u{1b}') && !text.contains('\u{202e}'),
        "{text:?}"
    );
    assert!(clean(&"x".repeat(500), 20).len() < 30);
}

// ---- conversion -------------------------------------------------------------------------

fn lines(role: &str, version: &str, body: &[Value]) -> Recording {
    let mut text = HEADER
        .replace("emulator", role)
        .replace("\"1.6\"", &format!("\"{version}\""));
    text.push('\n');
    for (seq, entry) in body.iter().enumerate() {
        let mut entry = entry.clone();
        entry["seq"] = json!(seq);
        entry["elapsed_ms"] = json!(seq * 1000);
        entry["session"] = json!(1);
        text.push_str(&entry.to_string());
        text.push('\n');
    }
    text.push_str(r#"{"type":"end","frames":0,"commands":0,"elapsed_ms":0}"#);
    text.push('\n');
    parse(&text).unwrap()
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "call sites pass json! literals"
)]
fn cmd(value: Value) -> Value {
    json!({"type": "command", "command": value})
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "call sites pass json! literals"
)]
fn out(frame: Value) -> Value {
    json!({"type": "frame", "direction": "out", "frame": frame})
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "call sites pass json! literals"
)]
fn inn(frame: Value) -> Value {
    json!({"type": "frame", "direction": "in", "frame": frame})
}

fn actions(scenario: &Scenario) -> Vec<&'static str> {
    scenario
        .steps
        .iter()
        .map(|step| step.action.name())
        .collect()
}

#[test]
fn commands_become_neutral_steps_and_the_result_loads() {
    let recording = lines(
        "emulator",
        "1.6",
        &[
            cmd(json!({"command": "connect"})),
            cmd(json!({"command": "plug_in", "connector": 1})),
            cmd(json!({"command": "authorize", "id_tag": "TAG-${x}"})),
            out(json!([2, "au", "Authorize", {"idTag": "TAG-${x}"}])),
            inn(json!([3, "au", {"idTagInfo": {"status": "Blocked"}}])),
            cmd(json!({"command": "start_transaction", "connector": 1, "id_tag": "TAG"})),
            out(
                json!([2, "st", "StartTransaction", {"connectorId": 1, "idTag": "TAG", "meterStart": 0, "timestamp": "2026-01-01T00:00:00Z"}]),
            ),
            inn(json!([3, "st", {"transactionId": 7, "idTagInfo": {"status": "Accepted"}}])),
            cmd(
                json!({"command": "meter_values", "connector": 1, "wh_delta": 500, "measurands": ["Voltage"]}),
            ),
            cmd(
                json!({"command": "stop_transaction", "connector": 1, "reason": "Local", "id_tag": null}),
            ),
            cmd(
                json!({"command": "send_raw", "action": "DataTransfer", "payload": {"vendorId": "v", "data": "${y}"}, "validate": false}),
            ),
            cmd(json!({"command": "set_local_list_entry", "id_tag": "A"})),
            cmd(
                json!({"command": "fault", "connector": 1, "error_code": "GroundFailure", "info": null, "vendor_error_code": "X1"}),
            ),
            inn(json!([2, "rs", "RemoteStartTransaction", {"idTag": "R"}])),
            cmd(json!({"command": "unplug", "connector": 1})),
            cmd(json!({"command": "shutdown"})),
        ],
    );
    let conversion = to_scenario(&recording).unwrap();
    let scenario = Scenario::from_toml(&conversion.toml).unwrap();
    assert_eq!(
        actions(&scenario),
        [
            "plug",
            "authorize",
            "start_tx",
            "meter",
            "stop_tx",
            "send_raw",
            "fault",
            "unplug"
        ]
    );
    // The neutral aliases are what is written.
    assert!(conversion.toml.contains("action = \"start_session\""));
    assert!(conversion.toml.contains("action = \"stop_session\""));
    assert!(!conversion.toml.contains("action = \"start_tx\""));
    // The CSMS rejected the tag in the recording: the scenario expects that.
    assert_eq!(
        scenario.steps[1].expect.as_ref().unwrap().status.as_deref(),
        Some("Blocked")
    );
    assert!(scenario.steps[2].expect.is_none());
    // Recorded text is data: a placeholder lookalike stays literal.
    assert!(conversion.toml.contains("TAG-$${x}") && conversion.toml.contains("$${y}"));
    let Action::SendRaw { validate, .. } = &scenario.steps[5].action else {
        panic!()
    };
    assert!(!validate);
    // Lossy or unsupported things are named, not guessed.
    let warnings = conversion.warnings.join("\n");
    assert!(warnings.contains("measurands"), "{warnings}");
    assert!(warnings.contains("set_local_list_entry"), "{warnings}");
    assert!(warnings.contains("vendor error code"), "{warnings}");
    assert!(
        conversion.toml.contains("RemoteStartTransaction x1"),
        "{}",
        conversion.toml
    );
    assert_eq!(conversion.steps, 8);
}

#[test]
fn an_offline_authorize_does_not_claim_the_call_of_the_next_one() {
    // The first Authorize was decided locally (no CALL); the second one got Blocked.
    let recording = lines(
        "emulator",
        "1.6",
        &[
            cmd(json!({"command": "authorize", "id_tag": "A"})),
            cmd(json!({"command": "authorize", "id_tag": "B"})),
            out(json!([2, "au", "Authorize", {"idTag": "B"}])),
            inn(json!([3, "au", {"idTagInfo": {"status": "Blocked"}}])),
        ],
    );
    let scenario = Scenario::from_toml(&to_scenario(&recording).unwrap().toml).unwrap();
    assert!(scenario.steps[0].expect.is_none());
    assert_eq!(
        scenario.steps[1].expect.as_ref().unwrap().status.as_deref(),
        Some("Blocked")
    );
}

#[test]
fn a_call_from_another_session_is_not_matched() {
    let mut recording = lines(
        "emulator",
        "1.6",
        &[
            cmd(json!({"command": "authorize", "id_tag": "A"})),
            out(json!([2, "au", "Authorize", {"idTag": "A"}])),
            inn(json!([3, "au", {"idTagInfo": {"status": "Blocked"}}])),
        ],
    );
    for entry in &mut recording.entries {
        if let Entry::Frame(frame) = entry {
            frame.session = 2;
        }
    }
    let scenario = Scenario::from_toml(&to_scenario(&recording).unwrap().toml).unwrap();
    assert!(scenario.steps[0].expect.is_none());
}

#[test]
fn reconnects_wait_for_the_registration_and_the_first_connect_is_not_a_step() {
    let recording = lines(
        "emulator",
        "1.6",
        &[
            cmd(json!({"command": "connect"})),
            cmd(json!({"command": "disconnect"})),
            cmd(json!({"command": "connect"})),
            cmd(json!({"command": "heartbeat"})),
        ],
    );
    let scenario = Scenario::from_toml(&to_scenario(&recording).unwrap().toml).unwrap();
    assert_eq!(
        actions(&scenario),
        ["disconnect", "wait_for", "connect", "wait_for", "heartbeat"]
    );
}

#[test]
fn only_an_emulator_recording_of_version_1_6_with_commands_converts() {
    let body = [cmd(json!({"command": "heartbeat"}))];
    let error = to_scenario(&lines("csms", "1.6", &body))
        .unwrap_err()
        .to_string();
    assert!(error.contains("emulator"), "{error}");
    let error = to_scenario(&lines("emulator", "2.0.1", &body))
        .unwrap_err()
        .to_string();
    assert!(error.contains("1.6"), "{error}");
    let frames_only = [out(json!([2, "a", "Heartbeat", {}]))];
    let error = to_scenario(&lines("emulator", "1.6", &frames_only))
        .unwrap_err()
        .to_string();
    assert!(error.contains("nothing to convert"), "{error}");
}

#[test]
fn an_incomplete_recording_is_warned_about() {
    let mut recording = lines("emulator", "1.6", &[cmd(json!({"command": "heartbeat"}))]);
    recording.end = None;
    let warnings = to_scenario(&recording).unwrap().warnings;
    assert!(
        warnings.iter().any(|w| w.contains("no end line")),
        "{warnings:?}"
    );
}

// ---- replay -----------------------------------------------------------------------------

fn session_with_transaction() -> Recording {
    lines(
        "emulator",
        "1.6",
        &[
            out(
                json!([2, "b0", "BootNotification", {"chargePointVendor": "v", "chargePointModel": "m"}]),
            ),
            inn(
                json!([3, "b0", {"status": "Accepted", "interval": 5, "currentTime": "2026-01-01T00:00:00Z"}]),
            ),
            out(
                json!([2, "s1", "StartTransaction", {"connectorId": 1, "idTag": "TAG", "meterStart": 0, "timestamp": "2026-01-01T00:00:00Z"}]),
            ),
            inn(json!([3, "s1", {"transactionId": 500, "idTagInfo": {"status": "Accepted"}}])),
            out(
                json!([2, "m1", "MeterValues", {"connectorId": 1, "transactionId": 500, "meterValue": []}]),
            ),
            inn(json!([3, "m1", {}])),
            out(
                json!([2, "e1", "StopTransaction", {"transactionId": 500, "meterStop": 5, "timestamp": "2026-01-01T00:00:00Z"}]),
            ),
            inn(json!([3, "e1", {}])),
            // Started in an earlier run: the id is not in the recording.
            out(
                json!([2, "e2", "StopTransaction", {"transactionId": 77, "meterStop": 5, "timestamp": "2026-01-01T00:00:00Z"}]),
            ),
            inn(json!([4, "e2", "InternalError", "no such transaction", {}])),
        ],
    )
}

fn payload_of(scenario: &Scenario, index: usize) -> Value {
    let Action::SendRaw { payload, .. } = &scenario.steps[index].action else {
        panic!("send_raw expected at {index}: {:?}", scenario.steps[index]);
    };
    payload.clone()
}

#[test]
fn replay_maps_transaction_ids_and_skips_the_boot() {
    let plan = plan_replay(
        &session_with_transaction(),
        &ReplayOptions {
            no_delay: true,
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    let scenario = &plan.scenario;
    assert_eq!(plan.replayed, 4);
    assert_eq!(actions(scenario), ["send_raw"; 4]);
    // StartTransaction captures the new id, later calls use it.
    assert_eq!(
        scenario.steps[0].capture.get("tx1").map(String::as_str),
        Some("/transactionId")
    );
    assert_eq!(payload_of(scenario, 1)["transactionId"], "${tx1}");
    assert_eq!(payload_of(scenario, 2)["transactionId"], "${tx1}");
    // An id that was never started in the recording is sent as recorded, with a warning.
    assert_eq!(payload_of(scenario, 3)["transactionId"], 77);
    let warnings = plan.warnings.join("\n");
    assert!(warnings.contains("77"), "{warnings}");
    // The CALLERROR of the recording is expected again.
    assert!(scenario.steps[3].expect.as_ref().unwrap().error);
    assert!(scenario.steps[2].expect.is_none());
    // The station does not validate: the recorded frame is replayed as it was.
    let Action::SendRaw { validate, call, .. } = &scenario.steps[0].action else {
        panic!()
    };
    assert!(!validate);
    assert_eq!(call, "StartTransaction");
    // Names are stable labels built from the recorded ids, never positions.
    assert_eq!(
        scenario.steps[0].name.as_deref(),
        Some("StartTransaction s1")
    );
}

#[test]
fn a_reused_csms_transaction_id_follows_the_latest_start() {
    let recording = lines(
        "emulator",
        "1.6",
        &[
            out(
                json!([2, "s1", "StartTransaction", {"connectorId": 1, "idTag": "T", "meterStart": 0, "timestamp": "2026-01-01T00:00:00Z"}]),
            ),
            inn(json!([3, "s1", {"transactionId": 5, "idTagInfo": {"status": "Accepted"}}])),
            out(
                json!([2, "e1", "StopTransaction", {"transactionId": 5, "meterStop": 1, "timestamp": "2026-01-01T00:00:00Z"}]),
            ),
            inn(json!([3, "e1", {}])),
            out(
                json!([2, "s2", "StartTransaction", {"connectorId": 1, "idTag": "T", "meterStart": 1, "timestamp": "2026-01-01T00:00:00Z"}]),
            ),
            inn(json!([3, "s2", {"transactionId": 5, "idTagInfo": {"status": "Accepted"}}])),
            out(
                json!([2, "e2", "StopTransaction", {"transactionId": 5, "meterStop": 2, "timestamp": "2026-01-01T00:00:00Z"}]),
            ),
            inn(json!([3, "e2", {}])),
        ],
    );
    let plan = plan_replay(
        &recording,
        &ReplayOptions {
            no_delay: true,
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert_eq!(payload_of(&plan.scenario, 1)["transactionId"], "${tx1}");
    assert_eq!(payload_of(&plan.scenario, 3)["transactionId"], "${tx2}");
}

#[test]
fn replay_never_sends_a_masked_credential_or_a_call_it_cannot_judge() {
    let big = json!({"type": "frame", "direction": "out", "frame": null, "truncated_bytes": 99999});
    let recording = lines(
        "emulator",
        "1.6",
        &[
            out(
                json!([2, "k1", "ChangeConfiguration", {"key": "AuthorizationKey", "value": "***"}]),
            ),
            inn(json!([3, "k1", {"status": "Accepted"}])),
            out(json!([2, "u1", "DataTransfer", {"vendorId": "v", "data": "ws://u:***@h/x"}])),
            inn(json!([3, "u1", {"status": "Accepted"}])),
            big,
            out(json!([2, "n1", "Heartbeat", {}])),
            out(json!([2, "ok", "Heartbeat", {}])),
            inn(json!([3, "ok", {"currentTime": "2026-01-01T00:00:00Z"}])),
        ],
    );
    let plan = plan_replay(
        &recording,
        &ReplayOptions {
            no_delay: true,
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert_eq!(plan.replayed, 1);
    assert_eq!(plan.skipped, 4);
    let warnings = plan.warnings.join("\n");
    assert!(
        warnings.contains("ChangeConfiguration") && warnings.contains("redacted"),
        "{warnings}"
    );
    assert!(
        warnings.contains("larger than the recorder keeps"),
        "{warnings}"
    );
    assert!(warnings.contains("no answer"), "{warnings}");
    let toml = format!("{:?}", plan.scenario);
    assert!(!toml.contains("***"), "{toml}");
}

#[test]
fn replay_keeps_the_recorded_pauses_scaled_by_speed() {
    let recording = lines(
        "emulator",
        "1.6",
        &[
            out(json!([2, "h1", "Heartbeat", {}])),
            inn(json!([3, "h1", {}])),
            out(json!([2, "h2", "Heartbeat", {}])),
            inn(json!([3, "h2", {}])),
        ],
    );
    let sleeps = |options: &ReplayOptions| -> Vec<u64> {
        plan_replay(&recording, options)
            .unwrap()
            .scenario
            .steps
            .iter()
            .filter_map(|step| match step.action {
                Action::Sleep { ms } => Some(ms),
                _ => None,
            })
            .collect()
    };
    // The two CALLs are 2 s apart in the fixture (sequence 0 and 2, 1000 ms each).
    assert_eq!(sleeps(&ReplayOptions::default()), [2000]);
    assert_eq!(
        sleeps(&ReplayOptions {
            speed: 4.0,
            no_delay: false
        }),
        [500]
    );
    assert_eq!(
        sleeps(&ReplayOptions {
            speed: 1.0,
            no_delay: true
        }),
        Vec::<u64>::new()
    );
}

#[test]
fn only_an_emulator_recording_can_be_replayed() {
    let body = [
        out(json!([2, "h1", "Heartbeat", {}])),
        inn(json!([3, "h1", {}])),
    ];
    let error = plan_replay(&lines("csms", "1.6", &body), &ReplayOptions::default()).unwrap_err();
    assert!(error.to_string().contains("emulator"), "{error}");
    let error = plan_replay(
        &lines("emulator", "2.0.1", &body),
        &ReplayOptions::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("1.6"), "{error}");
    let boot_only = [
        out(json!([2, "b", "BootNotification", {}])),
        inn(json!([3, "b", {}])),
    ];
    let error = plan_replay(
        &lines("emulator", "1.6", &boot_only),
        &ReplayOptions::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("no CALL"), "{error}");
}

fn overwriting() -> RecordOptions {
    RecordOptions {
        overwrite: true,
        ..RecordOptions::default()
    }
}

fn dir_entries(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[cfg(unix)]
#[test]
fn a_symlink_is_never_followed_or_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.txt");
    std::fs::write(&target, "precious\n").unwrap();
    let link = dir.path().join("link.jsonl");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    for options in [overwriting(), RecordOptions::default()] {
        let error = start(&link, "1.6", Role::Emulator, "CP-1", &options).unwrap_err();
        assert!(
            matches!(
                error,
                RecordError::NotAFile { .. } | RecordError::Exists { .. }
            ),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "precious\n");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    assert_eq!(dir_entries(dir.path()), ["link.jsonl", "target.txt"]);
}

#[test]
fn an_overwritten_recording_survives_until_the_new_one_is_finished() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rec.jsonl");
    std::fs::write(&path, "precious\n").unwrap();
    // Dropped without finish (a run that failed after the file was opened).
    let (_sink, guard) = start(&path, "1.6", Role::Emulator, "CP-1", &overwriting()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "precious\n");
    drop(guard);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "precious\n");
    // A run that recorded nothing leaves the old file as well.
    let (_sink, guard) = start(&path, "1.6", Role::Emulator, "CP-1", &overwriting()).unwrap();
    let summary = guard.finish_unless_empty();
    assert_eq!((summary.frames, summary.commands), (0, 0));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "precious\n");
    assert_eq!(dir_entries(dir.path()), ["rec.jsonl"]);
    // A recording with content replaces it.
    let (sink, guard) = start(&path, "1.6", Role::Emulator, "CP-9", &overwriting()).unwrap();
    sink.command(1, &json!({"command": "heartbeat"}));
    guard.finish_unless_empty();
    assert_eq!(Recording::read(&path).unwrap().header.station, "CP-9");
    assert_eq!(dir_entries(dir.path()), ["rec.jsonl"]);
}

#[test]
fn an_empty_new_recording_is_not_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (_sink, guard, path) = started(&dir, &RecordOptions::default());
    guard.finish_unless_empty();
    assert!(!path.exists());
}

fn redacted(value: Value) -> String {
    let mut value = value;
    redact_value(&mut value);
    value.to_string()
}

#[test]
fn url_passwords_are_hidden_wherever_the_url_sits_in_a_string() {
    // A JSON string value that is only a URL, `key=value` text, several URLs in one string.
    let text = redacted(json!({
        "url": "ftp://alice:hunter2@files.example/fw.bin",
        "line": "location=ftp://bob:hunter3@host/x",
        "many": "a ws://u1:hunter4@h1/x, then wss://u2:hunter5@h2:8443/y?z=1#f; end",
        "quoted": "{\"url\":\"ftp://carol:hunter6@h/x\"}",
        "list": ["(https://dave:hunter7@h)"],
    }));
    assert!(!text.contains("hunter"), "{text}");
    for kept in [
        "ftp://alice:***@files.example/fw.bin",
        "location=ftp://bob:***@host/x",
        "ws://u1:***@h1/x, then wss://u2:***@h2:8443/y?z=1#f; end",
        "ftp://carol:***@h/x",
        "(https://dave:***@h)",
    ] {
        assert!(text.contains(kept), "{kept} in {text}");
    }
    // A string far longer than the old scan limit is searched as well.
    let big = format!(
        "{} ftp://eve:hunter8@h/x {}",
        "x".repeat(100_000),
        "y".repeat(100_000)
    );
    let text = redacted(json!([big]));
    assert!(!text.contains("hunter8") && text.contains("ftp://eve:***@h/x"));
}

#[test]
fn text_without_url_credentials_is_left_alone() {
    for same in [
        "see http://example.com/a@b:c now",
        "ftp://host/path@x:y?q=1",
        "http://user@host/",
        "http://host/x?email=a:b@c",
        "http://u:@host/",
        "mailto:a:b@c and ://u:p@h",
        "no scheme u:p@host/x",
    ] {
        assert_eq!(redacted(json!(same)), json!(same).to_string(), "{same}");
    }
}

#[test]
fn long_url_authorities_are_redacted() {
    let password = "p".repeat(4096);
    let text = redacted(json!(format!("ftp://user:{password}@host/file")));
    assert_eq!(text, json!("ftp://user:***@host/file").to_string());
}

fn recording_with_failed_start() -> Recording {
    lines(
        "emulator",
        "1.6",
        &[
            out(
                json!([2, "s1", "StartTransaction", {"connectorId": 1, "idTag": "T", "meterStart": 0, "timestamp": "2026-01-01T00:00:00Z"}]),
            ),
            inn(json!([4, "s1", "InternalError", "no", {}])),
            out(json!([2, "h1", "Heartbeat", {}])),
            inn(json!([3, "h1", {}])),
        ],
    )
}

#[test]
fn a_start_the_csms_refused_captures_nothing() {
    let plan = plan_replay(
        &recording_with_failed_start(),
        &ReplayOptions {
            no_delay: true,
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert_eq!(plan.replayed, 2);
    assert!(plan.scenario.steps[0].capture.is_empty());
    assert!(plan.scenario.steps[0].expect.as_ref().unwrap().error);
}

#[test]
fn a_long_recording_replays_at_default_pacing() {
    let mut body = Vec::new();
    for index in 0..6000 {
        let id = format!("h{index}");
        body.push(out(json!([2, id, "Heartbeat", {}])));
        body.push(inn(json!([3, id, {}])));
    }
    let plan = plan_replay(&lines("emulator", "1.6", &body), &ReplayOptions::default()).unwrap();
    assert_eq!(plan.replayed, 6000);
    // One pause between every two CALLs: more steps than a hand-written scenario may have.
    assert_eq!(plan.scenario.steps.len(), 6000 + 5999);
    // The limit of a scenario file is 10000 steps.
    assert!(plan.scenario.expanded_len() > 10_000);
}

#[test]
fn recorded_action_names_are_never_placeholders() {
    let recording = lines(
        "emulator",
        "1.6",
        &[
            out(json!([2, "x${nope}", "Evil${nope}", {}])),
            inn(json!([3, "x${nope}", {}])),
        ],
    );
    let plan = plan_replay(
        &recording,
        &ReplayOptions {
            no_delay: true,
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    let Action::SendRaw { call, .. } = &plan.scenario.steps[0].action else {
        panic!()
    };
    assert_eq!(call, "Evil$${nope}");
    assert_eq!(
        plan.scenario.steps[0].name.as_deref(),
        Some("Evil$${nope} x$${nope}")
    );
}

#[test]
fn a_url_password_with_free_text_delimiters_is_hidden() {
    for (url, kept) in [
        (
            "ftp://svc:pa#ss(1)@host/fw.bin",
            "ftp://svc:***@host/fw.bin",
        ),
        ("ftp://svc:p(1@host/fw.bin", "ftp://svc:***@host/fw.bin"),
        ("ftp://svc:a#b@host/x", "ftp://svc:***@host/x"),
        ("ftp://svc:a?b@host/x", "ftp://svc:***@host/x"),
        ("ftp://svc:a[b]{c}<d>'e\"f@host", "ftp://svc:***@host"),
    ] {
        let text = redacted(json!({ "location": url }));
        assert_eq!(text, json!({ "location": kept }).to_string(), "{url}");
    }
    // Inside prose the delimiters still end the authority.
    let text = redacted(json!("fetch (ftp://svc:secret@host/x) now"));
    assert_eq!(text, json!("fetch (ftp://svc:***@host/x) now").to_string());
}

#[test]
fn authorization_key_inside_json_text_is_masked() {
    let text = redacted(json!({
        "data": "{\"key\":\"AuthorizationKey\",\"value\":\"s3cret\"}",
        "list": " [{\"key\":\"authorizationkey\",\"value\":\"s3cret2\"}]",
        "broken": "{\"key\":\"AuthorizationKey\",\"value\":\"s3cret3\"",
        "plain": "{\"key\":\"Other\",\"value\":\"kept\"}",
    }));
    assert!(!text.contains("s3cret"), "{text}");
    assert!(text.contains("kept"), "{text}");
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["broken"], json!("***"));
    assert!(holds_redacted(&value["data"]));
    assert!(holds_redacted(&value["list"]));
    assert!(holds_redacted(&value["broken"]));
    assert!(!holds_redacted(&value["plain"]));
}

#[test]
fn commands_with_redacted_payloads_are_not_converted() {
    let recording = lines(
        "emulator",
        "1.6",
        &[
            cmd(
                json!({"command": "send_raw", "action": "DataTransfer", "payload": {"data": "ws://u:***@h"}}),
            ),
            cmd(json!({"command": "send_raw", "action": "Heartbeat", "payload": {}})),
            cmd(json!({"command": "data_transfer", "vendor_id": "v", "data": "***"})),
            cmd(json!({"command": "data_transfer", "vendor_id": "v", "data": "ok"})),
        ],
    );
    let conversion = to_scenario(&recording).unwrap();
    let scenario = Scenario::from_toml(&conversion.toml).unwrap();
    assert_eq!(actions(&scenario), ["send_raw", "data_transfer"]);
    assert!(!conversion.toml.contains("***\"\n"), "{}", conversion.toml);
    assert_eq!(
        conversion
            .warnings
            .iter()
            .filter(|w| w.contains("skipped"))
            .count(),
        2
    );
}

#[test]
fn a_missing_recording_is_a_read_error() {
    let dir = tempfile::tempdir().unwrap();
    let error = Recording::read(&dir.path().join("nofile.jsonl")).unwrap_err();
    let text = error.to_string();
    assert!(text.starts_with("cannot read the recording "), "{text}");
    assert!(!text.contains("cannot write"), "{text}");
}
