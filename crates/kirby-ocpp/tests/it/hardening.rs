//! Process tests of the output hardening: hostile text from the CSMS, secrets in `--url`,
//! closed or unread stdout, `--json` and `--log-file` on their own.

use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use kirby_ocpp_testkit::{CsmsOptions, ErrorCode, MockCsms, Reply};
use kirby_ocpp_tui::text::is_unsafe_char;
use tempfile::TempDir;

const LIMIT: Duration = Duration::from_secs(60);
const HOSTILE: &str = "boom\u{85}\u{9b}31m\u{202e}\u{2028}\u{1b}]0;pwned\u{7}\n  PASS  9 forged step (1 ms)\nPASSED in 5 ms";

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
        "CP-HARD",
        "--no-persist",
        "--no-tui",
    ]
    .map(String::from)
    .into();
    args.extend(extra.iter().map(|s| (*s).to_owned()));
    args
}

fn write(dir: &TempDir, name: &str, text: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

/// Waits for a child that nobody reads from; kills it at the limit.
fn wait_exit(child: &mut std::process::Child) -> Option<i32> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code();
        }
        if start.elapsed() > LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::yield_now();
    }
}

/// TOML source; the escapes are TOML's own (`\\uXXXX`), so no hostile character is in this file.
const HOSTILE_SCENARIO: &str = concat!(
    "name = \"hostile\\u0085\\u001b[31m\\nPASS forged\"\n",
    "[[steps]]\n",
    "name = \"step\\u0085\\u202e\\u2028one\\nPASS  7 forged step (1 ms)\"\n",
    "action = \"data_transfer\"\n",
    "vendor_id = \"evil\"\n",
);

fn assert_clean_lines(text: &str, what: &str) {
    for line in text.lines() {
        assert!(!line.chars().any(is_unsafe_char), "{what}: {line:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn hostile_csms_text_is_neutralised_in_human_json_and_log_file() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    csms.respond("DataTransfer", |_| {
        Reply::Error(ErrorCode::GenericError, HOSTILE.to_owned())
    });
    let dir = tempfile::tempdir().unwrap();
    let scenario = write(&dir, "hostile.toml", HOSTILE_SCENARIO);
    for json in [false, true] {
        let log = dir.path().join(format!("log-{json}.txt"));
        let mut extra = vec!["--log-file", log.to_str().unwrap()];
        if json {
            extra.push("--json");
        }
        extra.extend(["run", "--scenario", &scenario]);
        let output = run(station_args(&csms, &extra)).await;
        assert_eq!(output.status.code(), Some(1), "json={json}");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let logged = std::fs::read_to_string(&log).unwrap();
        assert_clean_lines(&stdout, "stdout");
        assert_clean_lines(&logged, "log file");
        assert!(!stdout.contains('\u{1b}') && !logged.contains('\u{1b}'));
        if json {
            for line in stdout.lines().chain(logged.lines()) {
                serde_json::from_str::<serde_json::Value>(line).expect("valid JSON line");
            }
            assert!(stdout.contains("\\u0085"), "{stdout}");
        } else {
            // The forged lines stay inside the one line they were sent in.
            for line in stdout.lines().chain(logged.lines()) {
                let trimmed = line.trim_start();
                assert!(
                    !trimmed.starts_with("PASS  9") && !trimmed.starts_with("PASS  7"),
                    "{line:?}"
                );
                assert!(!trimmed.starts_with("PASSED in 5"), "{line:?}");
            }
            assert!(stdout.contains("FAIL"), "{stdout}");
        }
    }
    csms.shutdown().await;
}

#[test]
fn url_password_is_never_echoed() {
    let secret = "s3cretpw";
    let urls = [
        format!("ws://CP-1:{secret}@127.0.0.1:99999/ocpp"),
        format!("http://CP-1:{secret}@127.0.0.1:9000/ocpp"),
        format!("ws://CP-1:{secret}@[bad"),
        format!("ftp://CP-1:{secret}@host/x"),
    ];
    for url in &urls {
        for args in [
            vec!["--url", url.as_str()],
            vec!["--print-config", "--url", url.as_str()],
        ] {
            let output = command(&args).output().unwrap();
            assert_eq!(output.status.code(), Some(2), "{url}");
            let all = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!all.contains(secret), "{url}: {all}");
        }
        let output = command(&["--help"]).env("OCPP_URL", url).output().unwrap();
        assert!(output.status.success());
        let all = String::from_utf8_lossy(&output.stdout);
        assert!(!all.contains(secret), "{all}");
        let output = command(&[]).env("OCPP_URL", url).output().unwrap();
        let all = String::from_utf8_lossy(&output.stderr);
        assert!(!all.contains(secret), "{all}");
    }
}

