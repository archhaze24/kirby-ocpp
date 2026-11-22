//! The emulator binary speaking OCPP 2.1: protocol selection, headless run, `run`, `suite`,
//! `scenarios`, `state` and the mode gates. The 2.x station is the 2.0.1 one on the `ocpp2.1`
//! subprotocol, so the checks here are about what differs: the version in tokens, reports and
//! file names, and the limits that grew.
//!
//! The mock CSMS selects `ocpp2.1` and answers with payloads of its own, valid for the 2.1
//! schemas, so the tests do not depend on the mock's built-in 2.1 defaults.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use chrono::Utc;
use kirby_ocpp_testkit::{CsmsOptions, MockCsms, Reply, SubprotocolPolicy};
use serde_json::{Value, json};

const STEP: Duration = Duration::from_secs(15);
const URL: &str = "ws://127.0.0.1:9/ocpp";

const PLAIN: &str = "name = \"plain\"\nocpp_version = \"2.1\"\ndescription = \"x\"\n\n[[steps]]\naction = \"heartbeat\"\n\n[[steps]]\naction = \"expect_call\"\ncall = \"Heartbeat\"\ndirection = \"out\"\ntimeout_ms = 3000\n";
const V201_FILE: &str = "name = \"plain\"\nocpp_version = \"2.0.1\"\ndescription = \"x\"\n\n[[steps]]\naction = \"heartbeat\"\n";
const V21_FILE: &str = "name = \"plain\"\nocpp_version = \"2.1\"\ndescription = \"x\"\n\n[[steps]]\naction = \"heartbeat\"\n";

async fn v21_csms() -> MockCsms {
    let csms = MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Require("ocpp2.1".to_owned()),
        ..CsmsOptions::default()
    })
    .await
    .unwrap();
    csms.respond("BootNotification", |_| {
        Reply::Result(json!({
            "status": "Accepted",
            "currentTime": Utc::now().to_rfc3339(),
            "interval": 1,
        }))
    });
    csms.respond("Heartbeat", |_| {
        Reply::Result(json!({"currentTime": Utc::now().to_rfc3339()}))
    });
    csms.respond("Authorize", |_| {
        Reply::Result(json!({"idTokenInfo": {"status": "Accepted"}}))
    });
    csms.respond("DataTransfer", |_| {
        Reply::Result(json!({"status": "Accepted"}))
    });
    for action in [
        "TransactionEvent",
        "StatusNotification",
        "MeterValues",
        "NotifyEvent",
        "SecurityEventNotification",
    ] {
        csms.respond(action, |_| Reply::Result(json!({})));
    }
    csms
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

/// The binary with `args`, on a blocking thread with a deadline.
async fn run_emulator(args: Vec<String>) -> Output {
    tokio::task::spawn_blocking(move || crate::support::output(command(&args)))
        .await
        .unwrap()
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}

