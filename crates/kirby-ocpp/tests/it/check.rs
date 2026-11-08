//! Process tests of `kirby-ocpp check` against the mock CSMS and small hand-written servers.

use std::process::{Command, Output, Stdio};
use std::time::Duration;

use kirby_ocpp_testkit::{CsmsOptions, MockCsms, Reply, SubprotocolPolicy, TlsOpts};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Longest a hand-written server waits for the request head.
const HEAD_WAIT: Duration = Duration::from_secs(10);

fn command(args: &[String]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kirby-ocpp"));
    for (key, _) in std::env::vars() {
        if key.starts_with("OCPP_") || key.starts_with("KIRBY_OCPP_") {
            command.env_remove(key);
        }
    }
    command.args(args).stdin(Stdio::null());
    command
}

async fn check(args: &[&str]) -> Output {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    tokio::task::spawn_blocking(move || crate::support::output(command(&args)))
        .await
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn parsed(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("not one JSON object: {e}\n{}", text(&output.stdout)))
}

fn step<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap_or_else(|| panic!("no step {name} in {report}"))
}

fn statuses(report: &Value) -> Vec<(String, String)> {
    report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap().to_owned(),
                s["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn boot_calls(csms: &MockCsms) -> usize {
    csms.received()
        .iter()
        .filter(|r| r.action == "BootNotification")
        .count()
}

/// Serves one connection: reads the request head and answers `response`.
async fn one_shot_server(response: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let Ok(Ok((mut stream, _))) = tokio::time::timeout(HEAD_WAIT, listener.accept()).await
        else {
            return;
        };
        let mut head = Vec::new();
        let mut buf = [0_u8; 1024];
        let _ = tokio::time::timeout(HEAD_WAIT, async {
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&buf[..n]),
                }
            }
        })
        .await;
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
    });
    port
}

