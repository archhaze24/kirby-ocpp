//! Process-level tests of `--config`, `--profile`, `--print-config` and `--check-config` of
//! `kirby-csms`. Environment variables cannot be set in-process, so the real binary runs with
//! a controlled environment.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use kirby_ocpp_core::ids::IdTag;
use kirby_ocpp_core::station::{Command as StationCommand, Snapshot};
use kirby_ocpp_core::{Station, StationConfig, StationHandle, SystemClock};
use serde_json::Value;

const WAIT: Duration = Duration::from_secs(30);

const ENV_VARS: &[&str] = &[
    "KIRBY_CSMS_LISTEN",
    "KIRBY_CSMS_API_LISTEN",
    "KIRBY_CSMS_API_TOKEN",
    "KIRBY_CSMS_API_TOKEN_FILE",
    "KIRBY_CSMS_TLS_CERT",
    "KIRBY_CSMS_TLS_KEY",
    "KIRBY_CSMS_TLS_CLIENT_CA",
    "KIRBY_CSMS_SUBPROTOCOL",
    "KIRBY_CSMS_TRANSACTION_IDS",
    "KIRBY_CSMS_HEARTBEAT_INTERVAL",
    "KIRBY_CSMS_HISTORY_CAP",
    "KIRBY_CSMS_SHOW_SECRETS",
    "KIRBY_CSMS_VALIDATE_INBOUND",
    "KIRBY_CSMS_PING_INTERVAL",
    "KIRBY_CSMS_PONG_TIMEOUT",
    "KIRBY_CSMS_TRANSACTION_HISTORY_CAP",
    "KIRBY_CSMS_CALL_TIMEOUT",
    "KIRBY_CSMS_JSON",
    "KIRBY_CSMS_LOG_FILE",
    "KIRBY_CSMS_STDIN",
    "KIRBY_CSMS_SCRIPT",
    "KIRBY_CSMS_NO_TUI",
    "KIRBY_CSMS_SECURITY_PROFILE",
    "KIRBY_CSMS_AUTH_KEY",
    "KIRBY_CSMS_AUTH_KEYS_FILE",
    "KIRBY_CSMS_DEFAULT_AUTH_KEY",
    "KIRBY_CSMS_DEFAULT_AUTH_KEY_FILE",
    "KIRBY_CSMS_CONFIG",
    "KIRBY_CSMS_PROFILE",
];

fn base_command(args: &[&str], env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kirby-csms"));
    for var in ENV_VARS {
        command.env_remove(var);
    }
    command.args(args).envs(env.iter().copied());
    command
}

fn run(args: &[&str], env: &[(&str, &str)]) -> Output {
    base_command(args, env)
        .stdin(Stdio::null())
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

/// `text` as a TOML basic string (backslashes of Windows paths escaped).
fn toml_str(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\"))
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

fn check(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut all = args.to_vec();
    all.push("--check-config");
    run(&all, env)
}

#[test]
fn precedence_is_flag_env_profile_file_default() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "csms.toml",
        "
[behavior]
heartbeat_interval = 111
call_timeout = 22
history_cap = 33

[profile.fast.behavior]
heartbeat_interval = 44
call_timeout = 55
",
    );
    let plain = print(&["--config", &file], &[]);
    assert!(plain.contains("heartbeat_interval = 111"), "{plain}");
    assert!(plain.contains("call_timeout = 22"), "{plain}");
    assert!(plain.contains("history_cap = 33"), "{plain}");
    // A default shows when nothing sets it.
    assert!(plain.contains("ping_interval = 30"), "{plain}");

    let profile = print(&["--config", &file, "--profile", "fast"], &[]);
    assert!(profile.contains("heartbeat_interval = 44"), "{profile}");
    assert!(profile.contains("call_timeout = 55"), "{profile}");
    assert!(profile.contains("history_cap = 33"), "{profile}");

    let env = print(
        &["--config", &file, "--profile", "fast"],
        &[("KIRBY_CSMS_HEARTBEAT_INTERVAL", "66")],
    );
    assert!(env.contains("heartbeat_interval = 66"), "{env}");
    assert!(env.contains("call_timeout = 55"), "{env}");

    let flag = print(
        &[
            "--config",
            &file,
            "--profile",
            "fast",
            "--heartbeat-interval",
            "77",
        ],
        &[("KIRBY_CSMS_HEARTBEAT_INTERVAL", "66")],
    );
    assert!(flag.contains("heartbeat_interval = 77"), "{flag}");

    // The file and the profile also come from the environment.
    let via_env = print(
        &[],
        &[("KIRBY_CSMS_CONFIG", &file), ("KIRBY_CSMS_PROFILE", "fast")],
    );
    assert!(via_env.contains("heartbeat_interval = 44"), "{via_env}");
}

