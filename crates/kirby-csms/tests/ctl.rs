//! `kirby-csms ctl`: the real binary as a client of the real `kirby-csms` HTTP API, with real
//! `kirby-ocpp-core` stations behind it, a small fake API where the answer must be wrong, and
//! a test CA for TLS.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use kirby_ocpp_core::station::Snapshot;
use kirby_ocpp_core::{Station, StationConfig, StationHandle, SystemClock};
use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair};
use serde_json::Value;

const WAIT: Duration = Duration::from_secs(30);

fn scrub(command: &mut Command) {
    for (name, _) in std::env::vars() {
        if name.starts_with("KIRBY_CSMS_") {
            command.env_remove(name);
        }
    }
}

fn csms_command(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kirby-csms"));
    scrub(&mut command);
    command.args(args);
    command
}

/// Kills the child when the test ends, however it ends.
struct Guard(Child);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A running `kirby-csms` with the API on.
struct Csms {
    guard: Guard,
    _lines: Receiver<String>,
    station_addr: String,
    api_url: String,
}

impl Csms {
    fn start(args: &[&str]) -> Self {
        let mut all = vec![
            "--listen",
            "127.0.0.1:0",
            "--api-listen",
            "127.0.0.1:0",
            "--json",
        ];
        all.extend_from_slice(args);
        let mut child = csms_command(&all)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let guard = Guard(child);
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut station_addr = None;
        let mut api_url = None;
        while station_addr.is_none() || api_url.is_none() {
            let line = lines.recv_timeout(WAIT).expect("startup lines");
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match value["type"].as_str() {
                Some("listening") => station_addr = value["addr"].as_str().map(str::to_owned),
                Some("api_listening") => api_url = value["url"].as_str().map(str::to_owned),
                _ => {}
            }
        }
        Self {
            guard,
            _lines: lines,
            station_addr: station_addr.unwrap(),
            api_url: api_url.unwrap(),
        }
    }

    fn ctl(&self, args: &[&str]) -> Out {
        run_ctl(&["--url", &self.api_url], args, &[], None)
    }

    fn ctl_env(&self, env: &[(&str, &str)], args: &[&str]) -> Out {
        run_ctl(&["--url", &self.api_url], args, env, None)
    }
}

/// What a `ctl` run printed.
#[derive(Debug)]
struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Out {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {:?}", self.stdout))
    }

    fn lines(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn ok(self) -> Self {
        assert_eq!(self.code, 0, "{self:?}");
        self
    }
}

/// Runs `kirby-csms ctl <global> <args>`; every wait has a deadline and the child dies with
/// the test.
fn run_ctl(global: &[&str], args: &[&str], env: &[(&str, &str)], stdin: Option<&str>) -> Out {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kirby-csms"));
    scrub(&mut command);
    command.arg("ctl").args(global).args(args);
    for (name, value) in env {
        command.env(name, value);
    }
    let mut guard = Guard(
        command
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    if let Some(text) = stdin {
        let mut pipe = guard.0.stdin.take().unwrap();
        pipe.write_all(text.as_bytes()).unwrap();
    }
    let mut stdout = guard.0.stdout.take().unwrap();
    let mut stderr = guard.0.stderr.take().unwrap();
    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        let _ = out_tx.send(bytes);
    });
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        let _ = err_tx.send(bytes);
    });
    let stdout = out_rx.recv_timeout(WAIT).expect("ctl did not finish");
    let stderr = err_rx.recv_timeout(WAIT).expect("ctl did not finish");
    let status = guard.0.wait().unwrap();
    Out {
        code: status.code().unwrap_or(-1),
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(stderr).unwrap(),
    }
}

async fn station(addr: &str, id: &str) -> StationHandle {
    let mut config = StationConfig::new(format!("ws://{addr}/ocpp")).with_charge_point_id(id);
    config.persist = false;
    let (handle, _join) = Station::spawn(config, Arc::new(SystemClock));
    handle.connect().await.unwrap();
    let mut snapshots = handle.watch_snapshot();
    tokio::time::timeout(WAIT, snapshots.wait_for(Snapshot::booted))
        .await
        .expect("station did not boot")
        .unwrap();
    handle
}

