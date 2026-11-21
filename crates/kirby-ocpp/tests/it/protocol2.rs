//! The emulator binary speaking OCPP 2.0.1: protocol selection, headless run and mode gates.
//!
//! Environment variables cannot be set in-process, so the real binary runs with a controlled
//! environment. Tests that need a connected 2.0.1 station are marked `needs a 2.0.1 station`.

use std::path::Path;
use std::process::{Command, Output, Stdio};
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use kirby_ocpp_testkit::SubprotocolPolicy;
use kirby_ocpp_testkit::{CsmsOptions, MockCsms};
use serde_json::{Value, json};

#[cfg(unix)]
const STEP: Duration = Duration::from_secs(10);
const URL: &str = "ws://127.0.0.1:9/ocpp";

fn command(args: &[&str], env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kirby-ocpp"));
    for (key, _) in std::env::vars() {
        if key.starts_with("OCPP_") || key.starts_with("KIRBY_OCPP_") {
            command.env_remove(key);
        }
    }
    command
        .args(args)
        .envs(env.iter().copied())
        .stdin(Stdio::null());
    command
}

fn run(args: &[&str], env: &[(&str, &str)]) -> Output {
    crate::support::output(command(args, env))
}

async fn run_async(args: Vec<String>) -> Output {
    tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        run(&args, &[])
    })
    .await
    .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// `--print-config` as JSON; the command must succeed.
fn config(args: &[&str], env: &[(&str, &str)]) -> Value {
    let mut all = vec!["--url", URL, "--print-config"];
    all.extend_from_slice(args);
    let output = run(&all, env);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    serde_json::from_slice(&output.stdout).unwrap()
}

/// A usage failure: exit 2, nothing on stdout; returns stderr.
fn rejected(args: &[&str], env: &[(&str, &str)]) -> String {
    let output = run(args, env);
    assert_eq!(
        output.status.code(),
        Some(2),
        "{args:?}: stdout {} stderr {}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stdout(&output).is_empty(), "stdout: {}", stdout(&output));
    stderr(&output)
}

fn write(dir: &Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

#[cfg(unix)]
async fn v201_csms() -> MockCsms {
    MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Require("ocpp2.0.1".to_owned()),
        ..CsmsOptions::default()
    })
    .await
    .unwrap()
}

#[test]
fn the_default_protocol_is_1_6() {
    let c = config(&[], &[]);
    assert_eq!(c["protocol"], "ocpp1.6");
    assert_eq!(c["ws_subprotocol"], "ocpp1.6");
}

#[test]
fn protocol_2_0_1_derives_the_subprotocol() {
    let c = config(&["--protocol", "ocpp2.0.1"], &[]);
    assert_eq!(c["protocol"], "ocpp2.0.1");
    assert_eq!(c["ws_subprotocol"], "ocpp2.0.1");
}

#[test]
fn protocol_2_1_derives_the_subprotocol() {
    let c = config(&["--protocol", "ocpp2.1"], &[]);
    assert_eq!(c["protocol"], "ocpp2.1");
    assert_eq!(c["ws_subprotocol"], "ocpp2.1");
}

#[test]
fn an_unknown_protocol_is_a_usage_error() {
    let text = rejected(&["--url", URL, "--protocol", "ocpp3"], &[]);
    assert!(text.contains("ocpp2.0.1"), "{text}");
}