#[test]
fn print_config_round_trips_and_never_prints_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let keys = write(dir.path(), "keys.txt", "CP-1=filekeysecret\n");
    let file = write(
        dir.path(),
        "csms.toml",
        r#"
schema = 1
protocol = "ocpp1.6"

[listener]
listen = "127.0.0.1:9123"
subprotocol = "select:ocpp1.6"

[security]
security_profile = 1
auth_keys_file = "keys.txt"
default_auth_key_file = "default.key"

[behavior]
validate_inbound = false
transaction_ids = "incrementing:7"

[output]
json = true

[[station]]
id = "CP-A"
auth_key_file = "a.key"

[[rule]]
action = "Authorize"
behavior = "respond"
payload = '{"idTagInfo":{"status":"Blocked"}}'

[[rule]]
action = "Heartbeat"
behavior = "call_error"
times = 3

[[rule]]
action = "Authorize"
station = "CP-A"
protocol = "ocpp1.6"
behavior = "respond"
payload = '{"idTagInfo":{"status":"Expired"}}'

[[rule]]
action = "StartTransaction"
station = "CP-A"
behavior = "silence"
times = 2
"#,
    );
    // Neither the keys file nor the key files exist (`default.key`, `a.key`) or matter:
    // printing never reads them.
    let first = print(
        &[
            "--config",
            &file,
            "--api-listen",
            "127.0.0.1:0",
            "--api-token",
            "tokensecret",
        ],
        &[
            ("KIRBY_CSMS_AUTH_KEY", "CP-ENV=envkeysecret"),
            ("KIRBY_CSMS_DEFAULT_AUTH_KEY_FILE", &keys),
        ],
    );
    for secret in ["tokensecret", "envkeysecret", "filekeysecret"] {
        assert!(!first.contains(secret), "{secret} leaked:\n{first}");
    }
    assert!(first.contains("Inline secrets are not printed"), "{first}");
    assert!(first.contains("[[station]]"), "{first}");
    assert!(first.contains("[[rule]]"), "{first}");
    assert!(first.contains("times = 3"), "{first}");
    assert!(
        first.contains("station = \"CP-A\"\nprotocol = \"ocpp1.6\""),
        "{first}"
    );
    assert!(
        first.contains("transaction_ids = \"incrementing:7\""),
        "{first}"
    );
    assert!(first.contains("validate_inbound = false"), "{first}");
    let keys_path = dir.path().join("keys.txt");
    assert!(
        first.contains(&format!(
            "auth_keys_file = {}",
            toml_str(keys_path.to_str().unwrap())
        )),
        "{first}"
    );

    // The printed file is valid input and printing it again gives the same text.
    let printed = write(dir.path(), "effective.toml", &first);
    // The first print had flag values (API) that are not in the file; print the file as is.
    let again = print(&["--config", &printed], &[]);
    let third = write(dir.path(), "effective2.toml", &again);
    let fixed = print(&["--config", &third], &[]);
    assert_eq!(fixed, again);

    // A file with a profile prints without profile tables.
    let with_profile = write(
        dir.path(),
        "p.toml",
        "[behavior]\nhistory_cap = 1\n[profile.x.behavior]\nhistory_cap = 2\n",
    );
    let chosen = print(&["--config", &with_profile, "--profile", "x"], &[]);
    assert!(chosen.contains("history_cap = 2"), "{chosen}");
    assert!(!chosen.contains("[profile"), "{chosen}");
}

