//! Process tests of the scenario runner and the headless mode against a mock CSMS
//! running in the test process.

#[cfg(unix)]
use std::path::Path;
use std::process::{Command, Output, Stdio};
#[cfg(unix)]
use std::time::Duration;

use kirby_ocpp_testkit::{CsmsOptions, MockCsms};
use serde_json::Value;
use tempfile::TempDir;

#[cfg(unix)]
const STEP: Duration = Duration::from_secs(10);

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

async fn run(args: Vec<String>) -> Output {
    tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        command(&args).output().expect("binary runs")
    })
    .await
    .unwrap()
}

fn station_args(csms: &MockCsms, extra: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = [
        "--url",
        csms.url(),
        "--id",
        "CP-CLI",
        "--no-persist",
        "--no-tui",
    ]
    .map(String::from)
    .into();
    args.extend(extra.iter().map(|s| (*s).to_owned()));
    args
}

fn jsonl(output: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("`{line}`: {e}")))
        .collect()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn write(dir: &TempDir, name: &str, text: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

#[test]
fn scenarios_list_and_show() {
    let list = command(&["scenarios", "list"]).output().unwrap();
    assert!(list.status.success());
    let text = String::from_utf8_lossy(&list.stdout);
    for name in [
        "charge-session",
        "finish-session",
        "offline-sync",
        "fault-recovery",
        "heartbeat-and-status",
        "metering-burst",
        "data-transfer",
        "security-event",
        "reconnect",
    ] {
        assert!(text.contains(name), "{text}");
    }

    let show = command(&["scenarios", "show", "charge_session"])
        .output()
        .unwrap();
    assert!(show.status.success());
    assert!(String::from_utf8_lossy(&show.stdout).contains("name = \"charge-session\""));

    let unknown = command(&["scenarios", "show", "nope"]).output().unwrap();
    assert_eq!(unknown.status.code(), Some(2));
    assert!(stderr(&unknown).contains("nope"));
}

#[tokio::test(flavor = "multi_thread")]
async fn run_builtin_scenario_json_passes() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let report_path = dir.path().join("report.json");
    let mut args = station_args(
        &csms,
        &["run", "--scenario", "charge-session", "--json", "--report"],
    );
    args.push(report_path.to_str().unwrap().to_owned());
    let output = run(args).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));

    let lines = jsonl(&output);
    let frames: Vec<&Value> = lines.iter().filter(|l| l["type"] == "frame").collect();
    let action_out = |action: &str| {
        frames
            .iter()
            .any(|f| f["direction"] == "out" && f["action"] == action && f["msg_id"].is_string())
    };
    assert!(action_out("BootNotification"));
    assert!(action_out("StartTransaction"));
    let start = frames
        .iter()
        .find(|f| f["action"] == "StartTransaction")
        .unwrap();
    assert!(start["payload"]["idTag"].is_string());
    assert!(start["ts"].is_string());
    assert!(lines.iter().any(|l| l["type"] == "log"));

    let report = lines.last().unwrap();
    assert_eq!(report["type"], "report");
    assert_eq!(report["passed"], true);
    assert_eq!(report["scenario"], "charge-session");
    let saved: Value = serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(saved["passed"], true);
    assert!(
        saved["steps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["status"] == "passed")
    );
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_human_output_and_failing_scenario_exit_code() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "bad.toml",
        "name = \"bad\"\n[[steps]]\naction = \"authorize\"\nid_tag = \"BADTAG\"\n[[steps]]\naction = \"heartbeat\"\n",
    );
    let junit = dir.path().join("junit.xml");
    let output = run(station_args(
        &csms,
        &[
            "run",
            "--scenario",
            &file,
            "--junit",
            junit.to_str().unwrap(),
        ],
    ))
    .await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("FAIL") && text.contains("Blocked"), "{text}");
    assert!(text.contains("SKIP") && text.contains("FAILED"), "{text}");
    let xml = std::fs::read_to_string(&junit).unwrap();
    assert!(
        xml.contains(
            "<testsuite name=\"bad\" tests=\"2\" failures=\"1\" errors=\"0\" skipped=\"1\""
        ),
        "{xml}"
    );
    assert!(
        xml.contains("<failure message=") && xml.contains("Blocked"),
        "{xml}"
    );
    assert!(xml.contains("<skipped/>"), "{xml}");
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_rejects_bad_input_with_exit_code_2() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let broken = write(
        &dir,
        "broken.toml",
        "name = \"x\"\n[[steps]]\naction = \"explode\"\n",
    );
    let output = run(station_args(&csms, &["run", "--scenario", &broken])).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("step 1") && stderr(&output).contains("explode"));

    let output = run(station_args(
        &csms,
        &["run", "--scenario", "/no/such/file.toml"],
    ))
    .await;
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        csms.connections(),
        0,
        "bad input must fail before connecting"
    );
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_timeout_fails_the_scenario() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let slow = write(
        &dir,
        "slow.toml",
        "name = \"slow\"\n[[steps]]\naction = \"sleep\"\nms = 60000\n",
    );
    let output = run(station_args(
        &csms,
        &["run", "--scenario", &slow, "--timeout", "1", "--json"],
    ))
    .await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let lines = jsonl(&output);
    let report = lines.last().unwrap();
    assert_eq!(report["cancelled"], true);
    assert_eq!(report["passed"], false);
    csms.shutdown().await;
}