#[test]
fn an_explicit_subprotocol_wins_and_a_different_one_prints_a_note() {
    let output = run(
        &[
            "--url",
            URL,
            "--protocol",
            "ocpp2.0.1",
            "--ws-subprotocol",
            "ocpp1.6",
            "--print-config",
        ],
        &[],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let c: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(c["protocol"], "ocpp2.0.1");
    assert_eq!(c["ws_subprotocol"], "ocpp1.6");
    assert!(
        stderr(&output).contains("note: offering ocpp1.6 while behaving as ocpp2.0.1"),
        "{}",
        stderr(&output)
    );
    // The environment variable counts as explicit too.
    let output = run(
        &["--url", URL, "--protocol", "ocpp2.0.1", "--print-config"],
        &[("OCPP_WS_SUBPROTOCOL", "custom")],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("note: offering custom while behaving as ocpp2.0.1"),
        "{}",
        stderr(&output)
    );
    // The same token is not a difference.
    let output = run(
        &[
            "--url",
            URL,
            "--protocol",
            "ocpp2.0.1",
            "--ws-subprotocol",
            "ocpp2.0.1",
            "--print-config",
        ],
        &[],
    );
    assert!(output.status.success());
    assert!(!stderr(&output).contains("note:"), "{}", stderr(&output));
}

#[test]
fn environment_beats_the_file_and_the_flag_beats_both() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "c.toml", "protocol = \"ocpp2.0.1\"\n");
    let by = |args: &[&str], env: &[(&str, &str)]| -> String {
        let mut all = vec!["--config", file.as_str()];
        all.extend_from_slice(args);
        config(&all, env)["protocol"].as_str().unwrap().to_owned()
    };
    assert_eq!(by(&[], &[]), "ocpp2.0.1", "the file");
    assert_eq!(by(&[], &[("OCPP_PROTOCOL", "ocpp1.6")]), "ocpp1.6", "env");
    assert_eq!(
        by(
            &["--protocol", "ocpp2.0.1"],
            &[("OCPP_PROTOCOL", "ocpp1.6")]
        ),
        "ocpp2.0.1",
        "flag"
    );
    assert_eq!(
        by(&["--protocol", "ocpp1.6"], &[]),
        "ocpp1.6",
        "flag over file"
    );
    assert_eq!(
        config(&["--config", &file], &[])["ws_subprotocol"],
        "ocpp2.0.1"
    );
}

#[test]
fn the_environment_selects_the_protocol() {
    let c = config(&[], &[("OCPP_PROTOCOL", "ocpp2.0.1")]);
    assert_eq!(c["protocol"], "ocpp2.0.1");
}

#[test]
fn the_file_accepts_the_protocol_tokens_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let ok = write(dir.path(), "ok.toml", "protocol = \"ocpp1.6\"\n");
    assert_eq!(config(&["--config", &ok], &[])["protocol"], "ocpp1.6");
    for bad in ["ocpp3", "2.0.1"] {
        let file = write(dir.path(), "bad.toml", &format!("protocol = \"{bad}\"\n"));
        let text = rejected(&["--url", URL, "--config", &file], &[]);
        assert!(text.contains("key `protocol`"), "{text}");
    }
    // A profile cannot carry the protocol, and the version tables stay empty.
    let profile = write(
        dir.path(),
        "profile.toml",
        "[profile.x]\nprotocol = \"ocpp2.0.1\"\n",
    );
    let text = rejected(&["--url", URL, "--config", &profile, "--profile", "x"], &[]);
    assert!(text.contains("only valid at the top level"), "{text}");
    let tables = write(
        dir.path(),
        "tables.toml",
        "[station.v201]\nconnectors = 2\n",
    );
    let text = rejected(&["--url", URL, "--config", &tables], &[]);
    assert!(text.contains("station.v201"), "{text}");
    let empty = write(dir.path(), "empty.toml", "[station.v201]\n[station.v16]\n");
    assert_eq!(config(&["--config", &empty], &[])["protocol"], "ocpp1.6");
}

#[test]
fn print_config_toml_names_the_protocol_and_round_trips() {
    let output = run(
        &[
            "--url",
            URL,
            "--protocol",
            "ocpp2.0.1",
            "--print-config=toml",
        ],
        &[],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("protocol = \"ocpp2.0.1\"\n"), "{text}");
    assert!(text.contains("ws_subprotocol = \"ocpp2.0.1\""), "{text}");
    for only_1_6 in [
        "rule",
        "authorization_key_encoding",
        "security_profile_fallback",
        "additional_root_certificate_check",
    ] {
        assert!(!text.contains(only_1_6), "{only_1_6} in {text}");
    }
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "round.toml", &text);
    let again = run(&["--config", &file, "--print-config=toml"], &[]);
    assert!(again.status.success(), "{}", stderr(&again));
    assert_eq!(stdout(&again), text, "the file reproduces itself");
    let json = run(&["--config", &file, "--print-config"], &[]);
    let c: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(c["protocol"], "ocpp2.0.1");
}

#[test]
fn print_config_toml_of_1_6_still_has_the_1_6_settings() {
    let output = run(&["--url", URL, "--print-config=toml"], &[]);
    let text = stdout(&output);
    assert!(text.contains("protocol = \"ocpp1.6\"\n"), "{text}");
    for key in [
        "rule = ",
        "authorization_key_encoding = ",
        "security_profile_fallback = ",
        "additional_root_certificate_check = ",
    ] {
        assert!(text.contains(key), "{key} in {text}");
    }
}