#[test]
fn print_config_json_is_the_default_format() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "csms.toml",
        "[behavior]\nhistory_cap = 5\n[[rule]]\naction = \"Authorize\"\nbehavior = \"silence\"\n",
    );
    for args in [
        vec!["--config", file.as_str(), "--print-config"],
        vec!["--config", file.as_str(), "--print-config=json"],
    ] {
        let output = run(&args, &[]);
        assert!(output.status.success(), "{}", stderr(&output));
        let value: Value = serde_json::from_str(&stdout(&output)).unwrap();
        assert_eq!(value["behavior"]["history_cap"], 5);
        assert_eq!(value["protocol"], "ocpp1.6");
        assert_eq!(value["rule"][0]["behavior"], "silence");
        assert_eq!(value["rule"][0]["times"], 1);
    }
}

#[test]
fn check_config_reports_ok_and_does_not_bind() {
    let dir = tempfile::tempdir().unwrap();
    // The address is in use: a real start would fail to bind, the check must not try.
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = taken.local_addr().unwrap().to_string();
    let file = write(
        dir.path(),
        "csms.toml",
        &format!("[listener]\nlisten = \"{addr}\"\n\n[profile.p.behavior]\nhistory_cap = 9\n"),
    );
    let ok = check(&["--config", &file], &[]);
    assert_eq!(ok.status.code(), Some(0), "{}", stderr(&ok));
    let text = stdout(&ok);
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.starts_with("OK: "), "{text}");
    assert!(text.contains("csms.toml"), "{text}");

    let with_profile = check(&["--config", &file, "--profile", "p"], &[]);
    assert_eq!(with_profile.status.code(), Some(0));
    assert!(stdout(&with_profile).contains("(profile p)"));

    // Without a file the flags are checked.
    let flags = check(&["--listen", "127.0.0.1:0"], &[]);
    assert_eq!(flags.status.code(), Some(0));
    assert!(stdout(&flags).contains("no configuration file"));
    let bad = rejected(&["--security-profile", "1", "--check-config"], &[]);
    assert!(bad.contains("--security-profile 1 needs"), "{bad}");

    // Combination errors that depend on the file's values.
    let tls_only = write(dir.path(), "tls.toml", "[security]\nsecurity_profile = 2\n");
    let message = rejected(&["--config", &tls_only, "--check-config"], &[]);
    assert!(message.contains("--security-profile 2"), "{message}");

    // A key file that is missing fails the check, which reads them.
    let missing = write(
        dir.path(),
        "missing.toml",
        "[security]\nsecurity_profile = 1\ndefault_auth_key_file = \"nope.key\"\n",
    );
    let message = rejected(&["--config", &missing, "--check-config"], &[]);
    assert!(message.contains("nope.key"), "{message}");

    // The two options exclude each other.
    let both = run(&["--print-config", "--check-config"], &[]);
    assert_eq!(both.status.code(), Some(2));
}

#[test]
fn file_errors_exit_2_with_the_file_and_the_key_path() {
    let dir = tempfile::tempdir().unwrap();
    let cases: &[(&str, &[&str])] = &[
        (
            "[behavior]\nhistory_capp = 1\n",
            &["unknown key `behavior.history_capp`", "history_cap"],
        ),
        ("[nope]\nx = 1\n", &["unknown table `nope`"]),
        (
            "history_cap = 1\n",
            &["unknown key `history_cap`", "station"],
        ),
        ("schema = 2\n", &["key `schema`", "not supported"]),
        ("protocol = \"ocpp2.0.1\"\n", &["key `protocol`", "ocpp1.6"]),
        (
            "[behavior]\nhistory_cap = \"x\"\n",
            &["key `behavior.history_cap`", "expected an integer"],
        ),
        (
            "[[profile.p.station]]\nauth_key_file = \"k\"\n",
            &["`profile.p.station[0]`", "`id` is required"],
        ),
        ("[listener\n", &["line 1"]),
    ];
    for (index, (text, needles)) in cases.iter().enumerate() {
        let file = write(dir.path(), &format!("case{index}.toml"), text);
        let message = rejected(&["--config", &file, "--check-config"], &[]);
        assert!(message.contains(&format!("case{index}.toml")), "{message}");
        for needle in *needles {
            assert!(message.contains(needle), "{needle}: {message}");
        }
    }
    let missing = rejected(&["--config", "/nonexistent/csms.toml"], &[]);
    assert!(missing.contains("/nonexistent/csms.toml"), "{missing}");

    let profile = write(
        dir.path(),
        "profiles.toml",
        "[profile.a.behavior]\nhistory_cap = 1\n",
    );
    let message = rejected(
        &["--config", &profile, "--profile", "zzz", "--check-config"],
        &[],
    );
    assert!(message.contains("unknown profile `zzz`"), "{message}");
    assert!(message.contains("available profiles: a"), "{message}");

    let message = rejected(&["--profile", "a", "--check-config"], &[]);
    assert!(message.contains("--profile"), "{message}");
    assert!(message.contains("--config"), "{message}");

    // A value the flag would refuse is refused with a pointer to the key.
    let range = write(
        dir.path(),
        "range.toml",
        "[behavior]\nheartbeat_interval = 0\n",
    );
    let message = rejected(&["--config", &range, "--check-config"], &[]);
    assert!(message.contains("--heartbeat-interval"), "{message}");
    assert!(message.contains("behavior.heartbeat_interval"), "{message}");
}