// Sends a real SIGTERM through `kill`, which only exists on unix.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn headless_logs_jsonl_and_shuts_down_on_sigterm() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    let args = station_args(&csms, &["--json", "--log-file", log.to_str().unwrap()]);
    let child = command(&args.iter().map(String::as_str).collect::<Vec<_>>())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id().to_string();
    csms.wait_for_call("BootNotification", STEP).await.unwrap();
    csms.wait_for_call("StatusNotification", STEP)
        .await
        .unwrap();

    let status = Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    assert!(status.success());
    let output = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    csms.wait_for_disconnect(STEP).await.unwrap();

    let lines = jsonl(&output);
    assert!(
        lines
            .iter()
            .any(|l| l["type"] == "frame" && l["action"] == "BootNotification")
    );
    let file_lines = read_lines(&log);
    assert_eq!(file_lines.len(), lines.len(), "log file mirrors stdout");
    csms.shutdown().await;
}

// `run` interrupted by SIGINT exits 130 and still closes the WebSocket.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn run_exits_130_on_sigint() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let slow = write(
        &dir,
        "slow.toml",
        "name = \"slow\"\n[[steps]]\naction = \"sleep\"\nms = 60000\n",
    );
    let args = station_args(&csms, &["run", "--scenario", &slow, "--json"]);
    let child = command(&args.iter().map(String::as_str).collect::<Vec<_>>())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id().to_string();
    csms.wait_for_call("BootNotification", STEP).await.unwrap();
    csms.wait_for_call("StatusNotification", STEP)
        .await
        .unwrap();

    let status = Command::new("kill").args(["-INT", &pid]).status().unwrap();
    assert!(status.success());
    let output = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(130), "{}", stderr(&output));
    assert!(stderr(&output).contains("interrupted"));
    csms.wait_for_disconnect(STEP).await.unwrap();
    csms.shutdown().await;
}

#[cfg(unix)]
fn read_lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn run_var_overrides_scenario_vars_and_bad_names_exit_2() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "vars.toml",
        "name = \"v\"\n[vars]\nwho = \"scenario\"\nn = 1\n\
         [[steps]]\naction = \"data_transfer\"\nvendor_id = \"${station_id}-${index}-${who}\"\n\
         [[steps]]\naction = \"send_raw\"\ncall = \"StatusNotification\"\n\
         payload = { connectorId = \"${n}\", status = \"Available\", errorCode = \"NoError\" }\n",
    );
    let output = run(station_args(
        &csms,
        &[
            "run",
            "--scenario",
            &file,
            "--json",
            "--var",
            "who=cli",
            "--var",
            "n=2",
        ],
    ))
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let lines = jsonl(&output);
    let sent = |action: &str| {
        lines
            .iter()
            .find(|l| l["type"] == "frame" && l["direction"] == "out" && l["action"] == action)
            .map_or_else(|| panic!("no {action} frame"), |l| l["payload"].clone())
    };
    assert_eq!(sent("DataTransfer")["vendorId"], "CP-CLI-1-cli");
    let statuses: Vec<&Value> = lines
        .iter()
        .filter(|l| l["type"] == "frame" && l["action"] == "StatusNotification")
        .collect();
    assert!(
        statuses.iter().any(|l| l["payload"]["connectorId"] == 2),
        "the number stays a number: {statuses:?}"
    );

    for bad in ["nonsense", "1a=2", "index=3", "station_id=x"] {
        let output = run(station_args(
            &csms,
            &["run", "--scenario", "charge-session", "--var", bad],
        ))
        .await;
        assert_eq!(output.status.code(), Some(2), "{bad}");
    }
    csms.shutdown().await;
}
