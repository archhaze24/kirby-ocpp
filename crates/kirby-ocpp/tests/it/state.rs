//! Process tests of `kirby-ocpp state path | show | reset`.

use std::path::Path;
use std::process::{Command, Output, Stdio};

use kirby_ocpp_testkit::{CsmsOptions, MockCsms};
use serde_json::Value;

const SECRET: &str = "TOP-SECRET-AUTH-KEY-123";

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

fn run(args: &[&str]) -> Output {
    crate::support::output(command(args, &[]))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("not JSON: {e}\n{}", text(&output.stdout)))
}

/// Runs a scenario with `--persist` so the emulator writes a real state file, then adds a
/// secrets file next to it.
async fn persisted(dir: &Path, id: &str) {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let url = csms.url().to_owned();
    let (dir, id) = (dir.to_owned(), id.to_owned());
    let output = tokio::task::spawn_blocking(move || {
        crate::support::output(command(
            &[
                "--url",
                &url,
                "--id",
                &id,
                "--state-dir",
                dir.to_str().unwrap(),
                "--connectors",
                "2",
                "run",
                "--scenario",
                "charge-session",
                "--persist",
            ],
            &[],
        ))
    })
    .await
    .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
}

fn file_of(dir: &Path, id: &str) -> std::path::PathBuf {
    dir.join(format!("{id}.station.json"))
}

fn add_secrets(dir: &Path, id: &str) -> std::path::PathBuf {
    let path = dir.join(format!("{id}.station.secrets.json"));
    std::fs::write(
        &path,
        format!(r#"{{"version": 9, "authorization_key": "{SECRET}"}}"#),
    )
    .unwrap();
    path
}

#[test]
fn path_prints_the_directory_or_the_file_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("not-yet");
    let state_text = state.to_str().unwrap();
    let output = run(&["state", "path", "--state-dir", state_text]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(text(&output.stdout).trim(), state_text);

    let output = run(&["state", "path", "--state-dir", state_text, "--id", "CP/1"]);
    let printed = text(&output.stdout);
    assert_eq!(
        printed.trim(),
        state.join("CP%2F1.station.json").to_str().unwrap()
    );
    assert!(!state.exists(), "path must not create anything");

    let output = run(&["state", "path", "--state-dir", state_text, "--json"]);
    let value = json(&output);
    assert_eq!(value["schema"], 1);
    assert_eq!(value["dir"], state_text);
    assert!(value["file"].is_null());
    assert!(value["station"].is_null());
    let output = run(&[
        "state",
        "--state-dir",
        state_text,
        "path",
        "--id",
        "CP-9",
        "--json",
    ]);
    let value = json(&output);
    assert_eq!(value["station"], "CP-9");
    assert!(
        value["file"]
            .as_str()
            .unwrap()
            .ends_with("CP-9.station.json")
    );
    assert!(
        value["secrets_file"]
            .as_str()
            .unwrap()
            .ends_with("CP-9.station.secrets.json")
    );
}

#[test]
fn the_state_directory_comes_from_the_environment_too() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_str().unwrap();
    let output = crate::support::output(command(
        &["state", "path"],
        &[("KIRBY_OCPP_STATE_DIR", state)],
    ));
    assert_eq!(text(&output.stdout).trim(), state);
}

#[test]
fn show_and_reset_need_an_explicit_id() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_str().unwrap();
    for action in [&["show"][..], &["reset", "--yes"][..]] {
        let mut args = vec!["state"];
        args.extend_from_slice(action);
        args.extend(["--state-dir", state]);
        let output = run(&args);
        assert_eq!(output.status.code(), Some(2), "{action:?}");
        assert!(text(&output.stderr).contains("--id"));
    }
}