#[tokio::test(flavor = "multi_thread")]
async fn all_steps_pass_over_ws_and_one_boot_is_sent() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let output = check(&[
        "check",
        "--url",
        csms.url(),
        "--id",
        "CP-CHK",
        "--json",
        "--state-dir",
        state_dir.to_str().unwrap(),
    ])
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let report = parsed(&output);
    assert_eq!(report["schema"], 1);
    assert_eq!(report["ocpp_version"], "1.6");
    assert_eq!(report["tool_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["status"], "passed");
    assert!(report["url"].as_str().unwrap().ends_with("/ocpp/CP-CHK"));
    let expected: Vec<(String, String)> = [
        ("url", "passed"),
        ("dns", "passed"),
        ("tcp", "passed"),
        ("tls", "skipped"),
        ("websocket", "passed"),
        ("boot", "passed"),
        ("close", "passed"),
    ]
    .iter()
    .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
    .collect();
    assert_eq!(statuses(&report), expected, "{report}");
    assert!(
        step(&report, "boot")["detail"]
            .as_str()
            .unwrap()
            .starts_with("Accepted, heartbeat interval 5 s")
    );
    assert!(
        step(&report, "websocket")["detail"]
            .as_str()
            .unwrap()
            .contains("subprotocol `ocpp1.6`")
    );
    // Exactly one connection and one BootNotification; the station state is untouched.
    assert_eq!(csms.connections(), 1);
    assert_eq!(boot_calls(&csms), 1);
    assert!(
        !state_dir.exists(),
        "check must not create the state directory"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn json_report_has_a_fixed_key_set() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let output = check(&["check", "--url", csms.url(), "--json"]).await;
    let report = parsed(&output);
    let mut keys: Vec<&str> = report
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "ocpp_version",
            "schema",
            "status",
            "steps",
            "tool_version",
            "url"
        ]
    );
    for step in report["steps"].as_array().unwrap() {
        let mut keys: Vec<&str> = step
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["detail", "duration_ms", "hint", "name", "status"]);
        assert!(step["duration_ms"].is_u64());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn text_output_lists_the_steps_and_a_verdict() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let output = check(&["check", "--url", csms.url(), "--color", "never"]).await;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 8, "{out}");
    for (line, (label, name)) in lines.iter().zip([
        ("PASS", "url"),
        ("PASS", "dns"),
        ("PASS", "tcp"),
        ("SKIP", "tls"),
        ("PASS", "websocket"),
        ("PASS", "boot"),
        ("PASS", "close"),
    ]) {
        assert!(line.starts_with(&format!("{label}  {name}")), "{out}");
    }
    assert_eq!(lines[7], "PASSED  6 passed, 1 skipped");
    assert!(!out.contains('\u{1b}'), "no colors with --color never");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_connection_fails_tcp_and_skips_the_rest() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let url = format!("ws://127.0.0.1:{port}/ocpp");
    let output = check(&["check", "--url", &url, "--json"]).await;
    assert_eq!(output.status.code(), Some(1));
    let report = parsed(&output);
    assert_eq!(report["status"], "failed");
    let got = statuses(&report);
    assert_eq!(got[2], ("tcp".to_owned(), "failed".to_owned()));
    assert!(
        got[3..].iter().all(|(_, status)| status == "skipped"),
        "{got:?}"
    );
    assert!(
        step(&report, "tcp")["hint"]
            .as_str()
            .unwrap()
            .contains("nothing listens")
    );
    assert!(
        step(&report, "boot")["detail"]
            .as_str()
            .unwrap()
            .contains("`tcp` step failed")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unresolvable_host_fails_dns() {
    let output = check(&["check", "--url", "ws://nonexistent.invalid/ocpp", "--json"]).await;
    assert_eq!(output.status.code(), Some(1));
    let report = parsed(&output);
    assert_eq!(step(&report, "dns")["status"], "failed");
    assert_eq!(step(&report, "tcp")["status"], "skipped");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_selects_another_subprotocol_fails_the_upgrade() {
    let csms = MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Select("ocpp2.0.1".to_owned()),
        ..CsmsOptions::default()
    })
    .await
    .unwrap();
    let output = check(&["check", "--url", csms.url(), "--json"]).await;
    assert_eq!(output.status.code(), Some(1));
    let report = parsed(&output);
    let ws = step(&report, "websocket");
    assert_eq!(ws["status"], "failed");
    assert!(
        ws["detail"].as_str().unwrap().contains("`ocpp2.0.1`"),
        "{ws}"
    );
    assert!(ws["hint"].as_str().unwrap().contains("--ws-subprotocol"));
    assert_eq!(step(&report, "boot")["status"], "skipped");
    assert_eq!(boot_calls(&csms), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_selects_no_subprotocol_is_reported_as_none() {
    let csms = MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Omit,
        ..CsmsOptions::default()
    })
    .await
    .unwrap();
    let output = check(&["check", "--url", csms.url(), "--json"]).await;
    let report = parsed(&output);
    let ws = step(&report, "websocket");
    assert_eq!(ws["status"], "failed");
    assert!(
        ws["detail"]
            .as_str()
            .unwrap()
            .contains("selected subprotocol none")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn http_401_gets_a_hint_and_the_password_stays_out_of_the_output() {
    let port = one_shot_server(
        "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"ocpp\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )
    .await;
    let url = format!("ws://CP-AUTH:s3cr3t-pass@127.0.0.1:{port}/ocpp");
    let output = check(&["check", "--url", &url, "--id", "CP-AUTH"]).await;
    assert_eq!(output.status.code(), Some(1));
    let out = text(&output.stdout);
    let all = format!("{out}{}", text(&output.stderr));
    assert!(!all.contains("s3cr3t-pass"), "{all}");
    assert!(out.contains("FAIL  websocket"), "{out}");
    assert!(out.contains("401 Unauthorized"), "{out}");
    assert!(out.contains("rejected the credentials"), "{out}");
    assert!(out.contains("CP-AUTH:***@"), "{out}");
    assert!(out.contains("SKIP  boot"), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn http_404_and_403_get_their_own_hints() {
    let port =
        one_shot_server("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
    let url = format!("ws://127.0.0.1:{port}/nope");
    let output = check(&["check", "--url", &url, "--json"]).await;
    let report = parsed(&output);
    let ws = step(&report, "websocket");
    assert_eq!(ws["status"], "failed");
    assert!(ws["hint"].as_str().unwrap().contains("last segment"));

    let port =
        one_shot_server("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
    let url = format!("ws://127.0.0.1:{port}/ocpp");
    let output = check(&["check", "--url", &url, "--json"]).await;
    let report = parsed(&output);
    assert!(
        step(&report, "websocket")["hint"]
            .as_str()
            .unwrap()
            .contains("registered")
    );
}

fn boot_answer(status: &'static str) -> impl Fn(&Value) -> Reply + Send + Sync + 'static {
    move |_| {
        Reply::Result(json!({
            "status": status,
            "currentTime": "2026-01-01T00:00:00Z",
            "interval": 30,
        }))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_boot_fails_and_a_pending_boot_only_warns() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    csms.respond("BootNotification", boot_answer("Rejected"));
    let output = check(&["check", "--url", csms.url(), "--json"]).await;
    assert_eq!(output.status.code(), Some(1));
    let report = parsed(&output);
    let boot = step(&report, "boot");
    assert_eq!(boot["status"], "failed");
    assert!(
        boot["detail"]
            .as_str()
            .unwrap()
            .starts_with("Rejected, retry interval 30 s")
    );
    assert!(boot["hint"].as_str().unwrap().contains("refuses"));
    assert_eq!(step(&report, "close")["status"], "passed");
    assert_eq!(boot_calls(&csms), 1);

    csms.respond("BootNotification", boot_answer("Pending"));
    let output = check(&["check", "--url", csms.url(), "--json"]).await;
    assert_eq!(
        output.status.code(),
        Some(0),
        "warnings do not fail the check"
    );
    let report = parsed(&output);
    assert_eq!(report["status"], "passed");
    assert_eq!(step(&report, "boot")["status"], "warning");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wss_server_with_an_unknown_ca_fails_tls_until_trusted_or_skipped() {
    let csms = MockCsms::start_tls(TlsOpts::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, csms.tls_material().unwrap().ca_cert_pem()).unwrap();

    let output = check(&["check", "--url", csms.url(), "--json"]).await;
    assert_eq!(output.status.code(), Some(1));
    let report = parsed(&output);
    let tls = step(&report, "tls");
    assert_eq!(tls["status"], "failed");
    let detail = tls["detail"].as_str().unwrap();
    assert!(detail.contains("UnknownIssuer"), "{detail}");
    assert!(detail.contains("the server presents subject"), "{detail}");
    assert!(tls["hint"].as_str().unwrap().contains("--tls-ca"));
    assert_eq!(step(&report, "websocket")["status"], "skipped");
    assert_eq!(csms.connections(), 0, "no WebSocket connection was made");

    let output = check(&["check", "--url", csms.url(), "--json", "--tls-skip-verify"]).await;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let report = parsed(&output);
    let tls = step(&report, "tls");
    assert_eq!(tls["status"], "passed");
    let detail = tls["detail"].as_str().unwrap();
    assert!(detail.starts_with("TLS 1.3; subject"), "{detail}");
    assert!(
        detail.contains("verification skipped (--tls-skip-verify)"),
        "{detail}"
    );
    assert!(detail.contains("days left"), "{detail}");

    let output = check(&[
        "check",
        "--url",
        csms.url(),
        "--json",
        "--tls-ca",
        ca.to_str().unwrap(),
    ])
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let report = parsed(&output);
    assert!(
        step(&report, "tls")["detail"]
            .as_str()
            .unwrap()
            .contains("verified against --tls-ca")
    );
    assert_eq!(step(&report, "boot")["status"], "passed");
}

#[tokio::test(flavor = "multi_thread")]
async fn mutual_tls_needs_the_client_certificate() {
    let csms = MockCsms::start_tls(TlsOpts {
        require_client_cert: true,
    })
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let material = csms.tls_material().unwrap();
    let ca = dir.path().join("ca.pem");
    let cert = dir.path().join("client.pem");
    let key = dir.path().join("client.key");
    std::fs::write(&ca, material.ca_cert_pem()).unwrap();
    std::fs::write(&cert, material.client_cert_pem()).unwrap();
    std::fs::write(&key, material.client_key_pem()).unwrap();
    let ca = ca.to_str().unwrap();

    let output = check(&["check", "--url", csms.url(), "--tls-ca", ca, "--json"]).await;
    assert_eq!(output.status.code(), Some(1));
    let report = parsed(&output);
    assert!(
        report["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["status"] == "failed"),
        "{report}"
    );

    let output = check(&[
        "check",
        "--url",
        csms.url(),
        "--tls-ca",
        ca,
        "--tls-cert",
        cert.to_str().unwrap(),
        "--tls-key",
        key.to_str().unwrap(),
        "--json",
    ])
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let report = parsed(&output);
    assert!(
        step(&report, "tls")["detail"]
            .as_str()
            .unwrap()
            .contains("client certificate sent")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn usage_errors_exit_2() {
    let output = check(&["check"]).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("--url"));

    let output = check(&["check", "--url", "http://h/x"]).await;
    assert_eq!(output.status.code(), Some(2));

    let output = check(&["check", "--url", "ws://h:1/x", "--timeout", "0"]).await;
    assert_eq!(output.status.code(), Some(2));

    let output = check(&[
        "check",
        "--url",
        "ws://h:1/x",
        "--count",
        "2",
        "--id",
        "CP-{n}",
    ])
    .await;
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("one station"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_whole_check_respects_its_timeout() {
    // A server that accepts the TCP connection and never answers.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let held = tokio::spawn(async move {
        let accepted = listener.accept().await;
        tokio::time::sleep(Duration::from_secs(30)).await;
        drop(accepted);
    });
    let url = format!("ws://127.0.0.1:{port}/ocpp");
    let started = std::time::Instant::now();
    let output = check(&["check", "--url", &url, "--timeout", "1", "--json"]).await;
    held.abort();
    assert_eq!(output.status.code(), Some(1));
    assert!(started.elapsed() < Duration::from_secs(20));
    let report = parsed(&output);
    let ws = step(&report, "websocket");
    assert_eq!(ws["status"], "failed");
    assert!(
        ws["detail"]
            .as_str()
            .unwrap()
            .contains("did not finish in time")
    );
}