/// Global flags of a 2.1 station that connects to `url`, then `rest`.
fn station(url: &str, rest: &[&str]) -> Vec<String> {
    let mut args = strings(&[
        "--url",
        url,
        "--id",
        "CP-S21",
        "--protocol",
        "ocpp2.1",
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

fn json_file(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// `--print-config` as JSON; the command must succeed.
async fn config(extra: &[&str]) -> Value {
    let mut args = strings(&["--url", URL, "--print-config"]);
    args.extend(extra.iter().map(|s| (*s).to_owned()));
    let output = run_emulator(args).await;
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    serde_json::from_slice(&output.stdout).unwrap()
}

/// A URL that refuses connections.
fn unused_url() -> String {
    kirby_ocpp_testkit::refused_url()
}

#[tokio::test(flavor = "multi_thread")]
async fn protocol_2_1_derives_the_subprotocol() {
    let c = config(&["--protocol", "ocpp2.1"]).await;
    assert_eq!(c["protocol"], "ocpp2.1");
    assert_eq!(c["ws_subprotocol"], "ocpp2.1");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_environment_and_the_file_select_2_1() {
    let output = tokio::task::spawn_blocking(|| {
        let mut command = command(&strings(&["--url", URL, "--print-config"]));
        command.env("OCPP_PROTOCOL", "ocpp2.1");
        crate::support::output(command)
    })
    .await
    .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["protocol"], "ocpp2.1");

    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "c.toml", "protocol = \"ocpp2.1\"\n");
    let value = config(&["--config", &file]).await;
    assert_eq!(value["protocol"], "ocpp2.1");
    assert_eq!(value["ws_subprotocol"], "ocpp2.1");
}

#[tokio::test(flavor = "multi_thread")]
async fn version_lists_2_1() {
    let output = run_emulator(strings(&["version"])).await;
    assert!(
        stdout(&output).contains("protocols: OCPP 1.6J, OCPP 2.0.1, OCPP 2.1"),
        "{}",
        stdout(&output)
    );
    let output = run_emulator(strings(&["version", "--json"])).await;
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["protocols"],
        json!(["OCPP 1.6J", "OCPP 2.0.1", "OCPP 2.1"])
    );
    assert_eq!(
        value["protocol_tokens"],
        json!(["ocpp1.6", "ocpp2.0.1", "ocpp2.1"])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_2_1_headless_fleet_exits_when_both_stations_give_up() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let output = run_emulator(strings(&[
        "--url",
        csms.url(),
        "--protocol",
        "ocpp2.1",
        "--id",
        "CP-{n}",
        "--count",
        "2",
        "--no-persist",
        "--no-reconnect",
        "--no-tui",
    ]))
    .await;
    let errors = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{errors}");
    for id in ["CP-1", "CP-2"] {
        assert!(
            errors.contains(&format!("error: [{id}] reconnect stopped")),
            "{errors}"
        );
    }
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_1_6_only_flags_are_refused_with_2_1() {
    let output = run_emulator(station(URL, &["--print-config", "--rule", "Reset=reject"])).await;
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("--rule")
            && stderr(&output).contains("ocpp1.6")
            && stderr(&output).contains("ocpp2.1"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_station_id_note_names_2_1() {
    let output = run_emulator(strings(&[
        "--url",
        URL,
        "--protocol",
        "ocpp2.1",
        "--id",
        "CP:1",
        "--print-config",
    ]))
    .await;
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("note: station id") && stderr(&output).contains("OCPP 2.1 allows"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn scenarios_list_and_show_follow_the_station_protocol() {
    let output = run_emulator(strings(&["scenarios", "list", "--protocol", "ocpp2.1"])).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let listing = stdout(&output);
    assert_eq!(listing.lines().count(), 8, "{listing}");
    assert!(listing.contains("charge-session"), "{listing}");
    assert!(!listing.contains("finish-session"), "{listing}");

    let output = run_emulator(strings(&[
        "scenarios",
        "show",
        "charge-session",
        "--protocol",
        "ocpp2.1",
    ]))
    .await;
    assert!(output.status.success(), "{}", stderr(&output));
    let shown = stdout(&output);
    assert!(shown.contains("ocpp_version = \"2.1\""), "{shown}");
    assert!(!shown.contains("2.0.1"), "{shown}");

    let output = run_emulator(strings(&[
        "scenarios",
        "show",
        "finish-session",
        "--protocol",
        "ocpp2.1",
    ]))
    .await;
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("--protocol ocpp2.1"),
        "{}",
        stderr(&output)
    );

    // The 2.0.1 listing is unchanged.
    let output = run_emulator(strings(&[
        "scenarios",
        "show",
        "charge-session",
        "--protocol",
        "ocpp2.0.1",
    ]))
    .await;
    assert!(stdout(&output).contains("ocpp_version = \"2.0.1\""));
}

#[tokio::test(flavor = "multi_thread")]
async fn scenarios_validate_rejects_a_token_type_beyond_20_characters() {
    let dir = tempfile::tempdir().unwrap();
    let file = |name: &str, kind: &str| {
        write(
            dir.path(),
            name,
            &format!(
                "name = \"t\"\nocpp_version = \"2.1\"\n\n[[steps]]\naction = \"authorize\"\nid_token = \"T\"\ntype = \"{kind}\"\n"
            ),
        )
    };
    let ok = file("ok.toml", &"k".repeat(20));
    let output = run_emulator(strings(&["scenarios", "validate", &ok])).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let bad = file("bad.toml", &"k".repeat(21));
    let output = run_emulator(strings(&["scenarios", "validate", &bad])).await;
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("`type` must be at most 20 characters"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn scenarios_validate_accepts_a_2_1_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "v21.toml", V21_FILE);
    let output = run_emulator(strings(&["scenarios", "validate", &file])).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let text = format!("{}{}", stdout(&output), stderr(&output));
    assert!(text.contains("2.1"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn state_paths_are_the_2_x_files() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_str().unwrap();
    let args = |protocol: &'static str| {
        strings(&[
            "state",
            "path",
            "--state-dir",
            state,
            "--id",
            "CP-9",
            "--protocol",
            protocol,
            "--json",
        ])
    };
    let newer: Value = serde_json::from_slice(&run_emulator(args("ocpp2.1")).await.stdout).unwrap();
    let older: Value =
        serde_json::from_slice(&run_emulator(args("ocpp2.0.1")).await.stdout).unwrap();
    assert_eq!(newer["protocol"], "ocpp2.1");
    assert_eq!(newer["file"], older["file"]);
    assert_eq!(newer["secrets_file"], older["secrets_file"]);
    assert!(
        newer["file"]
            .as_str()
            .unwrap()
            .ends_with("CP-9.station2.json")
    );
    assert!(
        newer["secrets_file"]
            .as_str()
            .unwrap()
            .ends_with("CP-9.station2.secrets.json")
    );

    let output = run_emulator(strings(&[
        "state",
        "show",
        "--state-dir",
        state,
        "--id",
        "CP-9",
        "--protocol",
        "ocpp2.1",
        "--json",
    ]))
    .await;
    // A file that does not exist is exit code 1, as for the other versions.
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "missing");
    assert_eq!(value["protocol"], "ocpp2.1");
}

#[tokio::test(flavor = "multi_thread")]
async fn check_boots_with_the_2_1_subprotocol() {
    let csms = v21_csms().await;
    let output = run_emulator(strings(&[
        "check",
        "--url",
        csms.url(),
        "--id",
        "CP-V21",
        "--protocol",
        "ocpp2.1",
        "--json",
    ]))
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ocpp_version"], "2.1");
    assert_eq!(report["status"], "passed");
    let boot = csms.wait_for_call("BootNotification", STEP).await.unwrap();
    assert!(boot["chargingStation"].is_object(), "{boot}");
    let log = csms.connection_log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0].subprotocol.as_deref(), Some("ocpp2.1"));
    csms.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn headless_2_1_boots_sends_heartbeats_and_stops_on_sigterm() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let args = strings(&[
        "--url",
        csms.url(),
        "--id",
        "CP-V21",
        "--protocol",
        "ocpp2.1",
        "--no-persist",
        "--no-tui",
        "--json",
        "--heartbeat",
        "1",
        "--state-dir",
        dir.path().to_str().unwrap(),
    ]);
    let child = crate::support::ChildGuard::spawn(
        command(&args).stdout(Stdio::piped()).stderr(Stdio::piped()),
    );
    let pid = child.id().to_string();
    let boot = csms.wait_for_call("BootNotification", STEP).await.unwrap();
    assert!(boot["chargingStation"].is_object(), "{boot}");
    assert_eq!(boot["reason"], "PowerUp");
    csms.wait_for_call("Heartbeat", STEP).await.unwrap();
    let info = csms.wait_for_connection(STEP).await.unwrap();
    assert_eq!(info.subprotocol.as_deref(), Some("ocpp2.1"));

    let status = Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    assert!(status.success());
    let output = crate::support::finish(child).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    csms.wait_for_disconnect(STEP).await.unwrap();
    let lines: Vec<Value> = stdout(&output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        lines
            .iter()
            .any(|l| l["type"] == "frame" && l["action"] == "BootNotification")
    );
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_2_1_station_against_a_2_0_1_only_csms_gives_up_with_the_reason() {
    let csms = MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Require("ocpp2.0.1".to_owned()),
        ..CsmsOptions::default()
    })
    .await
    .unwrap();
    let output = run_emulator(station(csms.url(), &["--no-reconnect"])).await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("reconnect stopped"),
        "{}",
        stderr(&output)
    );
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_passes_a_2_1_file_and_the_reports_carry_2_1() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "plain.toml", PLAIN);
    let (report, junit) = (dir.path().join("r.json"), dir.path().join("j.xml"));
    let output = run_emulator(station(
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
    let value = json_file(&report);
    assert_eq!(value["ocpp_version"], "2.1");
    assert_eq!(value["status"], "passed");
    assert_eq!(value["station"], "CP-S21");
    let xml = std::fs::read_to_string(&junit).unwrap();
    assert!(xml.contains("classname=\"ocpp2.1.plain\""), "{xml}");
    assert!(!xml.contains("2.0.1"), "{xml}");
    csms.wait_for_call("Heartbeat", STEP).await.unwrap();
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_resolves_a_built_in_for_2_1_and_accepts_a_long_token() {
    let csms = v21_csms().await;
    let long = "T".repeat(100);
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("r.json");
    let output = run_emulator(station(
        csms.url(),
        &[
            "run",
            "--scenario",
            "charge-session",
            "--id-tag",
            &long,
            "--report",
            path_str(&report),
        ],
    ))
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let value = json_file(&report);
    assert_eq!(value["ocpp_version"], "2.1");
    assert_eq!(value["scenario"], "charge-session");
    let started = csms.wait_for_call("TransactionEvent", STEP).await.unwrap();
    assert_eq!(started["idToken"]["idToken"], long.as_str(), "{started}");
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_rejects_a_token_beyond_255_characters_for_2_1() {
    let url = unused_url();
    let long = "T".repeat(256);
    let output = run_emulator(station(
        &url,
        &["run", "--scenario", "charge-session", "--id-tag", &long],
    ))
    .await;
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("idToken is 256") && stderr(&output).contains("1 to 255"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scenario_for_another_version_is_a_symmetric_usage_error() {
    let url = unused_url();
    let dir = tempfile::tempdir().unwrap();
    let older_file = write(dir.path(), "v201.toml", V201_FILE);
    let newer_file = write(dir.path(), "v21.toml", V21_FILE);
    let output = run_emulator(station(&url, &["run", "--scenario", &older_file])).await;
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("scenario plain is for OCPP 2.0.1; this station speaks ocpp2.1"),
        "{}",
        stderr(&output)
    );
    let output = run_emulator(strings(&[
        "--url",
        &url,
        "--protocol",
        "ocpp2.0.1",
        "--no-persist",
        "run",
        "--scenario",
        &newer_file,
    ]))
    .await;
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("scenario plain is for OCPP 2.1; this station speaks ocpp2.0.1"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn suite_runs_the_built_ins_as_2_1() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let (report, junit) = (dir.path().join("r.json"), dir.path().join("j.xml"));
    let output = run_emulator(station(
        csms.url(),
        &[
            "suite",
            "--all-builtins",
            "--report",
            path_str(&report),
            "--junit",
            path_str(&junit),
        ],
    ))
    .await;
    let value = json_file(&report);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{} {}",
        stderr(&output),
        stdout(&output)
    );
    assert_eq!(value["ocpp_version"], "2.1");
    assert_eq!(value["total"], 8, "{value}");
    assert_eq!(value["passed"], true);
    assert_eq!(value["scenarios"][0]["ocpp_version"], "2.1");
    let xml = std::fs::read_to_string(&junit).unwrap();
    assert!(
        xml.contains("classname=\"ocpp2.1.charge-session\""),
        "{xml}"
    );
    csms.shutdown().await;
}
