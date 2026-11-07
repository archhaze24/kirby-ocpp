//! Process tests of fleet mode (`--count N`) against a mock CSMS in the test process.

use std::collections::BTreeSet;
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
        crate::support::output(command(&args))
    })
    .await
    .unwrap()
}

fn fleet_args(csms: &MockCsms, count: &str, extra: &[&str]) -> Vec<String> {
    args_with_id(csms, "CP-{n}", count, extra)
}

fn args_with_id(csms: &MockCsms, id: &str, count: &str, extra: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = [
        "--url",
        csms.url(),
        "--id",
        id,
        "--count",
        count,
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

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn write(dir: &TempDir, name: &str, text: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

fn connected_ids(csms: &MockCsms) -> BTreeSet<String> {
    csms.connection_log()
        .into_iter()
        .filter_map(|c| c.charge_point_id)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_fleet_options_are_usage_errors() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let url = csms.url().to_owned();
    let case = |extra: &[&str]| {
        let mut args = vec!["--url".to_owned(), url.clone(), "--no-tui".to_owned()];
        args.extend(extra.iter().map(|s| (*s).to_owned()));
        args
    };
    // No placeholder: every station would get the same id.
    let output = run(case(&["--count", "3"])).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("{n}"), "{}", stderr(&output));
    // Count out of range.
    for count in ["0", "1001"] {
        let output = run(case(&["--id", "CP-{n}", "--count", count])).await;
        assert_eq!(output.status.code(), Some(2), "{count}");
    }
    // Malformed template, with or without a fleet.
    for extra in [
        &["--id", "CP-{x}", "--count", "2"][..],
        &["--id-template", "CP-{n", "--count", "1"][..],
    ] {
        let output = run(case(extra)).await;
        assert_eq!(output.status.code(), Some(2), "{extra:?}");
        assert!(stderr(&output).contains("template"), "{}", stderr(&output));
    }
    assert_eq!(
        csms.connections(),
        0,
        "bad input must fail before connecting"
    );
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn print_config_shows_the_fleet() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let output = run(fleet_args(
        &csms,
        "5",
        &["--first-index", "7", "--stagger-ms", "20", "--print-config"],
    ))
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["fleet"]["count"], 5);
    assert_eq!(value["fleet"]["first_index"], 7);
    assert_eq!(value["fleet"]["stagger_ms"], 20);
    assert_eq!(value["config"]["charge_point_id"], "CP-7");

    // A single station is printed exactly as before.
    let output = run(args_with_id(&csms, "ONE", "1", &["--print-config"])).await;
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["charge_point_id"], "ONE");
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_scenario_on_every_station_human_output() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("report.json");
    let output = run(fleet_args(
        &csms,
        "2",
        &[
            "run",
            "--scenario",
            "charge-session",
            "--stagger-ms",
            "10",
            "--report",
            report.to_str().unwrap(),
            "--junit",
            dir.path().join("junit.xml").to_str().unwrap(),
        ],
    ))
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let xml = std::fs::read_to_string(dir.path().join("junit.xml")).unwrap();
    assert_eq!(xml.matches("<testsuite ").count(), 2, "{xml}");
    for id in ["CP-1", "CP-2"] {
        assert!(
            xml.contains(&format!("<testsuite name=\"charge-session [{id}]\"")),
            "{xml}"
        );
    }
    assert!(xml.contains("failures=\"0\" errors=\"0\""), "{xml}");
    let text = stdout(&output);
    for id in ["CP-1", "CP-2"] {
        assert!(
            text.contains(&format!("[{id}] scenario charge-session")),
            "{text}"
        );
        assert!(text.contains(&format!("[{id}] PASSED")), "{text}");
        assert!(report.with_file_name(format!("report.{id}.json")).exists());
    }
    assert!(text.contains("fleet: 2/2 stations PASSED"), "{text}");
    // Every non-aggregate line is tagged.
    for line in text.lines().filter(|l| !l.starts_with("fleet:")) {
        assert!(line.starts_with("[CP-"), "{line}");
    }
    assert_eq!(
        connected_ids(&csms),
        BTreeSet::from(["CP-1".to_owned(), "CP-2".to_owned()])
    );
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_scenario_json_tags_every_line_and_aggregates() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let output = run(fleet_args(
        &csms,
        "3",
        &[
            "--first-index",
            "10",
            "run",
            "--scenario",
            "charge-session",
            "--json",
        ],
    ))
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let lines = jsonl(&output);
    let (aggregate, rest) = lines.split_last().unwrap();
    assert_eq!(aggregate["type"], "fleet_report");
    assert_eq!(aggregate["passed"], true);
    assert_eq!(aggregate["total"], 3);
    assert_eq!(aggregate["failed"].as_array().unwrap().len(), 0);
    assert!(rest.iter().all(|l| l["station"].is_string()), "{rest:?}");
    let reports: BTreeSet<&str> = rest
        .iter()
        .filter(|l| l["type"] == "report")
        .map(|l| l["station"].as_str().unwrap())
        .collect();
    assert_eq!(reports, BTreeSet::from(["CP-10", "CP-11", "CP-12"]));
    assert!(
        rest.iter()
            .any(|l| l["type"] == "frame" && l["station"] == "CP-11")
    );
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failing_scenario_exits_1_with_the_failed_stations() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "bad.toml",
        "name = \"bad\"\n[[steps]]\naction = \"authorize\"\nid_tag = \"BADTAG\"\n",
    );
    let output = run(fleet_args(&csms, "2", &["run", "--scenario", &file])).await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("[CP-1] FAILED") && text.contains("[CP-2] FAILED"),
        "{text}"
    );
    assert!(
        text.contains("fleet: 0/2 stations passed, FAILED: CP-1, CP-2"),
        "{text}"
    );
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn single_station_output_stays_untagged() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let output = run(args_with_id(
        &csms,
        "ONLY",
        "1",
        &["run", "--scenario", "charge-session", "--json"],
    ))
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let lines = jsonl(&output);
    // Only the report names its station (report metadata); event lines stay untagged.
    assert!(
        lines
            .iter()
            .filter(|l| l["type"] != "report")
            .all(|l| l.get("station").is_none())
    );
    assert_eq!(lines.last().unwrap()["type"], "report");
    assert_eq!(lines.last().unwrap()["station"], "ONLY");
    assert_eq!(connected_ids(&csms), BTreeSet::from(["ONLY".to_owned()]));

    // An explicit template is expanded even for one station.
    let output = run(args_with_id(
        &csms,
        "UNUSED",
        "1",
        &[
            "--id-template",
            "T{n:03}",
            "--first-index",
            "4",
            "run",
            "--scenario",
            "charge-session",
        ],
    ))
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(connected_ids(&csms).contains("T004"));
    assert!(!stdout(&output).contains("[T004]"));
    csms.shutdown().await;
}