#[test]
fn flags_for_1_6_only_are_refused_with_2_0_1() {
    type Case<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)], &'a str);
    let cases: [Case; 6] = [
        (&["--rule", "Reset=reject"], &[], "--rule"),
        (
            &["--authorization-key-encoding", "hex"],
            &[],
            "--authorization-key-encoding",
        ),
        (
            &["--security-profile-fallback", "false"],
            &[],
            "--security-profile-fallback",
        ),
        (
            &["--additional-root-certificate-check"],
            &[],
            "--additional-root-certificate-check",
        ),
        (&[], &[("KIRBY_OCPP_RULES", "Reset=reject")], "--rule"),
        (
            &[],
            &[("OCPP_SECURITY_PROFILE_FALLBACK", "true")],
            "--security-profile-fallback",
        ),
    ];
    for (args, env, flag) in cases {
        let mut all = vec!["--url", URL, "--protocol", "ocpp2.0.1", "--print-config"];
        all.extend_from_slice(args);
        let text = rejected(&all, env);
        assert!(text.contains(flag), "{text}");
        assert!(text.contains("ocpp1.6"), "{text}");
        // The same invocation works with 1.6.
        let mut all = vec!["--url", URL, "--print-config"];
        all.extend_from_slice(args);
        let output = run(&all, env);
        assert!(output.status.success(), "{all:?}: {}", stderr(&output));
    }
    // Several flags are named together.
    let text = rejected(
        &[
            "--url",
            URL,
            "--protocol",
            "ocpp2.0.1",
            "--rule",
            "Reset=reject",
            "--authorization-key-encoding",
            "literal",
        ],
        &[],
    );
    assert!(
        text.contains("--rule") && text.contains("--authorization-key-encoding"),
        "{text}"
    );
}

#[test]
fn a_configuration_file_cannot_carry_1_6_settings_into_2_0_1() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "c.toml",
        "protocol = \"ocpp2.0.1\"\n[tls]\nsecurity_profile_fallback = false\n",
    );
    let text = rejected(&["--url", URL, "--config", &file], &[]);
    assert!(text.contains("--security-profile-fallback"), "{text}");
    // Overriding the protocol back to 1.6 makes the same file valid.
    let c = config(&["--config", &file, "--protocol", "ocpp1.6"], &[]);
    assert_eq!(c["security_profile_fallback"], false);
}

#[test]
fn defaults_of_the_1_6_only_flags_are_not_refusals() {
    let c = config(&["--protocol", "ocpp2.0.1"], &[]);
    assert_eq!(c["protocol"], "ocpp2.0.1");
}

#[test]
fn limits_follow_the_protocol() {
    let vendor = "v".repeat(50);
    let c = config(&["--protocol", "ocpp2.0.1", "--vendor", &vendor], &[]);
    assert_eq!(c["vendor"], vendor.as_str());
    let text = rejected(
        &[
            "--url",
            URL,
            "--protocol",
            "ocpp2.0.1",
            "--vendor",
            &"v".repeat(51),
        ],
        &[],
    );
    assert!(
        text.contains("--vendor") && text.contains("ocpp2.0.1"),
        "{text}"
    );
    // 1.6 keeps 20.
    let text = rejected(&["--url", URL, "--vendor", &"v".repeat(21)], &[]);
    assert!(text.contains("--vendor") && text.contains("20"), "{text}");
    // Model, serial and firmware keep their limits; the text names the protocol.
    let text = rejected(
        &[
            "--url",
            URL,
            "--protocol",
            "ocpp2.0.1",
            "--model",
            &"m".repeat(21),
        ],
        &[],
    );
    assert!(
        text.contains("--model") && text.contains("ocpp2.0.1"),
        "{text}"
    );
    let c = config(&["--protocol", "ocpp2.0.1", "--connectors", "64"], &[]);
    assert_eq!(c["connector_count"], 64);
    let text = rejected(
        &[
            "--url",
            URL,
            "--protocol",
            "ocpp2.0.1",
            "--connectors",
            "65",
        ],
        &[],
    );
    assert!(
        text.contains("--connectors") && text.contains("64"),
        "{text}"
    );
    // 1.6 has no such cap.
    let c = config(&["--connectors", "100"], &[]);
    assert_eq!(c["connector_count"], 100);
}

