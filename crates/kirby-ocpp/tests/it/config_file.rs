//! Process-level tests of `--config`, `--profile` and `--print-config=toml`. Environment
//! variables cannot be set in-process, so the real binary runs with a controlled environment.

use std::path::Path;
use std::process::{Command, Output};

const ENV_VARS: &[&str] = &[
    "OCPP_URL",
    "OCPP_CHARGE_POINT_ID",
    "OCPP_VENDOR",
    "OCPP_MODEL",
    "OCPP_WS_SUBPROTOCOL",
    "OCPP_WS_PING_INTERVAL",
    "OCPP_RECONNECT",
    "OCPP_RECONNECT_INITIAL_MS",
    "OCPP_RECONNECT_MAX_MS",
    "OCPP_RECONNECT_MAX_ATTEMPTS",
    "OCPP_CALL_TIMEOUT_MS",
    "OCPP_TLS_CA_FILE",
    "OCPP_TLS_CERT_FILE",
    "OCPP_TLS_KEY_FILE",
    "OCPP_TLS_SERVER_NAME",
    "OCPP_TLS_SKIP_VERIFY",
    "OCPP_SHOW_SECRETS",
    "OCPP_SECURITY_PROFILE_FALLBACK",
    "OCPP_COUNT",
    "OCPP_ID_TEMPLATE",
    "OCPP_FIRST_INDEX",
    "OCPP_STAGGER_MS",
    "OCPP_SERIAL",
    "OCPP_FIRMWARE_VERSION",
    "KIRBY_OCPP_NO_PERSIST",
    "KIRBY_OCPP_STATE_DIR",
    "KIRBY_OCPP_RULES",
    "KIRBY_OCPP_CONFIG",
    "KIRBY_OCPP_PROFILE",
];

fn run(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kirby-ocpp"));
    for var in ENV_VARS {
        command.env_remove(var);
    }
    command
        .args(args)
        .envs(env.iter().copied())
        .output()
        .unwrap()
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

/// The effective configuration as TOML text; the command must succeed.
fn print(args: &[&str], env: &[(&str, &str)]) -> String {
    let mut all = args.to_vec();
    all.push("--print-config=toml");
    let output = run(&all, env);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    stdout(&output)
}

/// A usage failure: exit 2 and the error text.
fn rejected(args: &[&str], env: &[(&str, &str)]) -> String {
    let output = run(args, env);
    assert_eq!(output.status.code(), Some(2), "stdout: {}", stdout(&output));
    assert!(stdout(&output).is_empty(), "stdout: {}", stdout(&output));
    stderr(&output)
}

#[test]
fn flag_beats_env_beats_file_beats_default() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "c.toml",
        "[connection]\nurl = \"ws://file:1/x\"\n\
         [identity]\nid = \"FILE\"\nvendor = \"FILEV\"\nmodel = \"FILEM\"\n\
         [station]\nconnectors = 3\nheartbeat = 77\n\
         [tls]\ntls_skip_verify = true\n\
         [reconnect]\nreconnect_max_attempts = 9\n\
         [faults]\nrule = [\"Reset=reject\", \"GetConfiguration=ignore\"]\n",
    );
    // File only: every kind reads from the file.
    let text = print(&["--config", &file], &[]);
    for line in [
        "url = \"ws://file:1/x\"",
        "id = \"FILE\"",
        "connectors = 3",
        "heartbeat = 77",
        "tls_skip_verify = true",
        "reconnect_max_attempts = 9",
        "rule = [\"Reset=reject\", \"GetConfiguration=ignore\"]",
        // Not in the file: the built-in default.
        "ws_ping = 30",
        "phases = 1",
    ] {
        assert!(text.contains(line), "missing `{line}` in:\n{text}");
    }
    // Environment beats the file; a flag beats both.
    let env = [
        ("OCPP_VENDOR", "ENVV"),
        ("OCPP_MODEL", "ENVM"),
        ("OCPP_TLS_SKIP_VERIFY", "false"),
        ("OCPP_RECONNECT_MAX_ATTEMPTS", "4"),
        ("KIRBY_OCPP_RULES", "UnlockConnector=drop"),
    ];
    let text = print(
        &["--config", &file, "--model", "FLAGM", "--connectors", "5"],
        &env,
    );
    for line in [
        "vendor = \"ENVV\"",
        "model = \"FLAGM\"",
        "id = \"FILE\"",
        "connectors = 5",
        "heartbeat = 77",
        "tls_skip_verify = false",
        "reconnect_max_attempts = 4",
        "rule = [\"UnlockConnector=drop\"]",
    ] {
        assert!(text.contains(line), "missing `{line}` in:\n{text}");
    }
    // A flag replaces the list of the file as a whole.
    let text = print(
        &["--config", &file, "--rule", "Reset=drop"],
        &[("KIRBY_OCPP_RULES", "UnlockConnector=drop")],
    );
    assert!(text.contains("rule = [\"Reset=drop\"]"), "{text}");
}