fn write(dir: &Path, file: &str, text: &str) -> String {
    let path = dir.join(file);
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

/// A one-connection fake API: answers any request with `answer` and closes.
fn fake_api(answer: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        if let Ok((mut socket, _)) = listener.accept() {
            let mut head = Vec::new();
            let mut byte = [0_u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if socket.read_exact(&mut byte).is_err() {
                    return;
                }
                head.push(byte[0]);
            }
            let _ = socket.write_all(&answer);
        }
    });
    url
}

// ---- the routes ------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reads_cover_stations_history_transactions_and_metrics() {
    let csms = Csms::start(&[]);
    // A space-free id with characters that need percent-encoding in a path.
    let _station = station(&csms.station_addr, "CP+я").await;

    let health = csms.ctl(&["health"]).ok().json();
    assert_eq!(health["ok"], true);
    assert_eq!(health["stations"], 1);
    assert!(csms.ctl(&["index"]).ok().json()["resources"].is_array());
    let stations = csms.ctl(&["stations"]).ok().json();
    assert_eq!(stations["stations"][0]["id"], "CP+я");
    let one = csms.ctl(&["station", "CP+я"]).ok().json();
    assert_eq!(one["id"], "CP+я");
    assert!(one["state"].is_object());

    let history = csms.ctl(&["history", "CP+я", "--kind", "call"]).ok().json();
    let frames = history["frames"].as_array().unwrap();
    assert_ne!(frames.len(), 0);
    assert!(frames.iter().all(|frame| frame["kind"] == "call"));
    let limited = csms.ctl(&["history", "CP+я", "--limit", "1"]).ok().json();
    assert_eq!(limited["frames"].as_array().unwrap().len(), 1);
    // The cursor of one answer continues the next.
    let cursor = history["next_cursor"].as_str().unwrap();
    let after = csms
        .ctl(&["history", "CP+я", "--since", cursor])
        .ok()
        .json();
    assert_eq!(after["gap"], false);
    let by_action = csms
        .ctl(&["history", "CP+я", "--action", "BootNotification"])
        .ok()
        .json();
    assert_ne!(by_action["frames"].as_array().unwrap().len(), 0);

    let transactions = csms.ctl(&["transactions"]).ok().json();
    assert_eq!(transactions["truncated"], false);
    assert!(transactions["transactions"].is_array());
    assert_eq!(
        csms.ctl(&["transactions", "--state", "stopped", "--station", "CP+я"])
            .ok()
            .json()["transactions"],
        serde_json::json!([])
    );

    let metrics = csms.ctl(&["metrics"]).ok();
    assert!(
        metrics.stdout.contains("kirby_csms_build_info"),
        "{metrics:?}"
    );
    assert!(
        csms.ctl(&["--pretty", "health"])
            .ok()
            .stdout
            .contains("\n  ")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn calls_report_the_outcome_in_the_exit_code() {
    let csms = Csms::start(&[]);
    let _station = station(&csms.station_addr, "CP-1").await;

    let answered = csms
        .ctl(&["call", "CP-1", "GetConfiguration", "--timeout-ms", "20000"])
        .ok()
        .json();
    assert_eq!(answered["ok"], true);
    assert_eq!(answered["outcome"], "result");
    let payload = csms
        .ctl(&[
            "call",
            "CP-1",
            "ChangeConfiguration",
            "--payload",
            r#"{"key":"HeartbeatInterval","value":"60"}"#,
        ])
        .ok()
        .json();
    assert_eq!(payload["result"]["status"], "Accepted");
    // The payload can come from a file or from standard input.
    let from_stdin = run_ctl(
        &["--url", &csms.api_url],
        &["call", "CP-1", "ChangeConfiguration", "--payload-file", "-"],
        &[],
        Some(r#"{"key":"HeartbeatInterval","value":"61"}"#),
    )
    .ok()
    .json();
    assert_eq!(from_stdin["result"]["status"], "Accepted");

    // A CALLERROR from the station is an answer of the API, and exit code 4.
    let refused = csms.ctl(&["call", "CP-1", "NoSuchAction"]);
    assert_eq!(refused.code, 4, "{refused:?}");
    assert_eq!(refused.json()["ok"], false);

    // The API refuses the request: the error body is on stderr, exit code 1.
    let unknown = csms.ctl(&["call", "nobody", "Reset"]);
    assert_eq!(unknown.code, 1, "{unknown:?}");
    assert_eq!(unknown.stdout, "");
    let error: Value = serde_json::from_str(&unknown.stderr).unwrap();
    assert_eq!(error["error"]["code"], "unknown_station");

    let bulk = csms
        .ctl(&[
            "bulk",
            "CP-*",
            "GetConfiguration",
            "--only-protocol",
            "ocpp1.6",
        ])
        .ok()
        .json();
    assert_eq!(bulk["results"].as_array().unwrap().len(), 1);
    assert_eq!(bulk["results"][0]["station"], "CP-1");
    let none = csms.ctl(&["bulk", "other-*", "Reset"]).ok().json();
    assert_eq!(none["results"], serde_json::json!([]));
    let mixed = csms.ctl(&["bulk", "CP-?", "NoSuchAction"]);
    assert_eq!(mixed.code, 4, "{mixed:?}");

    let forget = csms.ctl(&["forget", "CP-1"]);
    assert_eq!(forget.code, 1, "{forget:?}");
    let error: Value = serde_json::from_str(&forget.stderr).unwrap();
    assert_eq!(error["error"]["code"], "conflict");
}

#[test]
fn rules_round_trip() {
    let csms = Csms::start(&[]);
    let set = csms
        .ctl(&[
            "rules",
            "set",
            "Heartbeat",
            "--payload",
            r#"{"currentTime":"2030-01-01T00:00:00Z"}"#,
        ])
        .ok()
        .json();
    assert_eq!(set["set"], true);
    assert_eq!(
        csms.ctl(&["rules", "list"]).ok().json()["rules"][0],
        "Heartbeat"
    );
    let scoped = csms
        .ctl(&[
            "rules",
            "set",
            "Authorize",
            "--station",
            "CP 1",
            "--only-protocol",
            "ocpp1.6",
            "--payload",
            r#"{"idTagInfo":{"status":"Blocked"}}"#,
        ])
        .ok()
        .json();
    assert_eq!(scoped["station"], "CP 1");
    assert_eq!(scoped["protocol"], "ocpp1.6");
    let shown = csms
        .ctl(&["rules", "show", "Authorize", "--station", "CP 1"])
        .ok()
        .json();
    assert_eq!(shown["payload"]["idTagInfo"]["status"], "Blocked");
    let listed = csms
        .ctl(&["rules", "list", "--station", "CP 1"])
        .ok()
        .json();
    assert_eq!(listed["rules"].as_array().unwrap().len(), 1);
    let missing = csms.ctl(&["rules", "show", "Heartbeat", "--station", "CP 1"]);
    assert_eq!(missing.code, 1);
    assert!(missing.stderr.contains("unknown_rule"), "{missing:?}");
    assert_eq!(
        csms.ctl(&["rules", "clear", "Authorize", "--station", "CP 1"])
            .ok()
            .json()["cleared"],
        true
    );
    assert_eq!(
        csms.ctl(&["rules", "clear", "Heartbeat"]).ok().json()["cleared"],
        true
    );
    // A rule needs a payload; a bad action is the API's refusal.
    assert_eq!(csms.ctl(&["rules", "set", "Heartbeat"]).code, 2);
    let bad = csms.ctl(&["rules", "set", "NoSuchAction", "--payload", "{}"]);
    assert_eq!(bad.code, 1, "{bad:?}");
    assert!(bad.stderr.contains("unsupported_action"), "{bad:?}");
}

#[test]
fn faults_round_trip() {
    let csms = Csms::start(&[]);
    let added = csms
        .ctl(&[
            "faults",
            "add",
            "Heartbeat",
            "--kind",
            "call_error",
            "--times",
            "2",
        ])
        .ok()
        .json();
    assert_eq!(added["injected"], true);
    let id = added["id"].as_str().unwrap().to_owned();
    let station_fault = csms
        .ctl(&[
            "faults",
            "add",
            "StartTransaction",
            "--kind",
            "silence",
            "--station",
            "CP-1",
            "--only-protocol",
            "ocpp1.6",
        ])
        .ok()
        .json();
    assert_eq!(station_fault["remaining"], 3);
    let listed = csms.ctl(&["faults", "list"]).ok().json();
    assert_eq!(listed["faults"].as_array().unwrap().len(), 2);
    assert_eq!(listed["faults"][0]["id"], id.as_str());
    assert_eq!(
        csms.ctl(&["faults", "remove", &id]).ok().json()["removed"],
        true
    );
    assert_eq!(
        csms.ctl(&["faults", "remove", &id]).ok().json()["removed"],
        false
    );
    assert_eq!(csms.ctl(&["faults", "clear"]).ok().json()["remaining"], 0);
    let invalid = csms.ctl(&[
        "faults",
        "add",
        "Heartbeat",
        "--kind",
        "invalid_payload",
        "--payload",
        r#"{"currentTime":5}"#,
    ]);
    assert_eq!(invalid.code, 0, "{invalid:?}");
    assert_eq!(
        csms.ctl(&["faults", "add", "Heartbeat", "--kind", "invalid_payload"])
            .code,
        2
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exports_go_to_stdout_or_a_private_file() {
    let csms = Csms::start(&[]);
    let _station = station(&csms.station_addr, "CP-1").await;

    let ndjson = csms.ctl(&["export", "history", "CP-1"]).ok();
    let frames = ndjson.lines();
    assert_ne!(frames.len(), 0);
    assert!(frames.iter().all(|frame| frame["station"] == "CP-1"));
    let csv = csms
        .ctl(&["export", "history", "CP-1", "--as", "csv", "--kind", "call"])
        .ok();
    assert!(
        csv.stdout.starts_with(
            "cursor,ts,station,direction,kind,id,action,payload,code,description,details"
        ),
        "{csv:?}"
    );

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("history.csv");
    let file_text = file.to_str().unwrap();
    let written = csms
        .ctl(&[
            "export", "history", "CP-1", "--as", "csv", "--output", file_text,
        ])
        .ok();
    assert!(written.stdout.is_empty(), "{written:?}");
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .starts_with("cursor,ts,")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let json_file = dir.path().join("transactions.json");
    csms.ctl(&[
        "export",
        "transactions",
        "--as",
        "json",
        "--output",
        json_file.to_str().unwrap(),
    ])
    .ok();
    let value: Value = serde_json::from_slice(&std::fs::read(&json_file).unwrap()).unwrap();
    assert_eq!(value["truncated"], false);
    let csv = csms.ctl(&["export", "transactions"]).ok();
    assert!(csv.stdout.starts_with("station,id,connector,"), "{csv:?}");

    // A refused request leaves no file behind.
    let refused = dir.path().join("refused.csv");
    let out = csms.ctl(&[
        "export",
        "history",
        "nobody",
        "--output",
        refused.to_str().unwrap(),
    ]);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(!refused.exists());
}

// ---- the token ---------------------------------------------------------------------------------

#[test]
fn the_token_comes_from_the_environment_or_a_file_and_is_never_printed() {
    let dir = tempfile::tempdir().unwrap();
    let server_token = write(dir.path(), "server.token", "s3cret-token\n");
    let csms = Csms::start(&["--api-token-file", &server_token]);

    let missing = csms.ctl(&["health"]);
    assert_eq!(missing.code, 1, "{missing:?}");
    assert!(missing.stderr.contains("unauthorized"));
    let wrong = csms.ctl_env(&[("KIRBY_CSMS_API_TOKEN", "wrong-token")], &["health"]);
    assert_eq!(wrong.code, 1, "{wrong:?}");
    assert!(!wrong.stdout.contains("wrong-token") && !wrong.stderr.contains("wrong-token"));

    let by_env = csms.ctl_env(&[("KIRBY_CSMS_API_TOKEN", "s3cret-token")], &["health"]);
    assert_eq!(by_env.code, 0, "{by_env:?}");
    // The file has a trailing newline; the environment variable for the file works too.
    assert_eq!(csms.ctl(&["--token-file", &server_token, "health"]).code, 0);
    let by_file_env = csms.ctl_env(&[("KIRBY_CSMS_API_TOKEN_FILE", &server_token)], &["health"]);
    assert_eq!(by_file_env.code, 0, "{by_file_env:?}");

    // Both sources together are a usage error.
    let both = csms.ctl_env(
        &[("KIRBY_CSMS_API_TOKEN", "s3cret-token")],
        &["--token-file", &server_token, "health"],
    );
    assert_eq!(both.code, 2, "{both:?}");
    assert!(!both.stderr.contains("s3cret-token"));
    // A token that could break the request head is refused before anything is sent.
    let bad = write(dir.path(), "bad.token", "abc\r\nX-Evil: 1\n");
    let out = csms.ctl(&["--token-file", &bad, "health"]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(!out.stderr.contains("Evil"), "{out:?}");
    let spaced = csms.ctl_env(&[("KIRBY_CSMS_API_TOKEN", "two words")], &["health"]);
    assert_eq!(spaced.code, 2, "{spaced:?}");
    assert!(!spaced.stderr.contains("two words"));
    let empty = csms.ctl_env(&[("KIRBY_CSMS_API_TOKEN", "")], &["health"]);
    assert_eq!(empty.code, 2, "{empty:?}");
    let unreadable = csms.ctl(&["--token-file", "/nonexistent/token", "health"]);
    assert_eq!(unreadable.code, 2, "{unreadable:?}");
}

#[test]
fn the_url_comes_from_the_environment_and_never_holds_credentials() {
    let csms = Csms::start(&[]);
    let out = run_ctl(
        &[],
        &["health"],
        &[("KIRBY_CSMS_API_URL", &csms.api_url)],
        None,
    );
    assert_eq!(out.code, 0, "{out:?}");
    // Credentials in the URL are refused and never echoed.
    let out = run_ctl(
        &["--url", "http://user:hunter2@127.0.0.1:1"],
        &["health"],
        &[],
        None,
    );
    assert_eq!(out.code, 2, "{out:?}");
    assert!(!out.stderr.contains("hunter2"), "{out:?}");
}

// ---- usage and transport errors --------------------------------------------------------------

#[test]
fn usage_errors_exit_with_two() {
    let cases: &[&[&str]] = &[
        &[],
        &["nonsense"],
        &["--timeout", "0", "health"],
        &["--url", "ftp://x", "health"],
        &["--url", "http://127.0.0.1:1/?a=b", "health"],
        &["--client-cert", "c.pem", "health"],
        &[
            "rules",
            "clear",
            "Heartbeat",
            "--station",
            "A",
            "--only-protocol",
            "ocpp1.6",
        ],
        &["history", "CP-1", "--since", "42"],
        &["history", "CP-1", "--kind", "other"],
        &["transactions", "--limit", "10001"],
        &["call", "CP-1", "Reset", "--timeout-ms", "0"],
        &["call", "CP-1", "Reset", "--payload", "{not json"],
        &[
            "call",
            "CP-1",
            "Reset",
            "--payload",
            "{}",
            "--payload-file",
            "x",
        ],
        &[
            "call",
            "CP-1",
            "Reset",
            "--payload-file",
            "/nonexistent/payload.json",
        ],
        &["events", "--count", "0"],
    ];
    for args in cases {
        let out = run_ctl(&["--url", "http://127.0.0.1:9"], args, &[], None);
        assert_eq!(out.code, 2, "{args:?}: {out:?}");
        assert!(
            out.stdout.is_empty() || args.is_empty(),
            "{args:?}: {out:?}"
        );
    }
}

#[test]
fn ctl_must_be_the_first_argument_and_the_server_flags_do_not_apply_to_it() {
    let out = csms_command(&["--json", "ctl", "health"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("first argument"));
    // The server's token variable needs no --api-listen for the client.
    let csms = Csms::start(&[]);
    let out = run_ctl(
        &["--url", &csms.api_url],
        &["health"],
        &[("KIRBY_CSMS_API_TOKEN", "unused-by-an-open-api")],
        None,
    );
    assert_eq!(out.code, 0, "{out:?}");
}

#[test]
fn unreachable_and_foreign_servers_exit_with_three() {
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let out = run_ctl(&["--url", &closed], &["health"], &[], None);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stderr.contains("cannot connect"), "{out:?}");

    for answer in [
        &b"SSH-2.0-OpenSSH\r\n\r\n"[..],
        b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\n<html>",
        b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\n{\"short\":",
        b"",
    ] {
        let url = fake_api(answer.to_vec());
        let out = run_ctl(&["--url", &url], &["health"], &[], None);
        assert_eq!(out.code, 3, "{answer:?}: {out:?}");
    }
    // An error status without a JSON body is still an API error.
    let url = fake_api(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n".to_vec());
    let out = run_ctl(&["--url", &url], &["health"], &[], None);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("HTTP 400"), "{out:?}");
}

#[test]
fn text_from_the_api_cannot_reach_the_terminal_as_control_characters() {
    let body = "{\"note\":\"a\\u001b[2Jb\u{202e}c\u{85}d\"}";
    let answer = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let url = fake_api(answer.into_bytes());
    let out = run_ctl(&["--url", &url], &["health"], &[], None).ok();
    assert!(!out.stdout.contains('\u{202e}') && !out.stdout.contains('\u{85}'));
    assert!(!out.stdout.contains('\u{1b}'));
    let value = out.json();
    assert_eq!(value["note"], "a\u{1b}[2Jb\u{202e}c\u{85}d");
    // The error body goes through the same escaping.
    let body = "{\"error\":{\"code\":\"x\",\"message\":\"m\u{202e}\"}}";
    let answer = format!(
        "HTTP/1.1 500 Oops\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let url = fake_api(answer.into_bytes());
    let out = run_ctl(&["--url", &url], &["health"], &[], None);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(!out.stderr.contains('\u{202e}'), "{out:?}");
}

// ---- events ----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_event_tail_replays_exactly_after_an_event_id() {
    let csms = Csms::start(&[]);
    // An id from before the ring is answered with a resync that names the newest event.
    let resync = csms
        .ctl(&[
            "events",
            "--last-event-id",
            "1",
            "--count",
            "1",
            "--with-id",
        ])
        .ok()
        .lines();
    assert_eq!(resync[0]["event"], "resync");
    assert_eq!(resync[0]["data"]["type"], "resync");
    let start = resync[0]["id"].as_str().unwrap().to_owned();
    assert!(start.bytes().all(|b| b.is_ascii_digit()));

    let _a = station(&csms.station_addr, "CP-A").await;
    let _b = station(&csms.station_addr, "CP-B").await;

    let all = csms
        .ctl(&[
            "events",
            "--last-event-id",
            &start,
            "--count",
            "6",
            "--with-id",
        ])
        .ok()
        .lines();
    assert_eq!(all.len(), 6);
    let ids: Vec<u64> = all
        .iter()
        .map(|event| event["id"].as_str().unwrap().parse().unwrap())
        .collect();
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
    assert!(ids[0] > start.parse::<u64>().unwrap());
    assert_eq!(all[0]["event"], "connected");

    // The first three, then everything after the third: together the same six, once each.
    let first = csms
        .ctl(&[
            "events",
            "--last-event-id",
            &start,
            "--count",
            "3",
            "--with-id",
        ])
        .ok()
        .lines();
    assert_eq!(first, all[..3]);
    let third = ids[2].to_string();
    let rest = csms
        .ctl(&[
            "events",
            "--last-event-id",
            &third,
            "--count",
            "3",
            "--with-id",
        ])
        .ok()
        .lines();
    assert_eq!(rest, all[3..]);

    // Without --with-id the line is the event object itself.
    let bare = csms
        .ctl(&["events", "--last-event-id", &start, "--count", "1"])
        .ok()
        .lines();
    assert_eq!(bare[0], all[0]["data"]);

    // A station filter shows that station only.
    let only_a = csms
        .ctl(&[
            "events",
            "--station",
            "CP-A",
            "--last-event-id",
            &start,
            "--count",
            "3",
        ])
        .ok()
        .lines();
    assert!(
        only_a.iter().all(|event| event["station"] == "CP-A"),
        "{only_a:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_event_tail_follows_live_events_and_gives_up_when_the_server_is_gone() {
    let mut csms = Csms::start(&[]);
    let mut command = Command::new(env!("CARGO_BIN_EXE_kirby-csms"));
    scrub(&mut command);
    let mut tail = Guard(
        command
            .args(["ctl", "--url", &csms.api_url, "events"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let stdout = tail.0.stdout.take().unwrap();
    let (tx, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    // The stream is open once the server counts it; only then does a station make events
    // that the tail must show from the first one.
    let deadline = std::time::Instant::now() + WAIT;
    while !csms
        .ctl(&["metrics"])
        .ok()
        .stdout
        .lines()
        .any(|line| line == "kirby_csms_event_streams 1")
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the tail never connected"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let _station = station(&csms.station_addr, "CP-live").await;
    let first = lines.recv_timeout(WAIT).expect("a live event");
    let value: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(value["type"], "connected");
    assert_eq!(value["station"], "CP-live");
    // Stopping the server ends the stream; the tail retries and then gives up with code 3.
    csms.guard.0.kill().unwrap();
    loop {
        match lines.recv_timeout(WAIT) {
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("the tail did not give up"),
        }
    }
    let status = tail.0.wait().unwrap();
    assert_eq!(status.code(), Some(3));
}

// ---- TLS -------------------------------------------------------------------------------------

struct Ca {
    pem: String,
    issuer: rcgen::Issuer<'static, KeyPair>,
}

fn name(cn: &str) -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, cn);
    dn
}

fn ca(cn: &str) -> Ca {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name = name(cn);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let cert = params.self_signed(&key).unwrap();
    Ca {
        pem: cert.pem(),
        issuer: rcgen::Issuer::new(params, key),
    }
}

fn leaf(ca: &Ca, cn: &str) -> (String, String) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
    params.distinguished_name = name(cn);
    let cert = params.signed_by(&key, &ca.issuer).unwrap();
    (cert.pem(), key.serialize_pem())
}

#[test]
fn https_needs_a_trusted_ca_and_mutual_tls_a_client_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let servers = ca("server ca");
    let clients = ca("client ca");
    let (server_cert, server_key) = leaf(&servers, "api");
    let (client_cert, client_key) = leaf(&clients, "ctl");
    let token = write(dir.path(), "token", "tls-token\n");
    let cert_file = write(dir.path(), "cert.pem", &server_cert);
    let key_file = write(dir.path(), "key.pem", &server_key);
    let ca_file = write(dir.path(), "ca.pem", &servers.pem);
    let client_ca_file = write(dir.path(), "client-ca.pem", &clients.pem);
    let client_cert_file = write(dir.path(), "client.pem", &client_cert);
    let client_key_file = write(dir.path(), "client.key", &client_key);
    let other_ca = write(dir.path(), "other-ca.pem", &ca("other").pem);

    let csms = Csms::start(&[
        "--api-tls-cert",
        &cert_file,
        "--api-tls-key",
        &key_file,
        "--api-token-file",
        &token,
    ]);
    assert!(csms.api_url.starts_with("https://"));
    let ok = csms.ctl(&["--ca-file", &ca_file, "--token-file", &token, "health"]);
    assert_eq!(ok.code, 0, "{ok:?}");
    // A CA that did not sign the certificate, or the default roots, refuse it.
    for args in [
        vec!["--ca-file", &other_ca, "--token-file", &token, "health"],
        vec!["--token-file", &token, "health"],
    ] {
        let out = csms.ctl(&args);
        assert_eq!(out.code, 3, "{out:?}");
        assert!(out.stderr.contains("TLS"), "{out:?}");
        assert!(!out.stderr.contains("tls-token"));
    }
    // A plain-HTTP client against the TLS port is a transport error.
    let plain = csms.api_url.replacen("https://", "http://", 1);
    let out = run_ctl(
        &["--url", &plain, "--token-file", &token],
        &["health"],
        &[],
        None,
    );
    assert_eq!(out.code, 3, "{out:?}");
    // The token still applies over TLS.
    let out = csms.ctl(&["--ca-file", &ca_file, "health"]);
    assert_eq!(out.code, 1, "{out:?}");

    let mutual = Csms::start(&[
        "--api-tls-cert",
        &cert_file,
        "--api-tls-key",
        &key_file,
        "--api-tls-client-ca",
        &client_ca_file,
    ]);
    let ok = mutual.ctl(&[
        "--ca-file",
        &ca_file,
        "--client-cert",
        &client_cert_file,
        "--client-key",
        &client_key_file,
        "health",
    ]);
    assert_eq!(ok.code, 0, "{ok:?}");
    let without = mutual.ctl(&["--ca-file", &ca_file, "health"]);
    assert_eq!(without.code, 3, "{without:?}");
    // An unreadable or wrong key file is a usage error.
    let bad = mutual.ctl(&[
        "--ca-file",
        &ca_file,
        "--client-cert",
        &client_cert_file,
        "--client-key",
        &ca_file,
        "health",
    ]);
    assert_eq!(bad.code, 2, "{bad:?}");
}
