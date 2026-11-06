//! HTTP control API tests: the real `kirby-csms` binary with `--api-listen`, real
//! `kirby-ocpp-core` stations as clients and a tiny raw HTTP/1.1 client.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use kirby_ocpp_core::station::Snapshot;
use kirby_ocpp_core::{Station, StationConfig, StationHandle, SystemClock};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const WAIT: Duration = Duration::from_secs(30);

const ENV_VARS: &[&str] = &[
    "KIRBY_CSMS_LISTEN",
    "KIRBY_CSMS_API_LISTEN",
    "KIRBY_CSMS_API_TOKEN",
    "KIRBY_CSMS_API_TOKEN_FILE",
    "KIRBY_CSMS_SHOW_SECRETS",
    "KIRBY_CSMS_STDIN",
    "KIRBY_CSMS_SCRIPT",
    "KIRBY_CSMS_JSON",
];

fn command(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kirby-csms"));
    for var in ENV_VARS {
        command.env_remove(var);
    }
    command.args(args);
    command
}

/// A running `kirby-csms` with the API on.
struct Csms {
    child: Child,
    /// Keeps the stdout reader alive (and the pipe drained).
    _lines: Receiver<String>,
    station_addr: String,
    api_addr: String,
    api_line: Value,
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
        let mut child = command(&all)
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
        let mut station_addr = None;
        let mut api_line = None;
        while station_addr.is_none() || api_line.is_none() {
            let line = lines.recv_timeout(WAIT).expect("startup lines");
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match value["type"].as_str() {
                Some("listening") => station_addr = value["addr"].as_str().map(str::to_owned),
                Some("api_listening") => api_line = Some(value),
                _ => {}
            }
        }
        let api_line = api_line.unwrap();
        Self {
            child,
            _lines: lines,
            station_addr: station_addr.unwrap(),
            api_addr: api_line["addr"].as_str().unwrap().to_owned(),
            api_line,
        }
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        http(&self.api_addr, "GET", path, &[], None).await
    }

    async fn post_call(&self, id: &str, body: &Value) -> (u16, Value) {
        http(
            &self.api_addr,
            "POST",
            &format!("/v1/stations/{id}/calls"),
            &[],
            Some(&body.to_string()),
        )
        .await
    }
}

