//! OCPP 2.0.1 through the real `kirby-csms` binary: a raw 2.0.1 station client talks to the
//! listener, the checks read the stdin answers, the JSON lines and the HTTP API.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const WAIT: Duration = Duration::from_secs(30);

/// The policy of every test here, so none depends on the default.
const POLICY: &str = "require:ocpp1.6,ocpp2.0.1";

const ENV_VARS: &[&str] = &[
    "KIRBY_CSMS_LISTEN",
    "KIRBY_CSMS_SUBPROTOCOL",
    "KIRBY_CSMS_TRANSACTION_IDS",
    "KIRBY_CSMS_HISTORY_CAP",
    "KIRBY_CSMS_VALIDATE_INBOUND",
    "KIRBY_CSMS_CALL_TIMEOUT",
    "KIRBY_CSMS_JSON",
    "KIRBY_CSMS_STDIN",
    "KIRBY_CSMS_SCRIPT",
    "KIRBY_CSMS_NO_TUI",
    "KIRBY_CSMS_SECURITY_PROFILE",
    "KIRBY_CSMS_API_LISTEN",
    "KIRBY_CSMS_API_TOKEN",
];

/// A running `kirby-csms` with the station listener, the API and stdin commands.
struct Csms {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    seen: Vec<String>,
    station_addr: String,
    api_addr: String,
}

impl Csms {
    /// Starts the server with the API and waits for both `listening` lines.
    fn start() -> Self {
        let mut csms = Self::spawn(&["--api-listen", "127.0.0.1:0"]);
        csms.station_addr = csms.addr_of("listening");
        csms.api_addr = csms.addr_of("api_listening");
        csms
    }

    /// The `addr` of the next line of type `kind`.
    fn addr_of(&mut self, kind: &str) -> String {
        let line = self.wait(|v| v["type"] == kind);
        line["addr"].as_str().unwrap().to_owned()
    }

    fn spawn(extra: &[&str]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_kirby-csms"));
        for var in ENV_VARS {
            command.env_remove(var);
        }
        command.args(["--listen", "127.0.0.1:0", "--json", "--subprotocol", POLICY]);
        command.args(extra);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
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
        Self {
            child,
            stdin,
            lines,
            seen: Vec::new(),
            station_addr: String::new(),
            api_addr: String::new(),
        }
    }

    /// Starts a replay run (no API) and waits for the `listening` line.
    fn start_replay(path: &str) -> Self {
        let mut csms = Self::spawn(&["--replay", path, "--timeout", "30"]);
        csms.station_addr = csms.addr_of("listening");
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
                Err(error) => panic!("no matching line ({error:?}); output: {:#?}", self.seen),
            }
        }
    }

    fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    /// The `stations` stdin answer, as a list.
    fn stations(&mut self) -> Vec<Value> {
        self.send(r#"{"cmd":"stations"}"#);
        let answer = self.wait(|v| v["type"] == "stations");
        answer["stations"].as_array().unwrap().clone()
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        http(&self.api_addr, "GET", path, None).await
    }

    /// Waits for the process to end and returns its exit code.
    fn finish(mut self) -> i32 {
        self.stdin = None;
        loop {
            match self.lines.recv_timeout(WAIT) {
                Ok(line) => self.seen.push(line),
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => panic!("process did not exit: {:#?}", self.seen),
            }
        }
        self.child.wait().unwrap().code().unwrap_or(-1)
    }
}

impl Drop for Csms {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One request with `Connection: close`; returns the status and the JSON body.
async fn http(addr: &str, method: &str, path: &str, body: Option<&str>) -> (u16, Value) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if let Some(body) = body {
        let _ = write!(request, "Content-Length: {}\r\n", body.len());
    }
    request.push_str("\r\n");
    request.push_str(body.unwrap_or_default());
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    timeout(WAIT, stream.read_to_end(&mut raw))
        .await
        .expect("response timeout")
        .unwrap();
    let text = String::from_utf8(raw).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").expect("complete response");
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A raw station: plain OCPP-J frames over a WebSocket.
struct Raw {
    socket: Socket,
    /// The subprotocol the server picked.
    negotiated: String,
    next: u32,
}

impl Raw {
    async fn connect(csms: &Csms, id: &str, offered: &str) -> Self {
        let mut request = format!("ws://{}/ocpp/{id}", csms.station_addr)
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", offered.parse().unwrap());
        let (socket, response) = timeout(WAIT, tokio_tungstenite::connect_async(request))
            .await
            .expect("handshake in time")
            .expect("handshake");
        let negotiated = response
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        Self {
            socket,
            negotiated,
            next: 0,
        }
    }

    async fn send(&mut self, frame: &Value) {
        self.socket
            .send(Message::text(frame.to_string()))
            .await
            .unwrap();
    }

