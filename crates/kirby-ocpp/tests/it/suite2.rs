//! `run`, `suite` and `scenarios` against an OCPP 2.0.1 station, through the real binary and the
//! testkit mock CSMS.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use kirby_ocpp_testkit::{CsmsOptions, MockCsms, SubprotocolPolicy};
use serde_json::Value;

const STEP: Duration = Duration::from_secs(15);

const PLAIN: &str = "name = \"plain\"\nocpp_version = \"2.0.1\"\ndescription = \"x\"\ntags = [\"conformance\", \"ocpp2.0.1\"]\n\n[[steps]]\naction = \"heartbeat\"\n\n[[steps]]\naction = \"expect_call\"\ncall = \"Heartbeat\"\ndirection = \"out\"\ntimeout_ms = 3000\n";
const OTHER: &str = "name = \"other\"\nocpp_version = \"2.0.1\"\ndescription = \"x\"\n\n[[steps]]\naction = \"heartbeat\"\n";
const V16: &str = "name = \"old\"\ndescription = \"x\"\n\n[[steps]]\naction = \"heartbeat\"\n";

async fn v201_csms() -> MockCsms {
    MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Require("ocpp2.0.1".to_owned()),
        ..CsmsOptions::default()
    })
    .await
    .unwrap()
}

fn command(args: &[String]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kirby-ocpp"));
    for (key, _) in std::env::vars() {
        if key.starts_with("OCPP_") || key.starts_with("KIRBY_OCPP_") {
            command.env_remove(key);
        }
    }
    command.args(args).stdin(Stdio::null());
    command
}

/// The binary with `args` (no connection flags), on a blocking thread with a deadline.
async fn kirby(args: Vec<String>) -> Output {
    tokio::task::spawn_blocking(move || crate::support::output(command(&args)))
        .await
        .unwrap()
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}

