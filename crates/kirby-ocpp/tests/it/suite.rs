//! Process tests of `kirby-ocpp suite` against a mock CSMS running in the test process.

use std::process::{Command, Output, Stdio};

use kirby_ocpp_testkit::{CsmsOptions, MockCsms};
use serde_json::Value;
use tempfile::TempDir;

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

/// `suite` arguments: the station options first, then `suite` and `extra`.
fn suite_args(csms: &MockCsms, extra: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = ["--url", csms.url(), "--id", "CP-SUITE", "--no-tui", "suite"]
        .map(String::from)
        .into();
    args.extend(extra.iter().map(|s| (*s).to_owned()));
    args
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

const PASSING: &str = "[[steps]]\naction = \"heartbeat\"\n";
const FAILING: &str =
    "[[steps]]\naction = \"authorize\"\nid_tag = \"BADTAG\"\n[[steps]]\naction = \"heartbeat\"\n";

fn named(name: &str, body: &str) -> String {
    format!("name = \"{name}\"\n{body}")
}

#[tokio::test(flavor = "multi_thread")]
async fn all_builtins_pass_with_one_junit_suite_each() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let junit = dir.path().join("junit.xml");
    let output = run(suite_args(
        &csms,
        &["--all-builtins", "--junit", junit.to_str().unwrap()],
    ))
    .await;
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    let text = stdout(&output);
    let total = kirby_ocpp_core::scenario::builtin::all().len() - 1; // minus `finish-session`
    assert_eq!(text.matches("PASS ").count(), total, "{text}");
    assert!(
        text.contains(&format!("suite: {total}/{total} scenarios passed")),
        "{text}"
    );
    let xml = std::fs::read_to_string(&junit).unwrap();
    assert_eq!(xml.matches("<testsuite ").count(), total, "{xml}");
    assert!(xml.contains("<testsuite name=\"charge-session\""), "{xml}");
    // Some scenarios reconnect on purpose, so there can be more connections.
    assert!(csms.connections() >= total, "one station per scenario");
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failing_file_gives_exit_1_and_is_named() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    write(&dir, "a_good.toml", &named("good", PASSING));
    write(&dir, "b_bad.toml", &named("bad", FAILING));
    let output = run(suite_args(&csms, &[dir.path().to_str().unwrap()])).await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("PASS good ("), "{text}");
    assert!(
        text.contains("FAIL bad: ") && text.contains("Blocked"),
        "{text}"
    );
    assert!(
        text.contains("suite: 1/2 scenarios passed, FAILED: bad"),
        "{text}"
    );
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fail_fast_skips_the_rest_and_json_summary_has_its_shape() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let bad = write(&dir, "bad.toml", &named("bad", FAILING));
    let good = write(&dir, "good.toml", &named("good", PASSING));
    let report = dir.path().join("suite.json");
    let output = run(suite_args(
        &csms,
        &[
            &bad,
            &good,
            "--fail-fast",
            "--json",
            "--report",
            report.to_str().unwrap(),
        ],
    ))
    .await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let lines: Vec<Value> = stdout(&output)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let summary = lines.last().unwrap();
    assert_eq!(summary["type"], "suite_report");
    assert_eq!(summary["passed"], false);
    assert_eq!(summary["total"], 2);
    assert_eq!(summary["passed_count"], 0);
    assert_eq!(summary["failed"], serde_json::json!(["bad"]));
    assert_eq!(summary["skipped"], serde_json::json!(["good"]));
    assert_eq!(summary["scenarios"][0]["status"], "failed");
    assert_eq!(summary["scenarios"][1]["status"], "skipped");
    assert!(summary["duration_ms"].is_u64());
    assert!(
        lines
            .iter()
            .any(|l| l["type"] == "report" && l["scenario"] == "bad"),
        "one report line per scenario that ran"
    );
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    assert_eq!(written["type"], "suite_report");
    assert_eq!(csms.connections(), 1, "the skipped scenario never connects");
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn usage_errors_exit_2() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let output = run(suite_args(&csms, &[])).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("SOURCE"), "{}", stderr(&output));

    let output = run(suite_args(&csms, &[dir.path().to_str().unwrap()])).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("*.toml"), "{}", stderr(&output));

    let mut args = suite_args(&csms, &["charge-session"]);
    args.splice(0..0, ["--count".to_owned(), "2".to_owned()]);
    let args = {
        // `--id` needs an index placeholder for a fleet.
        let mut args = args;
        args.splice(0..0, ["--id-template".to_owned(), "CP-{n}".to_owned()]);
        args
    };
    let output = run(args).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("fleet"), "{}", stderr(&output));

    let output = run(suite_args(&csms, &["/no/such/file.toml"])).await;
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        csms.connections(),
        0,
        "bad input must fail before connecting"
    );
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn every_scenario_gets_a_fresh_station_without_persisted_state() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    // `first` leaves a transaction running; `second` needs an idle connector.
    let first = write(
        &dir,
        "1_first.toml",
        &named(
            "first",
            "[[steps]]\naction = \"plug\"\nconnector = 1\n[[steps]]\naction = \"authorize\"\nid_tag = \"TAG1\"\n[[steps]]\naction = \"start_tx\"\nconnector = 1\nid_tag = \"TAG1\"\n",
        ),
    );
    let second = write(
        &dir,
        "2_second.toml",
        &named(
            "second",
            "[[steps]]\naction = \"wait_for\"\ntimeout_ms = 1000\n[steps.condition]\nkind = \"tx_idle\"\nconnector = 1\n",
        ),
    );
    let output = run(suite_args(
        &csms,
        &[
            "--state-dir",
            state.path().to_str().unwrap(),
            &first,
            &second,
        ],
    ))
    .await;
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        std::fs::read_dir(state.path()).unwrap().count(),
        0,
        "suite runs write no state"
    );
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_names_get_their_source_appended() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let one = write(&dir, "one.toml", &named("same", PASSING));
    let two = write(&dir, "two.toml", &named("same", FAILING));
    let output = run(suite_args(&csms, &[&one, &two])).await;
    assert_eq!(output.status.code(), Some(1));
    let text = stdout(&output);
    assert!(text.contains(&format!("PASS same ({one})")), "{text}");
    assert!(text.contains(&format!("FAIL same ({two}): ")), "{text}");
    csms.shutdown().await;
}
