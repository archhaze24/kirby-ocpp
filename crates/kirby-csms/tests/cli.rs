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
    assert_eq!(saved["schema"], 1);
    assert_eq!(saved["status"], "passed");
    assert_eq!(saved["tool_version"], env!("CARGO_PKG_VERSION"));
    assert!(
        saved["started_at"]
            .as_str()
            .is_some_and(|t| t.ends_with('Z'))
    );
    assert!(saved["steps"][0]["action"].is_string());
    let xml = std::fs::read_to_string(&junit).unwrap();
    assert!(
        xml.contains("<testsuites") && xml.contains("tests=\"7\""),
        "{xml}"
    );
    let _ = station.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_call_matches_a_call_above_the_event_size_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mut csms = script_run(
        dir.path(),
        r#"
name = "big"
[[steps]]
action = "wait_station"
station = "CP-1"
[[steps]]
action = "wait_call"
call = "DataTransfer"
payload_contains = { vendorId = "bigvendor" }
timeout_ms = 20000
"#,
        &["--timeout", "30"],
    );
    let station = station(&csms.addr.clone(), "CP-1", false).await;
    station
        .send(StationCommand::DataTransfer {
            vendor_id: "bigvendor".to_owned(),
            message_id: None,
            data: Some("x".repeat(70 * 1024)),
        })
        .await
        .unwrap();
    let printed = csms.wait(|v| v["type"] == "report");
    assert_eq!(printed["passed"], true, "{printed}");
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
    // Do not wait for the station's own boot: the script finishes within milliseconds of the
    // boot (the first step is the only one that waits) and the server shuts down, so the
    // station may never see the boot reply.
    let mut config = StationConfig::new(format!("ws://{addr}/ocpp")).with_charge_point_id("CP-1");
    config.persist = false;
    let (station, _join) = Station::spawn(config, Arc::new(SystemClock));
    station.connect().await.unwrap();
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
fn new_engine_flags_are_validated() {
    for args in [
        &["--validate-inbound", "maybe"][..],
        &["--ping-interval", "x"],
        &["--pong-timeout", "0"],
        &["--ping-interval", "5", "--pong-timeout", "10"],
    ] {
        let (code, _, stderr) = run_to_end(args);
        assert_eq!(code, 2, "{args:?}: {stderr}");
    }
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
        vec!["--auth-key", "A=1", "--auth-key", "A=2"],
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

#[test]
fn bad_auth_key_values_are_never_echoed_and_empty_sources_are_usage_errors() {
    let (code, _, stderr) = run_to_end(&["--auth-key", "hunter2"]);
    assert_eq!(code, 2);
    assert!(!stderr.contains("hunter2"), "{stderr}");
    let (code, _, stderr) = run_to_end(&["--security-profile", "1", "--auth-key", "hunter2"]);
    assert_eq!(code, 2);
    assert!(!stderr.contains("hunter2"), "{stderr}");
    let (code, _, stderr) = run_to_end(&["--auth-key", "A=s3cret1", "--auth-key", "A=s3cret2"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("`A`") && !stderr.contains("s3cret"),
        "{stderr}"
    );

    let dir = tempfile::tempdir().unwrap();
    for (name, text) in [
        ("empty.txt", ""),
        ("comments.txt", "# nothing\n\n"),
        ("dup.txt", "A=s3cret1\nA=s3cret2\n"),
    ] {
        let file = write(dir.path(), name, text);
        let (code, _, stderr) = run_to_end(&["--security-profile", "1", "--auth-keys-file", &file]);
        assert_eq!(code, 2, "{name}: {stderr}");
        assert!(!stderr.contains("s3cret"), "{stderr}");
    }
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

    // ESC ] 0 ; x BEL, then a newline that would fake a second log line: refused at the
    // handshake (control characters in the id), so it never reaches the output at all.
    let id = "evil\u{1b}]0;x\u{7}\nfake";
    let mut config = StationConfig::new(format!("ws://{addr}/ocpp")).with_charge_point_id(id);
    config.persist = false;
    config.reconnect.enabled = false;
    let (hostile, _join) = Station::spawn(config, Arc::new(SystemClock));
    hostile.connect().await.unwrap();
    let _station = station(&addr, "CP-OK", false).await;
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
    assert!(seen.iter().any(|l| l.contains("CP-OK")), "{seen:#?}");
    assert!(!seen.iter().any(|l| l.contains("evil")), "{seen:#?}");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn script_vars_capture_and_expect_payload() {
    let dir = tempfile::tempdir().unwrap();
    let mut csms = script_run(
        dir.path(),
        r#"
name = "vars"
[vars]
key = "NotTheKey"
[[steps]]
action = "wait_station"
station = "CP-1"
[[steps]]
action = "wait_call"
call = "Authorize"
capture = { tag = "/idTag" }
[[steps]]
action = "call"
station = "CP-1"
call = "ChangeConfiguration"
payload = { key = "${key}", value = "60" }
expect_status = "Accepted"
expect_payload = { status = "Accepted" }
capture = { st = "/status" }
[[steps]]
action = "call"
station = "CP-1"
call = "DataTransfer"
payload = { vendorId = "${tag}", data = "id-${tag}" }
expect_payload = { status = "UnknownVendorId" }
[[steps]]
action = "call"
station = "CP-1"
call = "ChangeConfiguration"
payload = { key = "HeartbeatInterval", value = "61" }
expect_status = "${st}"
"#,
        &["--var", "key=HeartbeatInterval"],
    );
    let station = station(&csms.addr.clone(), "CP-1", false).await;
    station
        .authorize(IdTag::new("TAG7").unwrap())
        .await
        .unwrap();
    let printed = csms.wait(|v| v["type"] == "report");
    assert_eq!(printed["passed"], true, "{printed}");
    let (code, _, stderr) = csms.finish();
    assert_eq!(code, 0, "{stderr}");
    let _ = station.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn script_expect_payload_mismatch_and_unknown_var_fail_the_step() {
    for (extra, reason) in [
        (
            "expect_payload = { status = \"Rejected\" }",
            "does not contain",
        ),
        ("expect_payload = { status = \"${missing}\" }", "missing"),
        ("capture = { x = \"/nothing\" }", "/nothing"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let script = format!(
            "name = \"f\"\n[[steps]]\naction = \"wait_station\"\nstation = \"CP-1\"\nbooted = true\n\
             [[steps]]\naction = \"call\"\nstation = \"CP-1\"\ncall = \"ChangeConfiguration\"\n\
             payload = {{ key = \"HeartbeatInterval\", value = \"60\" }}\n{extra}\n"
        );
        let mut csms = script_run(dir.path(), &script, &[]);
        // Not waiting for the boot reply: the failing script ends the server first.
        let mut config =
            StationConfig::new(format!("ws://{}/ocpp", csms.addr)).with_charge_point_id("CP-1");
        config.persist = false;
        let (station, _join) = Station::spawn(config, Arc::new(SystemClock));
        station.connect().await.unwrap();
        let printed = csms.wait(|v| v["type"] == "report");
        assert_eq!(printed["passed"], false, "{printed}");
        assert!(
            printed["steps"][1]["reason"].to_string().contains(reason),
            "{printed}"
        );
        let (code, _, _) = csms.finish();
        assert_eq!(code, 1);
        let _ = station.shutdown().await;
    }
}

#[test]
fn bad_var_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "s.toml",
        "name = \"t\"\n[[steps]]\naction = \"sleep\"\nms = 1\n",
    );
    let (code, _, stderr) = run_to_end(&["--script", &path, "--var", "bad-name=1"]);
    assert_eq!(code, 2, "{stderr}");
    let (code, _, stderr) = run_to_end(&["--var", "a=1"]);
    assert_eq!(code, 2, "{stderr}");
}

/// Sends `ChangeConfiguration(AuthorizationKey)` and returns the logged outbound CALL payload.
async fn logged_key_change(args: &[&str]) -> Value {
    let mut csms = Csms::start(args);
    let station = station(&csms.addr.clone(), "CP-1", false).await;
    csms.wait(|v| v["type"] == "connected");
    csms.send(
        &json!({"station": "CP-1", "action": "ChangeConfiguration", "id": "k",
            "payload": {"key": "AuthorizationKey", "value": "0123456789abcdef0123"}})
        .to_string(),
    );
    // The answer is logged after the CALL that it answers, so both are in `seen` by now.
    csms.wait(|v| v["type"] == "frame" && v["direction"] == "in" && v["id"] == "csms-1");
    let frame = csms
        .seen
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|v| {
            v["type"] == "frame" && v["direction"] == "out" && v["action"] == "ChangeConfiguration"
        })
        .expect("the outbound CALL was logged");
    let _ = station.shutdown().await;
    frame["payload"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authorization_key_is_redacted_in_frame_logs_unless_show_secrets() {
    let hidden = logged_key_change(&[]).await;
    assert_eq!(hidden["key"], "AuthorizationKey");
    assert_eq!(hidden["value"], "***");
    let shown = logged_key_change(&["--show-secrets"]).await;
    assert_eq!(shown["value"], "0123456789abcdef0123");
}

/// Starts `kirby-csms` with human output and returns the child, its stdout lines and the address.
fn start_human(args: &[&str]) -> (Child, Receiver<String>, String) {
    let mut child = command(&["--listen", "127.0.0.1:0"])
        .args(args)
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
    (child, rx, addr)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn station_text_cannot_forge_script_report_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "script.toml",
        r#"
name = "forge"
[[steps]]
action = "wait_station"
station = "CP-1"
booted = true
[[steps]]
action = "call"
station = "CP-1"
call = "ChangeConfiguration"
payload = { key = "HeartbeatInterval", value = "60" }
expect_status = "Accepted"
"#,
    );
    let (mut child, rx, addr) = start_human(&["--script", &path, "--timeout", "30"]);
    let rule = "ChangeConfiguration=error:GenericError:boom\n  PASS  3 forged step (1 ms)\nPASSED in 5 ms\u{85}\u{202e}";
    let mut config = StationConfig::new(format!("ws://{addr}/ocpp")).with_charge_point_id("CP-1");
    config.persist = false;
    config.response_rules = vec![rule.parse().expect("rule")];
    let (station, _join) = Station::spawn(config, Arc::new(SystemClock));
    station.connect().await.unwrap();
    let mut all = Vec::new();
    while let Ok(line) = rx.recv_timeout(WAIT) {
        all.push(line);
    }
    assert_eq!(child.wait().unwrap().code(), Some(1), "{all:#?}");
    assert!(all.iter().any(|l| l.contains("FAIL  2")), "{all:#?}");
    for line in &all {
        let trimmed = line.trim_start();
        assert!(!trimmed.starts_with("PASS  3"), "{all:#?}");
        assert!(!trimmed.starts_with("PASSED in 5"), "{all:#?}");
        assert!(
            !line.chars().any(kirby_ocpp_tui::text::is_unsafe_char),
            "{line:?}"
        );
    }
    assert_eq!(all.iter().filter(|l| l.starts_with("FAILED")).count(), 1);
    let _ = station.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn json_lines_escape_c1_and_bidi_characters() {
    let mut csms = Csms::start(&[]);
    let station = station(&csms.addr.clone(), "CP-1", false).await;
    station
        .send(StationCommand::DataTransfer {
            vendor_id: "v".to_owned(),
            message_id: None,
            data: Some("a\u{85}b\u{202e}c\u{2028}d".to_owned()),
        })
        .await
        .unwrap();
    csms.wait(|v| v["type"] == "frame" && v["action"] == "DataTransfer");
    let line = csms
        .seen
        .iter()
        .rev()
        .find(|l| l.contains("DataTransfer") && l.contains("\"frame\""))
        .unwrap()
        .clone();
    assert!(
        !line.chars().any(kirby_ocpp_tui::text::is_unsafe_char),
        "{line:?}"
    );
    assert!(
        line.contains("\\u0085") && line.contains("\\u202e"),
        "{line}"
    );
    let _ = station.shutdown().await;
}

/// Nobody reads stdout and one frame is bigger than a pipe buffer: the log file still gets the
/// line and the server still shuts down.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unread_stdout_does_not_stall_the_log_file_or_the_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    let mut child = command(&["--listen", "127.0.0.1:0", "--json", "--log-file"])
        .arg(&log)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // The first line is read on a thread (with a deadline); the rest of stdout is never read,
    // but the pipe stays open until `_hold` drops.
    let stdout = child.stdout.take().unwrap();
    let (first_tx, first_rx) = mpsc::channel();
    let (_hold, hold_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut first = String::new();
        let _ = reader.read_line(&mut first);
        let _ = first_tx.send(first);
        let _ = hold_rx.recv();
    });
    let first = match first_rx.recv_timeout(WAIT) {
        Ok(first) => first,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("no first stdout line: {error:?}");
        }
    };
    let value: Value = serde_json::from_str(&first).unwrap();
    let addr = value["addr"].as_str().unwrap().to_owned();
    let station = station(&addr, "CP-1", false).await;
    // Frames just under the 64 KiB event limit (bigger ones are cut to a marker).
    for _ in 0..40 {
        station
            .send(StationCommand::DataTransfer {
                vendor_id: "v".to_owned(),
                message_id: None,
                data: Some("x".repeat(50_000)),
            })
            .await
            .unwrap();
    }
    // The file must get all the big frames although the pipe is full.
    let start = std::time::Instant::now();
    loop {
        let logged = std::fs::read_to_string(&log).unwrap_or_default();
        if logged.len() > 1_900_000 {
            break;
        }
        assert!(
            start.elapsed() < WAIT,
            "log file stalled at {} bytes",
            logged.len()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let pid = child.id().to_string();
    assert!(
        Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success()
    );
    let code = tokio::task::spawn_blocking(move || {
        let start = std::time::Instant::now();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status.code();
            }
            if start.elapsed() > WAIT {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    })
    .await
    .unwrap();
    assert_eq!(
        code,
        Some(0),
        "server must exit although stdout is not read"
    );
    let _ = station.shutdown().await;
}

/// SIGTERM during a script still leaves a JUnit file that says why (exit code 1).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sigterm_during_a_script_writes_the_junit_report() {
    let dir = tempfile::tempdir().unwrap();
    let junit = dir.path().join("junit.xml");
    let csms = script_run(
        dir.path(),
        "name = \"never\"\n[[steps]]\naction = \"wait_station\"\nstation = \"NOBODY\"\n",
        &["--junit", junit.to_str().unwrap(), "--timeout", "120"],
    );
    // A booted station proves the script is running (and listening for the signal).
    let booted = station(&csms.addr.clone(), "CP-1", false).await;
    let pid = csms.child.id().to_string();
    assert!(
        Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success()
    );
    let (code, _, stderr) = csms.finish();
    assert_eq!(code, 1, "{stderr}");
    let xml = std::fs::read_to_string(&junit).expect("junit written");
    assert!(
        xml.contains("interrupted") && xml.contains("never"),
        "{xml}"
    );
    let _ = booted.shutdown().await;
}

#[test]
fn oversized_auth_keys_file_is_refused_and_a_shared_one_is_warned_about() {
    let dir = tempfile::tempdir().unwrap();
    let text = format!("A={}\n", "k".repeat(1024 * 1024));
    let file = write(dir.path(), "big.txt", &text);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let (code, _, stderr) = run_to_end(&["--security-profile", "1", "--auth-keys-file", &file]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("larger than 1048576 bytes"), "{stderr}");
    assert!(!stderr.contains("kkkkkkkk"), "{stderr}");
    #[cfg(unix)]
    assert!(stderr.contains("readable by group or others"), "{stderr}");
}

/// Runs the binary with stderr merged into stdout (one pipe, like `> file 2>&1`) and returns
/// the first `count` lines.
#[cfg(unix)]
fn merged_lines(args: &[&str], count: usize) -> Vec<String> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("exec \"$0\" \"$@\" 2>&1")
        .arg(env!("CARGO_BIN_EXE_kirby-csms"))
        .args(args)
        .env_remove("KIRBY_CSMS_JSON")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
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
    let mut seen = Vec::new();
    while seen.len() < count {
        match lines.recv_timeout(WAIT) {
            Ok(line) => seen.push(line),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    seen
}

#[cfg(unix)]
#[test]
fn startup_lines_are_whole_and_ordered_on_one_stream() {
    for _ in 0..10 {
        let lines = merged_lines(&["--listen", "127.0.0.1:0", "--no-tui"], 2);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].starts_with("listening on ws://127.0.0.1:"),
            "{lines:?}"
        );
        assert!(
            lines[1].starts_with("note: stations connect to ws://"),
            "{lines:?}"
        );
        assert!(
            lines[1].contains("<station id>") || lines[1].contains("CP"),
            "{lines:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn startup_lines_are_whole_and_ordered_with_a_script() {
    let dir = tempfile::tempdir().unwrap();
    let script = write(
        dir.path(),
        "script.toml",
        "name = \"s\"\n[[steps]]\naction = \"sleep\"\nms = 60000\n",
    );
    for _ in 0..10 {
        let lines = merged_lines(&["--listen", "127.0.0.1:0", "--script", script.as_str()], 2);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].starts_with("listening on ws://127.0.0.1:"),
            "{lines:?}"
        );
        assert!(
            lines[1].starts_with("note: stations connect to ws://"),
            "{lines:?}"
        );
    }
}

#[test]
fn bind_error_names_the_address() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = taken.local_addr().unwrap().to_string();
    let output = command(&["--listen", &addr, "--no-tui"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("failed to bind the CSMS listener {addr}")),
        "{stderr}"
    );
}

#[test]
fn help_texts_match_the_behavior() {
    let output = command(&["--help"]).output().unwrap();
    let help = String::from_utf8_lossy(&output.stdout).to_lowercase();
    assert!(!help.contains("licence"), "{help}");
    assert!(!help.contains("permissions are not checked"), "{help}");
    let output = command(&["--licenses"]).output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("distributed under the MIT license"), "{text}");
}
