//! Security Whitepaper edition 4, transport half: wildcard server certificates, TLS failure
//! security events (once per series of failed attempts), the `SignCertificate` back-off, the
//! old/new client certificate alternation and expired client certificates.

mod station_support;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use kirby_ocpp_core::Station;
use kirby_ocpp_core::config::{IdentityPem, TlsConfig};
use kirby_ocpp_core::station::Command;
use kirby_ocpp_core::store::{PersistedStation, StationSecrets, StationStore};
use kirby_ocpp_core::ws::{ConnectError, ConnectOptions, Connection};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms, Reply, TlsOpts};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair,
};
use rustls::DistinguishedName as TlsDistinguishedName;
use rustls::SignatureScheme;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use serde_json::json;
use station_support::{Observed, Rig, STEP, t0, test_config, wait_state};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;

type Ca = (rcgen::Certificate, Issuer<'static, KeyPair>);

fn ca(name: &str) -> Ca {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, name);
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(30);
    let cert = params.self_signed(&key).unwrap();
    (cert, Issuer::new(params, key))
}

/// A server leaf for `sans`, signed by `issuer`, with an optional Common Name.
fn server_leaf(
    issuer: &Ca,
    sans: &[&str],
    common_name: Option<&str>,
) -> (CertificateDer<'static>, KeyPair) {
    let key = KeyPair::generate().unwrap();
    let mut params =
        CertificateParams::new(sans.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()).unwrap();
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    if let Some(cn) = common_name {
        params.distinguished_name.push(DnType::CommonName, cn);
    }
    let cert = params.signed_by(&key, &issuer.1).unwrap();
    (cert.der().clone(), key)
}

/// A client certificate and key (PEM) signed by `issuer`.
fn client_pair(issuer: &Ca, valid: bool) -> (String, String) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    params.distinguished_name.push(DnType::CommonName, "CP-001");
    let now = time::OffsetDateTime::now_utc();
    if valid {
        params.not_before = now - time::Duration::days(1);
        params.not_after = now + time::Duration::days(30);
    } else {
        params.not_before = now - time::Duration::days(10);
        params.not_after = now - time::Duration::days(1);
    }
    let cert = params.signed_by(&key, &issuer.1).unwrap();
    (cert.pem(), key.serialize_pem())
}

/// Accepts any client certificate (expired ones too); signatures are still checked.
#[derive(Debug)]
struct AnyClientCertificate(Arc<rustls::crypto::CryptoProvider>);

impl ClientCertVerifier for AnyClientCertificate {
    fn root_hint_subjects(&self) -> &[TlsDistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// What one TLS handshake with the raw server came to: the client certificates the server saw,
/// or the error text.
type Handshake = Result<Vec<Vec<u8>>, String>;

/// A TLS server that only completes handshakes and then drops the connection.
struct TlsServer {
    addr: SocketAddr,
    handshakes: mpsc::UnboundedReceiver<Handshake>,
}

impl TlsServer {
    async fn start(
        cert: CertificateDer<'static>,
        key: &KeyPair,
        ask_for_client_cert: bool,
    ) -> Self {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .unwrap();
        let builder = if ask_for_client_cert {
            builder.with_client_cert_verifier(Arc::new(AnyClientCertificate(provider)))
        } else {
            builder.with_no_client_auth()
        };
        let config = builder
            .with_single_cert(
                vec![cert],
                PrivateKeyDer::from(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, handshakes) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let result = match acceptor.accept(tcp).await {
                        Ok(stream) => Ok(stream
                            .get_ref()
                            .1
                            .peer_certificates()
                            .unwrap_or_default()
                            .iter()
                            .map(|c| c.as_ref().to_vec())
                            .collect()),
                        Err(error) => Err(error.to_string()),
                    };
                    let _ = tx.send(result);
                });
            }
        });
        Self { addr, handshakes }
    }

    fn url(&self) -> String {
        format!("wss://127.0.0.1:{}/ocpp", self.addr.port())
    }

    async fn next(&mut self) -> Handshake {
        timeout(STEP, self.handshakes.recv())
            .await
            .expect("a handshake within the step")
            .expect("server alive")
    }
}

