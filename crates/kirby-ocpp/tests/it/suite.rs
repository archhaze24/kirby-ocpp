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
    // A suite inside a CI job must not write into the job's own step summary.
    command.env_remove("GITHUB_STEP_SUMMARY");
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
        let mut command = command(&args);
        command.current_dir(dir.path());
        crate::support::output(command)
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
        crate::support::ChildGuard::spawn(
            command(&args).stdout(Stdio::piped()).stderr(Stdio::piped()),
        )
    };
    // The second scenario's station has connected and booted: the quick scenario finished and
    // the slow one is running (the signal streams exist since before the first scenario).
    csms.wait_for_connection_count(2, std::time::Duration::from_secs(30))
        .await
        .expect("second connection");
    csms.wait_for_call_count_after(
        0,
        "BootNotification",
        2,
        |_| true,
        std::time::Duration::from_secs(30),
    )
    .await
    .expect("second boot");
    let sent = Command::new("kill")
        .args([signal, &child.id().to_string()])
        .status()
        .expect("kill");
    assert!(sent.success());
    let output = crate::support::finish(child).await;
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

/// Top-level keys and JSON types of the suite report are a public contract.
#[tokio::test(flavor = "multi_thread")]
async fn suite_report_has_the_contract_shape() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let output = run(suite_args(&csms, &["charge-session", "--json"])).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let summary: Value = stdout(&output)
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .next_back()
        .unwrap();
    let mut keys: Vec<&str> = summary
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
            "flaky_names",
            "ocpp_version",
            "passed",
            "passed_count",
            "retries",
            "scenarios",
            "schema",
            "skipped",
            "started_at",
            "status",
            "tool_version",
            "total",
            "type",
        ]
    );
    for key in [
        "ocpp_version",
        "started_at",
        "status",
        "tool_version",
        "type",
    ] {
        assert!(summary[key].is_string(), "{key}");
    }
    assert!(summary["schema"].is_u64() && summary["duration_ms"].is_u64());
    assert!(summary["total"].is_u64() && summary["passed_count"].is_u64());
    assert!(summary["passed"].is_boolean());
    for key in [
        "failed",
        "failed_names",
        "error_names",
        "skipped",
        "scenarios",
    ] {
        assert!(summary[key].is_array(), "{key}");
    }
    assert_eq!(summary["counts"].as_object().unwrap().len(), 4);
    let entry = summary["scenarios"][0].as_object().unwrap();
    let mut keys: Vec<&str> = entry.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "attempt_results",
            "attempts",
            "duration_ms",
            "flaky",
            "name",
            "ocpp_version",
            "report",
            "scenario",
            "source",
            "started_at",
            "station",
            "status",
        ]
    );
    assert_eq!(entry["scenario"], entry["name"]);
    assert_eq!(entry["started_at"], entry["report"]["started_at"]);
    assert_eq!(entry["attempts"], 1);
    assert_eq!(entry["flaky"], false);
    assert_eq!(entry["attempt_results"].as_array().unwrap().len(), 1);
    assert_eq!(summary["retries"], 0);
    assert_eq!(summary["flaky_names"], serde_json::json!([]));
    csms.shutdown().await;
}

/// A scenario with a tag line, for the selection tests.
fn tagged(name: &str, tags: &[&str], body: &str) -> String {
    let list: Vec<String> = tags.iter().map(|t| format!("\"{t}\"")).collect();
    format!("name = \"{name}\"\ntags = [{}]\n{body}", list.join(", "))
}