#[test]
fn show_of_a_missing_state_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_str().unwrap();
    let output = run(&["state", "show", "--id", "CP-NONE", "--state-dir", state]);
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("no saved state for station `CP-NONE`"));
    let output = run(&[
        "state",
        "show",
        "--id",
        "CP-NONE",
        "--state-dir",
        state,
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(json(&output)["status"], "missing");
}

#[tokio::test(flavor = "multi_thread")]
async fn show_summarizes_a_real_state_and_never_prints_secrets() {
    let dir = tempfile::tempdir().unwrap();
    persisted(dir.path(), "CP-ST").await;
    add_secrets(dir.path(), "CP-ST");
    let state = dir.path().to_str().unwrap().to_owned();

    let output = tokio::task::spawn_blocking({
        let state = state.clone();
        move || run(&["state", "show", "--id", "CP-ST", "--state-dir", &state])
    })
    .await
    .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(out.contains("Station:        CP-ST"), "{out}");
    assert!(
        out.contains("Store version:  9 (this build reads up to 9)"),
        "{out}"
    );
    assert!(
        out.contains("Secrets file:   present (not read, not shown)"),
        "{out}"
    );
    assert!(out.contains("Connectors:     2"), "{out}");
    assert!(out.contains("Local list:"), "{out}");
    assert!(out.contains("Configuration:"), "{out}");
    assert!(!out.contains(SECRET), "{out}");
    assert!(!text(&output.stderr).contains(SECRET));

    let output = tokio::task::spawn_blocking(move || {
        run(&[
            "state",
            "show",
            "--id",
            "CP-ST",
            "--state-dir",
            &state,
            "--json",
        ])
    })
    .await
    .unwrap();
    assert!(!text(&output.stdout).contains(SECRET));
    let value = json(&output);
    assert_eq!(value["schema"], 1);
    assert_eq!(value["status"], "found");
    assert_eq!(value["station"], "CP-ST");
    assert_eq!(value["store_version"], 9);
    assert_eq!(value["secrets_file"], true);
    let state = &value["state"];
    assert_eq!(state["connector_count"], 2);
    assert_eq!(state["connectors"].as_array().unwrap().len(), 2);
    assert_eq!(state["connectors"][0]["id"], 1);
    assert!(state["connectors"][0]["transaction"]["state"].is_string());
    assert!(state["configuration"].is_object());
    let mut keys: Vec<&str> = state
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "authorization_cache_entries",
            "availability",
            "certificates",
            "charging_profiles",
            "configuration",
            "connector_count",
            "connectors",
            "local_authorization_list",
            "offline_samples",
            "offline_transactions",
            "pending_profile_switch",
            "security_events",
        ]
    );
}

#[test]
fn a_secret_looking_configuration_value_is_masked() {
    let dir = tempfile::tempdir().unwrap();
    let id = "CP-MASK";
    let file = file_of(dir.path(), id);
    std::fs::write(
        &file,
        format!(
            r#"{{"version": 9, "charge_point_id": "{id}", "configuration": {{"AuthorizationKey": "{SECRET}", "HeartbeatInterval": "60"}}}}"#
        ),
    )
    .unwrap();
    let state = dir.path().to_str().unwrap();
    for extra in [&[][..], &["--json"][..]] {
        let mut args = vec!["state", "show", "--id", id, "--state-dir", state];
        args.extend_from_slice(extra);
        let output = run(&args);
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
        let out = text(&output.stdout);
        assert!(!out.contains(SECRET), "{out}");
        assert!(out.contains("HeartbeatInterval"), "{out}");
        assert!(out.contains("60"), "{out}");
    }
}

#[test]
fn a_newer_state_file_is_refused_and_left_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let id = "CP-NEW";
    let file = file_of(dir.path(), id);
    let body = br#"{"version": 99, "future": true}"#;
    std::fs::write(&file, body).unwrap();
    let state = dir.path().to_str().unwrap();

    let output = run(&["state", "show", "--id", id, "--state-dir", state]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("format version 99"),
        "{}",
        text(&output.stderr)
    );

    let output = run(&["state", "reset", "--id", id, "--state-dir", state, "--yes"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("nothing was deleted"));
    assert_eq!(std::fs::read(&file).unwrap(), body);
}

