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
    assert_eq!(summary["schema"], 1);
    assert_eq!(summary["status"], "failed");
    assert_eq!(summary["tool_version"], env!("CARGO_PKG_VERSION"));
    assert!(
        summary["started_at"]
            .as_str()
            .is_some_and(|t| t.ends_with('Z'))
    );
    assert_eq!(
        summary["counts"],
        serde_json::json!({"passed": 0, "failed": 1, "error": 0, "skipped": 1})
    );
    assert_eq!(summary["scenarios"][0]["ocpp_version"], "1.6");
    assert!(
        summary["scenarios"][0]["source"]
            .as_str()
            .is_some_and(|s| s.ends_with("bad.toml"))
    );
    assert!(summary["scenarios"][0]["station"].is_string());
    assert_eq!(summary["scenarios"][0]["report"]["schema"], 1);
    assert_eq!(summary["scenarios"][0]["report"]["status"], "failed");
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

#[tokio::test(flavor = "multi_thread")]
async fn an_existing_path_beats_a_built_in_name_and_repeats_run_once() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    // A file called like a built-in, found through the working directory.
    write(&dir, "heartbeat", &named("mine", PASSING));
    let one = write(&dir, "one.toml", &named("one", PASSING));
    let dir_path = dir.path().to_str().unwrap().to_owned();
    let mut args = suite_args(&csms, &["heartbeat", &one, &dir_path]);
    args.push("--json".to_owned());
    let output = tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        command(&args)
            .current_dir(dir.path())
            .output()
            .expect("binary runs")
    })
    .await
    .unwrap();
    let text = stdout(&output);
    let summary: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    let names: Vec<&str> = summary["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    // `heartbeat` (a file here, the path wins), `one` once, and the directory adds only
    // the files not yet seen (`heartbeat` has no `.toml` extension).
    assert_eq!(names, ["mine", "one"], "{text}{}", stderr(&output));
    csms.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_fifo_source_is_refused_instead_of_blocking() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("pipe.toml");
    let made = Command::new("mkfifo").arg(&fifo).status().expect("mkfifo");
    assert!(made.success());
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        run(suite_args(&csms, &[fifo.to_str().unwrap()])),
    )
    .await
    .expect("must not block on a FIFO");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("not a regular file"),
        "{}",
        stderr(&output)
    );
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_vars_warn_in_suite_and_fail_run() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "v.toml",
        &named(
            "v",
            "[vars]\nk = 1\n[[steps]]\naction = \"data_transfer\"\nvendor_id = \"${w}\"\n",
        ),
    );
    let output = run(suite_args(
        &csms,
        &[&file, "--var", "k=2", "--var", "w=x", "--var", "zzz=1"],
    ))
    .await;
    assert!(
        stderr(&output).contains("does not use the variable `zzz`"),
        "{}",
        stderr(&output)
    );
    assert!(!stderr(&output).contains("`k`") && !stderr(&output).contains("`w`"));

    let args: Vec<String> = [
        "--url",
        csms.url(),
        "--id",
        "CP-RUN",
        "--no-tui",
        "run",
        "--scenario",
        &file,
        "--var",
        "zzz=1",
    ]
    .map(String::from)
    .into();
    let output = run(args).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("unknown variable `zzz`"),
        "{}",
        stderr(&output)
    );
    csms.shutdown().await;
}

/// Starts a suite whose first scenario passes and second sleeps, signals the process while
/// it sleeps and returns the exit code and the report file.
#[cfg(unix)]
async fn signalled_suite(signal: &str) -> (Option<i32>, Value) {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let quick = write(&dir, "a.toml", &named("quick", PASSING));
    let slow = write(
        &dir,
        "b.toml",
        &named("slow", "[[steps]]\naction = \"sleep\"\nms = 60000\n"),
    );
    let last = write(&dir, "c.toml", &named("last", PASSING));
    let report = dir.path().join("report.json");
    let junit = dir.path().join("junit.xml");
    let args = suite_args(
        &csms,
        &[
            &quick,
            &slow,
            &last,
            "--report",
            report.to_str().unwrap(),
            "--junit",
            junit.to_str().unwrap(),
        ],
    );
    let child = {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        command(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("binary runs")
    };
    // Two connections: the quick scenario finished, the slow one is sleeping.
    while csms.connections() < 2 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let sent = Command::new("kill")
        .args([signal, &child.id().to_string()])
        .status()
        .expect("kill");
    assert!(sent.success());
    let output = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    let text = std::fs::read_to_string(&report).expect("the report is written after a signal");
    assert!(junit.exists(), "the JUnit report is written after a signal");
    csms.shutdown().await;
    let summary: Value = serde_json::from_str(&text).unwrap();
    (output.status.code(), summary)
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn sigint_writes_the_reports_and_exits_130() {
    let (code, summary) = signalled_suite("-INT").await;
    assert_eq!(code, Some(130));
    let statuses: Vec<&str> = summary["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["passed", "error", "skipped"], "{summary}");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn sigterm_writes_the_reports_and_exits_1() {
    let (code, summary) = signalled_suite("-TERM").await;
    assert_eq!(code, Some(1));
    assert_eq!(summary["scenarios"][1]["status"], "error", "{summary}");
}