#[test]
fn help_shows_the_id_placeholder_and_the_licenses_rule() {
    let output = command(&["--help"]).output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("such as CP-{n:03}"), "{text}");
    let output = command(&["scenarios", "list", "--licenses"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

/// Spawns with a stdout whose reader is already gone.
#[cfg(unix)]
fn spawn_closed_stdout(args: &[String]) -> std::process::Child {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut child = command(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    child
}

#[cfg(unix)]
async fn closed_stdout_still_writes_reports(count: Option<&str>) {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let junit = dir.path().join("ep.xml");
    let report = dir.path().join("ep.json");
    let mut extra = vec!["--json"];
    if let Some(count) = count {
        extra.extend(["--count", count, "--id", "CP-{n}"]);
    }
    extra.extend([
        "run",
        "--scenario",
        "charge-session",
        "--junit",
        junit.to_str().unwrap(),
        "--report",
        report.to_str().unwrap(),
    ]);
    let mut args = station_args(&csms, &extra);
    if count.is_some() {
        // The fleet id comes from `--id`: drop the single-station one.
        let at = args.iter().position(|a| a == "CP-HARD").unwrap();
        args.drain(at - 1..=at);
    }
    let child = spawn_closed_stdout(&args);
    let output = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(!stderr.contains("Broken pipe"), "{stderr}");
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let xml = std::fs::read_to_string(&junit).expect("junit written");
    assert!(xml.contains("<testsuites"), "{xml}");
    if count.is_none() {
        assert!(report.exists());
    }
    csms.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn closed_stdout_does_not_panic_and_reports_are_written() {
    closed_stdout_still_writes_reports(None).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn closed_stdout_in_a_fleet_run_does_not_panic_and_junit_is_written() {
    closed_stdout_still_writes_reports(Some("5")).await;
}

fn read_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

/// Nobody reads stdout and one frame is bigger than any pipe buffer: the log file still gets
/// every line and the process still ends.
#[tokio::test(flavor = "multi_thread")]
async fn unread_stdout_does_not_stall_the_log_file_or_the_exit() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let big = "x".repeat(300_000);
    let mut text = String::from("name = \"big\"\n");
    for _ in 0..3 {
        let _ = write!(
            text,
            "[[steps]]\naction = \"data_transfer\"\nvendor_id = \"v\"\ndata = \"{big}\"\n"
        );
    }
    let scenario = write(&dir, "big.toml", &text);
    let log = dir.path().join("big.log");
    let args = station_args(
        &csms,
        &[
            "--log-file",
            log.to_str().unwrap(),
            "run",
            "--scenario",
            &scenario,
        ],
    );
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut child = command(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let wait = tokio::task::spawn_blocking(move || {
        let code = wait_exit(&mut child);
        (code, child)
    });
    let (code, _child) = wait.await.unwrap();
    assert_eq!(
        code,
        Some(0),
        "process must end although stdout is not read"
    );
    assert!(read_len(&log) > 900_000, "log has {} bytes", read_len(&log));
    csms.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn json_without_no_tui_runs_headless_and_fills_the_log_file() {
    // No terminal in tests, so this only pins the headless path (no `--no-tui` given).
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    let args: Vec<String> = [
        "--url",
        csms.url(),
        "--id",
        "CP-J",
        "--no-persist",
        "--json",
        "--log-file",
        log.to_str().unwrap(),
        "run",
        "--scenario",
        "charge-session",
    ]
    .map(String::from)
    .into();
    let output = run(args).await;
    assert_eq!(output.status.code(), Some(0));
    assert!(read_len(&log) > 0);
    csms.shutdown().await;
}