fn write_file(dir: &tempfile::TempDir, name: &str, contents: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

fn options() -> ConnectOptions {
    ConnectOptions::default()
}

// ---------------------------------------------------------------------------------------------
// Wildcard server certificates (A00.FR.308, A00.FR.510, AllowCentralSystemTLSWildcards)
// ---------------------------------------------------------------------------------------------

async fn connect_to(
    server: &TlsServer,
    ca_pem: &str,
    dir: &tempfile::TempDir,
    allow: bool,
) -> Result<(), ConnectError> {
    let tls = TlsConfig {
        ca_file: Some(write_file(dir, "ca.pem", ca_pem)),
        server_name: Some("csms.example.test".to_owned()),
        allow_wildcard_certificates: allow,
        ..TlsConfig::default()
    };
    Connection::connect(&server.url(), "CP-001", &tls, "ocpp1.6", options())
        .await
        .map(|_| ())
}

#[tokio::test]
async fn a_wildcard_server_certificate_is_refused_unless_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let authority = ca("Wildcard CA");
    // The wildcard sits in the SAN, or only in the Common Name.
    for (sans, cn) in [
        (vec!["*.example.test"], None),
        (vec!["csms.example.test"], Some("*.example.test")),
    ] {
        let (cert, key) = server_leaf(&authority, &sans, cn);
        let mut server = TlsServer::start(cert, &key, false).await;
        let error = connect_to(&server, &authority.0.pem(), &dir, false)
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ConnectError::Tls(text)
                if text.contains("invalid peer certificate")
                    && text.contains("wildcard Central System certificate")),
            "{error}"
        );
        assert!(
            server.next().await.is_err(),
            "the client aborts the handshake"
        );

        // Allowed: the handshake completes (the upgrade then fails: the server just hangs up).
        let _ = connect_to(&server, &authority.0.pem(), &dir, true).await;
        assert!(server.next().await.is_ok(), "allowed wildcard certificate");
    }
}

#[tokio::test]
async fn an_ordinary_server_certificate_passes_the_wildcard_check() {
    let dir = tempfile::tempdir().unwrap();
    let authority = ca("Plain CA");
    let (cert, key) = server_leaf(
        &authority,
        &["csms.example.test"],
        Some("csms.example.test"),
    );
    let mut server = TlsServer::start(cert, &key, false).await;
    let _ = connect_to(&server, &authority.0.pem(), &dir, false).await;
    assert!(server.next().await.is_ok());
}

#[tokio::test]
async fn the_wildcard_key_is_read_at_the_next_connect_and_the_event_is_sent_once() {
    let dir = tempfile::tempdir().unwrap();
    let authority = ca("Station CA");
    let (cert, key) = server_leaf(&authority, &["*.example.test"], None);
    let mut server = TlsServer::start(cert, &key, false).await;
    let mut config = test_config(&server.url(), &dir);
    config.tls.ca_file = Some(write_file(&dir, "ca.pem", &authority.0.pem()));
    config.tls.server_name = Some("csms.example.test".to_owned());
    let (station, join) = Station::spawn(config, Arc::new(kirby_ocpp_core::ManualClock::new(t0())));
    let mut observed = Observed::new(station.subscribe());
    station.connect().await.unwrap();
    // Three refused attempts; the third one proves the first two were fully processed.
    for _ in 0..3 {
        assert!(server.next().await.is_err());
    }
    observed.drain();
    let reported = observed
        .logs
        .iter()
        .filter(|l| l.contains("security event InvalidCentralSystemCertificate"))
        .count();
    assert_eq!(reported, 1, "one event per series: {:#?}", observed.logs);
    assert_eq!(station.snapshot().pending_security_events, 1);

    station
        .send(Command::SetConfiguration {
            key: "AllowCentralSystemTLSWildcards".into(),
            value: "true".into(),
        })
        .await
        .unwrap();
    loop {
        if server.next().await.is_ok() {
            break;
        }
    }
    station.shutdown().await.unwrap();
    join.await.unwrap();
}