    async fn recv(&mut self) -> Value {
        loop {
            match timeout(WAIT, self.socket.next())
                .await
                .expect("frame in time")
            {
                Some(Ok(Message::Text(text))) => {
                    return serde_json::from_str(text.as_str()).unwrap();
                }
                Some(Ok(_)) => {}
                other => panic!("socket ended: {other:?}"),
            }
        }
    }

    /// Sends a CALL and returns the answer frame.
    async fn call(&mut self, action: &str, payload: Value) -> Value {
        self.next += 1;
        let id = format!("m{}", self.next);
        self.send(&json!([2, id, action, payload])).await;
        let answer = self.recv().await;
        assert_eq!(answer[1], id, "answer to another message: {answer}");
        answer
    }

    /// Waits for a CALL of the server, answers it with `reply`, returns the action.
    async fn serve(&mut self, reply: Value) -> (String, Value) {
        let frame = self.recv().await;
        assert_eq!(frame[0], 2, "expected a CALL: {frame}");
        self.send(&json!([3, frame[1], reply])).await;
        (frame[2].as_str().unwrap().to_owned(), frame[3].clone())
    }

    async fn boot_201(&mut self) {
        let answer = self
            .call(
                "BootNotification",
                json!({"reason": "PowerUp", "chargingStation": {"vendorName": "Vend", "model": "Mod"}}),
            )
            .await;
        assert_eq!(answer[0], 3, "{answer}");
    }

