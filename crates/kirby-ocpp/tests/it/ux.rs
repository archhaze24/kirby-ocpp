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
    assert!(!err.contains("configuration error"), "{err}");
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_ws_against_a_tls_server_hints_at_wss() {
    let csms = MockCsms::start(CsmsOptions {
        tls: Some(TlsOpts::default()),
        ..CsmsOptions::default()
    })
    .await
    .unwrap();
    let url = csms.url().replacen("wss://", "ws://", 1);
    let output = run(args(&url, &["--no-reconnect"])).await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(err.contains("try a wss:// URL"), "{err}");
    csms.shutdown().await;
}

/// Runs `args` (a `run`, `suite` or plain invocation) and returns the exit code and stderr.
fn startup_failure(args: &[&str]) -> (Option<i32>, String) {
    let output = crate::support::output(command(args));
    (output.status.code(), stderr(&output))
}

#[test]
fn missing_tls_files_fail_at_startup_naming_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("absent.pem");
    let missing = missing.to_str().unwrap();
    let present = write(&dir, "present.pem", "not a certificate");
    let present = present.as_str();
    for flag in ["--tls-ca", "--tls-cert", "--tls-key"] {
        let mut base = vec!["--url", "wss://127.0.0.1:1/ocpp", "--no-tui", flag, missing];
        // The other half of the pair exists, so only `flag` is at fault.
        if flag == "--tls-cert" {
            base.extend(["--tls-key", present]);
        } else if flag == "--tls-key" {
            base.extend(["--tls-cert", present]);
        }
        let mut run_args = base.clone();
        run_args.extend(["run", "--scenario", "charge-session"]);
        let mut suite_args = base.clone();
        suite_args.extend(["suite", "charge-session"]);
        for args in [base, run_args, suite_args] {
            let (code, err) = startup_failure(&args);
            assert_eq!(code, Some(2), "{args:?}: {err}");
            assert!(err.contains(flag) && err.contains("absent.pem"), "{err}");
            assert_eq!(err.trim().lines().count(), 1, "{err}");
        }
    }
}

#[test]
fn over_long_boot_fields_fail_at_startup() {
    let url = ["--url", "ws://127.0.0.1:1/ocpp", "--no-tui"];
    for (flag, ok, bad, limit) in [
        ("--serial", 25, 26, "25"),
        ("--firmware-version", 50, 51, "50"),
        ("--vendor", 20, 21, "20"),
        ("--model", 20, 21, "20"),
    ] {
        let value = "x".repeat(bad);
        let (code, err) = startup_failure(&[url[0], url[1], url[2], flag, &value]);
        assert_eq!(code, Some(2), "{flag}: {err}");
        assert!(err.contains(flag) && err.contains(limit), "{err}");
        assert_eq!(err.trim().lines().count(), 1, "{err}");
        // The limit itself is accepted (printing the configuration does not connect).
        let value = "x".repeat(ok);
        let (code, err) =
            startup_failure(&[url[0], url[1], url[2], flag, &value, "--print-config"]);
        assert_eq!(code, Some(0), "{flag}: {err}");
    }
}

#[test]
fn unsafe_ids_fail_at_startup() {
    for id in ["CP\u{7}1", "CP\u{200b}1", "CP\n1", "CP\u{202e}1"] {
        let (code, err) = startup_failure(&["--url", "ws://127.0.0.1:1/ocpp", "--id", id]);
        assert_eq!(code, Some(2), "{id:?}: {err}");
        assert!(err.contains("--id"), "{err}");
        assert_eq!(err.trim().lines().count(), 1, "{err}");
    }
}

#[test]
fn one_sided_client_tls_names_both_flags() {
    for flag in ["--tls-cert", "--tls-key"] {
        let (code, err) = startup_failure(&["--url", "wss://127.0.0.1:1/ocpp", flag, "x.pem"]);
        assert_eq!(code, Some(2), "{err}");
        assert!(
            err.contains("--tls-cert") && err.contains("--tls-key"),
            "{err}"
        );
    }
}

#[test]
fn version_help_has_its_own_json_text() {
    let text = stdout(&command(&["version", "--help"]).output().unwrap());
    assert!(text.contains("one JSON object"), "{text}");
    assert!(!text.contains("never opens the terminal UI"), "{text}");
    let top = stdout(&command(&["--help"]).output().unwrap());
    assert!(top.contains("never opens the terminal UI"), "{top}");
}

#[test]
fn help_and_output_use_us_spelling() {
    let text = stdout(&command(&["--help"]).output().unwrap());
    assert!(!text.contains("licence"), "{text}");
    assert!(text.contains("license notices"), "{text}");
}

#[cfg(unix)]
#[test]
fn merged_stdout_and_stderr_keep_whole_lines() {
    // `2>&1` makes both streams one pipe, as in a CI log. A station that gives up prints
    // event lines on stdout and the final error on stderr: no line may contain another.
    for _ in 0..10 {
        let mut shell = Command::new("sh");
        shell
            .arg("-c")
            .arg(
                "exec \"$0\" --url \"$1\" --id CP-MERGE --no-persist --no-tui \
                 --reconnect-max-attempts 3 --reconnect-initial-ms 1 --reconnect-max-ms 1 2>&1",
            )
            .arg(env!("CARGO_BIN_EXE_kirby-ocpp"))
            .arg(dead_url())
            .stdin(Stdio::null());
        let output = crate::support::output(shell);
        assert_eq!(output.status.code(), Some(1));
        let text = stdout(&output);
        assert!(text.contains("reconnect stopped"), "{text}");
        for line in text.lines() {
            let is_error = line.starts_with("error: ");
            assert!(
                line.matches("error: reconnect").count() <= usize::from(is_error)
                    && (is_error || line.starts_with(|c: char| c.is_ascii_digit())),
                "torn line {line:?} in {text}"
            );
        }
    }
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
async fn boot_accepted_is_logged_once() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let output = run(args(
        csms.url(),
        &["-v", "run", "--scenario", "heartbeat-and-status"],
    ))
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    let boots = text
        .lines()
        .filter(|line| line.contains("BootNotification accepted"))
        .collect::<Vec<_>>();
    assert_eq!(boots.len(), 1, "{text}");
    assert!(boots[0].contains("heartbeat every"), "{text}");
    csms.shutdown().await;
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

#[test]
fn offline_subcommand_help_omits_station_flags() {
    for args in [
        &["completions", "--help"][..],
        &["version", "--help"],
        &["scenarios", "--help"],
        &["scenarios", "list", "--help"],
        &["scenarios", "validate", "--help"],
    ] {
        let text = stdout(&command(args).output().unwrap());
        assert!(
            !text.contains("--url") && !text.contains("--tls-ca"),
            "{args:?}"
        );
    }
    let version = stdout(&command(&["version", "--help"]).output().unwrap());
    assert!(version.contains("--json"));
    for args in [&["run", "--help"][..], &["suite", "--help"]] {
        let text = stdout(&command(args).output().unwrap());
        assert!(
            text.contains("--url") && text.contains("--tls-ca"),
            "{args:?}"
        );
        assert!(text.contains("kirby-ocpp --help"), "{args:?}");
    }
}

#[test]
fn hidden_station_flags_still_parse_on_offline_subcommands() {
    let output = command(&["version", "--url", "ws://x", "--count", "2", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let output = command(&["scenarios", "list", "--id", "CP-9"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let output = command(&["completions", "bash", "--no-tui"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
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
