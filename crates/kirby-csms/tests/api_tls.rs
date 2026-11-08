//! TLS of the HTTP control API: the real `kirby-csms` binary with `--api-tls-cert`, a test CA
//! made with `rcgen` and a small HTTPS/1.1 client on `tokio-rustls`.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

const WAIT: Duration = Duration::from_secs(30);

const ENV_VARS: &[&str] = &[
    "KIRBY_CSMS_LISTEN",
    "KIRBY_CSMS_API_LISTEN",
    "KIRBY_CSMS_API_TOKEN",
    "KIRBY_CSMS_API_TOKEN_FILE",
    "KIRBY_CSMS_API_TLS_CERT",
    "KIRBY_CSMS_API_TLS_KEY",
    "KIRBY_CSMS_API_TLS_CLIENT_CA",
    "KIRBY_CSMS_TLS_CERT",
    "KIRBY_CSMS_TLS_KEY",
    "KIRBY_CSMS_TLS_CLIENT_CA",
    "KIRBY_CSMS_STDIN",
    "KIRBY_CSMS_SCRIPT",
    "KIRBY_CSMS_JSON",
    "KIRBY_CSMS_CONFIG",
    "KIRBY_CSMS_PROFILE",
];

fn command(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kirby-csms"));
    for var in ENV_VARS {
        command.env_remove(var);
    }
    command.args(args);
    command
}

// ---- PKI ---------------------------------------------------------------------------------

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

/// `(certificate PEM, key PEM)` signed by `ca`.
fn leaf(ca: &Ca, cn: &str) -> (String, String) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
    params.distinguished_name = name(cn);
    let cert = params.signed_by(&key, &ca.issuer).unwrap();
    (cert.pem(), key.serialize_pem())
}

