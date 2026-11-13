//! Process tests of `--record`, `replay` and `scenarios from-recording`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use kirby_ocpp_core::recording::{Entry, Recording};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms, TransactionIds};
use serde_json::{Value, json};

use crate::support::{ChildGuard, finish, output};

const WAIT: Duration = Duration::from_secs(20);

fn command(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kirby-ocpp"));
    for (key, _) in std::env::vars() {
        if key.starts_with("OCPP_") || key.starts_with("KIRBY_OCPP_") {
            command.env_remove(key);
        }
    }
    command.args(args).stdin(Stdio::null());
    command
}

fn path(path: &Path) -> &str {
    path.to_str().expect("utf-8 path")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn stderr(output: &Output) -> String {
    text(&output.stderr)
}

/// Runs the binary on a blocking thread (the test runtime must keep serving the mock).
async fn run(args: Vec<String>) -> Output {
    tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        output(command(&args))
    })
    .await
    .expect("run task")
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

async fn mock(first_transaction: i32) -> MockCsms {
    MockCsms::start(CsmsOptions {
        transaction_ids: TransactionIds::Fixed(first_transaction),
        ..CsmsOptions::default()
    })
    .await
    .expect("mock csms")
}

/// Records `metering-burst` against a mock that hands out transaction id 500.
async fn recorded_session(dir: &Path) -> PathBuf {
    let csms = mock(500).await;
    let file = dir.join("session.jsonl");
    let mut args = strings(&["--url", csms.url(), "--id", "REC-1", "--record"]);
    args.extend([
        path(&file).to_owned(),
        "run".into(),
        "--scenario".into(),
        "metering-burst".into(),
    ]);
    let output = run(args).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stderr(&output).contains("recorded"), "{}", stderr(&output));
    file
}