// ---------------------------------------------------------------------------------------------
// TLS failure events: one per series (A00.FR.315/322/419/426)
// ---------------------------------------------------------------------------------------------

/// Reads the ClientHello, answers with a fatal TLS alert and hangs up; counts the attempts.
async fn alert_server(description: u8) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0_u8; 4096];
            let _ = socket.read(&mut buf).await;
            // record type alert (21), TLS 1.2, length 2, level fatal (2), description.
            let _ = socket.write_all(&[21, 3, 3, 0, 2, 2, description]).await;
            let _ = socket.shutdown().await;
            seen.fetch_add(1, Ordering::SeqCst);
        }
    });
    (addr, count)
}

async fn alert_event(description: u8, expected: &str) {
    let (addr, count) = alert_server(description).await;
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config(&format!("wss://127.0.0.1:{}/ocpp", addr.port()), &dir);
    config.tls.skip_verify = true;
    let (station, join) = Station::spawn(config, Arc::new(kirby_ocpp_core::ManualClock::new(t0())));
    let mut observed = Observed::new(station.subscribe());
    station.connect().await.unwrap();
    observed
        .wait_log(&format!("security event {expected}"))
        .await;
    // Two more failed attempts do not queue the event again.
    wait_until_count(&count, 3).await;
    observed.drain();
    let again = observed
        .logs
        .iter()
        .filter(|l| l.contains(&format!("security event {expected}")))
        .count();
    assert_eq!(again, 1, "{:#?}", observed.logs);
    assert_eq!(station.snapshot().pending_security_events, 1);
    station.shutdown().await.unwrap();
    join.await.unwrap();
}

async fn wait_until_count(count: &AtomicUsize, at_least: usize) {
    let deadline = Instant::now() + STEP;
    while count.load(Ordering::SeqCst) < at_least {
        assert!(Instant::now() < deadline, "attempts did not arrive");
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn a_protocol_version_alert_reports_invalid_tls_version_once() {
    alert_event(70, "InvalidTLSVersion").await;
}

#[tokio::test]
async fn a_handshake_failure_alert_reports_invalid_tls_cipher_suite_once() {
    alert_event(40, "InvalidTLSCipherSuite").await;
}

#[tokio::test]
async fn an_insufficient_security_alert_reports_invalid_tls_cipher_suite_once() {
    alert_event(71, "InvalidTLSCipherSuite").await;
}

/// A plain-TCP server that answers the first `refusals` upgrades with 401 and then one with
/// `101 Switching Protocols`, which it closes at once; afterwards it answers 401 again.
async fn flaky_auth_server(refusals: usize) -> (SocketAddr, Arc<AtomicUsize>) {
    use base64::Engine as _;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0_u8; 4096];
            let n = socket.read(&mut buf).await.unwrap_or(0);
            let head = String::from_utf8_lossy(buf.get(..n).unwrap_or_default()).into_owned();
            let attempt = seen.load(Ordering::SeqCst);
            if attempt == refusals {
                let key = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .starts_with("sec-websocket-key:")
                            .then(|| l.split_once(':').map(|(_, v)| v.trim().to_owned()))
                            .flatten()
                    })
                    .unwrap_or_default();
                let digest = ring::digest::digest(
                    &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
                    format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
                );
                let accept = base64::engine::general_purpose::STANDARD.encode(digest.as_ref());
                let response = format!(
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\nSec-WebSocket-Protocol: ocpp1.6\r\n\r\n"
                );
                let _ = socket.write_all(response.as_bytes()).await;
            } else {
                let _ = socket
                    .write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n")
                    .await;
            }
            let _ = socket.shutdown().await;
            seen.fetch_add(1, Ordering::SeqCst);
        }
    });
    (addr, count)
}