// Sends a real SIGTERM through `kill`, which only exists on unix.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn headless_fleet_tags_lines_and_shuts_down_on_sigterm() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.log");
    let args = fleet_args(
        &csms,
        "3",
        &["--log-file", log.to_str().unwrap(), "--stagger-ms", "5"],
    );
    let child = crate::support::ChildGuard::spawn(
        command(&args.iter().map(String::as_str).collect::<Vec<_>>())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
    let pid = child.id().to_string();
    csms.wait_for_connection_count(3, STEP).await.unwrap();
    csms.wait_for_call_count_after(0, "BootNotification", 3, |_| true, STEP)
        .await
        .unwrap();

    let status = Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    assert!(status.success());
    let output = crate::support::finish(child).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    csms.wait_for_disconnect(STEP).await.unwrap();

    let text = stdout(&output);
    assert!(text.lines().all(|l| l.starts_with("[CP-")), "{text}");
    for id in ["CP-1", "CP-2", "CP-3"] {
        assert!(
            text.lines()
                .any(|l| l.starts_with(&format!("[{id}] ")) && l.contains("BootNotification")),
            "{id}: {text}"
        );
    }
    let file = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        file.lines().count(),
        text.lines().count(),
        "log file mirrors stdout"
    );
    csms.shutdown().await;
}

/// Top-level keys and JSON types of the fleet report are a public contract.
#[tokio::test(flavor = "multi_thread")]
async fn fleet_report_has_the_contract_shape() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let output = run(fleet_args(
        &csms,
        "2",
        &["run", "--scenario", "charge-session", "--json"],
    ))
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let lines = jsonl(&output);
    let report = lines.last().unwrap();
    let mut keys: Vec<&str> = report
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "counts",
            "duration_ms",
            "error_names",
            "failed",
            "failed_names",
            "ocpp_version",
            "passed",
            "passed_count",
            "scenario",
            "schema",
            "source",
            "started_at",
            "stations",
            "status",
            "tool_version",
            "total",
            "type",
        ]
    );
    for key in [
        "ocpp_version",
        "scenario",
        "source",
        "started_at",
        "status",
        "tool_version",
        "type",
    ] {
        assert!(report[key].is_string(), "{key}");
    }
    assert!(report["schema"].is_u64() && report["duration_ms"].is_u64());
    assert!(report["total"].is_u64() && report["passed_count"].is_u64());
    assert!(report["passed"].is_boolean());
    for key in ["failed", "failed_names", "error_names", "stations"] {
        assert!(report[key].is_array(), "{key}");
    }
    let counts = report["counts"].as_object().unwrap();
    assert_eq!(counts.len(), 4);
    assert_eq!(counts["skipped"], 0);
    assert_eq!(counts["passed"], 2);
    let entry = report["stations"][0].as_object().unwrap();
    let mut keys: Vec<&str> = entry.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "duration_ms",
            "ocpp_version",
            "passed",
            "scenario",
            "schema",
            "source",
            "started_at",
            "station",
            "status",
            "tool_version",
        ]
    );
    assert!(entry["duration_ms"].is_u64() && entry["passed"].is_boolean());
    csms.shutdown().await;
}