impl Drop for Csms {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One request with `Connection: close`; returns the status and the JSON body.
async fn http(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> (u16, Value) {
    let (status, _, value) = http_raw(addr, method, path, headers, body, None).await;
    (status, value)
}

/// Like [`http`]; `declared_length` overrides `Content-Length` without sending that much.
async fn http_raw(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
    declared_length: Option<usize>,
) -> (u16, String, Value) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (name, value) in headers {
        let _ = write!(request, "{name}: {value}\r\n");
    }
    if let Some(length) = declared_length.or(body.map(str::len)) {
        let _ = write!(request, "Content-Length: {length}\r\n");
    }
    request.push_str("\r\n");
    request.push_str(body.unwrap_or_default());
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(WAIT, stream.read_to_end(&mut raw))
        .await
        .expect("response timeout")
        .unwrap();
    let text = String::from_utf8(raw).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").expect("complete response");
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    let value = serde_json::from_str(body).unwrap_or(Value::Null);
    (status, head.to_owned(), value)
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

fn run_to_end(args: &[&str]) -> (i32, String) {
    let output = command(args).stdin(Stdio::null()).output().unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn health_and_error_shapes() {
    let csms = Csms::start(&[]);
    assert_eq!(csms.api_line["url"], format!("http://{}", csms.api_addr));

    let (status, body) = csms.get("/v1/health").await;
    assert_eq!(status, 200);
    assert_eq!(body["ok"], true);
    assert_eq!(body["stations"], 0);
    assert!(body["version"].is_string());

    let (status, body) = csms.get("/v1/stations").await;
    assert_eq!((status, &body), (200, &json!({"stations": []})));

    let (status, body) = csms.get("/v1/nope").await;
    assert_eq!(status, 404);
    assert!(body["error"].is_string());
    let (status, body) = csms.get("/v1/stations/NOPE").await;
    assert_eq!(status, 404);
    assert!(body["error"].is_string());
    let (status, _) = csms.get("/v1/stations/NOPE/history").await;
    assert_eq!(status, 404);

    let (status, body) = http(&csms.api_addr, "POST", "/v1/health", &[], Some("{}")).await;
    assert_eq!(status, 405);
    assert!(body["error"].is_string());
    let (status, _) = http(&csms.api_addr, "GET", "/v1/stations/X/calls", &[], None).await;
    assert_eq!(status, 405);

    // Ids with control characters (percent-decoded by the router) are refused.
    let (status, _) = csms.get("/v1/stations/A%0AB").await;
    assert_eq!(status, 400);
    let (status, _) = csms.get("/v1/stations/A%E2%80%AEB/history").await;
    assert_eq!(status, 400);
    let (status, _) = csms.get("/v1/stations/X/history?limit=abc").await;
    assert_eq!(status, 400);

    // Bad bodies.
    let call = |body: &'static str| {
        let addr = csms.api_addr.clone();
        async move { http(&addr, "POST", "/v1/stations/X/calls", &[], Some(body)).await }
    };
    assert_eq!(call("not json").await.0, 400);
    assert_eq!(call("[1]").await.0, 400);
    assert_eq!(call("{}").await.0, 400);
    assert_eq!(call(r#"{"action":"Reset","timeout_ms":0}"#).await.0, 400);
    let (status, body) = call(r#"{"action":"Reset"}"#).await;
    assert_eq!(status, 404);
    assert_eq!(body["ok"], false);
    assert_eq!(body["station"], "X");

    let (status, _, body) = http_raw(
        &csms.api_addr,
        "POST",
        "/v1/stations/X/calls",
        &[],
        Some("{"),
        Some(2 * 1024 * 1024),
    )
    .await;
    assert_eq!(status, 413);
    assert!(body["error"].is_string());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn station_calls_history_and_forget() {
    let csms = Csms::start(&[]);
    let station = station(&csms.station_addr, "CP-1").await;

    let (status, body) = csms.get("/v1/stations").await;
    assert_eq!(status, 200);
    let first = &body["stations"][0];
    assert_eq!(first["id"], "CP-1");
    assert_eq!(first["connected"], true);
    assert!(first["vendor"].is_string());

    let (status, body) = csms.get("/v1/stations/CP-1").await;
    assert_eq!(status, 200);
    assert_eq!(body["id"], "CP-1");
    assert!(body["state"].is_object(), "{body}");

    // Same result object as the stdin command.
    let (status, body) = csms
        .post_call(
            "CP-1",
            &json!({"action": "ChangeConfiguration",
                "payload": {"key": "HeartbeatInterval", "value": "60"}, "timeout_ms": 10000}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        json!({"type": "result", "station": "CP-1", "action": "ChangeConfiguration",
            "ok": true, "result": {"status": "Accepted"}})
    );

    // A CALLERROR is a normal result.
    let (status, body) = csms
        .post_call("CP-1", &json!({"action": "NoSuchAction"}))
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["ok"], false);
    assert!(body["error"].as_str().unwrap().contains("CALLERROR"));

    // Unknown station: 404 with the result object.
    let (status, body) = csms.post_call("GHOST", &json!({"action": "Reset"})).await;
    assert_eq!(status, 404);
    assert!(body["error"].as_str().unwrap().contains("unknown station"));

    // History: oldest first, `limit` keeps the newest.
    let (status, body) = csms.get("/v1/stations/CP-1/history").await;
    assert_eq!(status, 200);
    let frames = body["frames"].as_array().unwrap();
    assert!(frames.len() >= 5, "{body}");
    assert_eq!(frames[0]["type"], "frame");
    assert_eq!(frames[0]["action"], "BootNotification");
    assert_eq!(frames[0]["direction"], "in");
    let (_, limited) = csms.get("/v1/stations/CP-1/history?limit=2").await;
    assert_eq!(
        limited["frames"].as_array().unwrap(),
        &frames[frames.len() - 2..]
    );
    let (_, none) = csms.get("/v1/stations/CP-1/history?limit=0").await;
    assert_eq!(none["frames"], json!([]));

    // Connected: cannot forget.
    let (status, body) = http(&csms.api_addr, "DELETE", "/v1/stations/CP-1", &[], None).await;
    assert_eq!(status, 409);
    assert!(body["error"].is_string());

    let _ = station.shutdown().await;
    loop {
        let (_, body) = csms.get("/v1/stations/CP-1").await;
        if body["connected"] == false {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (status, body) = http(&csms.api_addr, "DELETE", "/v1/stations/CP-1", &[], None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(csms.get("/v1/stations/CP-1").await.0, 404);
    let (status, _) = http(&csms.api_addr, "DELETE", "/v1/stations/CP-1", &[], None).await;
    assert_eq!(status, 404);
}

async fn history_text_after_key_change(extra: &[&str]) -> String {
    let csms = Csms::start(extra);
    let station = station(&csms.station_addr, "CP-1").await;
    let (status, body) = csms
        .post_call(
            "CP-1",
            &json!({"action": "ChangeConfiguration",
                "payload": {"key": "AuthorizationKey", "value": "supersecret12"}}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let (_, history) = csms.get("/v1/stations/CP-1/history").await;
    let _ = station.shutdown().await;
    history.to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_redacts_the_authorization_key() {
    let hidden = history_text_after_key_change(&[]).await;
    assert!(hidden.contains("ChangeConfiguration"), "{hidden}");
    assert!(!hidden.contains("supersecret12"), "{hidden}");
    assert!(hidden.contains("***"), "{hidden}");
    let shown = history_text_after_key_change(&["--show-secrets"]).await;
    assert!(shown.contains("supersecret12"), "{shown}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bearer_token_is_required_when_configured() {
    let csms = Csms::start(&["--api-token", "s3cret-token"]);
    let addr = &csms.api_addr;
    for path in ["/v1/health", "/v1/stations", "/v1/nope"] {
        let (status, body) = http(addr, "GET", path, &[], None).await;
        assert_eq!(status, 401, "{path}");
        assert!(body["error"].is_string());
    }
    let (status, head, _) = http_raw(addr, "GET", "/v1/health", &[], None, None).await;
    assert_eq!(status, 401);
    assert!(head.to_lowercase().contains("www-authenticate: bearer"));
    for header in [
        "Bearer wrong",
        "Bearer s3cret-toke",
        "Bearer s3cret-token2",
        "s3cret-token",
        "Basic s3cret-token",
    ] {
        let (status, _) = http(
            addr,
            "GET",
            "/v1/health",
            &[("Authorization", header)],
            None,
        )
        .await;
        assert_eq!(status, 401, "{header}");
    }
    let ok = [("Authorization", "Bearer s3cret-token")];
    assert_eq!(http(addr, "GET", "/v1/health", &ok, None).await.0, 200);
    assert_eq!(http(addr, "GET", "/v1/nope", &ok, None).await.0, 404);
    // With a token, an Origin header alone is not a problem (the bearer proves intent).
    let with_origin = [
        ("Authorization", "Bearer s3cret-token"),
        ("Origin", "http://x"),
    ];
    assert_eq!(
        http(addr, "GET", "/v1/health", &with_origin, None).await.0,
        200
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn token_file_is_read_and_trimmed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("token");
    std::fs::write(&path, "file-token\n").unwrap();
    let csms = Csms::start(&["--api-token-file", path.to_str().unwrap()]);
    let addr = &csms.api_addr;
    assert_eq!(http(addr, "GET", "/v1/health", &[], None).await.0, 401);
    let ok = [("Authorization", "Bearer file-token")];
    assert_eq!(http(addr, "GET", "/v1/health", &ok, None).await.0, 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn origin_header_is_refused_without_a_token() {
    let csms = Csms::start(&[]);
    let (status, body) = http(
        &csms.api_addr,
        "GET",
        "/v1/health",
        &[("Origin", "http://evil.example")],
        None,
    )
    .await;
    assert_eq!(status, 403);
    assert!(body["error"].is_string());
    assert_eq!(csms.get("/v1/health").await.0, 200);
}

#[test]
fn api_usage_errors_exit_with_two() {
    let (code, stderr) = run_to_end(&["--api-listen", "0.0.0.0:0"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("loopback"), "{stderr}");

    let (code, stderr) = run_to_end(&["--api-listen", "[::]:0"]);
    assert_eq!(code, 2, "{stderr}");

    let (code, stderr) = run_to_end(&["--api-token", "x"]);
    assert_eq!(code, 2, "{stderr}");

    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty");
    std::fs::write(&empty, "\n").unwrap();
    let (code, stderr) = run_to_end(&[
        "--api-listen",
        "127.0.0.1:0",
        "--api-token-file",
        empty.to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("empty"), "{stderr}");

    let (code, stderr) = run_to_end(&[
        "--api-listen",
        "127.0.0.1:0",
        "--api-token",
        "x",
        "--api-token-file",
        empty.to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "{stderr}");

    let (code, stderr) = run_to_end(&[
        "--api-listen",
        "127.0.0.1:0",
        "--api-token-file",
        dir.path().join("missing").to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "{stderr}");
}