/// Names of the scenarios of a suite report, in report order.
fn scenario_names(summary: &Value) -> Vec<String> {
    summary["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_owned())
        .collect()
}

async fn report_of(csms: &MockCsms, extra: &[&str]) -> (Output, Value) {
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("report.json");
    let mut all: Vec<&str> = extra.to_vec();
    all.extend(["--report", report.to_str().unwrap()]);
    let output = run(suite_args(csms, &all)).await;
    let text = std::fs::read_to_string(&report).unwrap_or_else(|e| {
        panic!("no report ({e}): {}", stderr(&output));
    });
    (output, serde_json::from_str(&text).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn filter_skip_and_tags_select_scenarios_in_order() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let a = write(&dir, "a.toml", &tagged("smoke-one", &["smoke"], PASSING));
    let b = write(
        &dir,
        "b.toml",
        &tagged("smoke-two", &["smoke", "slow"], PASSING),
    );
    let c = write(&dir, "c.toml", &tagged("other", &["slow"], PASSING));
    let d = write(&dir, "d.toml", &named("untagged", PASSING));
    let sources = [a.as_str(), b.as_str(), c.as_str(), d.as_str()];
    let with = |extra: &[&str]| -> Vec<String> {
        sources
            .iter()
            .chain(extra.iter())
            .map(|s| (*s).to_owned())
            .collect()
    };
    let select = |extra: &[&str]| {
        let all = with(extra);
        let csms = &csms;
        async move {
            let refs: Vec<&str> = all.iter().map(String::as_str).collect();
            let (output, summary) = report_of(csms, &refs).await;
            assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
            scenario_names(&summary)
        }
    };
    assert_eq!(
        select(&["--filter", "smoke-*"]).await,
        ["smoke-one", "smoke-two"]
    );
    assert_eq!(
        select(&["--filter", "smoke-one", "--filter", "other"]).await,
        ["smoke-one", "other"]
    );
    assert_eq!(
        select(&["--filter", "smoke-*", "--skip", "*two"]).await,
        ["smoke-one"]
    );
    assert_eq!(
        select(&["--skip", "smoke-?ne", "--skip", "untagged"]).await,
        ["smoke-two", "other"]
    );
    assert_eq!(select(&["--tag", "slow"]).await, ["smoke-two", "other"]);
    assert_eq!(
        select(&["--tag", "slow", "--tag", "smoke"]).await,
        ["smoke-two"]
    );
    assert_eq!(
        select(&["--skip-tag", "slow"]).await,
        ["smoke-one", "untagged"]
    );
    assert_eq!(
        select(&[
            "--filter",
            "smoke-*",
            "--tag",
            "smoke",
            "--skip-tag",
            "slow"
        ])
        .await,
        ["smoke-one"]
    );
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_selection_is_a_usage_error() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let a = write(&dir, "a.toml", &named("one", PASSING));
    for extra in [["--filter", "nothing-*"], ["--tag", "missing"]] {
        let output = run(suite_args(&csms, &[&a, extra[0], extra[1]])).await;
        assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
        assert!(
            stderr(&output).contains("matches none of the 1 scenarios"),
            "{}",
            stderr(&output)
        );
    }
    assert_eq!(csms.connections(), 0, "nothing ran");
    // Built-ins have no tags yet; a tag selection over them is empty too.
    let output = run(suite_args(&csms, &["--all-builtins", "--tag", "x"])).await;
    assert_eq!(output.status.code(), Some(2));
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_report_a_flaky_pass_without_changing_the_status() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    // The first Authorize is refused, every later one accepted.
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    csms.respond("Authorize", move |_| {
        let status = if seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            "Invalid"
        } else {
            "Accepted"
        };
        kirby_ocpp_testkit::Reply::Result(serde_json::json!({"idTagInfo": {"status": status}}))
    });
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "wobbly.toml",
        &named(
            "wobbly",
            "[[steps]]\naction = \"authorize\"\nid_tag = \"ANYTAG\"\n",
        ),
    );
    let junit = dir.path().join("junit.xml");
    let summary_md = dir.path().join("summary.md");
    let (output, summary) = report_of(
        &csms,
        &[
            &file,
            "--retries",
            "2",
            "--junit",
            junit.to_str().unwrap(),
            "--summary-md",
            summary_md.to_str().unwrap(),
            "--github-annotations",
        ],
    )
    .await;
    assert_eq!(
        output.status.code(),
        Some(0),
        "a flaky pass is a pass: {}",
        stderr(&output)
    );
    let text = stdout(&output);
    assert!(text.contains("RETRY wobbly: "), "{text}");
    assert!(text.contains("PASS wobbly ("), "{text}");
    assert!(text.contains("flaky: passed on attempt 2"), "{text}");
    assert!(
        text.contains("suite: 1/1 scenarios passed, FLAKY: wobbly"),
        "{text}"
    );
    assert!(text.contains("::warning file="), "{text}");
    assert_eq!(summary["status"], "passed");
    assert_eq!(summary["retries"], 2);
    assert_eq!(summary["flaky_names"], serde_json::json!(["wobbly"]));
    let entry = &summary["scenarios"][0];
    assert_eq!(entry["status"], "passed");
    assert_eq!(entry["attempts"], 2);
    assert_eq!(entry["flaky"], true);
    let results = entry["attempt_results"].as_array().unwrap();
    assert_eq!(results[0]["status"], "failed");
    assert_eq!(results[1]["status"], "passed");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(csms.connections(), 2, "each attempt uses a fresh station");

    let xml = std::fs::read_to_string(&junit).unwrap();
    assert!(
        xml.contains("<property name=\"flaky\" value=\"true\"/>"),
        "{xml}"
    );
    assert!(xml.contains("<flakyFailure "), "{xml}");
    assert!(xml.contains("classname=\"ocpp1.6.wobbly\""), "{xml}");
    let md = std::fs::read_to_string(&summary_md).unwrap();
    assert!(md.contains("| wobbly | flaky |"), "{md}");
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_keep_a_persistent_failure_failed() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = write(&dir, "bad.toml", &named("bad", FAILING));
    let junit = dir.path().join("junit.xml");
    let (output, summary) = report_of(
        &csms,
        &[&file, "--retries", "1", "--junit", junit.to_str().unwrap()],
    )
    .await;
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout(&output).contains("FAIL bad: "),
        "{}",
        stdout(&output)
    );
    assert!(
        stdout(&output).contains("(after 2 attempts)"),
        "{}",
        stdout(&output)
    );
    let entry = &summary["scenarios"][0];
    assert_eq!(entry["status"], "failed");
    assert_eq!(entry["attempts"], 2);
    assert_eq!(entry["flaky"], false);
    assert_eq!(summary["flaky_names"], serde_json::json!([]));
    let xml = std::fs::read_to_string(&junit).unwrap();
    assert!(xml.contains("<rerunFailure "), "{xml}");
    assert!(!xml.contains("flakyFailure"), "{xml}");
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn jobs_run_in_parallel_and_report_in_selection_order() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    // The first scenario is the slowest: it finishes last but is reported first.
    let slow = write(
        &dir,
        "a.toml",
        &named("slow", "[[steps]]\naction = \"sleep\"\nms = 1500\n"),
    );
    let one = write(&dir, "b.toml", &named("one", PASSING));
    let two = write(&dir, "c.toml", &named("two", PASSING));
    let (output, summary) = report_of(&csms, &[&slow, &one, &two, "--jobs", "3"]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(scenario_names(&summary), ["slow", "one", "two"]);
    let text = stdout(&output);
    let position = |name: &str| text.find(&format!("PASS {name} (")).unwrap();
    assert!(
        position("one") < position("slow"),
        "progress lines come in completion order: {text}"
    );
    for line in text.lines() {
        assert!(
            line.starts_with("PASS ") || line.starts_with("suite: "),
            "whole lines only: {line:?}"
        );
    }
    // Three jobs, three different station ids.
    let mut ids: Vec<String> = summary["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["station"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    assert_eq!(ids, ["CP-SUITE", "CP-SUITE-2", "CP-SUITE-3"]);
    assert_eq!(csms.connections(), 3);
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn jobs_and_retries_reject_out_of_range_values() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    for extra in [["--jobs", "0"], ["--jobs", "17"], ["--retries", "11"]] {
        let output = run(suite_args(&csms, &["heartbeat", extra[0], extra[1]])).await;
        assert_eq!(output.status.code(), Some(2), "{extra:?}");
    }
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn annotations_and_summary_describe_a_failing_suite() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let good = write(&dir, "a.toml", &named("good", PASSING));
    let bad = write(&dir, "b.toml", &named("bad,name", FAILING));
    let md = dir.path().join("summary.md");
    let env_md = dir.path().join("step-summary.md");
    std::fs::write(&env_md, "earlier step\n").unwrap();
    let args = suite_args(
        &csms,
        &[
            &good,
            &bad,
            "--github-annotations",
            "--summary-md",
            md.to_str().unwrap(),
        ],
    );
    let output = {
        let mut cmd = command(&args.iter().map(String::as_str).collect::<Vec<_>>());
        cmd.env("GITHUB_STEP_SUMMARY", &env_md);
        crate::support::output(cmd)
    };
    assert_eq!(output.status.code(), Some(1));
    let text = stdout(&output);
    let annotation = text
        .lines()
        .find(|l| l.starts_with("::error "))
        .unwrap_or_else(|| panic!("no annotation: {text}"));
    assert!(
        annotation.starts_with(&format!(
            "::error file={},title=bad%2Cname::",
            b_escaped(&bad)
        )),
        "{annotation}"
    );
    assert!(annotation.contains("authorize"), "{annotation}");
    assert_eq!(text.lines().filter(|l| l.starts_with("::")).count(), 1);
    let table = std::fs::read_to_string(&md).unwrap();
    assert!(table.contains("| good | passed |"), "{table}");
    assert!(table.contains("| bad,name | failed |"), "{table}");
    let appended = std::fs::read_to_string(&env_md).unwrap();
    assert!(
        appended.starts_with("earlier step\n## kirby-ocpp suite"),
        "{appended}"
    );
    csms.shutdown().await;
}

/// A path as it appears in the `file=` property of a workflow command.
fn b_escaped(path: &str) -> String {
    path.replace('%', "%25")
        .replace(':', "%3A")
        .replace(',', "%2C")
}

#[tokio::test(flavor = "multi_thread")]
async fn annotations_cannot_be_combined_with_json() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let output = run(suite_args(
        &csms,
        &["heartbeat", "--github-annotations", "--json"],
    ))
    .await;
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("--json"), "{}", stderr(&output));
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn summary_md_refuses_a_scenario_file() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = write(&dir, "a.toml", &named("one", PASSING));
    let output = run(suite_args(&csms, &[&file, "--summary-md", &file])).await;
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("heartbeat")
    );
    csms.shutdown().await;
}