#[test]
fn inline_secrets_are_refused_without_echoing_them() {
    let dir = tempfile::tempdir().unwrap();
    for (table, key, alternative) in [
        ("api", "api_token", "api_token_file"),
        ("security", "auth_key", "auth_keys_file"),
        ("security", "default_auth_key", "default_auth_key_file"),
    ] {
        let text = format!("[{table}]\n{key} = \"hunter2hunter2\"\n");
        let file = write(dir.path(), &format!("{key}.toml"), &text);
        let message = rejected(&["--config", &file, "--check-config"], &[]);
        assert!(
            message.contains(&format!("key `{table}.{key}`")),
            "{message}"
        );
        assert!(message.contains(alternative), "{message}");
        assert!(!message.contains("hunter2"), "{message}");
        // The same inside a profile.
        let text = format!("[profile.p.{table}]\n{key} = \"hunter2hunter2\"\n");
        let file = write(dir.path(), &format!("p_{key}.toml"), &text);
        let message = rejected(&["--config", &file, "--check-config"], &[]);
        assert!(
            message.contains(&format!("key `profile.p.{table}.{key}`")),
            "{message}"
        );
        assert!(!message.contains("hunter2"), "{message}");
    }
    // Block secrets as well.
    let file = write(
        dir.path(),
        "station.toml",
        "[[station]]\nid = \"CP-1\"\nauth_key = \"hunter2hunter2\"\n",
    );
    let message = rejected(&["--config", &file, "--check-config"], &[]);
    assert!(
        message.contains("unknown key `station[0].auth_key`"),
        "{message}"
    );
    assert!(message.contains("auth_key_file"), "{message}");
    assert!(!message.contains("hunter2"), "{message}");
}