    async fn boot_16(&mut self) {
        let answer = self
            .call(
                "BootNotification",
                json!({"chargePointVendor": "Vend16", "chargePointModel": "Mod16"}),
            )
            .await;
        assert_eq!(answer[0], 3, "{answer}");
    }
}

const NOW: &str = "2026-01-01T00:00:00Z";

fn transaction_event(kind: &str, seq: u32) -> Value {
    json!({
        "eventType": kind, "timestamp": NOW, "triggerReason": "Authorized", "seqNo": seq,
        "transactionInfo": {"transactionId": "tx-1"},
        "evse": {"id": 1, "connectorId": 1},
        "idToken": {"idToken": "TAG", "type": "ISO14443"}
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines, reason = "one session through every surface")]
async fn an_ocpp_2_0_1_station_shows_up_in_stdin_the_api_and_calls() {
    let mut csms = Csms::start();
    let mut cp = Raw::connect(&csms, "CP-201", "ocpp2.0.1").await;
    assert_eq!(cp.negotiated, "ocpp2.0.1");
    let mut old = Raw::connect(&csms, "CP-16", "ocpp1.6").await;
    assert_eq!(old.negotiated, "ocpp1.6");

    cp.boot_201().await;
    old.boot_16().await;
    let answer = cp
        .call(
            "StatusNotification",
            json!({"timestamp": NOW, "connectorStatus": "Available", "evseId": 1, "connectorId": 1}),
        )
        .await;
    assert_eq!(answer[0], 3, "{answer}");
    let answer = cp
        .call("TransactionEvent", transaction_event("Started", 0))
        .await;
    assert_eq!(answer[0], 3, "{answer}");
    let state = csms.wait(|v| {
        v["type"] == "state" && v["station"] == "CP-201" && v["active_transactions"][0].is_object()
    });
    assert_eq!(
        state["active_transactions"][0],
        json!({"id": "tx-1", "connector": 1, "evse": 1})
    );
    assert_eq!(state["connectors"], json!({"1/1": "Available"}));

    // The stdin list.
    let list = csms.stations();
    let summary = list.iter().find(|s| s["id"] == "CP-201").unwrap();
    assert_eq!(summary["protocol"], "ocpp2.0.1");
    assert_eq!(summary["vendor"], "Vend");
    assert_eq!(summary["model"], "Mod");
    assert_eq!(summary["connectors"], json!({"1/1": "Available"}));
    let legacy = list.iter().find(|s| s["id"] == "CP-16").unwrap();
    assert_eq!(legacy["protocol"], "ocpp1.6");
    assert_eq!(legacy["vendor"], "Vend16");

    // A raw call of an action only the CSMS sends gets the station's answer.
    csms.send(
        &json!({"station": "CP-201", "action": "Reset", "id": "r", "payload": {"type": "Immediate"}})
            .to_string(),
    );
    let (action, payload) = cp.serve(json!({"status": "Accepted"})).await;
    assert_eq!(
        (action.as_str(), &payload["type"]),
        ("Reset", &json!("Immediate"))
    );
    let reply = csms.wait(|v| v["type"] == "result" && v["id"] == "r");
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["result"]["status"], "Accepted");

    // The API station object.
    let (status, station) = csms.get("/v1/stations/CP-201").await;
    assert_eq!(status, 200, "{station}");
    assert_eq!(station["protocol"], "ocpp2.0.1");
    assert_eq!(
        station["active_transactions"][0],
        json!({"id": "tx-1", "connector": 1, "evse": 1})
    );
    assert!(station["state"]["v2"].is_object(), "{station}");
    assert_eq!(station["state"]["v2"]["transactions"][0]["id"], "tx-1");
    let (_, legacy) = csms.get("/v1/stations/CP-16").await;
    assert!(legacy["state"].get("v2").is_none(), "{legacy}");

    // The transaction list.
    let (status, listed) = csms.get("/v1/transactions").await;
    assert_eq!(status, 200, "{listed}");
    let rows = listed["transactions"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{listed}");
    assert_eq!(rows[0]["station"], "CP-201");
    assert_eq!(rows[0]["id"], "tx-1");
    assert_eq!(rows[0]["evse"], 1);
    assert_eq!(rows[0]["connector"], 1);
    assert_eq!(rows[0]["id_tag"], "TAG");
    assert_eq!(rows[0]["status"], "active");

    // A bulk call with a protocol selects only the stations of that protocol.
    let body = json!({"station": "*", "action": "Reset", "protocol": "ocpp2.0.1",
        "payload": {"type": "Soft"}})
    .to_string();
    let addr = csms.api_addr.clone();
    let (answer, served) = tokio::join!(
        http(&addr, "POST", "/v1/calls", Some(&body)),
        cp.serve(json!({"status": "Accepted"}))
    );
    assert_eq!(served.0, "Reset");
    assert_eq!(answer.0, 200, "{}", answer.1);
    let results = answer.1["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "{}", answer.1);
    assert_eq!(results[0]["station"], "CP-201");
    assert_eq!(results[0]["protocol"], "ocpp2.0.1");
    assert_eq!(results[0]["outcome"], "result");

    // A rule scoped to 2.0.1 answers 2.0.1 stations only.
    let time = "2031-02-03T04:05:06Z";
    let rule = json!({"currentTime": time}).to_string();
    let (status, put) = http(
        &csms.api_addr,
        "PUT",
        "/v1/rules/Heartbeat?protocol=ocpp2.0.1",
        Some(&rule),
    )
    .await;
    assert_eq!(status, 200, "{put}");
    let heartbeat = cp.call("Heartbeat", json!({})).await;
    assert_eq!(heartbeat[2]["currentTime"], time, "{heartbeat}");
    let heartbeat = old.call("Heartbeat", json!({})).await;
    assert_eq!(heartbeat[0], 3, "{heartbeat}");
    assert_ne!(heartbeat[2]["currentTime"], time, "{heartbeat}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_offer_decides_between_ocpp_1_6_and_2_0_1() {
    let mut csms = Csms::start();
    let dual = Raw::connect(&csms, "CP-dual", "ocpp2.0.1, ocpp1.6").await;
    assert_eq!(dual.negotiated, "ocpp2.0.1");
    let legacy = Raw::connect(&csms, "CP-old", "ocpp1.6").await;
    assert_eq!(legacy.negotiated, "ocpp1.6");
    let new = Raw::connect(&csms, "CP-new", "ocpp2.0.1").await;
    assert_eq!(new.negotiated, "ocpp2.0.1");
    let list = csms.stations();
    let protocol = |id: &str| {
        list.iter().find(|s| s["id"] == id).unwrap()["protocol"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(protocol("CP-dual"), "ocpp2.0.1");
    assert_eq!(protocol("CP-old"), "ocpp1.6");
    assert_eq!(protocol("CP-new"), "ocpp2.0.1");
}

/// A recording of one `DataTransfer` the server sent to `station`, written as an OCPP 1.6
/// session (the format of the file does not name a protocol).
fn recording(station: &str) -> String {
    let frames = [
        ("out", 0, json!([2, "a", "DataTransfer", {"vendorId": "v"}])),
        ("in", 1, json!([3, "a", {"status": "Rejected"}])),
    ];
    let mut lines = vec![
        json!({"type": "header", "schema": 1, "ocpp_version": "1.6", "role": "csms"}).to_string(),
    ];
    for (seq, (direction, elapsed, frame)) in frames.iter().enumerate() {
        lines.push(
            json!({"type": "frame", "seq": seq, "elapsed_ms": elapsed, "session": 1,
                "station": station, "direction": direction, "frame": frame})
            .to_string(),
        );
    }
    lines.push(json!({"type": "end", "frames": 2, "commands": 0, "elapsed_ms": 1}).to_string());
    lines.join("\n") + "\n"
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replay_refuses_a_station_that_negotiated_ocpp_2_0_1() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    std::fs::write(&path, recording("CP-201")).unwrap();
    let mut csms = Csms::start_replay(Path::new(&path).to_str().unwrap());
    let cp = Raw::connect(&csms, "CP-201", "ocpp2.0.1").await;
    assert_eq!(cp.negotiated, "ocpp2.0.1");
    let verdict = csms.wait(|v| v["type"] == "replay");
    assert_eq!(verdict["status"], "failed", "{verdict}");
    assert_eq!(
        verdict["error"],
        "station CP-201 negotiated ocpp2.0.1; only ocpp1.6 stations can be replayed",
        "{verdict}"
    );
    assert_eq!(csms.finish(), 1);
}