#[test]
fn config_and_profile_come_from_the_environment_too() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "c.toml",
        "[identity]\nid = \"TOP\"\n[profile.lab.identity]\nid = \"LAB\"\n",
    );
    let text = print(
        &["--url", "ws://h:1/x"],
        &[("KIRBY_OCPP_CONFIG", &file), ("KIRBY_OCPP_PROFILE", "lab")],
    );
    assert!(text.contains("id = \"LAB\""), "{text}");
}

#[test]
fn config_flag_works_after_a_subcommand() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "c.toml", "[identity]\nvendor = \"ACME\"\n");
    let text = print(
        &[
            "--url",
            "ws://h:1/x",
            "run",
            "--scenario",
            "charge-session",
            "--config",
            &file,
        ],
        &[],
    );
    assert!(text.contains("vendor = \"ACME\""), "{text}");
}

#[test]
fn print_config_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "c.toml",
        "schema = 1\nprotocol = \"ocpp1.6\"\n\
         [connection]\nurl = \"ws://h:1/x\"\n\
         [identity]\nid = \"CP-{n:03}\"\nserial = \"S\\\"1\"\n\
         [station]\nstate_dir = \"state\"\nno_persist = true\n\
         [tls]\ntls_ca = \"certs/ca.pem\"\n\
         [fleet]\ncount = 4\nid_template = \"X-{n}\"\nstagger_ms = 10\n\
         [faults]\nrule = [\"Reset=delay:50*2\"]\n\
         [output]\nverbose = true\ncolor = \"never\"\n\
         [profile.p.reconnect]\nno_reconnect = true\n",
    );
    let first = print(&["--config", &file, "--profile", "p"], &[]);
    let copy = write(dir.path(), "effective.toml", &first);
    let second = print(&["--config", &copy], &[]);
    assert_eq!(first, second);
    // The copy sits in the same directory, but paths are printed absolute anyway.
    let nested = dir.path().join("sub");
    std::fs::create_dir(&nested).unwrap();
    let moved = write(&nested, "moved.toml", &first);
    assert_eq!(print(&["--config", &moved], &[]), first);
}

#[test]
fn relative_paths_resolve_against_the_file_directory() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "c.toml",
        "[station]\nstate_dir = \"state\"\n[tls]\ntls_ca = \"certs/ca.pem\"\n[output]\nlog_file = \"x.log\"\n",
    );
    let text = print(&["--config", &file, "--url", "ws://h:1/x"], &[]);
    let base = std::path::absolute(dir.path()).unwrap();
    for (key, rest) in [
        ("state_dir", "state"),
        ("tls_ca", "certs/ca.pem"),
        ("log_file", "x.log"),
    ] {
        let expected = base.join(rest);
        let line = format!("{key} = {:?}", expected.to_str().unwrap());
        assert!(text.contains(&line), "missing `{line}` in:\n{text}");
    }
}

#[test]
fn secrets_never_appear_in_printed_config() {
    let dir = tempfile::tempdir().unwrap();
    let key = write(dir.path(), "key.pem", "SECRET-KEY-MATERIAL");
    let output = run(
        &[
            "--url",
            "ws://cp:hunter2@h:1/x",
            "--tls-key",
            &key,
            "--tls-cert",
            &key,
            "--print-config=toml",
        ],
        &[],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(!text.contains("hunter2"), "{text}");
    assert!(!text.contains("SECRET-KEY-MATERIAL"), "{text}");
    assert!(text.contains("url = \"ws://cp@h:1/x\""), "{text}");
    assert!(text.contains("password is not printed"), "{text}");
    // The printed file is accepted again.
    let copy = write(dir.path(), "effective.toml", &text);
    let again = print(&["--config", &copy], &[]);
    // Only the comment about the dropped password differs.
    let body = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|line| !line.starts_with('#'))
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(body(&again), body(&text));
}