#[test]
fn block_errors_name_the_element() {
    let dir = tempfile::tempdir().unwrap();
    let cases: &[(&str, &[&str])] = &[
        (
            "[[station]]\nauth_key_file = \"k\"\n",
            &["`station[0]`", "`id` is required"],
        ),
        (
            "[[station]]\nid = \"A\"\n[[station]]\nid = \"A\"\n",
            &["`station[1]`", "repeats"],
        ),
        (
            "[[station]]\nid = \"CP-*\"\n",
            &["`station[0]`", "patterns are not supported"],
        ),
        (
            "[[station]]\nid = \"A\"\nprotocol = \"ocpp2.0.1\"\n",
            &["station[0].protocol", "ocpp1.6"],
        ),
        (
            "[[station]]\nid = \"A\"\nrole = \"x\"\n",
            &["unknown key `station[0].role`"],
        ),
        (
            "[[rule]]\naction = \"Authorize\"\n",
            &["`rule[0]`", "`behavior` is required"],
        ),
        (
            "[[rule]]\naction = \"A\"\nbehavior = \"explode\"\n",
            &["`rule[0]`", "respond, call_error"],
        ),
        (
            "[[rule]]\naction = \"A\"\nbehavior = \"respond\"\n",
            &["needs a `payload`"],
        ),
        (
            "[[rule]]\naction = \"A\"\nbehavior = \"respond\"\npayload = \"{\"\n",
            &["not valid JSON"],
        ),
        (
            "[[rule]]\naction = \"A\"\nbehavior = \"respond\"\npayload = \"[]\"\n",
            &["must be a JSON object"],
        ),
        (
            "[[rule]]\naction = \"A\"\nbehavior = \"silence\"\npayload = \"{}\"\n",
            &["takes no `payload`"],
        ),
        (
            "[[rule]]\naction = \"A\"\nbehavior = \"respond\"\npayload = \"{}\"\ntimes = 2\n",
            &["faults only"],
        ),
        (
            "[[rule]]\naction = \"A\"\nbehavior = \"close\"\ntimes = 0\n",
            &["`times` must be 1 to"],
        ),
        (
            "[[rule]]\naction = \"\"\nbehavior = \"close\"\n",
            &["`action` must not be empty"],
        ),
        (
            "[[rule]]\naction = \"A\"\nbehavior = \"close\"\nstation = \"CP-*\"\n",
            &["`rule[0]`", "patterns are not supported yet"],
        ),
        (
            "[[rule]]\naction = \"A\"\nbehavior = \"close\"\nstation = \"\"\n",
            &["`rule[0]`", "`station` must not be empty"],
        ),
        (
            "[[rule]]\naction = \"Heartbeat\"\nbehavior = \"respond\"\npayload = \"{}\"\nstation = \"CP-1\"\nprotocol = \"ocpp2.0.1\"\n",
            &["key `rule[0].protocol`"],
        ),
        (
            "[[rule]]\naction = \"Reset\"\nbehavior = \"respond\"\npayload = \"{}\"\nstation = \"CP-1\"\n",
            &["`rule[0]`", "would never apply"],
        ),
        ("rule = 3\n", &["key `rule`: expected an array of tables"]),
    ];
    for (index, (text, needles)) in cases.iter().enumerate() {
        let file = write(dir.path(), &format!("b{index}.toml"), text);
        let message = rejected(&["--config", &file, "--check-config"], &[]);
        assert!(message.contains(&format!("b{index}.toml")), "{message}");
        for needle in *needles {
            assert!(message.contains(needle), "{needle}: {message}");
        }
    }
}

#[test]
fn flags_that_need_each_other_may_be_satisfied_by_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let token = write(dir.path(), "token", "tok\n");
    let file = write(
        dir.path(),
        "csms.toml",
        "[api]\napi_listen = \"127.0.0.1:0\"\n[tls]\ntls_key = \"key.pem\"\n",
    );
    // `--api-token-file` needs `--api-listen`, and `--tls-cert` needs `--tls-key`: both of
    // the missing halves are in the file.
    let printed = print(
        &[
            "--config",
            &file,
            "--api-token-file",
            &token,
            "--tls-cert",
            "cert.pem",
        ],
        &[],
    );
    assert!(printed.contains("api_token_file"), "{printed}");
    assert!(printed.contains("api_listen"), "{printed}");
    assert!(printed.contains("tls_cert"), "{printed}");
    assert!(printed.contains("tls_key"), "{printed}");
    // Still an error when nothing supplies the other half.
    let output = run(&["--tls-cert", "cert.pem", "--print-config"], &[]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn a_set_token_variable_wins_over_a_token_file_of_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let token = write(dir.path(), "token", "tok\n");
    let file = write(
        dir.path(),
        "csms.toml",
        &format!(
            "[api]\napi_listen = \"127.0.0.1:0\"\napi_token_file = {}\n",
            toml_str(&token)
        ),
    );
    let printed = print(&["--config", &file], &[("KIRBY_CSMS_API_TOKEN", "fromenv")]);
    assert!(!printed.contains("api_token_file ="), "{printed}");
    assert!(!printed.contains("fromenv"), "{printed}");
}

