//! Process-level tests: the real `kirby-csms` binary, real `kirby-ocpp-core` stations as clients.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use kirby_ocpp_core::ids::IdTag;
use kirby_ocpp_core::station::{Command as StationCommand, Snapshot};
use kirby_ocpp_core::{Station, StationConfig, StationHandle, SystemClock};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(30);

const ENV_VARS: &[&str] = &[
    "KIRBY_CSMS_LISTEN",
    "KIRBY_CSMS_TLS_CERT",
    "KIRBY_CSMS_TLS_KEY",
    "KIRBY_CSMS_SUBPROTOCOL",
    "KIRBY_CSMS_TRANSACTION_IDS",
    "KIRBY_CSMS_HEARTBEAT_INTERVAL",
    "KIRBY_CSMS_HISTORY_CAP",
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
    "KIRBY_CSMS_TLS_CLIENT_CA",
];

fn command(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kirby-csms"));
    for var in ENV_VARS {
        command.env_remove(var);
    }
    command.args(args);
    command
}

/// A running `kirby-csms`: stdout lines arrive on a channel.
struct Csms {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    seen: Vec<String>,
    stderr: Arc<Mutex<String>>,
    addr: String,
}

impl Csms {
    /// Starts it on an ephemeral port and waits for the `listening` line (JSON mode only).
    fn start(args: &[&str]) -> Self {
        let mut all = vec!["--listen", "127.0.0.1:0", "--json"];
        all.extend_from_slice(args);
        let mut child = command(&all)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&stderr);
        let pipe = child.stderr.take().unwrap();
        thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                if let Ok(mut text) = sink.lock() {
                    text.push_str(&line);
                    text.push('\n');
                }
            }
        });
        let mut csms = Self {
            child,
            stdin,
            lines,
            seen: Vec::new(),
            stderr,
            addr: String::new(),
        };
        let listening = csms.wait(|v| v["type"] == "listening");
        listening["addr"]
            .as_str()
            .unwrap()
            .clone_into(&mut csms.addr);
        csms
    }

    /// The next JSON line (from here on) that satisfies `pred`.
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
                Err(error) => panic!(
                    "no matching line ({error:?}); output so far: {:#?}\nstderr: {}",
                    self.seen,
                    self.stderr.lock().unwrap()
                ),
            }
        }
    }

    fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    fn send_raw(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    /// Waits for the process to end (stdout closes first) and returns its exit code.
    fn finish(mut self) -> (i32, Vec<String>, String) {
        self.stdin = None;
        loop {
            match self.lines.recv_timeout(WAIT) {
                Ok(line) => self.seen.push(line),
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => panic!("process did not exit: {:#?}", self.seen),
            }
        }
        let status = self.child.wait().unwrap();
        let stderr = self.stderr.lock().unwrap().clone();
        (status.code().unwrap_or(-1), self.seen.clone(), stderr)
    }
}