#[test]
fn inline_url_password_in_the_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "c.toml",
        "[connection]\nurl = \"ws://cp:hunter2@h:1/x\"\n",
    );
    let message = rejected(&["--config", &file, "--print-config=toml"], &[]);
    assert!(message.contains("connection.url"), "{message}");
    assert!(message.contains(&file), "{message}");
    assert!(!message.contains("hunter2"), "{message}");
}

#[test]
fn profile_selection() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "c.toml",
        "[connection]\nurl = \"ws://top:1/x\"\n[identity]\nvendor = \"TOPV\"\nmodel = \"TOPM\"\n\
         [profile.a.connection]\nurl = \"ws://a:1/x\"\n\
         [profile.b.identity]\nmodel = \"BM\"\n",
    );
    let top = print(&["--config", &file], &[]);
    assert!(top.contains("url = \"ws://top:1/x\""), "{top}");
    let a = print(&["--config", &file, "--profile", "a"], &[]);
    assert!(a.contains("url = \"ws://a:1/x\""), "{a}");
    assert!(a.contains("model = \"TOPM\""), "{a}");
    let b = print(&["--config", &file, "--profile", "b"], &[]);
    assert!(b.contains("url = \"ws://top:1/x\""), "{b}");
    assert!(b.contains("model = \"BM\""), "{b}");
    assert!(!b.contains("[profile"), "{b}");
    // A flag still wins over the profile.
    let flag = print(
        &["--config", &file, "--profile", "a", "--url", "ws://f:1/x"],
        &[],
    );
    assert!(flag.contains("url = \"ws://f:1/x\""), "{flag}");
}

#[test]
fn unknown_profile_and_missing_config_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "c.toml",
        "[profile.a.identity]\nid = \"A\"\n[profile.b.identity]\nid = \"B\"\n",
    );
    let message = rejected(
        &["--config", &file, "--profile", "zzz", "--url", "ws://h:1/x"],
        &[],
    );
    assert!(message.contains("unknown profile `zzz`"), "{message}");
    assert!(message.contains("available profiles: a, b"), "{message}");
    let message = rejected(&["--profile", "a", "--url", "ws://h:1/x"], &[]);
    assert!(message.contains("--profile"), "{message}");
    assert!(message.contains("--config"), "{message}");
}

/// `suite` arguments are per invocation: no key of a configuration file stands for them.
#[test]
fn suite_arguments_have_no_configuration_keys() {
    let dir = tempfile::tempdir().unwrap();
    for key in [
        "filter",
        "skip",
        "tag",
        "skip_tag",
        "retries",
        "jobs",
        "fail_fast",
        "summary_md",
        "github_annotations",
    ] {
        let file = write(dir.path(), &format!("{key}.toml"), &format!("{key} = 1\n"));
        let message = rejected(&["--config", &file, "--url", "ws://h:1/x"], &[]);
        assert!(
            message.contains(&format!("unknown key `{key}`")),
            "{key}: {message}"
        );
        let nested = write(
            dir.path(),
            &format!("{key}-run.toml"),
            &format!("[run]\n{key} = 1\n"),
        );
        let message = rejected(&["--config", &nested, "--url", "ws://h:1/x"], &[]);
        assert!(message.contains("unknown table `run`"), "{key}: {message}");
    }
}