#[test]
fn an_unusable_state_file_is_reported_but_not_moved() {
    let dir = tempfile::tempdir().unwrap();
    let id = "CP-BAD";
    let file = file_of(dir.path(), id);
    std::fs::write(&file, b"not json").unwrap();
    let state = dir.path().to_str().unwrap();
    let output = run(&["state", "show", "--id", id, "--state-dir", state, "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let value = json(&output);
    assert_eq!(value["status"], "unusable");
    assert!(value["reason"].as_str().unwrap().contains("not valid JSON"));
    assert_eq!(std::fs::read(&file).unwrap(), b"not json");
    assert!(!dir.path().join(format!("{id}.station.json.bak")).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn reset_needs_yes_without_a_terminal_and_deletes_only_that_station() {
    let dir = tempfile::tempdir().unwrap();
    persisted(dir.path(), "CP-A").await;
    persisted(dir.path(), "CP-B").await;
    let secrets = add_secrets(dir.path(), "CP-A");
    let unrelated = dir.path().join("notes.txt");
    std::fs::write(&unrelated, b"keep").unwrap();
    let state = dir.path().to_str().unwrap().to_owned();
    let file_a = file_of(dir.path(), "CP-A");
    let file_b = file_of(dir.path(), "CP-B");

    let refused = tokio::task::spawn_blocking({
        let state = state.clone();
        move || run(&["state", "reset", "--id", "CP-A", "--state-dir", &state])
    })
    .await
    .unwrap();
    assert_eq!(refused.status.code(), Some(2));
    assert!(text(&refused.stderr).contains("--yes"));
    assert!(file_a.exists() && secrets.exists());

    let done = tokio::task::spawn_blocking({
        let state = state.clone();
        move || {
            run(&[
                "state",
                "reset",
                "--id",
                "CP-A",
                "--state-dir",
                &state,
                "--yes",
                "--json",
            ])
        }
    })
    .await
    .unwrap();
    assert_eq!(done.status.code(), Some(0), "{}", text(&done.stderr));
    let value = json(&done);
    assert_eq!(value["station"], "CP-A");
    assert_eq!(value["deleted"].as_array().unwrap().len(), 2);
    assert!(!file_a.exists() && !secrets.exists());
    assert!(file_b.exists(), "another station is untouched");
    assert!(unrelated.exists(), "unrelated files are untouched");

    let again = tokio::task::spawn_blocking(move || {
        run(&[
            "state",
            "reset",
            "--id",
            "CP-A",
            "--state-dir",
            &state,
            "-y",
        ])
    })
    .await
    .unwrap();
    assert_eq!(again.status.code(), Some(0));
    assert!(text(&again.stdout).contains("nothing to delete"));
}

#[test]
fn reset_never_leaves_the_state_directory() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("states");
    std::fs::create_dir(&state).unwrap();
    // A file next to the state directory that a path traversal would hit.
    let victim = dir.path().join("victim.station.json");
    std::fs::write(&victim, b"keep").unwrap();
    let output = run(&[
        "state",
        "reset",
        "--id",
        "../victim",
        "--state-dir",
        state.to_str().unwrap(),
        "--yes",
    ]);
    assert_eq!(output.status.code(), Some(0));
    assert!(victim.exists());
}

#[test]
fn state_arguments_have_no_configuration_keys() {
    let dir = tempfile::tempdir().unwrap();
    for key in ["yes", "timeout"] {
        let file = dir.path().join(format!("{key}.toml"));
        std::fs::write(&file, format!("{key} = 1\n")).unwrap();
        let output = run(&[
            "--config",
            file.to_str().unwrap(),
            "--url",
            "ws://h:1/x",
            "check",
        ]);
        assert_eq!(output.status.code(), Some(2), "{key}");
        assert!(
            text(&output.stderr).contains(&format!("unknown key `{key}`")),
            "{key}: {}",
            text(&output.stderr)
        );
    }
}