#[test]
fn relative_paths_resolve_against_the_file_directory() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("conf");
    std::fs::create_dir(&nested).unwrap();
    let file = write(
        &nested,
        "csms.toml",
        "[output]\nlog_file = \"events.log\"\n[security]\nauth_keys_file = \"keys/all.txt\"\nsecurity_profile = 1\n[[station]]\nid = \"CP\"\nauth_key_file = \"cp.key\"\n",
    );
    let printed = print(&["--config", &file], &[]);
    let line = |key: &str, rest: &str| {
        // Printed paths are normalized by `std::path::absolute` (backslashes on Windows).
        let joined = std::path::absolute(nested.join(rest)).unwrap();
        format!("{key} = {}", toml_str(joined.to_str().unwrap()))
    };
    assert!(
        printed.contains(&line("log_file", "events.log")),
        "{printed}"
    );
    assert!(
        printed.contains(&line("auth_keys_file", "keys/all.txt")),
        "{printed}"
    );
    assert!(
        printed.contains(&line("auth_key_file", "cp.key")),
        "{printed}"
    );
    // A path given by flag keeps resolving against the working directory.
    let flagged = print(&["--config", &file, "--log-file", "here.log"], &[]);
    let cwd = std::env::current_dir().unwrap();
    let here = format!(
        "log_file = {}",
        toml_str(
            std::path::absolute(cwd.join("here.log"))
                .unwrap()
                .to_str()
                .unwrap()
        )
    );
    assert!(flagged.contains(&here), "{flagged}");
}

/// A running `kirby-csms` (JSON mode); killed on drop.
struct Server {
    child: Child,
    lines: Receiver<String>,
    seen: Vec<String>,
    addr: String,
}

impl Server {
    fn start(config: &str, extra: &[&str]) -> Self {
        let mut args = vec!["--config", config, "--json"];
        args.extend_from_slice(extra);
        let mut child = base_command(&args, &[])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut server = Self {
            child,
            lines,
            seen: Vec::new(),
            addr: String::new(),
        };
        let listening = server.wait(|v| v["type"] == "listening");
        listening["addr"]
            .as_str()
            .unwrap()
            .clone_into(&mut server.addr);
        server
    }