impl Drop for Csms {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A station connected to `addr` and booted.
async fn station(addr: &str, id: &str, tls: bool) -> StationHandle {
    let scheme = if tls { "wss" } else { "ws" };
    let mut config = StationConfig::new(format!("{scheme}://{addr}/ocpp")).with_charge_point_id(id);
    config.persist = false;
    config.tls.skip_verify = tls;
    let (handle, _join) = Station::spawn(config, Arc::new(SystemClock));
    handle.connect().await.unwrap();
    let mut snapshots = handle.watch_snapshot();
    tokio::time::timeout(WAIT, snapshots.wait_for(Snapshot::booted))
        .await
        .expect("station did not boot")
        .unwrap();
    handle
}

fn write(dir: &Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

fn run_to_end(args: &[&str]) -> (i32, String, String) {
    let output = command(args).stdin(Stdio::null()).output().unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn logs_a_station_and_stops_cleanly_on_sigterm() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    let mut csms = Csms::start(&["--log-file", log.to_str().unwrap()]);
    let station = station(&csms.addr.clone(), "CP-1", false).await;

    csms.wait(|v| v["type"] == "connected" && v["station"] == "CP-1");
    let boot = csms.wait(|v| v["type"] == "frame" && v["action"] == "BootNotification");
    assert_eq!(boot["direction"], "in");
    assert_eq!(boot["kind"], "call");
    assert!(boot["payload"]["chargePointVendor"].is_string());
    csms.wait(|v| v["type"] == "frame" && v["direction"] == "out" && v["kind"] == "result");

    let pid = csms.child.id().to_string();
    let killed = Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    assert!(killed.success());
    let (code, lines, _) = csms.finish();
    assert_eq!(code, 0, "{lines:#?}");
    assert!(
        lines.iter().any(|l| l.contains("\"reason\":\"shutdown\"")),
        "{lines:#?}"
    );
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(
        logged
            .lines()
            .all(|l| serde_json::from_str::<Value>(l).is_ok())
    );
    assert!(logged.contains("\"type\":\"listening\""));
    let _ = station.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stdin_commands_round_trip() {
    let mut csms = Csms::start(&[]);
    let station = station(&csms.addr.clone(), "CP-1", false).await;
    csms.wait(|v| v["type"] == "connected");

    csms.send(
        &json!({"station": "CP-1", "action": "ChangeConfiguration", "id": "a",
            "payload": {"key": "HeartbeatInterval", "value": "60"}})
        .to_string(),
    );
    let reply = csms.wait(|v| v["type"] == "result" && v["id"] == "a");
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["station"], "CP-1");
    assert_eq!(reply["action"], "ChangeConfiguration");
    assert_eq!(reply["result"]["status"], "Accepted");

    csms.send("this is not json");
    let reply = csms.wait(|v| v["type"] == "result" && v["ok"] == false);
    assert!(reply["error"].as_str().unwrap().contains("invalid JSON"));

    // Invalid UTF-8 gets an answer and does not stop the reader.
    csms.send_raw(b"\xff\xfe\n");
    let reply = csms.wait(|v| v["type"] == "result" && v["ok"] == false);
    assert!(
        reply["error"].as_str().unwrap().contains("UTF-8"),
        "{reply}"
    );

    csms.send(
        &json!({"station": "NOPE", "action": "Reset", "payload": {"type": "Soft"}}).to_string(),
    );
    let reply = csms.wait(|v| v["type"] == "result" && v["station"] == "NOPE");
    assert_eq!(reply["ok"], false);
    assert!(reply["error"].as_str().unwrap().contains("unknown station"));

    csms.send(&json!({"action": "Reset"}).to_string());
    let reply = csms.wait(|v| v["type"] == "result" && v["action"] == "Reset");
    assert_eq!(reply["ok"], false);

    // A CALLERROR from the station is a normal result.
    csms.send(&json!({"station": "CP-1", "action": "NoSuchAction", "id": "b"}).to_string());
    let reply = csms.wait(|v| v["type"] == "result" && v["id"] == "b");
    assert_eq!(reply["ok"], false);
    assert!(reply["error"].as_str().unwrap().contains("CALLERROR"));

    // The commands above did not stop anything: the list still works.
    csms.send(r#"{"cmd":"stations","id":"c"}"#);
    let list = csms.wait(|v| v["type"] == "stations");
    assert_eq!(list["id"], "c");
    let first = &list["stations"][0];
    assert_eq!(first["id"], "CP-1");
    assert_eq!(first["connected"], true);
    assert!(first["vendor"].is_string() && first["model"].is_string());

    csms.send(r#"{"cmd":"bogus"}"#);
    csms.wait(|v| v["type"] == "result" && v["ok"] == false);

    // End of input does not stop the server.
    csms.stdin = None;
    let tag = IdTag::new("TAG1").unwrap();
    station.authorize(tag).await.unwrap();
    csms.wait(|v| v["type"] == "frame" && v["action"] == "Authorize");
    let _ = station.shutdown().await;
}

fn script_run(dir: &Path, script: &str, extra: &[&str]) -> Csms {
    let path = write(dir, "script.toml", script);
    let mut args = vec!["--script", path.as_str()];
    args.extend_from_slice(extra);
    Csms::start(&args)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn script_passes_and_writes_reports() {
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("report.json");
    let junit = dir.path().join("junit.xml");
    let mut csms = script_run(
        dir.path(),
        r#"
name = "smoke"
[[steps]]
action = "respond"
call = "DataTransfer"
payload = { status = "Rejected" }
[[steps]]
action = "clear_respond"
call = "DataTransfer"
[[steps]]
action = "inject_fault"
call = "DataTransfer"
kind = "call_error"
[[steps]]
action = "wait_station"
station = "CP-1"
[[steps]]
action = "wait_call"
call = "Authorize"
payload_contains = { idTag = "TAG1" }
[[steps]]
action = "call"
station = "CP-1"
call = "ChangeConfiguration"
payload = { key = "HeartbeatInterval", value = "60" }
expect_status = "Accepted"
[[steps]]
action = "call"
station = "CP-1"
call = "NoSuchAction"
expect_error = true
"#,
        &[
            "--report",
            report.to_str().unwrap(),
            "--junit",
            junit.to_str().unwrap(),
        ],
    );
    let station = station(&csms.addr.clone(), "CP-1", false).await;
    station
        .authorize(IdTag::new("TAG1").unwrap())
        .await
        .unwrap();
    let printed = csms.wait(|v| v["type"] == "report");
    assert_eq!(printed["passed"], true, "{printed}");
    let (code, _, stderr) = csms.finish();
    assert_eq!(code, 0, "{stderr}");
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    assert_eq!(saved["scenario"], "smoke");
    assert_eq!(saved["steps"].as_array().unwrap().len(), 7);
    let xml = std::fs::read_to_string(&junit).unwrap();
    assert!(
        xml.contains("<testsuites") && xml.contains("tests=\"7\""),
        "{xml}"
    );
    let _ = station.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn script_failure_skips_the_rest_and_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let junit = dir.path().join("junit.xml");
    let path = write(
        dir.path(),
        "script.toml",
        r#"
name = "failing"
[[steps]]
action = "wait_station"
station = "CP-1"
booted = true
[[steps]]
action = "call"
station = "CP-1"
call = "ChangeConfiguration"
payload = { key = "HeartbeatInterval", value = "60" }
expect_status = "Rejected"
[[steps]]
action = "sleep"
ms = 1
"#,
    );
    let mut args = vec![
        "--script",
        path.as_str(),
        "--junit",
        junit.to_str().unwrap(),
    ];
    args.push("--timeout");
    args.push("30");
    // Human output this time.
    let mut child = command(&["--listen", "127.0.0.1:0"])
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let first = rx.recv_timeout(WAIT).unwrap();
    let addr = first.rsplit("ws://").next().unwrap().trim().to_owned();
    let station = station(&addr, "CP-1", false).await;
    let mut all = vec![first];
    while let Ok(line) = rx.recv_timeout(WAIT) {
        all.push(line);
    }
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(1), "{all:#?}");
    let text = all.join("\n");
    assert!(text.contains("PASS  1"), "{text}");
    assert!(
        text.contains("FAIL  2") && text.contains("expected status Rejected"),
        "{text}"
    );
    assert!(text.contains("SKIP  3"), "{text}");
    assert!(text.contains("FAILED"), "{text}");
    assert!(
        std::fs::read_to_string(&junit)
            .unwrap()
            .contains("<failure")
    );
    let _ = station.shutdown().await;
}

#[test]
fn version_works_without_arguments() {
    let (code, stdout, _) = run_to_end(&["--version"]);
    assert_eq!(code, 0);
    assert!(stdout.starts_with("kirby-csms "), "{stdout}");
}

#[test]
fn script_timeout_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let csms = script_run(
        dir.path(),
        "name = \"t\"\n[[steps]]\naction = \"wait_station\"\nstation = \"NOPE\"\ntimeout_ms = 600000\n",
        &["--timeout", "1"],
    );
    let (code, lines, _) = csms.finish();
    assert_eq!(code, 1, "{lines:#?}");
    let report = lines
        .iter()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["type"] == "report")
        .unwrap();
    assert_eq!(report["passed"], false);
    assert!(report.to_string().contains("--timeout"), "{report}");
}

#[test]
fn bad_input_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let bad_script = write(
        dir.path(),
        "bad.toml",
        "name = \"t\"\n[[steps]]\naction = \"sleep\"\nms = 1\n[[steps]]\naction = \"nope\"\n",
    );
    let (code, _, stderr) = run_to_end(&["--script", &bad_script]);
    assert_eq!(code, 2);
    assert!(stderr.contains("step 2"), "{stderr}");

    let missing = dir.path().join("missing.toml");
    let (code, _, stderr) = run_to_end(&["--script", missing.to_str().unwrap()]);
    assert_eq!(code, 2, "{stderr}");

    let junk = write(dir.path(), "junk.pem", "not a pem");
    for args in [
        vec!["--listen", "not-an-address"],
        vec!["--subprotocol", "bogus"],
        vec!["--tls-cert", junk.as_str()],
        vec!["--tls-cert", junk.as_str(), "--tls-key", junk.as_str()],
        vec![
            "--tls-cert",
            "/nonexistent/c.pem",
            "--tls-key",
            "/nonexistent/k.pem",
        ],
        vec!["--report", "r.json"],
        vec!["--call-timeout", "0"],
        vec!["--transaction-ids", "fixed"],
        vec!["--security-profile", "4"],
        vec!["--security-profile", "1"],
        vec!["--security-profile", "2", "--default-auth-key", "k"],
        vec!["--auth-key", "A=k"],
        vec!["--security-profile", "1", "--auth-key", "nokey"],
        vec![
            "--security-profile",
            "1",
            "--auth-keys-file",
            "/nonexistent/keys",
        ],
        vec!["--security-profile", "1", "--default-auth-key", ""],
        vec!["--tls-client-ca", "ca.pem"],
    ] {
        let (code, _, stderr) = run_to_end(&args);
        assert_eq!(code, 2, "{args:?}: {stderr}");
        assert_ne!(stderr, "");
    }

    // Strict booleans, also through the environment.
    let output = command(&[])
        .env("KIRBY_CSMS_JSON", "maybe")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

/// A profile-1 station: id as user name, `key` as the AuthorizationKey. Does not wait for boot.
async fn station_with_key(addr: &str, id: &str, key: &str) -> StationHandle {
    let mut config = StationConfig::new(format!("ws://{addr}/ocpp")).with_charge_point_id(id);
    config.persist = false;
    config.reconnect.enabled = false;
    let (handle, _join) = Station::spawn(config, Arc::new(SystemClock));
    for (name, value) in [("AuthorizationKey", key), ("SecurityProfile", "1")] {
        let outcome = handle
            .send(StationCommand::SetConfiguration {
                key: name.to_owned(),
                value: value.to_owned(),
            })
            .await
            .unwrap();
        assert!(
            matches!(outcome, kirby_ocpp_core::station::Outcome::Done),
            "{name}: {outcome:?}"
        );
    }
    handle.connect().await.unwrap();
    handle
}

async fn booted(handle: &StationHandle) {
    let mut snapshots = handle.watch_snapshot();
    tokio::time::timeout(WAIT, snapshots.wait_for(Snapshot::booted))
        .await
        .expect("station did not boot")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn basic_auth_keys_from_flags_file_and_environment() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "keys.txt",
        "# stations\nCP-FILE=fedcba9876543210\n",
    );
    let mut csms = Csms::start(&[
        "--security-profile",
        "1",
        "--auth-key",
        "CP-FLAG=0123456789abcdef",
        "--auth-keys-file",
        &file,
        "--default-auth-key",
        "00112233445566778899",
    ]);
    let addr = csms.addr.clone();
    for (id, key) in [
        ("CP-FLAG", "0123456789abcdef"),
        ("CP-FILE", "fedcba9876543210"),
        ("CP-OTHER", "00112233445566778899"),
    ] {
        let station = station_with_key(&addr, id, key).await;
        booted(&station).await;
        csms.wait(|v| v["type"] == "connected" && v["station"] == id);
        let _ = station.shutdown().await;
    }
    // A wrong key is refused, reported, and never printed.
    let station = station_with_key(&addr, "CP-FLAG", "ffffffffffffffff").await;
    let failed = csms.wait(|v| v["type"] == "auth_failed");
    assert_eq!(failed["station"], "CP-FLAG");
    assert_eq!(failed["reason"], "invalid credentials");
    let _ = station.shutdown().await;
    let output = csms.seen.join("\n");
    for secret in [
        "0123456789abcdef",
        "fedcba9876543210",
        "00112233445566778899",
        "ffffffffffffffff",
    ] {
        assert!(!output.contains(secret), "{secret} leaked: {output}");
    }

    // The environment variable (several entries) works without flags.
    let mut env_csms = {
        let mut child = command(&[
            "--listen",
            "127.0.0.1:0",
            "--json",
            "--security-profile",
            "1",
        ])
        .env(
            "KIRBY_CSMS_AUTH_KEY",
            "CP-E1=aabbccddeeff0011;CP-E2=1100ffeeddccbbaa",
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let listening: Value = serde_json::from_str(&line).unwrap();
        (child, listening["addr"].as_str().unwrap().to_owned())
    };
    let station = station_with_key(&env_csms.1, "CP-E2", "1100ffeeddccbbaa").await;
    booted(&station).await;
    let _ = station.shutdown().await;
    let _ = env_csms.0.kill();
    let _ = env_csms.0.wait();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serves_wss_with_a_self_signed_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let key_pair = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()]).unwrap();
    let cert = write(dir.path(), "cert.pem", &key_pair.cert.pem());
    let key = write(dir.path(), "key.pem", &key_pair.signing_key.serialize_pem());
    let mut csms = Csms::start(&["--tls-cert", &cert, "--tls-key", &key]);
    let listening = csms.seen.iter().find(|l| l.contains("listening")).unwrap();
    assert!(listening.contains("wss://"), "{listening}");
    let station = station(&csms.addr.clone(), "CP-TLS", true).await;
    csms.wait(|v| v["type"] == "connected" && v["station"] == "CP-TLS");
    let _ = station.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn human_output_neutralises_hostile_station_ids() {
    let mut child = command(&["--listen", "127.0.0.1:0", "--no-tui"])
        .stdin(Stdio::null())
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
    let first = lines.recv_timeout(WAIT).expect("listening line");
    let addr = first
        .strip_prefix("listening on ws://")
        .unwrap_or_else(|| panic!("unexpected first line {first:?}"))
        .to_owned();

    // ESC ] 0 ; x BEL, then a newline that would fake a second log line.
    let id = "evil\u{1b}]0;x\u{7}\nfake";
    let _station = station(&addr, id, false).await;
    let mut seen = Vec::new();
    loop {
        let line = lines.recv_timeout(WAIT).expect("station lines");
        seen.push(line.clone());
        if line.contains("BootNotification") {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(seen.iter().any(|l| l.contains("evil")), "{seen:#?}");
    for line in &seen {
        assert!(!line.chars().any(char::is_control), "{line:?}");
        assert!(!line.starts_with("fake"), "{line:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_call_matches_in_any_order_and_consumes_each_call_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut csms = script_run(
        dir.path(),
        r#"
name = "order"
[[steps]]
action = "wait_station"
station = "CP-1"
[[steps]]
action = "wait_call"
call = "Authorize"
payload_contains = { idTag = "TAG2" }
[[steps]]
action = "wait_call"
call = "Authorize"
payload_contains = { idTag = "TAG1" }
[[steps]]
action = "wait_call"
call = "Authorize"
payload_contains = { idTag = "TAG1" }
timeout_ms = 300
"#,
        &[],
    );
    let station = station(&csms.addr.clone(), "CP-1", false).await;
    for tag in ["TAG1", "TAG2"] {
        station.authorize(IdTag::new(tag).unwrap()).await.unwrap();
    }
    // TAG2 arrived after TAG1 but is waited for first; the second wait for TAG1 finds it
    // already consumed and times out.
    let printed = csms.wait(|v| v["type"] == "report");
    assert_eq!(printed["passed"], false, "{printed}");
    let steps = printed["steps"].as_array().unwrap();
    let status = |i: usize| steps[i]["status"].to_string();
    assert!(status(1).contains("passed"), "{printed}");
    assert!(status(2).contains("passed"), "{printed}");
    assert!(status(3).contains("ailed"), "{printed}");
    let _ = station.shutdown().await;
}