#[tokio::test(flavor = "multi_thread")]
async fn record_convert_validate_and_run_green() {
    let dir = tempfile::tempdir().unwrap();
    let file = recorded_session(dir.path()).await;

    // The recording holds both the frames and the commands of the run.
    let recording = Recording::read(&file).unwrap();
    assert_eq!(recording.header.station, "REC-1");
    assert!(recording.is_complete(), "{:?}", recording.incompleteness());
    let commands: Vec<&str> = recording
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Command(line) => line.command.as_ref()?.get("command")?.as_str(),
            Entry::Frame(_) => None,
        })
        .collect();
    assert!(
        commands.contains(&"start_transaction") && commands.contains(&"meter_values"),
        "{commands:?}"
    );
    assert!(
        recording.entries.iter().any(|entry| matches!(entry, Entry::Frame(frame)
            if frame.action.as_deref() == Some("StartTransaction")
                && frame.frame.as_ref().is_some_and(|f| f[0] == 3 && f[2]["transactionId"] == 500)))
    );

    // Convert, validate, run.
    let scenario = dir.path().join("session.toml");
    let converted = run(strings(&[
        "scenarios",
        "from-recording",
        path(&file),
        "-o",
        path(&scenario),
    ]))
    .await;
    assert_eq!(converted.status.code(), Some(0), "{}", stderr(&converted));
    let validated = run(strings(&["scenarios", "validate", path(&scenario)])).await;
    assert_eq!(validated.status.code(), Some(0), "{}", stderr(&validated));
    let toml = std::fs::read_to_string(&scenario).unwrap();
    assert!(
        toml.contains("start_session") && toml.contains("stop_session"),
        "{toml}"
    );

    let csms = mock(900).await;
    let report = dir.path().join("report.json");
    let ran = run(strings(&[
        "--url",
        csms.url(),
        "--id",
        "REC-2",
        "run",
        "--scenario",
        path(&scenario),
        "--report",
        path(&report),
    ]))
    .await;
    assert_eq!(
        ran.status.code(),
        Some(0),
        "{}{}",
        text(&ran.stdout),
        stderr(&ran)
    );
    let start = csms.wait_for_call("StartTransaction", WAIT).await.unwrap();
    assert_eq!(start["connectorId"], 1);
    let report: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert_eq!(report["status"], "passed");
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_sends_the_calls_again_with_the_new_transaction_id() {
    let dir = tempfile::tempdir().unwrap();
    let file = recorded_session(dir.path()).await;

    let csms = mock(900).await;
    let report = dir.path().join("replay.json");
    let junit = dir.path().join("replay.xml");
    let output = run(strings(&[
        "--url",
        csms.url(),
        "--id",
        "REC-3",
        "replay",
        path(&file),
        "--no-delay",
        "--report",
        path(&report),
        "--junit",
        path(&junit),
    ]))
    .await;
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}{}",
        text(&output.stdout),
        stderr(&output)
    );

    let received = csms.received();
    let count = |action: &str| received.iter().filter(|call| call.action == action).count();
    // The replaying station boots itself; the recorded boot is not sent again.
    assert_eq!(count("BootNotification"), 1);
    assert_eq!(count("StartTransaction"), 1);
    assert_eq!(count("StopTransaction"), 1);
    assert_eq!(count("MeterValues"), 3);
    let start_id = received
        .iter()
        .find(|call| call.action == "StartTransaction")
        .map(|call| call.message_id.clone())
        .unwrap();
    // The ids of the new session, not of the recorded one (500), and fresh message ids.
    for call in received
        .iter()
        .filter(|call| matches!(call.action.as_str(), "StopTransaction" | "MeterValues"))
    {
        assert_eq!(
            call.payload["transactionId"], 900,
            "{} {}",
            call.action, call.payload
        );
    }
    let recorded: Recording = Recording::read(&file).unwrap();
    let recorded_ids: Vec<&str> = recorded
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Frame(frame) => frame.msg_id.as_deref(),
            Entry::Command(_) => None,
        })
        .collect();
    assert!(!recorded_ids.contains(&start_id.as_str()));

    let report: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert_eq!(report["schema"], 1);
    assert_eq!(report["status"], "passed");
    assert_eq!(report["ocpp_version"], "1.6");
    assert_eq!(report["source"], path(&file));
    assert_eq!(report["station"], "REC-3");
    assert!(
        std::fs::read_to_string(&junit)
            .unwrap()
            .contains("<testsuite")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_reports_a_call_the_csms_now_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let file = recorded_session(dir.path()).await;
    let csms = mock(900).await;
    csms.respond("MeterValues", |_| {
        kirby_ocpp_testkit::Reply::Error(
            kirby_ocpp_testkit::ErrorCode::InternalError,
            "no".to_owned(),
        )
    });
    let report = dir.path().join("failed.json");
    let output = run(strings(&[
        "--url",
        csms.url(),
        "--id",
        "REC-4",
        "replay",
        path(&file),
        "--no-delay",
        "--report",
        path(&report),
    ]))
    .await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let report: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert_eq!(report["status"], "failed");
    assert!(
        report["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["status"] == "failed")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_that_pass_through_the_station_never_reach_the_recording() {
    let dir = tempfile::tempdir().unwrap();
    let csms = mock(1).await;
    let scenario = dir.path().join("wait.toml");
    std::fs::write(
        &scenario,
        r#"
name = "wait-for-commands"

[[steps]]
action = "expect_call"
call = "ChangeConfiguration"
direction = "in"
timeout_ms = 20000

[[steps]]
action = "expect_call"
call = "GetConfiguration"
direction = "in"
timeout_ms = 20000

[[steps]]
action = "send_raw"
call = "ChangeConfiguration"
payload = { key = "AuthorizationKey", value = "SECRET-FROM-SCENARIO" }
validate = false
"#,
    )
    .unwrap();
    let file = dir.path().join("secrets.jsonl");
    let mut cmd = command(&[
        "--url",
        csms.url(),
        "--id",
        "REC-5",
        "--show-secrets",
        "--record",
        path(&file),
        "run",
        "--scenario",
        path(&scenario),
    ]);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = ChildGuard::spawn(&mut cmd);
    csms.wait_for_call("BootNotification", WAIT).await.unwrap();
    csms.send_call(
        "ChangeConfiguration",
        json!({"key": "AuthorizationKey", "value": "SECRET-FROM-CSMS"}),
    )
    .await
    .unwrap();
    csms.send_call(
        "GetConfiguration",
        json!({"key": ["AuthorizationKey", "HeartbeatInterval"]}),
    )
    .await
    .unwrap();
    let output = finish(child).await;
    let raw = std::fs::read_to_string(&file).unwrap();
    assert!(!raw.contains("SECRET-FROM"), "{raw}");
    assert!(
        raw.contains("\"ChangeConfiguration\""),
        "the frames are there"
    );
    assert!(raw.contains("***"));
    // The station really ran with --show-secrets: what it printed to its own log may show the
    // key, the recording never does.
    let _ = output;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plain_headless_session_is_recorded_until_it_is_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let csms = mock(1).await;
    let file = dir.path().join("headless.jsonl");
    let mut cmd = command(&[
        "--url",
        csms.url(),
        "--id",
        "REC-6",
        "--no-tui",
        "--record",
        path(&file),
    ]);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = ChildGuard::spawn(&mut cmd);
    csms.wait_for_call("BootNotification", WAIT).await.unwrap();
    #[cfg(unix)]
    {
        let status = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let output = finish(child).await;
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        let recording = Recording::read(&file).unwrap();
        assert!(recording.is_complete());
        assert!(recording.entries.iter().any(|entry| matches!(entry,
            Entry::Frame(frame) if frame.action.as_deref() == Some("BootNotification"))));
    }
    #[cfg(not(unix))]
    drop(child);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_recording_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let file = recorded_session(dir.path()).await;
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_existing_recording_is_kept_unless_overwrite_is_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("keep.jsonl");
    std::fs::write(&file, "precious\n").unwrap();
    let csms = mock(1).await;
    let base = [
        "--url",
        csms.url(),
        "--id",
        "REC-7",
        "--record",
        path(&file),
    ];
    let mut args = strings(&base);
    args.extend(strings(&["run", "--scenario", "heartbeat-and-status"]));
    let refused = run(args.clone()).await;
    assert_eq!(refused.status.code(), Some(2), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("already exists")
            && stderr(&refused).contains("--record-overwrite")
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "precious\n");
    assert_eq!(csms.connections(), 0, "nothing ran");

    args.insert(0, "--record-overwrite".to_owned());
    let replaced = run(args).await;
    assert_eq!(replaced.status.code(), Some(0), "{}", stderr(&replaced));
    assert!(Recording::read(&file).is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn record_is_refused_where_it_cannot_work_or_would_destroy_an_input() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("x.jsonl");
    let scenario = dir.path().join("s.toml");
    let body = "name = \"s\"\n\n[[steps]]\naction = \"heartbeat\"\n";
    std::fs::write(&scenario, body).unwrap();
    let log = dir.path().join("log.txt");
    let url = "ws://127.0.0.1:9/ocpp";
    let cases: Vec<(Vec<String>, &str)> = vec![
        (
            strings(&[
                "--url",
                url,
                "--record",
                path(&file),
                "suite",
                "--all-builtins",
            ]),
            "suite",
        ),
        (
            strings(&["--url", url, "--record", path(&file), "check"]),
            "check",
        ),
        (
            strings(&[
                "--url",
                url,
                "--id",
                "CP-{n}",
                "--count",
                "2",
                "--record",
                path(&file),
                "--no-tui",
            ]),
            "fleet",
        ),
        (strings(&["--url", url, "--record-overwrite"]), "--record"),
        (
            strings(&[
                "--url",
                url,
                "--record",
                path(&scenario),
                "--record-overwrite",
                "run",
                "--scenario",
                path(&scenario),
            ]),
            "would overwrite",
        ),
        (
            strings(&[
                "--url",
                url,
                "--record",
                path(&log),
                "--log-file",
                path(&log),
                "--no-tui",
            ]),
            "--log-file",
        ),
    ];
    for (args, needle) in cases {
        let output = run(args.clone()).await;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains(needle),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    assert!(!file.exists(), "no recording was created");
    assert_eq!(
        std::fs::read_to_string(&scenario).unwrap(),
        body,
        "the scenario is untouched"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_cannot_start_leaves_no_recording_behind() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("none.jsonl");
    let missing_dir = dir.path().join("no-such-dir").join("log.txt");
    let output = run(strings(&[
        "--url",
        "ws://127.0.0.1:9/ocpp",
        "--record",
        path(&file),
        "--log-file",
        path(&missing_dir),
        "run",
        "--scenario",
        "heartbeat-and-status",
    ]))
    .await;
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(!file.exists(), "an empty recording is not kept");
}

fn write(dir: &Path, name: &str, lines: &[Value]) -> PathBuf {
    let file = dir.join(name);
    let body: Vec<String> = lines.iter().map(ToString::to_string).collect();
    std::fs::write(&file, body.join("\n") + "\n").unwrap();
    file
}

fn header(role: &str, schema: u32, version: &str) -> Value {
    json!({"type": "header", "schema": schema, "ocpp_version": version, "tool_version": "1", "role": role, "station": "X", "started_at": "2026-01-01T00:00:00Z"})
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_and_conversion_refuse_what_they_cannot_use() {
    let dir = tempfile::tempdir().unwrap();
    let url = "ws://127.0.0.1:9/ocpp";
    let newer = write(dir.path(), "newer.jsonl", &[header("emulator", 2, "1.6")]);
    let csms_side = write(dir.path(), "csms.jsonl", &[header("csms", 1, "1.6")]);
    let empty = write(dir.path(), "empty.jsonl", &[header("emulator", 1, "1.6")]);
    let garbage = dir.path().join("garbage.jsonl");
    std::fs::write(&garbage, "not a recording\n").unwrap();
    let missing = dir.path().join("missing.jsonl");
    let target = dir.path().join("out.toml");
    std::fs::write(&target, "keep\n").unwrap();

    let mut cases: Vec<(Vec<String>, &str)> = Vec::new();
    for recording in [&newer, &csms_side, &empty, &garbage, &missing] {
        cases.push((strings(&["--url", url, "replay", path(recording)]), ""));
        cases.push((
            strings(&["scenarios", "from-recording", path(recording)]),
            "",
        ));
    }
    cases.push((
        strings(&["--url", url, "replay", path(&empty), "--speed", "0"]),
        "0.01",
    ));
    cases.push((
        strings(&[
            "--url",
            url,
            "replay",
            path(&empty),
            "--speed",
            "2",
            "--no-delay",
        ]),
        "cannot be used",
    ));
    cases.push((
        strings(&[
            "scenarios",
            "from-recording",
            path(&empty),
            "-o",
            path(&target),
        ]),
        "",
    ));
    cases.push((
        strings(&["scenarios", "from-recording", path(&empty), "--overwrite"]),
        "--output",
    ));
    for (args, needle) in cases {
        let output = run(args.clone()).await;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains(needle),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    assert!(
        stderr(&run(strings(&["scenarios", "from-recording", path(&newer)])).await)
            .contains("schema 2")
    );
    assert!(
        stderr(&run(strings(&["scenarios", "from-recording", path(&csms_side)])).await)
            .contains("emulator")
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn from_recording_refuses_to_replace_a_file_and_never_overwrites_its_input() {
    let dir = tempfile::tempdir().unwrap();
    let file = recorded_session(dir.path()).await;
    let target = dir.path().join("out.toml");
    std::fs::write(&target, "keep\n").unwrap();
    let refused = run(strings(&[
        "scenarios",
        "from-recording",
        path(&file),
        "-o",
        path(&target),
    ]))
    .await;
    assert_eq!(refused.status.code(), Some(2));
    assert!(
        stderr(&refused).contains("--overwrite"),
        "{}",
        stderr(&refused)
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep\n");
    let replaced = run(strings(&[
        "scenarios",
        "from-recording",
        path(&file),
        "-o",
        path(&target),
        "--overwrite",
    ]))
    .await;
    assert_eq!(replaced.status.code(), Some(0), "{}", stderr(&replaced));
    assert!(
        std::fs::read_to_string(&target)
            .unwrap()
            .contains("[[steps]]")
    );
    let own = run(strings(&[
        "scenarios",
        "from-recording",
        path(&file),
        "-o",
        path(&file),
        "--overwrite",
    ]))
    .await;
    assert_eq!(own.status.code(), Some(2));
    assert!(Recording::read(&file).is_ok(), "the recording survived");
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_does_not_overwrite_the_recording_it_reads() {
    let dir = tempfile::tempdir().unwrap();
    let file = recorded_session(dir.path()).await;
    let output = run(strings(&[
        "--url",
        "ws://127.0.0.1:9/ocpp",
        "--record",
        path(&file),
        "--record-overwrite",
        "replay",
        path(&file),
    ]))
    .await;
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("being replayed"),
        "{}",
        stderr(&output)
    );
    assert!(Recording::read(&file).is_ok());
}