#[tokio::test]
async fn the_failure_series_ends_with_the_next_successful_connection() {
    let (addr, count) = flaky_auth_server(3).await;
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(&format!("ws://127.0.0.1:{}/ocpp", addr.port()), &dir);
    let (station, join) = Station::spawn(config, Arc::new(kirby_ocpp_core::ManualClock::new(t0())));
    station.connect().await.unwrap();
    // 401, 401, 401 (one event), 101 (series over), 401, 401 (a second event).
    wait_until_count(&count, 6).await;
    wait_state(&station, "failures processed", |s| {
        s.pending_security_events >= 2
    })
    .await;
    assert_eq!(station.snapshot().pending_security_events, 2);
    station.shutdown().await.unwrap();
    join.await.unwrap();
}

// ---------------------------------------------------------------------------------------------
// SignCertificate back-off (A02.FR.17 to A02.FR.20)
// ---------------------------------------------------------------------------------------------

fn sign(csr_pem: &str, issuer: &Ca) -> String {
    let csr = rcgen::CertificateSigningRequestParams::from_pem(csr_pem).unwrap();
    let leaf = csr.signed_by(&issuer.1).unwrap();
    format!("{}\n{}", leaf.pem(), issuer.0.pem())
}

async fn signing_rig(wait: &str, repeats: &str, status: &'static str) -> Rig {
    let mut rig = Rig::start().await;
    rig.csms
        .respond("SecurityEventNotification", |_| Reply::Result(json!({})));
    rig.csms.respond("SignCertificate", move |_| {
        Reply::Result(json!({"status": status}))
    });
    rig.connect_booted().await;
    rig.cfg("CertSigningWaitMinimum", wait).await;
    rig.cfg("CertSigningRepeatTimes", repeats).await;
    rig
}

#[tokio::test]
async fn sign_certificate_is_repeated_with_doubling_waits_and_then_stops() {
    let mut rig = signing_rig("1", "2", "Accepted").await;
    let started = Instant::now();
    rig.send(Command::RequestCertificate).await;
    let mut elapsed = Vec::new();
    for n in 2..=3 {
        rig.csms
            .wait_for_call_count_after(0, "SignCertificate", n, |_| true, STEP)
            .await
            .expect("a repeat");
        elapsed.push(started.elapsed());
    }
    // Waits of 1 s and 2 s (lower bounds only: scheduling can delay, never advance).
    assert!(elapsed[0] >= Duration::from_millis(950), "{elapsed:?}");
    assert!(elapsed[1] >= Duration::from_millis(2_950), "{elapsed:?}");
    rig.observed
        .wait_log("no CertificateSigned after 2 repeat(s)")
        .await;
    assert_eq!(rig.calls("SignCertificate", 0).len(), 3);
    rig.shutdown().await;
}

#[tokio::test]
async fn a_rejected_sign_certificate_is_repeated_too() {
    let mut rig = signing_rig("1", "1", "Rejected").await;
    rig.send(Command::RequestCertificate).await;
    rig.observed
        .wait_log("no CertificateSigned after 1 repeat(s)")
        .await;
    assert_eq!(rig.calls("SignCertificate", 0).len(), 2);
    assert!(!rig.station.snapshot().certificate_request_pending);
    rig.shutdown().await;
}

#[tokio::test]
async fn certificate_signed_ends_the_back_off() {
    let authority = ca("Back-off CA");
    let mut rig = signing_rig("1", "3", "Accepted").await;
    rig.send(Command::RequestCertificate).await;
    let csr = rig.calls("SignCertificate", 0)[0]["csr"]
        .as_str()
        .unwrap()
        .to_owned();
    let reply = rig
        .csms_call(
            "CertificateSigned",
            json!({"certificateChain": sign(&csr, &authority)}),
        )
        .await;
    assert_eq!(reply["status"], "Accepted");
    rig.observed.wait_log("back-off ended").await;
    assert_eq!(rig.calls("SignCertificate", 0).len(), 1);
    rig.shutdown().await;
}