/// Global flags of a 2.0.1 station that connects to `url`, then `rest`.
fn station(url: &str, rest: &[&str]) -> Vec<String> {
    let mut args = strings(&[
        "--url",
        url,
        "--id",
        "CP-S2",
        "--protocol",
        "ocpp2.0.1",
        "--no-persist",
        "--no-tui",
    ]);
    args.extend(rest.iter().map(|s| (*s).to_owned()));
    args
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn write(dir: &Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

fn json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// A URL that refuses connections.
fn unused_url() -> String {
    kirby_ocpp_testkit::refused_url()
}

// run

#[tokio::test(flavor = "multi_thread")]
async fn run_passes_a_2_0_1_file_and_writes_the_reports() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "plain.toml", PLAIN);
    let (report, junit) = (dir.path().join("r.json"), dir.path().join("j.xml"));
    let output = kirby(station(
        csms.url(),
        &[
            "run",
            "--scenario",
            &file,
            "--report",
            path_str(&report),
            "--junit",
            path_str(&junit),
        ],
    ))
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let value = json(&report);
    assert_eq!(value["ocpp_version"], "2.0.1");
    assert_eq!(value["status"], "passed");
    assert_eq!(value["station"], "CP-S2");
    let xml = std::fs::read_to_string(&junit).unwrap();
    assert!(xml.contains("classname=\"ocpp2.0.1.plain\""), "{xml}");
    assert!(xml.contains("2.0.1"), "{xml}");
    csms.wait_for_call("Heartbeat", STEP).await.unwrap();
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_resolves_a_built_in_for_2_0_1_and_retargets_the_token() {
    let csms = v201_csms().await;
    let output = kirby(station(
        csms.url(),
        &[
            "run",
            "--scenario",
            "charge-session",
            "--id-tag",
            "MY-TOKEN",
        ],
    ))
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("PASSED"), "{}", stdout(&output));
    let started = csms.wait_for_call("TransactionEvent", STEP).await.unwrap();
    assert_eq!(started["eventType"], "Started", "{started}");
    assert_eq!(started["idToken"]["idToken"], "MY-TOKEN", "{started}");
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_rejects_a_token_longer_than_36_characters_for_2_0_1() {
    let url = unused_url();
    let long = "T".repeat(37);
    let output = kirby(station(
        &url,
        &["run", "--scenario", "charge-session", "--id-tag", &long],
    ))
    .await;
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("idToken is 37"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_setup_error_names_2_0_1_in_the_junit_file() {
    let url = unused_url();
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "plain.toml", PLAIN);
    let junit = dir.path().join("j.xml");
    let output = kirby(station(
        &url,
        &[
            "--no-reconnect",
            "run",
            "--scenario",
            &file,
            "--junit",
            path_str(&junit),
        ],
    ))
    .await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let xml = std::fs::read_to_string(&junit).unwrap();
    assert!(xml.contains("ocpp2.0.1.plain"), "{xml}");
    assert!(xml.contains("<error"), "{xml}");
}

const SESSION: &str = "name = \"session\"\nocpp_version = \"2.0.1\"\ndescription = \"x\"\n\n[[steps]]\naction = \"plug\"\nevse = 1\n\n[[steps]]\naction = \"start_tx\"\nevse = 1\nid_token = \"TAG-1\"\n\n[[steps]]\naction = \"meter\"\nevse = 1\nwh_delta = 1000\n\n[[steps]]\naction = \"stop_tx\"\nevse = 1\n\n[[steps]]\naction = \"unplug\"\nevse = 1\n";

#[tokio::test(flavor = "multi_thread")]
async fn run_waits_for_queued_transaction_events_before_it_ends() {
    let csms = v201_csms().await;
    // Every TransactionEvent answer is slow, so the queue is still full when the last step ends.
    csms.respond("TransactionEvent", |_| {
        std::thread::sleep(Duration::from_millis(150));
        kirby_ocpp_testkit::Reply::Result(serde_json::json!({}))
    });
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "session.toml", SESSION);
    let output = kirby(station(csms.url(), &["run", "--scenario", &file])).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let ended = csms
        .received()
        .into_iter()
        .any(|call| call.action == "TransactionEvent" && call.payload["eventType"] == "Ended");
    assert!(ended, "the run ended before the Ended event was delivered");
    assert!(!stderr(&output).contains("warning"), "{}", stderr(&output));
    csms.shutdown().await;
}

// suite

#[tokio::test(flavor = "multi_thread")]
async fn suite_runs_a_directory_of_2_0_1_files_and_selects_by_tag() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let scenarios = dir.path().join("s");
    std::fs::create_dir(&scenarios).unwrap();
    write(&scenarios, "a-plain.toml", PLAIN);
    write(&scenarios, "b-other.toml", OTHER);
    let (report, junit) = (dir.path().join("r.json"), dir.path().join("j.xml"));
    let output = kirby(station(
        csms.url(),
        &[
            "suite",
            path_str(&scenarios),
            "--report",
            path_str(&report),
            "--junit",
            path_str(&junit),
        ],
    ))
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let value = json(&report);
    assert_eq!(value["ocpp_version"], "2.0.1");
    assert_eq!(value["total"], 2);
    assert_eq!(value["scenarios"][0]["ocpp_version"], "2.0.1");
    let xml = std::fs::read_to_string(&junit).unwrap();
    assert!(xml.contains("classname=\"ocpp2.0.1.plain\""), "{xml}");
    assert!(xml.contains("classname=\"ocpp2.0.1.other\""), "{xml}");

    let tagged = dir.path().join("t.json");
    let output = kirby(station(
        csms.url(),
        &[
            "suite",
            path_str(&scenarios),
            "--tag",
            "conformance",
            "--tag",
            "ocpp2.0.1",
            "--report",
            path_str(&tagged),
        ],
    ))
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let value = json(&tagged);
    assert_eq!(value["total"], 1);
    assert_eq!(value["scenarios"][0]["name"], "plain");
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_suite_with_a_scenario_of_another_version_is_a_usage_error() {
    let url = unused_url();
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a-plain.toml", PLAIN);
    let old = write(dir.path(), "b-old.toml", V16);
    let output = kirby(station(&url, &["suite", path_str(dir.path())])).await;
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let text = stderr(&output);
    assert!(
        text.contains(&old)
            && text.contains("scenario old is for OCPP 1.6; this station speaks ocpp2.0.1"),
        "{text}"
    );

    // The same file with a 1.6 station.
    let v201 = write(dir.path(), "c-201.toml", OTHER);
    let output = kirby(strings(&["--url", &url, "--no-persist", "suite", &v201])).await;
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let text = stderr(&output);
    assert!(
        text.contains(&v201)
            && text.contains("scenario other is for OCPP 2.0.1; this station speaks ocpp1.6"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn suite_runs_the_built_ins_of_2_0_1() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("r.json");
    let output = kirby(station(
        csms.url(),
        &["suite", "--all-builtins", "--report", path_str(&report)],
    ))
    .await;
    let value = json(&report);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{} {}",
        stderr(&output),
        stdout(&output)
    );
    assert_eq!(value["ocpp_version"], "2.0.1");
    assert_eq!(value["total"], 8, "{value}");
    assert_eq!(value["passed"], true);
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_suite_setup_error_names_2_0_1_in_the_reports() {
    let url = unused_url();
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "plain.toml", PLAIN);
    let (report, junit) = (dir.path().join("r.json"), dir.path().join("j.xml"));
    let output = kirby(station(
        &url,
        &[
            "--no-reconnect",
            "suite",
            &file,
            "--report",
            path_str(&report),
            "--junit",
            path_str(&junit),
        ],
    ))
    .await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let value = json(&report);
    assert_eq!(value["ocpp_version"], "2.0.1");
    assert_eq!(value["scenarios"][0]["ocpp_version"], "2.0.1");
    assert_eq!(value["scenarios"][0]["status"], "error");
    let xml = std::fs::read_to_string(&junit).unwrap();
    assert!(xml.contains("ocpp2.0.1.plain"), "{xml}");
}

// scenarios

#[tokio::test(flavor = "multi_thread")]
async fn scenarios_list_defaults_to_1_6_and_follows_the_protocol() {
    let default = kirby(strings(&["scenarios", "list"])).await;
    assert!(default.status.success());
    let text = stdout(&default);
    assert!(text.contains("finish-session"), "{text}");
    assert_eq!(text.lines().count(), 9, "{text}");

    let v201 = kirby(strings(&["--protocol", "ocpp2.0.1", "scenarios", "list"])).await;
    assert!(v201.status.success(), "{}", stderr(&v201));
    let text = stdout(&v201);
    assert_eq!(text.lines().count(), 8, "{text}");
    assert!(text.contains("charge-session") && !text.contains("finish-session"));

    let show = kirby(strings(&[
        "scenarios",
        "show",
        "charge_session",
        "--protocol",
        "ocpp2.0.1",
    ]))
    .await;
    assert!(show.status.success(), "{}", stderr(&show));
    assert!(stdout(&show).contains("ocpp_version = \"2.0.1\""));
}

#[tokio::test(flavor = "multi_thread")]
async fn scenarios_validate_accepts_both_versions_and_names_only_2_0_1() {
    let dir = tempfile::tempdir().unwrap();
    let old = write(dir.path(), "old.toml", V16);
    let new = write(dir.path(), "new.toml", OTHER);
    let output = kirby(strings(&["scenarios", "validate", &old, &new])).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains(&format!("ok {old} (1 steps)")), "{text}");
    assert!(
        text.contains(&format!("ok {new} (1 steps, OCPP 2.0.1)")),
        "{text}"
    );
}

// still refused

#[tokio::test(flavor = "multi_thread")]
async fn from_recording_stays_refused_for_2_0_1() {
    let output = kirby(strings(&[
        "--protocol",
        "ocpp2.0.1",
        "scenarios",
        "from-recording",
        "nope.jsonl",
    ]))
    .await;
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("not available for ocpp2.0.1"));
}