fn write(dir: &Path, file: &str, text: &str) -> String {
    let path = dir.join(file);
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

/// A CA, a server certificate and key written to `dir`: `(ca file, cert file, key file, ca)`.
fn server_files(dir: &Path) -> (String, String, String, Ca) {
    let ca = ca("test ca");
    let (cert, key) = leaf(&ca, "api");
    (
        write(dir, "ca.pem", &ca.pem),
        write(dir, "cert.pem", &cert),
        write(dir, "key.pem", &key),
        ca,
    )
}

// ---- the process ---------------------------------------------------------------------------

/// A running `kirby-csms` with the API on.
struct Csms {
    child: Child,
    _lines: Receiver<String>,
    api_addr: String,
    api_url: String,
}

impl Csms {
    fn start(listen: &str, args: &[&str]) -> Self {
        let mut all = vec!["--listen", "127.0.0.1:0", "--api-listen", listen, "--json"];
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
        let api_line = loop {
            let line = lines.recv_timeout(WAIT).expect("startup lines");
            if let Ok(value) = serde_json::from_str::<Value>(&line)
                && value["type"] == "api_listening"
            {
                break value;
            }
        };
        Self {
            child,
            _lines: lines,
            api_addr: api_line["addr"].as_str().unwrap().to_owned(),
            api_url: api_line["url"].as_str().unwrap().to_owned(),
        }
    }

    /// The address to connect to (a wildcard bind is reached through the loopback).
    fn port(&self) -> u16 {
        self.api_addr.rsplit(':').next().unwrap().parse().unwrap()
    }
}

impl Drop for Csms {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn run_to_end(args: &[&str]) -> (i32, String, String) {
    let output = command(args).stdin(Stdio::null()).output().unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// ---- the client ----------------------------------------------------------------------------

fn client_config(ca_pem: &str, identity: Option<(&str, &str)>) -> Arc<ClientConfig> {
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
        roots.add(cert.unwrap()).unwrap();
    }
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots);
    let config = match identity {
        Some((cert, key)) => {
            let chain = CertificateDer::pem_slice_iter(cert.as_bytes())
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let key = PrivateKeyDer::from_pem_slice(key.as_bytes()).unwrap();
            builder.with_client_auth_cert(chain, key).unwrap()
        }
        None => builder.with_no_client_auth(),
    };
    Arc::new(config)
}

/// One `GET` over HTTPS; the status code, or the error text.
async fn https_get(
    port: u16,
    config: Arc<ClientConfig>,
    path: &str,
    token: Option<&str>,
) -> Result<u16, String> {
    let request = async {
        let tcp = TcpStream::connect(("127.0.0.1", port))
            .await
            .map_err(|e| e.to_string())?;
        let server = ServerName::try_from("127.0.0.1").unwrap();
        let mut stream = TlsConnector::from(config)
            .connect(server, tcp)
            .await
            .map_err(|e| e.to_string())?;
        let auth = token
            .map(|token| format!("Authorization: Bearer {token}\r\n"))
            .unwrap_or_default();
        let text = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{auth}Connection: close\r\n\r\n"
        );
        stream
            .write_all(text.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        // A close without close_notify after the full response is not an error here.
        let _ = stream.read_to_end(&mut raw).await;
        let text = String::from_utf8_lossy(&raw).into_owned();
        text.split(' ')
            .nth(1)
            .and_then(|code| code.parse().ok())
            .ok_or_else(|| format!("no HTTP response: {text:?}"))
    };
    tokio::time::timeout(WAIT, request)
        .await
        .map_err(|_| "timeout".to_owned())?
}

// ---- tests ---------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn https_works_when_the_ca_is_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let (_, cert, key, ca) = server_files(dir.path());
    let token = write(dir.path(), "token", "s3cret\n");
    let csms = Csms::start(
        "127.0.0.1:0",
        &[
            "--api-tls-cert",
            &cert,
            "--api-tls-key",
            &key,
            "--api-token-file",
            &token,
        ],
    );
    assert!(csms.api_url.starts_with("https://"), "{}", csms.api_url);
    let config = client_config(&ca.pem, None);
    let status = https_get(csms.port(), config.clone(), "/v1/stations", Some("s3cret")).await;
    assert_eq!(status, Ok(200));
    // The token still applies over TLS.
    let status = https_get(csms.port(), config, "/v1/stations", None).await;
    assert_eq!(status, Ok(401));
    // A client that does not trust the CA refuses the connection.
    let other = ca_pem_of_another_ca();
    let status = https_get(
        csms.port(),
        client_config(&other, None),
        "/v1/stations",
        None,
    )
    .await;
    assert!(status.is_err(), "{status:?}");
}

