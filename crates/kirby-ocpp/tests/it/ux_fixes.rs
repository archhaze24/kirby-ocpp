//! Process tests for the fixes after the CLI/TUI review: a link lost mid-scenario, the
//! `--state-dir` note of `run`, and `scenarios validate` duplicates.

use std::process::{Command, Stdio};
use std::time::Duration;

use kirby_ocpp_testkit::{CsmsOptions, MockCsms};

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

fn write(dir: &tempfile::TempDir, name: &str, text: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_lost_during_the_scenario_fails_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let scenario = write(
        &dir,
        "long.toml",
        "name = \"long\"\n[[steps]]\naction = \"heartbeat\"\n[[steps]]\naction = \"sleep\"\nms = 40000\n",
    );
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let mut cmd = command(&[
        "--url",
        csms.url(),
        "--id",
        "CP-LOST",
        "--no-persist",
        "--no-tui",
        "--no-reconnect",
        "run",
        "--scenario",
        &scenario,
        "--timeout",
        "50",
    ]);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = crate::support::ChildGuard::spawn(&mut cmd);
    csms.wait_for_call("Heartbeat", Duration::from_secs(20))
        .await
        .unwrap();
    csms.drop_connection().unwrap();
    let output = crate::support::finish(child).await;
    // Without the lost link the 40 s scenario would pass (exit 0): exit 1 proves the run failed
    // fast. Depending on timing the link is noticed while the heartbeat waits for its answer
    // (a failed step in the report) or during the sleep (`connection lost` on stderr).
    assert_eq!(output.status.code(), Some(1));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        ["connection lost", "reconnect stopped", "FAIL"]
            .iter()
            .any(|needle| text.contains(needle)),
        "{text}"
    );
}

#[test]
fn run_notes_an_ignored_state_dir() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_str().unwrap();
    let output = crate::support::output(command(&[
        "--state-dir",
        state,
        "--url",
        "ws://127.0.0.1:1/x",
        "--no-reconnect",
        "run",
        "--scenario",
        "charge-session",
        "--timeout",
        "5",
    ]));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("--state-dir is ignored by `run`"), "{err}");
}

#[test]
fn validate_reports_duplicates_and_skipped_entries() {
    let dir = tempfile::tempdir().unwrap();
    let text = "name = \"same\"\n[[steps]]\naction = \"heartbeat\"\n";
    let one = write(&dir, "one.toml", text);
    let two = write(&dir, "two.toml", text);
    // The same file twice is validated once.
    let output = command(&["scenarios", "validate", &one, &one])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .matches("ok ")
            .count(),
        1
    );
    // Two files with one scenario name: a warning, exit stays 0.
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        dir.path().join("missing.toml"),
        dir.path().join("dangling.toml"),
    )
    .unwrap();
    let output = command(&["scenarios", "validate", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("scenario name `same` is used by"), "{err}");
    assert!(err.contains(&two) || err.contains("two.toml"), "{err}");
    #[cfg(unix)]
    assert!(
        err.contains("skipped") && err.contains("dangling.toml"),
        "{err}"
    );
}