#[tokio::test]
async fn a_rejected_certificate_signed_stops_the_repeats() {
    let authority = ca("Back-off CA 2");
    let mut rig = signing_rig("1", "3", "Accepted").await;
    rig.send(Command::RequestCertificate).await;
    // A chain for some other key: Rejected (A02.FR.07), so no repeat (A02.FR.20).
    let (other_csr, _) =
        kirby_ocpp_core::station::certificates::generate_csr("CP-001", "x").unwrap();
    let reply = rig
        .csms_call(
            "CertificateSigned",
            json!({"certificateChain": sign(&other_csr, &authority)}),
        )
        .await;
    assert_eq!(reply["status"], "Rejected");
    rig.observed.wait_log("A02.FR.20").await;
    assert_eq!(rig.calls("SignCertificate", 0).len(), 1);
    rig.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Old/new client certificate (A02.FR.28/29, A03.FR.24/25)
// ---------------------------------------------------------------------------------------------

struct MtlsCase {
    csms: MockCsms,
    dir: tempfile::TempDir,
    config: kirby_ocpp_core::StationConfig,
}

/// A TLS mock CSMS that requires a client certificate.
async fn mtls_csms() -> MockCsms {
    let csms = MockCsms::start(CsmsOptions {
        tls: Some(TlsOpts {
            require_client_cert: true,
        }),
        ..CsmsOptions::default()
    })
    .await
    .unwrap();
    csms.respond("SecurityEventNotification", |_| Reply::Result(json!({})));
    csms
}

/// The certificate pair (PEM chain, PEM key) the mock CSMS accepts.
fn accepted_pair(csms: &MockCsms) -> (String, String) {
    let material = csms.tls_material().unwrap();
    (
        material.client_cert_pem().to_owned(),
        material.client_key_pem().to_owned(),
    )
}

/// A pair from an unknown CA: the mock CSMS refuses it.
fn refused_pair() -> (String, String) {
    client_pair(&ca("Unknown CA"), true)
}

/// A station state with profile 3 and the given new and previous certificates.
fn mtls_case(csms: MockCsms, new: (String, String), previous: (String, String)) -> MtlsCase {
    let ca_pem = csms.tls_material().unwrap().ca_cert_pem().to_owned();
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config(csms.url(), &dir);
    config.tls.ca_file = Some(write_file(&dir, "ca.pem", &ca_pem));
    config.tls.server_name = Some("localhost".to_owned());
    let store = StationStore::new(dir.path(), "CP-001");
    let mut state = PersistedStation::new("CP-001");
    state
        .configuration
        .insert("SecurityProfile".into(), "3".into());
    state.certificates.charge_point_chain = Some(new.0.trim().to_owned());
    state.certificates.previous_charge_point_chain = Some(previous.0.trim().to_owned());
    store.save(&state).unwrap();
    store
        .save_secrets(&StationSecrets {
            charge_point_key_pem: Some(new.1),
            previous_charge_point_key_pem: Some(previous.1),
            ..StationSecrets::default()
        })
        .unwrap();
    MtlsCase { csms, dir, config }
}

/// Runs the station until it is registered, then stops it and returns the saved files.
async fn run_mtls(
    case: MtlsCase,
    wait_for_event: Option<&str>,
) -> (PersistedStation, StationSecrets, Vec<String>, MtlsCase) {
    let clock = Arc::new(kirby_ocpp_core::ManualClock::new(t0()));
    let (station, join) = Station::spawn(case.config.clone(), clock);
    let mut observed = Observed::new(station.subscribe());
    station.connect().await.unwrap();
    wait_state(&station, "registered", |s| {
        s.registration == Some(kirby_ocpp_core::ocpp::types::RegistrationStatus::Accepted)
    })
    .await;
    // The first StatusNotification follows the boot answer, after the security events left.
    case.csms
        .wait_for_call("StatusNotification", STEP)
        .await
        .unwrap();
    if let Some(name) = wait_for_event {
        case.csms
            .wait_for_call_after(0, "SecurityEventNotification", |p| p["type"] == name, STEP)
            .await
            .unwrap_or_else(|e| panic!("{name} not reported: {e}"));
    }
    station.shutdown().await.unwrap();
    join.await.unwrap();
    observed.drain();
    let store = StationStore::new(case.dir.path(), "CP-001");
    let state = match store.load().unwrap() {
        kirby_ocpp_core::store::LoadOutcome::Loaded(state) => *state,
        other => panic!("{other:?}"),
    };
    let secrets = store.load_secrets().unwrap();
    (state, secrets, observed.logs, case)
}

#[tokio::test]
async fn a_refused_new_certificate_is_replaced_by_the_previous_one_and_reported() {
    let csms = mtls_csms().await;
    let previous = accepted_pair(&csms);
    let case = mtls_case(csms, refused_pair(), previous.clone());
    let (state, secrets, _logs, case) =
        run_mtls(case, Some("DiscardedRenewedClientCertificate")).await;
    assert_eq!(
        state.certificates.charge_point_chain.as_deref(),
        Some(previous.0.trim())
    );
    assert_eq!(state.certificates.previous_charge_point_chain, None);
    assert_eq!(
        secrets.charge_point_key_pem.as_deref(),
        Some(previous.1.as_str())
    );
    assert_eq!(secrets.previous_charge_point_key_pem, None);
    case.csms.shutdown().await;
}

#[tokio::test]
async fn a_working_new_certificate_discards_the_previous_one_silently() {
    let csms = mtls_csms().await;
    let new = accepted_pair(&csms);
    let case = mtls_case(csms, new.clone(), refused_pair());
    let (state, secrets, logs, case) = run_mtls(case, None).await;
    assert!(
        !logs
            .iter()
            .any(|l| l.contains("DiscardedRenewedClientCertificate"))
    );
    assert_eq!(
        state.certificates.charge_point_chain.as_deref(),
        Some(new.0.trim())
    );
    assert_eq!(state.certificates.previous_charge_point_chain, None);
    assert_eq!(secrets.previous_charge_point_key_pem, None);
    assert!(
        !case
            .csms
            .received()
            .iter()
            .any(|r| r.payload["type"] == "DiscardedRenewedClientCertificate"),
        "nothing to report"
    );
    case.csms.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Expired client certificate (A00.FR.407, A00.FR.429 to A00.FR.431)
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_expired_client_certificate_is_still_presented() {
    let authority = ca("Expired client CA");
    let (cert, key) = server_leaf(&authority, &["localhost"], None);
    let mut server = TlsServer::start(cert, &key, true).await;
    let (chain_pem, key_pem) = client_pair(&authority, false);
    let tls = TlsConfig {
        skip_verify: true,
        identity_pem: Some(IdentityPem {
            chain_pem: chain_pem.clone(),
            key_pem,
        }),
        ..TlsConfig::default()
    };
    // The upgrade fails (the server only does TLS); the handshake is what counts.
    let _ = Connection::connect(&server.url(), "CP-001", &tls, "ocpp1.6", options()).await;
    let seen = server
        .next()
        .await
        .expect("handshake with an expired client certificate");
    assert_eq!(seen.len(), 1);
    let presented = CertificateDer::from_pem_slice(chain_pem.as_bytes()).unwrap();
    assert_eq!(seen[0], presented.as_ref());
}

#[test]
fn old_certificate_stores_load_without_the_previous_chain() {
    let store: kirby_ocpp_core::station::certificates::CertificateStore =
        serde_json::from_str(r#"{"roots":[],"charge_point_chain":null}"#).unwrap();
    assert_eq!(store.previous_charge_point_chain, None);
    let secrets: StationSecrets =
        serde_json::from_str(r#"{"version":6,"pending_key_pem":"k"}"#).unwrap();
    assert_eq!(secrets.previous_charge_point_key_pem, None);
}