fn ca_pem_of_another_ca() -> String {
    ca("another ca").pem
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plain_http_to_the_tls_port_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let (_, cert, key, ca) = server_files(dir.path());
    let csms = Csms::start(
        "127.0.0.1:0",
        &["--api-tls-cert", &cert, "--api-tls-key", &key],
    );
    let mut tcp = TcpStream::connect(("127.0.0.1", csms.port()))
        .await
        .unwrap();
    let _ = tcp
        .write_all(b"GET /v1/stations HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await;
    let mut raw = Vec::new();
    let read = tokio::time::timeout(WAIT, tcp.read_to_end(&mut raw))
        .await
        .expect("the server closes the connection");
    let _ = read;
    // No HTTP answer in clear text (a TLS alert record is fine).
    assert!(!raw.starts_with(b"HTTP/"), "{raw:?}");
    // The server is still fine.
    let status = https_get(
        csms.port(),
        client_config(&ca.pem, None),
        "/v1/stations",
        None,
    )
    .await;
    assert_eq!(status, Ok(200));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_connections_do_not_block_other_clients() {
    let dir = tempfile::tempdir().unwrap();
    let (_, cert, key, ca) = server_files(dir.path());
    let csms = Csms::start(
        "127.0.0.1:0",
        &["--api-tls-cert", &cert, "--api-tls-key", &key],
    );
    let connect = || async {
        TcpStream::connect(("127.0.0.1", csms.port()))
            .await
            .unwrap()
    };
    // Silent peers below the handshake cap do not delay a real client.
    let mut idle = Vec::new();
    for _ in 0..40 {
        idle.push(connect().await);
    }
    let config = client_config(&ca.pem, None);
    let status = https_get(csms.port(), config.clone(), "/v1/stations", None).await;
    assert_eq!(status, Ok(200));
    // Far more silent peers than the cap: the surplus is closed, the server stays up, and
    // once the peers are gone a real client is served again.
    for _ in 0..100 {
        idle.push(connect().await);
    }
    drop(idle);
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let status = https_get(csms.port(), config.clone(), "/v1/stations", None).await;
        if status == Ok(200) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{status:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mutual_tls_requires_a_client_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let (_, cert, key, server_ca) = server_files(dir.path());
    let clients = ca("client ca");
    let client_ca = write(dir.path(), "client-ca.pem", &clients.pem);
    let (client_cert, client_key) = leaf(&clients, "client");
    let csms = Csms::start(
        "127.0.0.1:0",
        &[
            "--api-tls-cert",
            &cert,
            "--api-tls-key",
            &key,
            "--api-tls-client-ca",
            &client_ca,
        ],
    );
    let without = client_config(&server_ca.pem, None);
    let status = https_get(csms.port(), without, "/v1/stations", None).await;
    assert!(status.is_err(), "{status:?}");
    let with = client_config(&server_ca.pem, Some((&client_cert, &client_key)));
    let status = https_get(csms.port(), with, "/v1/stations", None).await;
    assert_eq!(status, Ok(200));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_token_on_a_non_loopback_address_needs_tls() {
    let dir = tempfile::tempdir().unwrap();
    let (_, cert, key, ca) = server_files(dir.path());

    // Plain HTTP with a token on a non-loopback address is refused.
    let (code, _, stderr) = run_to_end(&["--api-listen", "0.0.0.0:0", "--api-token", "tok"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("--api-tls-cert"), "{stderr}");
    assert!(stderr.contains("loopback"), "{stderr}");
    let (code, _, stderr) = run_to_end(&[
        "--api-listen",
        "0.0.0.0:0",
        "--api-token",
        "tok",
        "--check-config",
    ]);
    assert_eq!(code, 2, "{stderr}");

    // TLS alone is not enough on a non-loopback address: the token stays required.
    let (code, _, stderr) = run_to_end(&[
        "--api-listen",
        "0.0.0.0:0",
        "--api-tls-cert",
        &cert,
        "--api-tls-key",
        &key,
    ]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("--api-token"), "{stderr}");

    // Loopback with a token over plain HTTP stays allowed.
    let csms = Csms::start("127.0.0.1:0", &["--api-token", "tok"]);
    assert!(csms.api_url.starts_with("http://"), "{}", csms.api_url);
    drop(csms);

    // TLS and a token on a wildcard address work.
    let csms = Csms::start(
        "0.0.0.0:0",
        &[
            "--api-tls-cert",
            &cert,
            "--api-tls-key",
            &key,
            "--api-token",
            "tok",
        ],
    );
    assert!(csms.api_url.starts_with("https://"), "{}", csms.api_url);
    let status = https_get(
        csms.port(),
        client_config(&ca.pem, None),
        "/v1/stations",
        Some("tok"),
    )
    .await;
    assert_eq!(status, Ok(200));
}

#[test]
fn a_wrong_or_missing_key_pair_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let (ca_file, cert, key, _) = server_files(dir.path());
    let other_dir = tempfile::tempdir().unwrap();
    let (_, _, other_key, _) = server_files(other_dir.path());
    let junk = write(dir.path(), "junk.pem", "not pem\n");
    let missing = dir.path().join("missing.pem");
    let missing = missing.to_str().unwrap();
    let cases: Vec<Vec<&str>> = vec![
        // Halves of the pair.
        vec!["--api-listen", "127.0.0.1:0", "--api-tls-cert", &cert],
        vec!["--api-listen", "127.0.0.1:0", "--api-tls-key", &key],
        // The client CA needs the pair.
        vec![
            "--api-listen",
            "127.0.0.1:0",
            "--api-tls-client-ca",
            &ca_file,
        ],
        // The pair needs the API.
        vec!["--api-tls-cert", &cert, "--api-tls-key", &key],
        // Unreadable, not PEM, a key of another certificate.
        vec![
            "--api-listen",
            "127.0.0.1:0",
            "--api-tls-cert",
            missing,
            "--api-tls-key",
            &key,
        ],
        vec![
            "--api-listen",
            "127.0.0.1:0",
            "--api-tls-cert",
            &junk,
            "--api-tls-key",
            &junk,
        ],
        vec![
            "--api-listen",
            "127.0.0.1:0",
            "--api-tls-cert",
            &cert,
            "--api-tls-key",
            &other_key,
        ],
        vec![
            "--api-listen",
            "127.0.0.1:0",
            "--api-tls-cert",
            &cert,
            "--api-tls-key",
            &key,
            "--api-tls-client-ca",
            &junk,
        ],
    ];
    for mut args in cases {
        // Every case fails at startup and with `--check-config`, before anything is bound.
        let (code, _, stderr) = run_to_end(&args);
        assert_eq!(code, 2, "{args:?}: {stderr}");
        assert!(!stderr.contains("PRIVATE KEY"), "{stderr}");
        args.push("--check-config");
        let (code, _, stderr) = run_to_end(&args);
        assert_eq!(code, 2, "{args:?}: {stderr}");
    }
}

#[test]
fn check_config_loads_the_api_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let (_, cert, key, _) = server_files(dir.path());
    let (code, stdout, stderr) = run_to_end(&[
        "--api-listen",
        "127.0.0.1:0",
        "--api-tls-cert",
        &cert,
        "--api-tls-key",
        &key,
        "--check-config",
    ]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.starts_with("OK"), "{stdout}");
    assert!(!stdout.contains("PRIVATE KEY") && !stderr.contains("PRIVATE KEY"));
}

#[test]
fn the_config_file_carries_the_api_tls_keys() {
    let dir = tempfile::tempdir().unwrap();
    let (ca_file, cert, key, _) = server_files(dir.path());
    let toml = |path: &str| format!("{path:?}");
    let file = write(
        dir.path(),
        "csms.toml",
        &format!(
            "[api]\napi_listen = \"127.0.0.1:0\"\napi_tls_cert = {}\napi_tls_key = {}\n\
             api_tls_client_ca = {}\n",
            toml(&cert),
            toml(&key),
            toml(&ca_file),
        ),
    );
    let (code, stdout, stderr) = run_to_end(&["--config", &file, "--print-config=toml"]);
    assert_eq!(code, 0, "{stderr}");
    for key in ["api_tls_cert", "api_tls_key", "api_tls_client_ca"] {
        assert!(stdout.contains(key), "{key}: {stdout}");
    }
    assert!(!stdout.contains("PRIVATE KEY"), "{stdout}");
    // The printed file is itself a valid configuration.
    let printed = write(dir.path(), "printed.toml", &stdout);
    let (code, _, stderr) = run_to_end(&["--config", &printed, "--check-config"]);
    assert_eq!(code, 0, "{stderr}");
    // The same through flags.
    let (code, stdout, stderr) = run_to_end(&[
        "--api-listen",
        "127.0.0.1:0",
        "--api-tls-cert",
        &cert,
        "--api-tls-key",
        &key,
        "--print-config=toml",
    ]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("api_tls_cert"), "{stdout}");
    assert!(!stdout.contains("api_tls_client_ca"), "{stdout}");
    // A file with only one half is an error.
    let half = write(
        dir.path(),
        "half.toml",
        &format!(
            "[api]\napi_listen = \"127.0.0.1:0\"\napi_tls_cert = {}\n",
            toml(&cert)
        ),
    );
    let (code, _, _) = run_to_end(&["--config", &half, "--check-config"]);
    assert_eq!(code, 2);
}