#[test]
fn file_errors_name_the_file_and_the_key_and_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    let cases: &[(&str, &str)] = &[
        (
            "[connection]\nurll = \"x\"\n",
            "unknown key `connection.urll`",
        ),
        ("[nope]\na = 1\n", "unknown table `nope`"),
        ("url = \"ws://h:1/x\"\n", "unknown key `url`"),
        ("ocpp_version = \"1.6\"\n", "unknown key `ocpp_version`"),
        (
            "[station]\nconnectors = \"2\"\n",
            "key `station.connectors`: expected an integer",
        ),
        (
            "[faults]\nrule = \"x\"\n",
            "key `faults.rule`: expected an array of strings",
        ),
        ("schema = 2\n", "version 2 is not supported"),
        ("protocol = \"ocpp2.0.1\"\n", "`ocpp2.0.1` is not supported"),
        ("[connection\n", "line 1"),
        (
            "[profile.x.identity]\nbogus = 1\n",
            "unknown key `profile.x.identity.bogus`",
        ),
        (
            "[profile.x]\nprotocol = \"ocpp1.6\"\n",
            "only valid at the top level",
        ),
    ];
    for (index, (text, expected)) in cases.iter().enumerate() {
        let file = write(dir.path(), &format!("bad{index}.toml"), text);
        let message = rejected(&["--config", &file, "--url", "ws://h:1/x"], &[]);
        assert!(message.contains(expected), "{text}: {message}");
        assert!(message.contains(&file), "{text}: {message}");
    }
    let missing = dir.path().join("missing.toml");
    let message = rejected(
        &["--config", missing.to_str().unwrap(), "--url", "ws://h:1/x"],
        &[],
    );
    assert!(message.contains("missing.toml"), "{message}");
    let big = write(
        dir.path(),
        "big.toml",
        &format!("# {}\n", "x".repeat(1024 * 1024 + 8)),
    );
    let message = rejected(&["--config", &big, "--url", "ws://h:1/x"], &[]);
    assert!(message.contains("larger than"), "{message}");
}

#[test]
fn a_value_the_flag_refuses_is_refused_from_the_file_with_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "c.toml", "[fleet]\ncount = 0\n");
    let message = rejected(&["--config", &file, "--url", "ws://h:1/x"], &[]);
    assert!(message.contains("--count"), "{message}");
    assert!(message.contains("`fleet.count`"), "{message}");
    // The same value on the command line has no file note.
    let message = rejected(&["--count", "0", "--url", "ws://h:1/x"], &[]);
    assert!(!message.contains("configuration file"), "{message}");
}

#[test]
fn file_values_never_cause_flag_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "c.toml",
        "[output]\nverbose = true\n[station]\nno_persist = true\n[reconnect]\nno_reconnect = true\n",
    );
    let text = print(&["--config", &file, "--quiet", "--url", "ws://h:1/x"], &[]);
    assert!(text.contains("quiet = true"), "{text}");
    assert!(text.contains("verbose = false"), "{text}");
    let text = print(
        &[
            "--config",
            &file,
            "--url",
            "ws://h:1/x",
            "run",
            "--scenario",
            "charge-session",
            "--persist",
        ],
        &[],
    );
    assert!(text.contains("no_persist = false"), "{text}");
    let text = print(
        &[
            "--config",
            &file,
            "--url",
            "ws://h:1/x",
            "--reconnect",
            "true",
        ],
        &[],
    );
    assert!(text.contains("no_reconnect = false"), "{text}");
    assert!(text.contains("reconnect = true"), "{text}");
}

#[test]
fn offline_subcommands_ignore_the_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "c.toml", "[nope]\n");
    for args in [["version"], ["scenarios"]] {
        let mut all = vec!["--config", file.as_str()];
        all.extend_from_slice(&args);
        if args[0] == "scenarios" {
            all.push("list");
        }
        let output = run(&all, &[]);
        assert!(output.status.success(), "{args:?}: {}", stderr(&output));
    }
}

#[test]
fn json_print_config_applies_the_file_and_keeps_its_shape() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "c.toml",
        "[identity]\nid = \"FROMFILE\"\nvendor = \"V\"\n",
    );
    let output = run(
        &["--config", &file, "--url", "ws://h:1/x", "--print-config"],
        &[],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["charge_point_id"], "FROMFILE");
    assert_eq!(value["vendor"], "V");
    let explicit = run(&["--url", "ws://h:1/x", "--print-config=json"], &[]);
    assert!(explicit.status.success());
    let bad = run(&["--url", "ws://h:1/x", "--print-config=yaml"], &[]);
    assert_eq!(bad.status.code(), Some(2));
}
