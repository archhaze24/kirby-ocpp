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
fn reset_refuses_a_backup_of_a_newer_version() {
    let dir = tempfile::tempdir().unwrap();
    let id = "CP-BAK";
    let file = file_of(dir.path(), id);
    std::fs::write(&file, br#"{"version": 1}"#).unwrap();
    let backup = dir.path().join(format!("{id}.station.json.bak"));
    let body = br#"{"version": 99, "future": true}"#;
    std::fs::write(&backup, body).unwrap();
    let state = dir.path().to_str().unwrap();

    let output = run(&["state", "reset", "--id", id, "--state-dir", state, "--yes"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    assert!(text(&output.stderr).contains("nothing was deleted"));
    assert_eq!(std::fs::read(&backup).unwrap(), body);
    assert!(file.exists());
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

// OCPP 2.x state files. The `--protocol` flag is needed by the tests named `v201_*`.

const PASSWORD: &str = "S3NTINEL-BASIC-PASSWORD";

/// Writes a 2.x state file (and a secrets file holding `PASSWORD`) next to nothing else.
fn write_v201(dir: &Path, id: &str) {
    use kirby_ocpp_core::store::{FileKind, PersistedStation2, StationSecrets2, StationStore};
    let store = StationStore::new_for(FileKind::V2, dir, id);
    let mut state = PersistedStation2::new(id);
    state.boot_count = 4;
    state.station_operative = false;
    state
        .device_model
        .values
        .insert("OCPPCommCtrlr.HeartbeatInterval".to_owned(), "9".to_owned());
    state.device_model.values.insert(
        "SecurityCtrlr.BasicAuthPassword".to_owned(),
        PASSWORD.to_owned(),
    );
    store.save2(&state).unwrap();
    store
        .save_secrets2(&StationSecrets2 {
            version: 0,
            basic_auth_password: Some(PASSWORD.to_owned()),
        })
        .unwrap();
}

#[test]
fn path_and_show_json_name_the_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_str().unwrap();
    let output = run(&[
        "state",
        "path",
        "--state-dir",
        state,
        "--id",
        "CP-9",
        "--json",
    ]);
    assert_eq!(json(&output)["protocol"], "ocpp1.6");
    let output = run(&[
        "state",
        "show",
        "--state-dir",
        state,
        "--id",
        "CP-9",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let value = json(&output);
    assert_eq!(value["protocol"], "ocpp1.6");
    assert_eq!(value["status"], "missing");
}

#[test]
fn v201_path_prints_both_names_of_the_2_x_files() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_str().unwrap();
    let args = ["state", "path", "--state-dir", state, "--id", "CP-9"];
    let output = run(&[&args[..], &["--protocol", "ocpp2.0.1"]].concat());
    assert!(text(&output.stdout).trim().ends_with("CP-9.station2.json"));
    let output = run(&[&args[..], &["--protocol", "ocpp2.0.1", "--json"]].concat());
    let value = json(&output);
    assert_eq!(value["protocol"], "ocpp2.0.1");
    assert!(
        value["file"]
            .as_str()
            .unwrap()
            .ends_with("CP-9.station2.json")
    );
    assert!(
        value["secrets_file"]
            .as_str()
            .unwrap()
            .ends_with("CP-9.station2.secrets.json")
    );
}

#[test]
fn v201_show_reads_the_2_x_file_and_never_prints_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_str().unwrap();
    write_v201(dir.path(), "CP-9");
    let base = [
        "state",
        "show",
        "--state-dir",
        state,
        "--id",
        "CP-9",
        "--protocol",
        "ocpp2.0.1",
    ];
    let output = run(&base);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let printed = text(&output.stdout);
    assert!(printed.contains("ocpp2.0.1"), "{printed}");
    assert!(printed.contains("Boots:          4"), "{printed}");
    assert!(printed.contains("Inoperative"), "{printed}");
    assert!(printed.contains("Secrets file:   present (not read, not shown)"));
    assert!(
        printed.contains("OCPPCommCtrlr.HeartbeatInterval = 9"),
        "{printed}"
    );
    assert!(
        printed.contains("SecurityCtrlr.BasicAuthPassword = ***"),
        "{printed}"
    );
    assert!(!printed.contains(PASSWORD));

    let output = run(&[&base[..], &["--json"]].concat());
    let value = json(&output);
    assert_eq!(value["protocol"], "ocpp2.0.1");
    assert_eq!(value["status"], "found");
    assert_eq!(value["secrets_file"], true);
    assert_eq!(
        value["supported_store_version"],
        kirby_ocpp_core::store::STORE2_VERSION
    );
    assert_eq!(value["state"]["boot_count"], 4);
    assert_eq!(
        value["state"]["device_model"]["SecurityCtrlr.BasicAuthPassword"],
        "***"
    );
    assert!(!text(&output.stdout).contains(PASSWORD));
}

#[test]
fn v201_show_prints_the_energy_of_an_active_transaction_in_whole_wh() {
    use kirby_ocpp_core::store::{FileKind, PersistedStation2, StationStore};
    let dir = tempfile::tempdir().unwrap();
    let store = StationStore::new_for(FileKind::V2, dir.path(), "CP-9");
    let mut state = PersistedStation2::new("CP-9");
    state.tx.active.push(
        serde_json::from_value(serde_json::json!({
            "id": "tx-1",
            "evse": 1,
            "next_seq_no": 2,
            "started_at": "2026-01-01T00:00:00Z",
            "charging_state": "Charging",
            "token": null,
            "remote_start_id": null,
            "authorized": true,
            "ev_connected": true,
            "evse_sent": true,
            "token_sent": true,
            "meter_start_wh": 0,
            "energy_wh": 744.796_641_529_166_7
        }))
        .unwrap(),
    );
    store.save2(&state).unwrap();
    let output = run(&[
        "state",
        "show",
        "--state-dir",
        dir.path().to_str().unwrap(),
        "--id",
        "CP-9",
        "--protocol",
        "ocpp2.0.1",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let printed = text(&output.stdout);
    assert!(printed.contains(", 744 Wh"), "{printed}");
    assert!(!printed.contains("744."), "{printed}");
}

/// A 2.1 state with a local list, a reservation, a default tariff and a charging profile.
fn write_v21_with_settings(dir: &Path, id: &str) {
    use kirby_ocpp_core::store::{FileKind, PersistedStation2, StationStore};
    let store = StationStore::new_for(FileKind::V2, dir, id);
    let mut state = PersistedStation2::new(id);
    "ocpp2.1".clone_into(&mut state.protocol);
    state.auth_cache.local_list.version = 3;
    state.auth_cache.local_list.entries.insert(
        "ISO14443|TOKEN-A".to_owned(),
        serde_json::from_value(serde_json::json!({"status": "Accepted", "expires_at": null}))
            .unwrap(),
    );
    state.reservations.reservations.push(
        serde_json::from_value(serde_json::json!({
            "id": 5,
            "expires_at": "2030-01-01T10:00:00Z",
            "evse": 1,
            "connector_type": "cCCS2",
            "id_token": {"id_token": "TOK\u{202e}EN", "kind": "Central"},
            "group_id_token": "GROUP-1",
            "lost": true
        }))
        .unwrap(),
    );
    state.reservations.reservations.push(
        serde_json::from_value(serde_json::json!({
            "id": 6,
            "expires_at": "2030-01-01T11:00:00Z",
            "evse": null,
            "connector_type": null,
            "id_token": {"id_token": "TOKEN-B", "kind": "ISO14443"},
            "group_id_token": null
        }))
        .unwrap(),
    );
    state.default_tariffs.push(serde_json::json!({
        "evseId": 0,
        "tariff": {"tariffId": "T-1", "currency": "EUR", "energy": {"prices": [{"priceKwh": 0.3}]}},
        "validFrom": "2030-02-01T00:00:00Z"
    }));
    state.charging_profiles.profiles.push(
        serde_json::from_value(serde_json::json!({
            "evse": 1,
            "profile": {
                "id": 7,
                "stackLevel": 2,
                "chargingProfilePurpose": "TxDefaultProfile",
                "chargingProfileKind": "Absolute",
                "validTo": "2031-01-01T00:00:00Z",
                "chargingSchedule": []
            },
            "transaction_id": null,
            "installed_at": "2026-01-01T00:00:00Z",
            "source": "CSO"
        }))
        .unwrap(),
    );
    store.save2(&state).unwrap();
}

#[test]
fn v21_show_lists_reservations_tariffs_profiles_and_the_local_list() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_str().unwrap();
    write_v21_with_settings(dir.path(), "CP-9");
    let base = [
        "state",
        "show",
        "--state-dir",
        state,
        "--id",
        "CP-9",
        "--protocol",
        "ocpp2.1",
    ];
    let output = run(&base);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let printed = text(&output.stdout);
    assert!(
        printed.contains("Local list:     version 3, 1 entries"),
        "{printed}"
    );
    assert!(printed.contains("Reservations:   2\n"), "{printed}");
    assert!(
        printed.contains(
            "  EVSE 1  id 5, expires 2030-01-01T10:00:00Z, idToken TOK·EN (Central), group GROUP-1, connector cCCS2, report lost\n"
        ),
        "{printed}"
    );
    assert!(
        printed.contains(
            "  any EVSE  id 6, expires 2030-01-01T11:00:00Z, idToken TOKEN-B (ISO14443)\n"
        ),
        "{printed}"
    );
    assert!(
        printed.contains("Tariffs:        1 default tariffs\n"),
        "{printed}"
    );
    assert!(
        printed.contains("  all EVSEs  id T-1, EUR, valid from 2030-02-01T00:00:00Z\n"),
        "{printed}"
    );
    assert!(
        printed.contains("Profiles:       1 charging profiles\n"),
        "{printed}"
    );
    assert!(
        printed.contains(
            "  EVSE 1  id 7, stack 2, TxDefaultProfile, Absolute, source CSO, valid to 2031-01-01T00:00:00Z\n"
        ),
        "{printed}"
    );
    assert!(!printed.contains('\u{202e}'), "{printed}");

    let value = json(&run(&[&base[..], &["--json"]].concat()));
    let state = &value["state"];
    assert_eq!(state["local_authorization_list"]["version"], 3);
    assert_eq!(state["local_authorization_list"]["entries"], 1);
    assert_eq!(state["reservations"][0]["id"], 5);
    assert_eq!(state["reservations"][0]["evse"], 1);
    assert_eq!(state["reservations"][0]["id_token_type"], "Central");
    assert_eq!(state["reservations"][0]["lost"], true);
    assert!(state["reservations"][1]["evse"].is_null());
    assert_eq!(state["default_tariffs"][0]["evse"], 0);
    assert_eq!(state["default_tariffs"][0]["tariff_id"], "T-1");
    assert_eq!(state["default_tariffs"][0]["currency"], "EUR");
    assert_eq!(state["charging_profiles"][0]["id"], 7);
    assert_eq!(state["charging_profiles"][0]["purpose"], "TxDefaultProfile");
    assert_eq!(state["charging_profiles"][0]["source"], "CSO");
    assert_eq!(state["charging_profiles"][0]["profile"]["stackLevel"], 2);
}

#[test]
fn v201_show_prints_empty_sections_and_no_tariffs_line() {
    let dir = tempfile::tempdir().unwrap();
    write_v201(dir.path(), "CP-9");
    let args = [
        "state",
        "show",
        "--state-dir",
        dir.path().to_str().unwrap(),
        "--id",
        "CP-9",
        "--protocol",
        "ocpp2.0.1",
    ];
    let printed = text(&run(&args).stdout);
    assert!(
        printed.contains("Local list:     version 0, 0 entries"),
        "{printed}"
    );
    assert!(printed.contains("Reservations:   0\n"), "{printed}");
    assert!(
        printed.contains("Profiles:       0 charging profiles\n"),
        "{printed}"
    );
    assert!(!printed.contains("Tariffs:"), "{printed}");
    let value = json(&run(&[&args[..], &["--json"]].concat()));
    assert_eq!(value["state"]["reservations"], serde_json::json!([]));
    assert_eq!(value["state"]["default_tariffs"], serde_json::json!([]));
    assert_eq!(value["state"]["charging_profiles"], serde_json::json!([]));
}

#[test]
fn v201_show_and_reset_never_touch_the_1_6_file() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_str().unwrap();
    let v16 = file_of(dir.path(), "CP-9");
    std::fs::write(&v16, "not the 2.x file").unwrap();
    let secrets16 = add_secrets(dir.path(), "CP-9");

    // Only the 1.6 files exist: the 2.x commands see nothing.
    let args = |action: &'static str| {
        [
            "state",
            action,
            "--state-dir",
            state,
            "--id",
            "CP-9",
            "--protocol",
            "ocpp2.0.1",
        ]
    };
    let output = run(&args("show"));
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("no saved state"),
        "{}",
        text(&output.stderr)
    );
    let output = run(&[&args("reset")[..], &["--yes"]].concat());
    assert_eq!(output.status.code(), Some(0));
    assert!(text(&output.stdout).contains("nothing to delete"));
    assert_eq!(std::fs::read_to_string(&v16).unwrap(), "not the 2.x file");
    assert!(secrets16.exists());

    // Both kinds exist: `reset` removes the 2.x files only.
    write_v201(dir.path(), "CP-9");
    let output = run(&[&args("reset")[..], &["--yes"]].concat());
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(!dir.path().join("CP-9.station2.json").exists());
    assert!(!dir.path().join("CP-9.station2.secrets.json").exists());
    assert!(v16.exists());
    assert!(secrets16.exists());

    // And the other way round: a default `reset` leaves the 2.x files alone.
    write_v201(dir.path(), "CP-9");
    let output = run(&[
        "state",
        "reset",
        "--state-dir",
        state,
        "--id",
        "CP-9",
        "--yes",
    ]);
    assert_eq!(output.status.code(), Some(0));
    assert!(!v16.exists());
    assert!(dir.path().join("CP-9.station2.json").exists());
    assert!(dir.path().join("CP-9.station2.secrets.json").exists());
}

#[test]
fn v201_show_refuses_a_newer_file_and_leaves_it_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_str().unwrap();
    let file = dir.path().join("CP-9.station2.json");
    let newer = r#"{"version": 99, "charge_point_id": "CP-9"}"#;
    std::fs::write(&file, newer).unwrap();
    let output = run(&[
        "state",
        "show",
        "--state-dir",
        state,
        "--id",
        "CP-9",
        "--protocol",
        "ocpp2.0.1",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("format version 99"),
        "{}",
        text(&output.stderr)
    );
    let output = run(&[
        "state",
        "reset",
        "--state-dir",
        state,
        "--id",
        "CP-9",
        "--protocol",
        "ocpp2.0.1",
        "--yes",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), newer);
}