#[test]
fn an_odd_station_id_prints_a_note_for_2_0_1_only() {
    let long = "A".repeat(49);
    for id in [long.as_str(), "CP:1", "CP 1", "CPé"] {
        let output = run(
            &[
                "--url",
                URL,
                "--protocol",
                "ocpp2.0.1",
                "--id",
                id,
                "--print-config",
            ],
            &[],
        );
        assert!(output.status.success(), "{id}: {}", stderr(&output));
        assert!(
            stderr(&output).contains("note: station id"),
            "{id}: {}",
            stderr(&output)
        );
        let output = run(&["--url", URL, "--id", id, "--print-config"], &[]);
        assert!(
            !stderr(&output).contains("note: station id"),
            "{id} with 1.6: {}",
            stderr(&output)
        );
    }
    let output = run(
        &[
            "--url",
            URL,
            "--protocol",
            "ocpp2.0.1",
            "--id",
            "CP-001_a.b",
            "--print-config",
        ],
        &[],
    );
    assert!(output.status.success());
    assert!(!stderr(&output).contains("note:"), "{}", stderr(&output));
}

#[test]
fn version_lists_the_protocols() {
    let output = run(&["version"], &[]);
    assert!(
        stdout(&output).contains("protocols: OCPP 1.6J, OCPP 2.0.1"),
        "{}",
        stdout(&output)
    );
    let output = run(&["version", "--json"], &[]);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    // The list grows with the versions; earlier entries keep their place.
    assert_eq!(value["protocols"][0], "OCPP 1.6J");
    assert_eq!(value["protocols"][1], "OCPP 2.0.1");
    assert_eq!(value["protocol_tokens"][0], "ocpp1.6");
    assert_eq!(value["protocol_tokens"][1], "ocpp2.0.1");
    // The offline commands do not mind the protocol of the environment.
    let output = run(&["version"], &[("OCPP_PROTOCOL", "ocpp2.1")]);
    assert!(output.status.success());
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_exits_2_with_2_0_1_even_in_fleet_mode() {
    let dir = tempfile::tempdir().unwrap();
    let recording = write(
        dir.path(),
        "r.jsonl",
        &json!({"type": "header", "schema": 1, "ocpp_version": "1.6", "tool_version": "1", "role": "emulator", "station": "X", "started_at": "2026-01-01T00:00:00Z"}).to_string(),
    );
    for extra in [&[][..], &["--id", "CP-{n}", "--count", "2"][..]] {
        let mut args: Vec<String> = ["--url", URL, "--protocol", "ocpp2.0.1"]
            .map(String::from)
            .into();
        args.extend(extra.iter().map(|s| (*s).to_owned()));
        args.extend(["replay".to_owned(), recording.clone()]);
        let output = run_async(args.clone()).await;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains("`replay` is not available for ocpp2.0.1 yet"),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn a_fleet_suite_exits_2_with_2_0_1() {
    let text = rejected(
        &[
            "--url",
            URL,
            "--protocol",
            "ocpp2.0.1",
            "--id",
            "CP-{n}",
            "--count",
            "2",
            "suite",
            "--all-builtins",
        ],
        &[],
    );
    assert!(
        text.contains("`suite`") && text.contains("fleet mode"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fleet_print_config_still_works_with_2_0_1() {
    let c = config(
        &["--protocol", "ocpp2.0.1", "--id", "CP-{n}", "--count", "2"],
        &[],
    );
    assert_eq!(c["fleet"]["count"], 2);
    assert_eq!(c["config"]["protocol"], "ocpp2.0.1");
}

#[test]
fn converting_a_2_0_1_recording_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let recording = write(
        dir.path(),
        "r201.jsonl",
        &json!({"type": "header", "schema": 1, "ocpp_version": "2.0.1", "tool_version": "1", "role": "emulator", "station": "X", "started_at": "2026-01-01T00:00:00Z"}).to_string(),
    );
    let out = dir.path().join("out.toml");
    let text = rejected(
        &[
            "scenarios",
            "from-recording",
            &recording,
            "-o",
            out.to_str().unwrap(),
        ],
        &[],
    );
    assert!(text.contains("1.6 recordings only"), "{text}");
    assert!(text.contains("`2.0.1`"), "{text}");
    assert!(!out.exists(), "nothing is written");
}

const SCENARIO_V16: &str =
    "name = \"plain\"\ndescription = \"x\"\n\n[[steps]]\naction = \"heartbeat\"\n";
const SCENARIO_V201: &str = "name = \"plain\"\nocpp_version = \"2.0.1\"\ndescription = \"x\"\n\n[[steps]]\naction = \"heartbeat\"\n";

#[test]
fn run_refuses_a_scenario_for_another_version() {
    let dir = tempfile::tempdir().unwrap();
    let v16 = write(dir.path(), "v16.toml", SCENARIO_V16);
    let v201 = write(dir.path(), "v201.toml", SCENARIO_V201);
    let text = rejected(
        &[
            "--url",
            URL,
            "--protocol",
            "ocpp2.0.1",
            "run",
            "--scenario",
            &v16,
        ],
        &[],
    );
    assert!(
        text.contains("scenario plain is for OCPP 1.6; this station speaks ocpp2.0.1"),
        "{text}"
    );
    let text = rejected(&["--url", URL, "run", "--scenario", &v201], &[]);
    assert!(
        text.contains("scenario plain is for OCPP 2.0.1; this station speaks ocpp1.6"),
        "{text}"
    );
}

#[test]
fn a_2_x_fleet_refuses_a_1_6_scenario() {
    let dir = tempfile::tempdir().unwrap();
    let scenario = write(dir.path(), "v16.toml", SCENARIO_V16);
    let text = rejected(
        &[
            "--url",
            URL,
            "--protocol",
            "ocpp2.0.1",
            "--id",
            "CP-{n}",
            "--count",
            "2",
            "run",
            "--scenario",
            &scenario,
        ],
        &[],
    );
    assert!(
        text.contains("scenario plain is for OCPP 1.6; this station speaks ocpp2.0.1"),
        "{text}"
    );
}

#[test]
fn a_built_in_name_resolves_for_the_station_protocol() {
    // `finish-session` exists for 1.6 only: with 2.0.1 it is an unknown name.
    let text = rejected(
        &[
            "--url",
            URL,
            "--protocol",
            "ocpp2.0.1",
            "run",
            "--scenario",
            "finish-session",
        ],
        &[],
    );
    assert!(
        text.contains("no built-in scenario `finish-session`")
            && text.contains("--protocol ocpp2.0.1"),
        "{text}"
    );
}

#[test]
fn state_takes_the_protocol_without_complaint_about_1_6_flags() {
    // `state` ignores the connection flags; the protocol is for S6's file kinds.
    let dir = tempfile::tempdir().unwrap();
    let output = run(
        &[
            "--protocol",
            "ocpp2.0.1",
            "--state-dir",
            dir.path().to_str().unwrap(),
            "state",
            "path",
        ],
        &[],
    );
    assert!(output.status.success(), "{}", stderr(&output));
}

// The checks below connect a real 2.0.1 station (needs a 2.0.1 station).

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn headless_2_0_1_boots_sends_heartbeats_and_stops_on_sigterm() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let args = [
        "--url",
        csms.url(),
        "--id",
        "CP-V201",
        "--protocol",
        "ocpp2.0.1",
        "--no-persist",
        "--no-tui",
        "--json",
        "--heartbeat",
        "1",
        "--state-dir",
        dir.path().to_str().unwrap(),
    ];
    let child = crate::support::ChildGuard::spawn(
        command(&args, &[])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
    let pid = child.id().to_string();
    let boot = csms.wait_for_call("BootNotification", STEP).await.unwrap();
    assert!(boot["chargingStation"].is_object(), "{boot}");
    assert_eq!(boot["reason"], "PowerUp");
    csms.wait_for_call("Heartbeat", STEP).await.unwrap();
    let info = csms.wait_for_connection(STEP).await.unwrap();
    assert_eq!(info.subprotocol.as_deref(), Some("ocpp2.0.1"));

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
async fn a_2_0_1_station_against_a_1_6_only_csms_gives_up_with_the_reason() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let output = run_async(
        [
            "--url",
            csms.url(),
            "--protocol",
            "ocpp2.0.1",
            "--no-persist",
            "--no-tui",
            "--no-reconnect",
        ]
        .map(String::from)
        .into(),
    )
    .await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("reconnect stopped"),
        "{}",
        stderr(&output)
    );
    csms.shutdown().await;
}
