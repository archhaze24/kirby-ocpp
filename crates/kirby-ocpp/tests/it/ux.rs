//! Process tests of the emulator's user-facing behavior: dead connections, error hints,
//! output levels, ephemeral `run`, colour, `version`, `completions`, `scenarios validate`.

use std::process::{Command, Output, Stdio};

use kirby_ocpp_testkit::{CsmsOptions, MockCsms, TlsOpts};
use tempfile::TempDir;

fn command(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kirby-ocpp"));
    for (key, _) in std::env::vars() {
        if key.starts_with("OCPP_") || key.starts_with("KIRBY_OCPP_") || key == "NO_COLOR" {
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

fn args(url: &str, extra: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = ["--url", url, "--id", "CP-UX", "--no-persist", "--no-tui"]
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

/// A URL nothing listens on.
fn dead_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("ws://127.0.0.1:{port}/ocpp")
}

const REFUSED_HINT: &str = "check the host and port and that the CSMS is running";

#[tokio::test(flavor = "multi_thread")]
async fn headless_without_reconnect_exits_1_with_the_last_error() {
    let output = run(args(&dead_url(), &["--no-reconnect"])).await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(err.contains("reconnect stopped"), "{err}");
    assert!(err.contains("automatic reconnect is disabled"), "{err}");
    assert!(err.contains("cannot reach the central system"), "{err}");
    assert!(err.contains(REFUSED_HINT), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn headless_exits_1_when_the_attempts_are_used_up() {
    let output = run(args(
        &dead_url(),
        &[
            "--reconnect-max-attempts",
            "2",
            "--reconnect-initial-ms",
            "10",
            "--reconnect-max-ms",
            "20",
        ],
    ))
    .await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("after 2 failed attempt(s)"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn run_and_suite_fail_fast_on_a_dead_connection() {
    let url = dead_url();
    let mut run_args = args(
        &url,
        &["--no-reconnect", "run", "--scenario", "charge-session"],
    );
    run_args.extend(["--timeout".to_owned(), "30".to_owned()]);
    let output = run(run_args).await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(
        err.contains("reconnect stopped") && err.contains(REFUSED_HINT),
        "{err}"
    );

    let output = run(args(
        &url,
        &[
            "--no-reconnect",
            "suite",
            "charge-session",
            "heartbeat-and-status",
        ],
    ))
    .await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let out = stdout(&output);
    assert_eq!(out.matches("FAIL ").count(), 2, "{out}");
    assert!(out.contains("reconnect stopped"), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn fleet_headless_exits_1_when_every_station_gave_up() {
    let output = run(args(
        &dead_url(),
        &["--no-reconnect", "--count", "2", "--id-template", "CP-{n}"],
    ))
    .await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(err.contains("[CP-1]") && err.contains("[CP-2]"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_against_a_plain_server_hints_at_ws() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let url = csms.url().replacen("ws://", "wss://", 1);
    let output = run(args(&url, &["--no-reconnect"])).await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(err.contains("the server may speak plain ws://"), "{err}");
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_ca_hints_at_tls_ca() {
    let csms = MockCsms::start(CsmsOptions {
        tls: Some(TlsOpts::default()),
        ..CsmsOptions::default()
    })
    .await
    .unwrap();
    let output = run(args(csms.url(), &["--no-reconnect"])).await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(err.contains("use --tls-ca or --tls-skip-verify"), "{err}");
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn subprotocol_refusal_shows_the_offered_subprotocol() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let output = run(args(
        csms.url(),
        &["--no-reconnect", "--ws-subprotocol", "ocpp9.9"],
    ))
    .await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(
        err.contains("offered subprotocol `ocpp9.9`") && err.contains("--ws-subprotocol"),
        "{err}"
    );
    csms.shutdown().await;
}

#[test]
fn missing_url_shows_an_example_and_a_local_peer() {
    let output = command(&["--no-tui"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let err = stderr(&output);
    assert!(
        err.contains("kirby-ocpp --url ws://localhost:9000/ocpp"),
        "{err}"
    );
    assert!(err.contains("kirby-csms --listen 127.0.0.1:9000"), "{err}");
}

#[test]
fn unknown_scenario_names_the_closest_built_in() {
    let output = command(&[
        "--url",
        "ws://127.0.0.1:1/x",
        "run",
        "--scenario",
        "charge-sesion",
    ])
    .output()
    .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let err = stderr(&output);
    assert!(
        err.contains("no built-in scenario `charge-sesion` and no such file"),
        "{err}"
    );
    assert!(err.contains("kirby-ocpp scenarios list"), "{err}");
    assert!(err.contains("did you mean `charge-session`?"), "{err}");

    let output = command(&[
        "--url",
        "ws://127.0.0.1:1/x",
        "run",
        "--scenario",
        "zzzzzzzz",
    ])
    .output()
    .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        !stderr(&output).contains("did you mean"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn run_prints_the_report_only_unless_verbose() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let quiet_default = run(args(
        csms.url(),
        &["run", "--scenario", "heartbeat-and-status"],
    ))
    .await;
    assert_eq!(
        quiet_default.status.code(),
        Some(0),
        "{}",
        stderr(&quiet_default)
    );
    let text = stdout(&quiet_default);
    assert!(
        text.contains("PASSED") && text.contains("scenario "),
        "{text}"
    );
    assert!(
        !text.contains(" -> ") && !text.contains(" <- ") && !text.contains("[Info]"),
        "{text}"
    );

    let verbose = run(args(
        csms.url(),
        &["-v", "run", "--scenario", "heartbeat-and-status"],
    ))
    .await;
    assert_eq!(verbose.status.code(), Some(0), "{}", stderr(&verbose));
    let text = stdout(&verbose);
    assert!(
        text.contains(" -> ") && text.contains(" <- ") && text.contains("PASSED"),
        "{text}"
    );

    let quiet = run(args(
        csms.url(),
        &["run", "--scenario", "heartbeat-and-status", "-q"],
    ))
    .await;
    assert_eq!(quiet.status.code(), Some(0), "{}", stderr(&quiet));
    let text = stdout(&quiet);
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.starts_with("PASSED in "), "{text}");

    let both = run(args(
        csms.url(),
        &["-q", "-v", "run", "--scenario", "heartbeat-and-status"],
    ))
    .await;
    assert_eq!(both.status.code(), Some(2), "{}", stderr(&both));
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn suite_levels() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let default = run(args(csms.url(), &["suite", "heartbeat-and-status"])).await;
    let text = stdout(&default);
    assert!(
        text.contains("PASS heartbeat-and-status") && text.contains("suite: 1/1"),
        "{text}"
    );
    assert!(!text.contains(" -> "), "{text}");

    let verbose = run(args(
        csms.url(),
        &["suite", "heartbeat-and-status", "--verbose"],
    ))
    .await;
    assert!(stdout(&verbose).contains(" -> "), "{}", stdout(&verbose));

    let quiet = run(args(
        csms.url(),
        &["suite", "heartbeat-and-status", "--quiet"],
    ))
    .await;
    let text = stdout(&quiet);
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.starts_with("suite: 1/1"), "{text}");
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn json_run_keeps_events_and_quiet_drops_them() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let output = run(args(
        csms.url(),
        &["--json", "run", "--scenario", "heartbeat-and-status"],
    ))
    .await;
    let text = stdout(&output);
    assert!(
        text.contains("\"type\":\"frame\"") && text.contains("\"type\":\"report\""),
        "{text}"
    );
    let output = run(args(
        csms.url(),
        &["--json", "-q", "run", "--scenario", "heartbeat-and-status"],
    ))
    .await;
    let text = stdout(&output);
    assert!(
        !text.contains("\"type\":\"frame\"") && text.contains("\"type\":\"report\""),
        "{text}"
    );
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_is_ephemeral_unless_persist() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_str().unwrap();
    let base = |extra: &[&str]| {
        let mut v: Vec<String> = [
            "--url",
            csms.url(),
            "--id",
            "CP-UX",
            "--no-tui",
            "--state-dir",
            state,
        ]
        .map(String::from)
        .into();
        v.extend(extra.iter().map(|s| (*s).to_owned()));
        v
    };
    let output = run(base(&["run", "--scenario", "charge-session"])).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

    let output = run(base(&["run", "--scenario", "charge-session", "--persist"])).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(std::fs::read_dir(dir.path()).unwrap().count() > 0);

    let output = run(base(&[
        "run",
        "--scenario",
        "charge-session",
        "--persist",
        "--no-persist",
    ]))
    .await;
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn colour_follows_the_flag() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let scenario = ["run", "--scenario", "heartbeat-and-status"];
    // Not a terminal: auto means plain.
    let plain = run(args(csms.url(), &scenario)).await;
    assert!(!stdout(&plain).contains('\x1b'), "{}", stdout(&plain));
    let always = run(args(
        csms.url(),
        &[&["--color", "always"], &scenario[..]].concat(),
    ))
    .await;
    let text = stdout(&always);
    assert!(
        text.contains("\x1b[32mPASS\x1b[0m") && text.contains("\x1b[32mPASSED\x1b[0m"),
        "{text:?}"
    );
    let never = run(args(
        csms.url(),
        &[&["--color", "never"], &scenario[..]].concat(),
    ))
    .await;
    assert!(!stdout(&never).contains('\x1b'));
    // Never in JSON.
    let json = run(args(
        csms.url(),
        &[&["--color", "always", "--json"], &scenario[..]].concat(),
    ))
    .await;
    assert!(!stdout(&json).contains('\x1b'));
    csms.shutdown().await;
}

#[test]
fn version_command_and_flag() {
    let flag = command(&["--version"]).output().unwrap();
    assert_eq!(stdout(&flag).lines().count(), 1, "{}", stdout(&flag));

    let output = command(&["version"]).output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    for needle in [
        "kirby-ocpp ",
        "commit: ",
        "built: ",
        "target: ",
        "protocols: OCPP 1.6J",
    ] {
        assert!(text.contains(needle), "{text}");
    }

    let output = command(&["version", "--json"]).output().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["name"], "kirby-ocpp");
    assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
    assert!(value["commit"].is_string() && value["target"].is_string());
    assert!(value["build_date"].as_str().is_some_and(|d| d.len() == 10));
    assert_eq!(value["protocols"][0], "OCPP 1.6J");
}

#[test]
fn completions_are_generated_for_every_shell() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let output = command(&["completions", shell]).output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{shell}");
        let text = stdout(&output);
        assert!(!text.is_empty() && text.contains("kirby-ocpp"), "{shell}");
    }
    let output = command(&["completions", "nushell"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}

fn write(dir: &TempDir, name: &str, text: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

#[test]
fn scenarios_validate_checks_files_directories_and_built_ins() {
    let dir = tempfile::tempdir().unwrap();
    let good = write(
        &dir,
        "good.toml",
        "name = \"good\"\n[[steps]]\naction = \"heartbeat\"\n[[steps]]\naction = \"heartbeat\"\n",
    );
    let bad = write(
        &dir,
        "bad.toml",
        "name = \"x\"\n[[steps]]\naction = \"explode\"\n",
    );

    let output = command(&["scenarios", "validate", &good, "charge-session"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains(&format!("ok {good} (2 steps)")), "{text}");
    assert!(text.contains("ok built-in charge-session ("), "{text}");

    let output = command(&["scenarios", "validate", &bad]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let err = stderr(&output);
    assert!(err.contains(&bad) && err.contains("explode"), "{err}");

    // A directory is scanned; the good file is still reported.
    let output = command(&["scenarios", "validate", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).contains("good.toml"), "{}", stdout(&output));
    assert!(stderr(&output).contains("bad.toml"), "{}", stderr(&output));

    let output = command(&["scenarios", "validate", "nope"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("no built-in scenario `nope`"),
        "{}",
        stderr(&output)
    );

    let output = command(&["scenarios", "validate"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}