    fn wait(&mut self, pred: impl Fn(&Value) -> bool) -> Value {
        loop {
            match self.lines.recv_timeout(WAIT) {
                Ok(line) => {
                    self.seen.push(line.clone());
                    if let Ok(value) = serde_json::from_str::<Value>(&line)
                        && pred(&value)
                    {
                        return value;
                    }
                }
                Err(error) => panic!("no matching line ({error:?}): {:#?}", self.seen),
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn booted(handle: &StationHandle) {
    let mut snapshots = handle.watch_snapshot();
    tokio::time::timeout(WAIT, snapshots.wait_for(Snapshot::booted))
        .await
        .expect("station did not boot")
        .unwrap();
}

async fn station(addr: &str, id: &str, key: Option<&str>) -> StationHandle {
    let mut config = StationConfig::new(format!("ws://{addr}/ocpp")).with_charge_point_id(id);
    config.persist = false;
    config.reconnect.enabled = false;
    let (handle, _join) = Station::spawn(config, Arc::new(SystemClock));
    if let Some(key) = key {
        for (name, value) in [("AuthorizationKey", key), ("SecurityProfile", "1")] {
            handle
                .send(StationCommand::SetConfiguration {
                    key: name.to_owned(),
                    value: value.to_owned(),
                })
                .await
                .unwrap();
        }
    }
    handle.connect().await.unwrap();
    handle
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rule_blocks_answer_stations_like_the_equivalent_commands() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "csms.toml",
        r#"
[listener]
listen = "127.0.0.1:0"

[[rule]]
action = "Authorize"
behavior = "call_error"

[[rule]]
action = "Authorize"
behavior = "respond"
payload = '{"idTagInfo":{"status":"Blocked"}}'
"#,
    );
    let mut server = Server::start(&file, &[]);
    let station = station(&server.addr.clone(), "CP-1", None).await;
    booted(&station).await;
    let tag = IdTag::new("TAG1").unwrap();
    // The fault wins once, then the response rule answers.
    let _ = station.authorize(tag.clone()).await;
    server.wait(|v| v["type"] == "frame" && v.to_string().contains("injected CALLERROR"));
    let _ = station.authorize(tag).await;
    server.wait(|v| v["type"] == "frame" && v.to_string().contains("Blocked"));
    let _ = station.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rule_blocks_with_a_station_answer_only_that_station() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "csms.toml",
        r#"
[listener]
listen = "127.0.0.1:0"

[[rule]]
action = "Authorize"
behavior = "respond"
payload = '{"idTagInfo":{"status":"Blocked"}}'
station = "CP-1"
protocol = "ocpp1.6"
"#,
    );
    let mut server = Server::start(&file, &[]);
    let addr = server.addr.clone();
    let tag = IdTag::new("TAG1").unwrap();
    let other = station(&addr, "CP-2", None).await;
    booted(&other).await;
    let _ = other.authorize(tag.clone()).await;
    server.wait(|v| {
        v["type"] == "frame"
            && v["station"] == "CP-2"
            && v["direction"] == "out"
            && v.to_string().contains("Accepted")
    });
    let ruled = station(&addr, "CP-1", None).await;
    booted(&ruled).await;
    let _ = ruled.authorize(tag).await;
    server.wait(|v| {
        v["type"] == "frame"
            && v["station"] == "CP-1"
            && v["direction"] == "out"
            && v.to_string().contains("Blocked")
    });
    let _ = other.shutdown().await;
    let _ = ruled.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn station_blocks_supply_basic_auth_keys_and_flags_win() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.key", "keyfora-0123456789\n");
    write(dir.path(), "b.key", "keyforb-0123456789\n");
    let file = write(
        dir.path(),
        "csms.toml",
        r#"
[listener]
listen = "127.0.0.1:0"

[security]
security_profile = 1

[[station]]
id = "CP-A"
auth_key_file = "a.key"
protocol = "ocpp1.6"

[[station]]
id = "CP-B"
auth_key_file = "b.key"
"#,
    );
    let mut server = Server::start(&file, &["--auth-key", "CP-B=flagkeyb-0123456789"]);
    let addr = server.addr.clone();

    let a = station(&addr, "CP-A", Some("keyfora-0123456789")).await;
    booted(&a).await;
    server.wait(|v| v["type"] == "connected" && v["station"] == "CP-A");
    let _ = a.shutdown().await;

    // The flag entry wins over the block of the same id.
    let b = station(&addr, "CP-B", Some("flagkeyb-0123456789")).await;
    booted(&b).await;
    server.wait(|v| v["type"] == "connected" && v["station"] == "CP-B");
    let _ = b.shutdown().await;

    let wrong = station(&addr, "CP-B", Some("keyforb-0123456789")).await;
    let failed = server.wait(|v| v["type"] == "auth_failed");
    assert_eq!(failed["station"], "CP-B");
    let _ = wrong.shutdown().await;
    let output = server.seen.join("\n");
    for secret in [
        "keyfora-0123456789",
        "keyforb-0123456789",
        "flagkeyb-0123456789",
    ] {
        assert!(!output.contains(secret), "{secret} leaked: {output}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn default_auth_key_file_works_like_the_default_key() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "default.key", "defaultkey-0123456789\n");
    let file = write(
        dir.path(),
        "csms.toml",
        "[listener]\nlisten = \"127.0.0.1:0\"\n[security]\nsecurity_profile = 1\ndefault_auth_key_file = \"default.key\"\n",
    );
    let mut server = Server::start(&file, &[]);
    let station = station(&server.addr.clone(), "CP-X", Some("defaultkey-0123456789")).await;
    booted(&station).await;
    server.wait(|v| v["type"] == "connected" && v["station"] == "CP-X");
    let _ = station.shutdown().await;
}

#[test]
fn default_auth_key_flag_and_file_exclude_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let key = write(dir.path(), "k", "abcdef\n");
    let output = run(
        &[
            "--security-profile",
            "1",
            "--default-auth-key",
            "abc",
            "--default-auth-key-file",
            &key,
            "--check-config",
        ],
        &[],
    );
    assert_eq!(output.status.code(), Some(2));
    let empty = write(dir.path(), "empty", "\n");
    let message = rejected(
        &[
            "--security-profile",
            "1",
            "--default-auth-key-file",
            &empty,
            "--check-config",
        ],
        &[],
    );
    assert!(message.contains("is empty"), "{message}");
    assert!(!message.contains("abcdef"), "{message}");
}
