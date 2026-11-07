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
    assert_eq!(body["api_version"], 1);
    assert!(body["version"].is_string());

    // The index lists every route; each listed path answers OPTIONS with an Allow header.
    let (status, body) = csms.get("/v1").await;
    assert_eq!(status, 200);
    assert_eq!(body["api_version"], 1);
    assert!(body["version"].is_string());
    let resources = body["resources"].as_array().unwrap();
    assert!(resources.len() >= 10, "{body}");
    assert!(resources.contains(&json!({"path": "/v1/health", "methods": ["GET"]})));
    assert!(
        resources.contains(&json!({"path": "/v1/faults", "methods": ["GET", "POST", "DELETE"]}))
    );
    for resource in resources {
        let path = resource["path"]
            .as_str()
            .unwrap()
            .replace("{id}", "X")
            .replace("{action}", "Heartbeat");
        let answer = raw_exchange(
            &csms.api_addr,
            &format!("OPTIONS {path} HTTP/1.1\r\nHost: {}\r\n\r\n", csms.api_addr),
        )
        .await;
        assert_eq!(status_of(&answer), 204, "{path}: {answer}");
        let lower = answer.to_lowercase();
        for method in resource["methods"].as_array().unwrap() {
            let allow = lower
                .lines()
                .find(|line| line.starts_with("allow:"))
                .unwrap_or_default();
            assert!(
                allow.contains(&method.as_str().unwrap().to_lowercase()),
                "{path}: {answer}"
            );
        }
        assert!(!lower.contains("access-control-"), "{answer}");
    }

    let (status, body) = csms.get("/v1/stations").await;
    assert_eq!((status, &body), (200, &json!({"stations": []})));

    let (status, body) = csms.get("/v1/nope").await;
    assert_eq!(status, 404);
    assert_eq!(body["error"]["code"], "not_found");
    assert!(body["error"]["message"].is_string());
    let (status, body) = csms.get("/v1/stations/NOPE").await;
    assert_eq!(status, 404);
    assert_eq!(body["error"]["code"], "unknown_station");
    let (status, _) = csms.get("/v1/stations/NOPE/history").await;
    assert_eq!(status, 404);

    let (status, body) = http(&csms.api_addr, "POST", "/v1/health", &[], Some("{}")).await;
    assert_eq!(status, 405);
    assert_eq!(body["error"]["code"], "method_not_allowed");
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
    let (status, body) = call("not json").await;
    assert_eq!(
        (status, &body["error"]["code"]),
        (400, &json!("bad_request"))
    );
    assert_eq!(call("[1]").await.0, 400);
    assert_eq!(call("{}").await.0, 400);
    assert_eq!(call(r#"{"action":"Reset","timeout_ms":0}"#).await.0, 400);
    let (status, body) = call(r#"{"action":"Reset"}"#).await;
    assert_eq!(status, 404);
    assert_eq!(body["error"]["code"], "unknown_station");

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
    assert_eq!(body["error"]["code"], "payload_too_large");
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
    assert_eq!(first["protocol"], "ocpp1.6");

    let (status, body) = csms.get("/v1/stations/CP-1").await;
    assert_eq!(status, 200);
    assert_eq!(body["id"], "CP-1");
    assert_eq!(body["protocol"], "ocpp1.6");
    assert!(body["state"].is_object(), "{body}");

    // The shared result object without `type`, `station` and `action`, plus `outcome`.
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
        json!({"outcome": "result", "ok": true, "result": {"status": "Accepted"}})
    );

    // A CALLERROR is a normal result.
    let (status, body) = csms
        .post_call("CP-1", &json!({"action": "NoSuchAction"}))
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["ok"], false);
    assert_eq!(body["outcome"], "call_error");
    assert!(
        body.get("type").is_none() && body.get("station").is_none() && body.get("action").is_none()
    );
    assert!(body["code"].is_string());
    assert!(body["error"].as_str().unwrap().contains("CALLERROR"));

    // Unknown station: 404 with the error body.
    let (status, body) = csms.post_call("GHOST", &json!({"action": "Reset"})).await;
    assert_eq!(status, 404);
    assert_eq!(body["error"]["code"], "unknown_station");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown station")
    );

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
    assert_eq!(body["error"]["code"], "conflict");

    let _ = station.shutdown().await;
    let disconnect_deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let (_, body) = csms.get("/v1/stations/CP-1").await;
        if body["connected"] == false {
            break;
        }
        assert!(
            tokio::time::Instant::now() < disconnect_deadline,
            "station never seen as disconnected"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Known but disconnected: 409.
    let (status, body) = csms.post_call("CP-1", &json!({"action": "Reset"})).await;
    assert_eq!(status, 409);
    assert_eq!(body["error"]["code"], "station_not_connected");
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
        assert!(body["error"]["code"].is_string());
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
    assert!(body["error"]["code"].is_string());
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

/// An open SSE response: status line read, body read on demand.
struct Sse {
    stream: TcpStream,
    buf: String,
}

impl Sse {
    /// Sends `GET path` and reads the response head.
    async fn open(addr: &str, path: &str, headers: &[(&str, &str)]) -> (u16, Self) {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let mut request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n");
        for (name, value) in headers {
            let _ = write!(request, "{name}: {value}\r\n");
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut sse = Self {
            stream,
            buf: String::new(),
        };
        sse.read_until("\r\n\r\n").await;
        let status = sse.buf.split(' ').nth(1).unwrap().parse().unwrap();
        (status, sse)
    }

    /// Reads until the buffer holds `needle`; returns everything read so far.
    async fn read_until(&mut self, needle: &str) -> &str {
        tokio::time::timeout(WAIT, async {
            let mut chunk = [0_u8; 4096];
            while !self.buf.contains(needle) {
                let n = self.stream.read(&mut chunk).await.unwrap();
                assert!(n > 0, "stream closed; got {:?}", self.buf);
                self.buf.push_str(&String::from_utf8_lossy(&chunk[..n]));
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timeout waiting for {needle:?}; got {:?}", self.buf));
        &self.buf
    }

    /// The JSON objects of all `data:` lines read so far.
    fn data(&self) -> Vec<Value> {
        self.buf
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str(data).ok())
            .collect()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn event_stream_with_station_filter() {
    let csms = Csms::start(&[]);
    let (status, mut all) = Sse::open(&csms.api_addr, "/v1/events", &[]).await;
    assert_eq!(status, 200);
    assert!(
        all.buf
            .to_lowercase()
            .contains("content-type: text/event-stream")
    );
    let (_, mut only2) = Sse::open(&csms.api_addr, "/v1/events?station=CP-2", &[]).await;
    let (_, ghost) = Sse::open(&csms.api_addr, "/v1/events?station=GHOST", &[]).await;
    let _s1 = station(&csms.station_addr, "CP-1").await;
    let _s2 = station(&csms.station_addr, "CP-2").await;

    all.read_until("\"station\":\"CP-2\",\"ts\"").await;
    let text = all.buf.clone();
    assert!(text.contains("event: connected\ndata: {"), "{text}");
    assert!(text.contains("event: frame\n"), "{text}");
    assert!(text.contains("CP-1"));
    let connected: Vec<Value> = all
        .data()
        .into_iter()
        .filter(|event| event["type"] == "connected")
        .collect();
    assert!(connected.iter().any(|event| event["station"] == "CP-1"));
    assert!(connected.iter().all(|event| event["ts"].is_string()));

    only2.read_until("event: connected").await;
    only2.read_until("event: frame").await;
    assert!(!only2.buf.contains("CP-1"), "{}", only2.buf);
    assert!(only2.data().iter().all(|event| event["station"] == "CP-2"));

    // The ghost filter matches nothing.
    assert!(!ghost.buf.contains("data:"));
    let (status, body) = csms.get("/v1/events?bogus=1").await;
    assert_eq!(status, 400, "{body}");
    drop(ghost.stream);
}

async fn streamed_key_change(extra: &[&str], needle: &str) -> String {
    let csms = Csms::start(extra);
    let (_, mut sse) = Sse::open(&csms.api_addr, "/v1/events?station=CP-1", &[]).await;
    let _station = station(&csms.station_addr, "CP-1").await;
    let (status, body) = csms
        .post_call(
            "CP-1",
            &json!({"action": "ChangeConfiguration",
                "payload": {"key": "AuthorizationKey", "value": "supersecret12"}}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    sse.read_until(needle).await.to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn event_stream_redacts_the_authorization_key() {
    let hidden = streamed_key_change(&[], "***").await;
    assert!(hidden.contains("ChangeConfiguration"), "{hidden}");
    assert!(!hidden.contains("supersecret12"), "{hidden}");
    let shown = streamed_key_change(&["--show-secrets"], "supersecret12").await;
    assert!(shown.contains("supersecret12"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn event_stream_limit_is_sixteen_and_slots_are_freed() {
    let csms = Csms::start(&[]);
    let mut streams = Vec::new();
    for _ in 0..16 {
        let (status, sse) = Sse::open(&csms.api_addr, "/v1/events", &[]).await;
        assert_eq!(status, 200);
        streams.push(sse);
    }
    let (status, mut refused) = Sse::open(&csms.api_addr, "/v1/events", &[]).await;
    assert_eq!(status, 503);
    assert!(refused.read_until("}").await.contains("\"error\""));

    // A dropped client frees its slot (the server notices the closed socket).
    drop(streams.pop());
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let (status, sse) = Sse::open(&csms.api_addr, "/v1/events", &[]).await;
        if status == 200 {
            drop(sse);
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "slot never freed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn event_stream_uses_the_auth_rules() {
    let csms = Csms::start(&["--api-token", "tok"]);
    let (status, _) = Sse::open(&csms.api_addr, "/v1/events", &[]).await;
    assert_eq!(status, 401);
    let (status, _) = Sse::open(
        &csms.api_addr,
        "/v1/events",
        &[("Authorization", "Bearer tok")],
    )
    .await;
    assert_eq!(status, 200);
    let open = Csms::start(&[]);
    let (status, _) = Sse::open(&open.api_addr, "/v1/events", &[("Origin", "http://x")]).await;
    assert_eq!(status, 403);
}

fn heartbeat_time(
    outcome: Result<kirby_ocpp_core::station::Outcome, kirby_ocpp_core::StationError>,
) -> String {
    match outcome {
        Ok(kirby_ocpp_core::station::Outcome::Response { payload }) => {
            payload["currentTime"].as_str().unwrap().to_owned()
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rules_round_trip() {
    use kirby_ocpp_core::station::Command;
    let csms = Csms::start(&[]);
    let station = station(&csms.station_addr, "CP-1").await;
    let addr = &csms.api_addr;

    let (status, body) = csms.get("/v1/rules").await;
    assert_eq!((status, body), (200, json!({"rules": []})));
    let put = http(
        addr,
        "PUT",
        "/v1/rules/Heartbeat",
        &[],
        Some(r#"{"currentTime":"2030-01-01T00:00:00Z"}"#),
    )
    .await;
    assert_eq!(put.0, 200, "{}", put.1);
    assert_eq!(
        csms.get("/v1/rules").await.1,
        json!({"rules": ["Heartbeat"]})
    );
    assert_eq!(
        heartbeat_time(station.send(Command::Heartbeat).await),
        "2030-01-01T00:00:00Z"
    );
    let (status, body) = http(addr, "DELETE", "/v1/rules/Heartbeat", &[], None).await;
    assert_eq!((status, body["cleared"].as_bool()), (200, Some(true)));
    assert_eq!(csms.get("/v1/rules").await.1, json!({"rules": []}));
    assert_ne!(
        heartbeat_time(station.send(Command::Heartbeat).await),
        "2030-01-01T00:00:00Z"
    );
    let (_, body) = http(addr, "DELETE", "/v1/rules/Heartbeat", &[], None).await;
    assert_eq!(body["cleared"], false);

    for (path, body) in [
        ("/v1/rules/NoSuchAction", "{}"),
        ("/v1/rules/Reset", "{}"),
        ("/v1/rules/Heartbeat", "[]"),
        ("/v1/rules/Heartbeat", "not json"),
    ] {
        let (status, reply) = http(addr, "PUT", path, &[], Some(body)).await;
        assert_eq!(status, 400, "{path} {body}: {reply}");
        assert!(reply["error"]["code"].is_string());
    }
    let (_, reply) = http(addr, "PUT", "/v1/rules/Reset", &[], Some("{}")).await;
    assert_eq!(reply["error"]["code"], "unsupported_action");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("ocpp1.6")
    );
    assert_eq!(
        http(addr, "DELETE", "/v1/rules/Reset", &[], None).await.0,
        400
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn faults_round_trip() {
    use kirby_ocpp_core::station::Command;
    let csms = Csms::start(&[]);
    let station = station(&csms.station_addr, "CP-1").await;
    let addr = &csms.api_addr;

    let post = |body: &'static str| http(addr, "POST", "/v1/faults", &[], Some(body));
    let (status, body) = post(r#"{"action":"Heartbeat","kind":"call_error"}"#).await;
    assert_eq!(
        (status, body["remaining"].as_u64()),
        (200, Some(1)),
        "{body}"
    );
    assert_eq!(csms.get("/v1/faults").await.1, json!({"remaining": 1}));
    assert!(station.send(Command::Heartbeat).await.is_err());
    assert_eq!(csms.get("/v1/faults").await.1, json!({"remaining": 0}));
    assert!(station.send(Command::Heartbeat).await.is_ok());

    let (_, body) = post(r#"{"action":"Heartbeat","kind":"silence","times":3}"#).await;
    assert_eq!(body["remaining"], 3);
    let (status, body) = http(addr, "DELETE", "/v1/faults", &[], None).await;
    assert_eq!((status, body), (200, json!({"remaining": 0})));
    assert_eq!(csms.get("/v1/faults").await.1, json!({"remaining": 0}));
    assert!(station.send(Command::Heartbeat).await.is_ok());

    for body in [
        r#"{"action":"Heartbeat","kind":"nope"}"#,
        r#"{"action":"Heartbeat","kind":"close","times":0}"#,
        r#"{"action":"Heartbeat","kind":"invalid_payload"}"#,
        r#"{"action":"","kind":"close"}"#,
        r#"{"kind":"close"}"#,
        r#"{"action":"Heartbeat","kind":"close","extra":1}"#,
        "[]",
    ] {
        let (status, reply) = post(body).await;
        assert_eq!(status, 400, "{body}: {reply}");
        assert!(reply["error"]["code"].is_string());
    }
    assert_eq!(csms.get("/v1/faults").await.1, json!({"remaining": 0}));
}

/// A client that completes the WebSocket handshake and then never answers anything.
async fn silent_station(addr: &str, id: &str) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "GET /ocpp/{id} HTTP/1.1\r\nHost: {addr}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Sec-WebSocket-Protocol: ocpp1.6\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        tokio::time::timeout(WAIT, stream.read_exact(&mut byte))
            .await
            .expect("handshake response timeout")
            .unwrap();
        head.push(byte[0]);
    }
    assert!(String::from_utf8_lossy(&head).contains(" 101 "));
    stream
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn too_many_commands_in_flight_get_429() {
    let csms = Csms::start(&[]);
    let _silent = silent_station(&csms.station_addr, "CP-1").await;
    let deadline = tokio::time::Instant::now() + WAIT;
    while csms.get("/v1/stations/CP-1").await.0 != 200 {
        assert!(tokio::time::Instant::now() < deadline, "station not seen");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let pending: Vec<_> = (0..256)
        .map(|_| {
            let addr = csms.api_addr.clone();
            tokio::spawn(async move {
                // A probe below may hold a slot for a moment: retry on 429.
                loop {
                    let answer = http(
                        &addr,
                        "POST",
                        "/v1/stations/CP-1/calls",
                        &[],
                        Some(r#"{"action":"Reset","payload":{"type":"Soft"},"timeout_ms":60000}"#),
                    )
                    .await;
                    if answer.0 != 429 {
                        return answer;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
        })
        .collect();
    // A probe to an unknown station takes a slot and gives it back at once (404); the first
    // 429 proves all 256 slots are held by the silent station's calls.
    loop {
        let (status, body) = csms.post_call("GHOST", &json!({"action": "Reset"})).await;
        if status == 429 {
            assert!(body["error"]["code"].is_string());
            break;
        }
        assert_eq!(status, 404, "{body}");
        assert!(tokio::time::Instant::now() < deadline, "never saw 429");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drop(csms);
    for task in pending {
        task.abort();
    }
}

/// Sends `request` as is and returns the whole answer up to the close of the connection.
async fn raw_exchange(addr: &str, request: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(WAIT, stream.read_to_end(&mut raw))
        .await
        .expect("response timeout")
        .ok();
    String::from_utf8_lossy(&raw).into_owned()
}

fn status_of(answer: &str) -> u16 {
    answer
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_header_must_be_loopback_without_a_token() {
    let csms = Csms::start(&[]);
    let addr = &csms.api_addr;
    let port = addr.rsplit(':').next().unwrap();
    let get = |host: String| {
        let request = format!("GET /v1/stations HTTP/1.1\r\nHost: {host}\r\n\r\n");
        async move { status_of(&raw_exchange(addr, &request).await) }
    };
    for host in [
        format!("127.0.0.1:{port}"),
        "127.0.0.1".to_owned(),
        format!("localhost:{port}"),
        "LOCALHOST".to_owned(),
        format!("[::1]:{port}"),
        "[::1]".to_owned(),
        "[0:0:0:0:0:0:0:1]".to_owned(),
        format!("[0:0:0:0:0:0:0:1]:{port}"),
        "localhost.".to_owned(),
        format!("Localhost.:{port}"),
    ] {
        assert_eq!(get(host.clone()).await, 200, "{host}");
    }
    for host in [
        format!("evil.example:{port}"),
        "evil.example".to_owned(),
        format!("127.0.0.1.evil.example:{port}"),
        format!("localhost.evil.example:{port}"),
        "127.0.0.1:99999999".to_owned(),
        "127.0.0.1:".to_owned(),
        "[::1".to_owned(),
        "[::2]".to_owned(),
        "192.168.1.5".to_owned(),
        "[::ffff:192.168.1.5]".to_owned(),
        "localhost..".to_owned(),
    ] {
        assert_eq!(get(host.clone()).await, 403, "{host}");
    }
    let refused = raw_exchange(
        addr,
        "GET /v1/stations HTTP/1.1\r\nHost: evil.example\r\n\r\n",
    )
    .await;
    assert!(
        refused.contains("127.0.0.1") && refused.contains("[::1]") && refused.contains("localhost"),
        "{refused}"
    );
    // Same-origin requests carry no Origin, but a cross-site fetch says so.
    let (status, body) = http(
        addr,
        "GET",
        "/v1/stations",
        &[("Sec-Fetch-Site", "cross-site")],
        None,
    )
    .await;
    assert_eq!(status, 403, "{body}");
    let (status, _) = http(
        addr,
        "GET",
        "/v1/stations",
        &[("Sec-Fetch-Site", "same-origin")],
        None,
    )
    .await;
    assert_eq!(status, 200);

    // With a token the bearer is the proof; the Host does not matter.
    let tokened = Csms::start(&["--api-token", "tok"]);
    let request = "GET /v1/health HTTP/1.1\r\nHost: api.internal\r\n\
                   Authorization: Bearer tok\r\n\r\n";
    assert_eq!(
        status_of(&raw_exchange(&tokened.api_addr, request).await),
        200
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authorization_header_parsing_is_strict() {
    let csms = Csms::start(&["--api-token", "tok"]);
    let addr = &csms.api_addr;
    let status = |headers: &'static str| {
        let request = format!("GET /v1/health HTTP/1.1\r\nHost: {addr}\r\n{headers}\r\n");
        async move { status_of(&raw_exchange(addr, &request).await) }
    };
    assert_eq!(status("Authorization: bearer tok\r\n").await, 200);
    assert_eq!(status("Authorization: BEARER tok\r\n").await, 200);
    assert_eq!(status("Authorization: Bearer  tok\r\n").await, 401);
    assert_eq!(status("Authorization: Bearer\ttok\r\n").await, 401);
    // Two headers are ambiguous, even when the first one is right.
    assert_eq!(
        status("Authorization: Bearer tok\r\nAuthorization: Bearer tok\r\n").await,
        401
    );
    assert_eq!(
        status("Authorization: Bearer nope\r\nAuthorization: Bearer tok\r\n").await,
        401
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn head_and_options_are_answered() {
    let csms = Csms::start(&[]);
    let addr = &csms.api_addr;
    let head = raw_exchange(
        addr,
        &format!("HEAD /v1/health HTTP/1.1\r\nHost: {addr}\r\n\r\n"),
    )
    .await;
    assert_eq!(status_of(&head), 200, "{head}");
    assert!(head.ends_with("\r\n\r\n"), "no body for HEAD: {head:?}");
    let options = raw_exchange(
        addr,
        &format!("OPTIONS /v1/health HTTP/1.1\r\nHost: {addr}\r\n\r\n"),
    )
    .await;
    assert_eq!(status_of(&options), 204, "{options}");
    assert!(
        options.to_lowercase().contains("allow: get, head, options"),
        "{options}"
    );
    // Every answer closes the connection.
    assert!(options.to_lowercase().contains("connection: close"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chunked_body_over_the_limit_is_413() {
    let csms = Csms::start(&[]);
    let addr = &csms.api_addr;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST /v1/stations/CP-1/calls HTTP/1.1\r\nHost: {addr}\r\n\
         Transfer-Encoding: chunked\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    let chunk = "a".repeat(64 * 1024);
    for _ in 0..17 {
        let frame = format!("{:x}\r\n{chunk}\r\n", chunk.len());
        if stream.write_all(frame.as_bytes()).await.is_err() {
            break;
        }
    }
    let _ = stream.write_all(b"0\r\n\r\n").await;
    let mut raw = Vec::new();
    let _ = tokio::time::timeout(WAIT, stream.read_to_end(&mut raw)).await;
    let answer = String::from_utf8_lossy(&raw);
    assert_eq!(status_of(&answer), 413, "{answer}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn limit_is_percent_decoded_like_station() {
    let csms = Csms::start(&[]);
    let _station = station(&csms.station_addr, "CP-1").await;
    let (status, body) = csms.get("/v1/stations/CP-1/history?limit=%31").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["frames"].as_array().unwrap().len(), 1);
    let (status, _) = csms.get("/v1/stations/CP-1/history?limit=%zz").await;
    assert_eq!(status, 400);
    let (status, _) = csms.get("/v1/stations/CP-1/history?limit=-1").await;
    assert_eq!(status, 400);
    let (status, _) = csms.get("/v1/stations/CP-1/history?limit=%2B5").await;
    assert_eq!(status, 400);
    let (status, _) = csms.get("/v1/stations/CP-1/history?limit=").await;
    assert_eq!(status, 400);
    let (status, body) = csms.get("/v1/stations/CP-1/history?station=x").await;
    assert_eq!(status, 400);
    assert!(body["error"]["message"].as_str().unwrap().contains("limit"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fault_action_names_are_checked() {
    let csms = Csms::start(&[]);
    let addr = &csms.api_addr;
    let long = "a".repeat(129);
    for action in [
        String::new(),
        "Heart\\nbeat".to_owned(),
        "Heart\\u001b[31mbeat".to_owned(),
        long,
    ] {
        let body = format!(r#"{{"action":"{action}","kind":"silence"}}"#);
        let (status, reply) = http(addr, "POST", "/v1/faults", &[], Some(&body)).await;
        assert_eq!(status, 400, "{action:?}: {reply}");
        assert_eq!(reply["error"]["code"], "bad_request");
    }
    let (status, reply) = http(
        addr,
        "POST",
        "/v1/faults",
        &[],
        Some(r#"{"action":"NoSuchAction","kind":"silence"}"#),
    )
    .await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(csms.get("/v1/faults").await.1, json!({"remaining": 1}));
}

#[tokio::test]
async fn fault_sizes_and_counts_are_bounded() {
    let csms = Csms::start(&[]);
    let addr = &csms.api_addr;
    let post =
        |body: String| async move { http(addr, "POST", "/v1/faults", &[], Some(&body)).await };
    // `times` that would overflow the counter sum.
    for _ in 0..2 {
        let (status, body) = post(
            r#"{"action":"Heartbeat","kind":"silence","times":18446744073709551615}"#.to_owned(),
        )
        .await;
        assert_eq!(status, 400, "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("at most")
        );
    }
    let (status, body) =
        post(r#"{"action":"Heartbeat","kind":"silence","times":1000000}"#.to_owned()).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["remaining"], 1_000_000);
    http(addr, "DELETE", "/v1/faults", &[], None).await;

    for _ in 0..1000 {
        let (status, _) = post(r#"{"action":"Heartbeat","kind":"silence"}"#.to_owned()).await;
        assert_eq!(status, 200);
    }
    let (status, body) = post(r#"{"action":"Heartbeat","kind":"silence"}"#.to_owned()).await;
    assert_eq!(status, 409, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("too many faults")
    );
    assert_eq!(csms.get("/v1/faults").await.1, json!({"remaining": 1000}));
    http(addr, "DELETE", "/v1/faults", &[], None).await;

    // Byte budget: 8 payloads of ~900 KiB do not all fit in 8 MiB.
    let big = "a".repeat(900 * 1024);
    let body = json!({"action": "Heartbeat", "kind": "invalid_payload", "payload": {"x": big}})
        .to_string();
    let mut last = 0;
    for _ in 0..10 {
        last = http(addr, "POST", "/v1/faults", &[], Some(&body)).await.0;
        if last != 200 {
            break;
        }
    }
    assert_eq!(last, 409);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_clients_and_connection_flood_are_cut() {
    let csms = Csms::start(&[]);
    let addr = &csms.api_addr;

    // A head that never completes is cut after about 10 s.
    let mut slow = TcpStream::connect(addr).await.unwrap();
    slow.write_all(b"GET /v1/health HTTP/1.1\r\nHost: x")
        .await
        .unwrap();
    // A complete head with a body that never arrives gets 408.
    let mut stalled = TcpStream::connect(addr).await.unwrap();
    stalled
        .write_all(
            format!(
                "POST /v1/stations/CP-1/calls HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 50\r\n\r\n{{"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let started = tokio::time::Instant::now();
    let mut raw = Vec::new();
    tokio::time::timeout(WAIT, slow.read_to_end(&mut raw))
        .await
        .expect("slow head not cut")
        .ok();
    assert!(started.elapsed() >= Duration::from_secs(8), "cut too early");
    let mut raw = Vec::new();
    tokio::time::timeout(WAIT, stalled.read_to_end(&mut raw))
        .await
        .expect("stalled body not cut")
        .ok();
    assert_eq!(status_of(&String::from_utf8_lossy(&raw)), 408);

    // More than 512 idle connections: the extra ones are closed at once.
    let mut open = Vec::new();
    for _ in 0..512 {
        match TcpStream::connect(addr).await {
            Ok(stream) => open.push(stream),
            Err(error) if is_fd_limit(&error) => {
                eprintln!("skipping the connection flood: descriptor limit reached ({error})");
                return;
            }
            Err(error) => panic!("connect: {error}"),
        }
    }
    // Give the accept loop time to take all of them.
    let mut extra = TcpStream::connect(addr).await.unwrap();
    let mut buf = [0_u8; 16];
    let read = tokio::time::timeout(Duration::from_secs(5), extra.read(&mut buf))
        .await
        .expect("extra connection stays open");
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    drop(open);
    assert_eq!(csms.get("/v1/health").await.0, 200);
}

/// `EMFILE` ("Too many open files"): a low `ulimit -n` on the runner, not a product failure.
fn is_fd_limit(error: &std::io::Error) -> bool {
    error
        .to_string()
        .to_lowercase()
        .contains("too many open files")
}

/// Reads until the peer closes or `limit` passes; whether it was still open at the end.
async fn open_after(stream: &mut TcpStream, limit: Duration) -> bool {
    let mut buf = [0_u8; 4096];
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        match tokio::time::timeout_at(deadline, stream.read(&mut buf)).await {
            Err(_) => return true,
            Ok(Ok(0) | Err(_)) => return false,
            Ok(Ok(_)) => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn head_timer_only_stops_for_a_real_request() {
    let csms = Csms::start(&[]);
    let addr = &csms.api_addr;
    let started = tokio::time::Instant::now();

    // Blank lines before the request line do not count as a head.
    let blank = async {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(b"\r\n\r\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        stream.write_all(b"GET /v1/he").await.unwrap();
        let open = open_after(&mut stream, WAIT).await;
        (open, started.elapsed())
    };
    // A complete head with a bare-LF line and a `\n\r\n` end still stops the timer: the
    // event stream is not cut after 10 s of silence.
    let sloppy = async {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = format!("GET /v1/events HTTP/1.1\nHost: {addr}\r\n\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        open_after(&mut stream, Duration::from_secs(13)).await
    };
    // A call that outlasts the head timeout is not cut either.
    let _silent = silent_station(&csms.station_addr, "CP-1").await;
    let long_call = async {
        csms.post_call(
            "CP-1",
            &json!({"action": "Reset", "payload": {"type": "Soft"}, "timeout_ms": 12000}),
        )
        .await
    };
    let ((blank_open, cut_after), sloppy_open, (status, body)) =
        tokio::join!(blank, sloppy, long_call);
    assert!(!blank_open, "blank lines kept the connection open");
    assert!(cut_after >= Duration::from_secs(8), "cut too early");
    assert!(sloppy_open, "the event stream was cut");
    assert_eq!(status, 200, "{body}");
    assert!(
        started.elapsed() >= Duration::from_secs(12),
        "the call returned early: {body}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn event_streams_get_the_closing_events_on_shutdown() {
    let mut csms = Csms::start(&[]);
    let (status, mut sse) = Sse::open(&csms.api_addr, "/v1/events", &[]).await;
    assert_eq!(status, 200);
    let _station = station(&csms.station_addr, "CP-1").await;
    sse.read_until("\"type\":\"connected\"").await;
    let killed = Command::new("kill")
        .args(["-TERM", &csms.child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    // The stream ends by itself, after the `disconnected` event of the shutdown.
    tokio::time::timeout(WAIT, async {
        let mut chunk = [0_u8; 4096];
        loop {
            let n = sse.stream.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            sse.buf.push_str(&String::from_utf8_lossy(&chunk[..n]));
        }
    })
    .await
    .expect("stream did not end");
    assert!(
        sse.buf.contains("event: disconnected") && sse.buf.contains("shutdown"),
        "{}",
        sse.buf
    );
    let _ = csms.child.wait();
}

#[test]
fn api_token_must_be_visible_ascii() {
    for token in ["has space", "tab\there", "naïve", "a\u{7f}b"] {
        let (code, stderr) = run_to_end(&["--api-listen", "127.0.0.1:0", "--api-token", token]);
        assert_eq!(code, 2, "{token:?}: {stderr}");
        assert!(stderr.contains("visible ASCII"), "{token:?}: {stderr}");
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("token");
    std::fs::write(&path, "two words\n").unwrap();
    let (code, stderr) = run_to_end(&[
        "--api-listen",
        "127.0.0.1:0",
        "--api-token-file",
        path.to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "{stderr}");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shared_token_file_gets_a_warning_and_the_env_token_conflicts_with_the_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("token");
    std::fs::write(&path, "tok\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut child = command(&[
        "--listen",
        "127.0.0.1:0",
        "--api-listen",
        "127.0.0.1:0",
        "--api-token-file",
        path.to_str().unwrap(),
        "--json",
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut lines = BufReader::new(stderr).lines();
    let line = tokio::task::spawn_blocking(move || lines.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        line.contains("warning") && line.contains("chmod 600"),
        "{line}"
    );

    let output = command(&[
        "--api-listen",
        "127.0.0.1:0",
        "--api-token-file",
        path.to_str().unwrap(),
    ])
    .env("KIRBY_CSMS_API_TOKEN", "from-env")
    .stdin(Stdio::null())
    .output()
    .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn more_than_one_host_header_is_refused() {
    let csms = Csms::start(&[]);
    let addr = &csms.api_addr;
    let request = format!(
        "GET /v1/health HTTP/1.1\r\nHost: {addr}\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    );
    let answer = raw_exchange(addr, &request).await;
    assert_eq!(status_of(&answer), 400, "{answer}");
    assert!(answer.contains("Host"), "{answer}");
    // Also with a token configured.
    let csms = Csms::start(&["--api-token", "t0ken"]);
    let addr = &csms.api_addr;
    let request = format!(
        "GET /v1/health HTTP/1.1\r\nHost: {addr}\r\nHost: {addr}\r\nAuthorization: Bearer t0ken\r\n\
         Connection: close\r\n\r\n"
    );
    assert_eq!(status_of(&raw_exchange(addr, &request).await), 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn early_errors_reach_a_client_that_is_still_sending_its_body() {
    let csms = Csms::start(&["--api-token", "t0ken"]);
    let addr = &csms.api_addr;
    // 401 (no token) and 413 (declared too large) are answered before the body is read.
    for (headers, want) in [("", 401), ("Authorization: Bearer t0ken\r\n", 413)] {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let declared = if want == 413 {
            2 * 1024 * 1024
        } else {
            64 * 1024
        };
        let head = format!(
            "POST /v1/stations/CP-1/calls HTTP/1.1\r\nHost: {addr}\r\n{headers}\
             Content-Length: {declared}\r\n\r\n"
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        let chunk = [b'a'; 4096];
        for _ in 0..16 {
            if stream.write_all(&chunk).await.is_err() {
                break;
            }
        }
        let mut raw = Vec::new();
        let read = tokio::time::timeout(WAIT, stream.read_to_end(&mut raw))
            .await
            .expect("response timeout");
        let answer = String::from_utf8_lossy(&raw);
        assert!(read.is_ok(), "reset instead of an answer: {read:?}");
        assert_eq!(status_of(&answer), want, "{answer}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connected_event_ids_and_string_transaction_ids() {
    use kirby_ocpp_core::ids::{ConnectorId, IdTag};
    let csms = Csms::start(&[]);
    let (_, mut events) = Sse::open(&csms.api_addr, "/v1/events", &[]).await;
    let station = station(&csms.station_addr, "CP-1").await;
    station
        .start_transaction(ConnectorId::new(1), IdTag::new("TAG").unwrap())
        .await
        .unwrap();
    events.read_until("\"active_transactions\":[{").await;

    let data = events.data();
    let connected = data.iter().find(|e| e["type"] == "connected").unwrap();
    assert_eq!(connected["protocol"], "ocpp1.6");
    assert!(
        connected["remote_addr"]
            .as_str()
            .unwrap()
            .starts_with("127.0.0.1:")
    );
    // Every message has an increasing numeric `id:`.
    // Only whole lines count: a read can end in the middle of the last one.
    let ids: Vec<u64> = events
        .buf
        .split_inclusive('\n')
        .filter_map(|line| line.strip_suffix('\n'))
        .filter_map(|line| line.strip_prefix("id: "))
        .map(|id| id.parse().unwrap())
        .collect();
    assert!(ids.len() >= 3, "{}", events.buf);
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
    // The `state` event carries the transaction id as a string.
    let state = data
        .iter()
        .find(|e| e["type"] == "state" && e["active_transactions"][0].is_object())
        .unwrap();
    assert!(state["active_transactions"][0]["id"].is_string(), "{state}");

    let (_, list) = csms.get("/v1/stations").await;
    assert!(
        list["stations"][0]["active_transactions"][0]["id"].is_string(),
        "{list}"
    );
    let (_, detail) = csms.get("/v1/stations/CP-1").await;
    assert!(
        detail["active_transactions"][0]["id"].is_string(),
        "{detail}"
    );
    assert!(
        detail["state"]["transactions"][0]["id"].is_string(),
        "{detail}"
    );
}

/// The sorted top-level keys of a JSON object.
fn keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap_or_else(|| panic!("not an object: {value}"))
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

/// The value of the `Allow` header of a raw answer, if any (header name case-insensitive).
fn allow_of(answer: &str) -> Option<String> {
    let head = answer.split("\r\n\r\n").next().unwrap_or_default();
    head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("allow")
            .then(|| value.trim().to_owned())
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn allow_header_is_set_on_405_and_options_only() {
    let csms = Csms::start(&[]);
    let addr = &csms.api_addr;
    let exchange = |method: &'static str, path: &'static str, extra: &'static str| async move {
        raw_exchange(
            addr,
            &format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\n{extra}\r\n"),
        )
        .await
    };
    // 405: same format as OPTIONS, sorted, HEAD and OPTIONS included.
    let answer = exchange("POST", "/v1/health", "").await;
    assert_eq!(status_of(&answer), 405, "{answer}");
    assert_eq!(allow_of(&answer).as_deref(), Some("GET, HEAD, OPTIONS"));
    let answer = exchange("PATCH", "/v1/faults", "").await;
    assert_eq!(
        allow_of(&answer).as_deref(),
        Some("DELETE, GET, HEAD, OPTIONS, POST")
    );
    let answer = exchange("GET", "/v1/stations/X/calls", "").await;
    assert_eq!(status_of(&answer), 405, "{answer}");
    assert_eq!(allow_of(&answer).as_deref(), Some("OPTIONS, POST"));
    // 204 for OPTIONS: identical value.
    let answer = exchange("OPTIONS", "/v1/health", "").await;
    assert_eq!(status_of(&answer), 204, "{answer}");
    assert_eq!(allow_of(&answer).as_deref(), Some("GET, HEAD, OPTIONS"));
    let answer = exchange("OPTIONS", "/v1/rules/Heartbeat", "").await;
    assert_eq!(allow_of(&answer).as_deref(), Some("DELETE, OPTIONS, PUT"));
    // Other statuses carry none: 200, 404 and the refusals of the auth layer.
    for (method, path, extra, status) in [
        ("GET", "/v1/health", "", 200),
        ("GET", "/v1/nope", "", 404),
        ("OPTIONS", "/v1/nope", "", 404),
        ("OPTIONS", "/v1", "Host: evil.com\r\n", 400),
        ("POST", "/v1/health", "Origin: http://x\r\n", 403),
    ] {
        let answer = exchange(method, path, extra).await;
        if status == 400 {
            continue; // two Host headers: only checks that nothing panics
        }
        assert_eq!(status_of(&answer), status, "{method} {path}: {answer}");
        assert_eq!(allow_of(&answer), None, "{method} {path}: {answer}");
    }
    let answer = raw_exchange(addr, "OPTIONS /v1 HTTP/1.1\r\nHost: evil.com\r\n\r\n").await;
    assert_eq!(status_of(&answer), 403, "{answer}");
    assert_eq!(allow_of(&answer), None, "{answer}");

    // With a token: 401 on an unauthenticated OPTIONS, 405/204 once authenticated.
    let csms = Csms::start(&["--api-token", "sekret"]);
    let addr = &csms.api_addr;
    for (method, path) in [("OPTIONS", "/v1/health"), ("POST", "/v1/health")] {
        let answer = raw_exchange(
            addr,
            &format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n"),
        )
        .await;
        assert_eq!(status_of(&answer), 401, "{answer}");
        assert_eq!(allow_of(&answer), None, "{answer}");
    }
    let answer = raw_exchange(
        addr,
        &format!(
            "POST /v1/health HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer sekret\r\n\r\n"
        ),
    )
    .await;
    assert_eq!(status_of(&answer), 405, "{answer}");
    assert_eq!(allow_of(&answer).as_deref(), Some("GET, HEAD, OPTIONS"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_query_parameters_are_refused() {
    let csms = Csms::start(&[]);
    let _station = station(&csms.station_addr, "CP-1").await;
    for path in [
        "/v1/stations/CP-1/history?limit=1&limit=2",
        "/v1/events?station=a&station=b",
    ] {
        let (status, body) = csms.get(path).await;
        assert_eq!(status, 400, "{path}: {body}");
        assert_eq!(body["error"]["code"], "bad_request");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("limit") || message.contains("station"),
            "{message}"
        );
    }
    let (status, _) = csms.get("/v1/stations/CP-1/history?limit=1").await;
    assert_eq!(status, 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn listed_methods_are_the_routers_methods() {
    let csms = Csms::start(&[]);
    let (_, index) = csms.get("/v1").await;
    for resource in index["resources"].as_array().unwrap() {
        let path = resource["path"]
            .as_str()
            .unwrap()
            .replace("{id}", "X")
            .replace("{action}", "Heartbeat");
        let listed: Vec<&str> = resource["methods"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m.as_str().unwrap())
            .collect();
        for method in ["GET", "POST", "PUT", "DELETE"] {
            if listed.contains(&method) {
                // The event stream never ends; the 405 check of the others covers the route.
                if method == "GET" && path == "/v1/events" {
                    continue;
                }
                let (status, body) = http(&csms.api_addr, method, &path, &[], Some("{}")).await;
                assert_ne!(status, 405, "{method} {path} is listed but not routed");
                assert_ne!(
                    body["error"]["code"], "not_found",
                    "{method} {path} is listed but not routed"
                );
                continue;
            }
            let (status, body) = http(&csms.api_addr, method, &path, &[], Some("{}")).await;
            assert_eq!(
                status, 405,
                "{method} {path} is routed but not listed: {body}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn golden_key_sets_of_the_shapes() {
    const STATION: [&str; 10] = [
        "active_transactions",
        "connected",
        "connectors",
        "id",
        "model",
        "protocol",
        "received_calls",
        "remote_addr",
        "security_events",
        "vendor",
    ];
    let csms = Csms::start(&[]);
    let (_, mut events) = Sse::open(&csms.api_addr, "/v1/events", &[]).await;

    let (_, health) = csms.get("/v1/health").await;
    assert_eq!(keys(&health), ["api_version", "ok", "stations", "version"]);
    let (_, index) = csms.get("/v1").await;
    assert_eq!(keys(&index), ["api_version", "resources", "version"]);
    for resource in index["resources"].as_array().unwrap() {
        assert_eq!(keys(resource), ["methods", "path"]);
    }

    // A station that never booted: every key is there, unknown values are null or empty.
    let _silent = silent_station(&csms.station_addr, "RAW-1").await;
    events.read_until("\"station\":\"RAW-1\"").await;
    let (_, list) = csms.get("/v1/stations").await;
    let raw = &list["stations"][0];
    assert_eq!(keys(raw), STATION, "{raw}");
    assert_eq!(raw["vendor"], Value::Null);
    assert_eq!(raw["model"], Value::Null);
    assert!(raw["remote_addr"].is_string());
    let (_, detail) = csms.get("/v1/stations/RAW-1").await;
    let mut with_state = STATION.to_vec();
    with_state.push("state");
    with_state.sort_unstable();
    assert_eq!(keys(&detail), with_state, "{detail}");

    // A booted one.
    let _booted = station(&csms.station_addr, "CP-1").await;
    let (_, detail) = csms.get("/v1/stations/CP-1").await;
    assert_eq!(keys(&detail), with_state, "{detail}");
    assert!(detail["vendor"].is_string() && detail["model"].is_string());
    let (_, list) = csms.get("/v1/stations").await;
    for item in list["stations"].as_array().unwrap() {
        assert_eq!(keys(item), STATION, "{item}");
    }
    let (_, no_state) = csms.get("/v1/stations/NOPE").await;
    assert_eq!(keys(&no_state), ["error"]);
    assert_eq!(keys(&no_state["error"]), ["code", "message"]);

    // `state` (as serialized by the engine) keeps its keys, with explicit nulls.
    assert_eq!(
        keys(&detail["state"]),
        [
            "boot",
            "certificate_request",
            "connectors",
            "diagnostics_status",
            "failed_calls",
            "firmware_status",
            "installed_certificates",
            "last_heartbeat_at",
            "log_status",
            "security_events",
            "security_events_total",
            "signed_firmware_status",
            "transactions",
            "unknown_stops",
        ]
    );
    assert_eq!(detail["state"]["log_status"], Value::Null);

    // History frames and events.
    let (_, history) = csms.get("/v1/stations/CP-1/history").await;
    assert_eq!(keys(&history), ["frames", "station"]);
    let frame = &history["frames"][0];
    assert_eq!(
        keys(frame),
        [
            "action",
            "direction",
            "id",
            "kind",
            "payload",
            "station",
            "ts",
            "type"
        ]
    );
    events.read_until("event: state").await;
    let data = events.data();
    let by_type = |kind: &str| data.iter().find(|event| event["type"] == kind).unwrap();
    assert_eq!(
        keys(by_type("connected")),
        ["protocol", "remote_addr", "station", "ts", "type"]
    );
    assert_eq!(
        keys(by_type("state")),
        ["active_transactions", "connectors", "station", "ts", "type"]
    );
    let call = data
        .iter()
        .find(|event| event["type"] == "frame" && event["kind"] == "call")
        .unwrap();
    assert_eq!(keys(call), keys(frame));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hyper_answers_before_the_router_have_an_empty_body() {
    let csms = Csms::start(&[]);
    let addr = &csms.api_addr;
    let cases = [
        ("GARBAGE\r\n\r\n".to_owned(), 400),
        (
            // Far more headers than hyper accepts (its limit is 100): the answer is the same
            // on every platform, unlike a single huge header that depends on socket buffering.
            format!(
                "GET /v1 HTTP/1.1\r\nHost: {addr}\r\n{}\r\n",
                "X-H: a\r\n".repeat(300)
            ),
            431,
        ),
    ];
    for (request, status) in cases {
        let answer = raw_exchange(addr, &request).await;
        assert_eq!(status_of(&answer), status, "{answer:.200}");
        let (head, body) = answer.split_once("\r\n\r\n").unwrap();
        assert!(body.is_empty(), "{answer:.200}");
        assert!(!head.to_lowercase().contains("application/json"), "{head}");
    }
}
