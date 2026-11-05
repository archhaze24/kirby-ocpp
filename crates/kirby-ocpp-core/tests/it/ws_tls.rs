//! `wss://` matrix (ports the TLS cases of `tests/ocpp-websocket.test.mjs`) against the
//! testkit's TLS mock CSMS, whose certificates are issued by a generated CA.

use std::path::{Path, PathBuf};

use crate::ws_support::{connect_with_tls, options};
use kirby_ocpp_core::config::TlsConfig;
use kirby_ocpp_core::ws::{ConnectError, ConnectOptions, Connection};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms, TlsOpts};

/// Temporary directory for PEM files (under the test target dir, not `/tmp`).
fn temp_dir() -> tempfile::TempDir {
    tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("temp dir")
}

fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("write PEM");
    path
}

fn tls(ca_file: Option<PathBuf>) -> TlsConfig {
    TlsConfig {
        ca_file,
        ..TlsConfig::default()
    }
}

async fn start_tls(require_client_cert: bool) -> MockCsms {
    MockCsms::start(CsmsOptions {
        tls: Some(TlsOpts {
            require_client_cert,
        }),
        ..CsmsOptions::default()
    })
    .await
    .expect("start TLS mock CSMS")
}

fn charge_point_url(csms: &MockCsms) -> String {
    csms.url().to_owned()
}

#[tokio::test]
async fn self_signed_server_is_rejected_by_default() {
    let csms = start_tls(false).await;
    let result = Connection::connect(
        &charge_point_url(&csms),
        "CP-001",
        &TlsConfig::default(),
        "ocpp1.6",
        options(),
    )
    .await;
    assert!(matches!(result, Err(ConnectError::Tls(_))), "{result:?}");
    assert_eq!(
        csms.connections(),
        0,
        "a failed TLS handshake is never a connection"
    );
}

#[tokio::test]
async fn self_signed_server_is_accepted_when_verification_is_disabled() {
    let csms = start_tls(false).await;
    let tls = TlsConfig {
        skip_verify: true,
        ..TlsConfig::default()
    };
    let (_connection, _events) = connect_with_tls(&charge_point_url(&csms), &tls, options()).await;
    let info = csms
        .wait_for_connection(crate::ws_support::STEP)
        .await
        .expect("connection recorded");
    assert_eq!(info.subprotocol.as_deref(), Some("ocpp1.6"));
    csms.shutdown().await;
}

#[tokio::test]
async fn skip_verify_still_completes_a_full_exchange() {
    let csms = start_tls(false).await;
    let tls = TlsConfig {
        skip_verify: true,
        ..TlsConfig::default()
    };
    let (connection, _events) = connect_with_tls(&charge_point_url(&csms), &tls, options()).await;
    let answer = connection
        .call(
            "BootNotification",
            serde_json::json!({"chargePointVendor": "Kirby", "chargePointModel": "TUI-16"}),
            None,
        )
        .await
        .expect("boot over TLS");
    assert_eq!(answer["status"], "Accepted");
    csms.shutdown().await;
}

#[tokio::test]
async fn ca_file_with_server_name_override_is_accepted() {
    let csms = start_tls(false).await;
    let material = csms.tls_material().expect("material").clone();
    let dir = temp_dir();
    let ca = write(dir.path(), "ca.pem", material.ca_cert_pem());

    let tls = TlsConfig {
        ca_file: Some(ca),
        server_name: Some("localhost".to_owned()),
        ..TlsConfig::default()
    };
    let (connection, _events) = connect_with_tls(&charge_point_url(&csms), &tls, options()).await;
    assert!(connection.is_open());
    csms.wait_for_connection(crate::ws_support::STEP)
        .await
        .expect("connection recorded");
    csms.shutdown().await;
}

#[tokio::test]
async fn ca_file_is_rejected_when_the_name_does_not_match() {
    let csms = start_tls(false).await;
    let material = csms.tls_material().expect("material").clone();
    let dir = temp_dir();
    let ca = write(dir.path(), "ca.pem", material.ca_cert_pem());

    let tls = TlsConfig {
        ca_file: Some(ca),
        server_name: Some("wrong.example".to_owned()),
        ..TlsConfig::default()
    };
    let result = Connection::connect(
        &charge_point_url(&csms),
        "CP-001",
        &tls,
        "ocpp1.6",
        options(),
    )
    .await;
    assert!(matches!(result, Err(ConnectError::Tls(_))), "{result:?}");
    csms.shutdown().await;
}

#[tokio::test]
async fn ca_file_that_does_not_sign_the_server_is_rejected() {
    let csms = start_tls(false).await;
    let other = MockCsms::start_tls(TlsOpts::default())
        .await
        .expect("second TLS CSMS");
    let other_ca = other
        .tls_material()
        .expect("material")
        .ca_cert_pem()
        .to_owned();
    let dir = temp_dir();
    let ca = write(dir.path(), "other-ca.pem", &other_ca);

    let result = Connection::connect(
        &charge_point_url(&csms),
        "CP-001",
        &tls(Some(ca)),
        "ocpp1.6",
        options(),
    )
    .await;
    assert!(matches!(result, Err(ConnectError::Tls(_))), "{result:?}");
    csms.shutdown().await;
    other.shutdown().await;
}

#[tokio::test]
async fn mutual_tls_with_a_ca_issued_client_certificate_is_accepted() {
    let csms = start_tls(true).await;
    let material = csms.tls_material().expect("material").clone();
    let dir = temp_dir();
    let tls = TlsConfig {
        ca_file: Some(write(dir.path(), "ca.pem", material.ca_cert_pem())),
        cert_file: Some(write(dir.path(), "client.pem", material.client_cert_pem())),
        key_file: Some(write(
            dir.path(),
            "client-key.pem",
            material.client_key_pem(),
        )),
        server_name: Some("localhost".to_owned()),
        skip_verify: false,
        ..TlsConfig::default()
    };
    let (connection, _events) = connect_with_tls(&charge_point_url(&csms), &tls, options()).await;
    assert!(connection.is_open());
    csms.wait_for_connection(crate::ws_support::STEP)
        .await
        .expect("connection recorded");
    csms.shutdown().await;
}

#[tokio::test]
async fn mutual_tls_without_a_client_certificate_is_rejected() {
    let csms = start_tls(true).await;
    let material = csms.tls_material().expect("material").clone();
    let dir = temp_dir();
    let tls = TlsConfig {
        ca_file: Some(write(dir.path(), "ca.pem", material.ca_cert_pem())),
        server_name: Some("localhost".to_owned()),
        ..TlsConfig::default()
    };
    // TLS 1.3 reports the rejected client certificate after our handshake returns,
    // so the failure can surface as either a TLS or a handshake error.
    let result = Connection::connect(
        &charge_point_url(&csms),
        "CP-001",
        &tls,
        "ocpp1.6",
        ConnectOptions {
            call_timeout: crate::ws_support::STEP,
            ..options()
        },
    )
    .await;
    assert!(
        matches!(
            result,
            Err(ConnectError::Tls(_) | ConnectError::Handshake(_))
        ),
        "{result:?}"
    );
    assert_eq!(csms.connections(), 0);
    csms.shutdown().await;
}

#[tokio::test]
async fn unreadable_ca_file_is_a_typed_file_error() {
    let dir = temp_dir();
    let missing = dir.path().join("missing-ca.pem");
    let result = Connection::connect(
        "wss://127.0.0.1:9/ocpp",
        "CP-001",
        &tls(Some(missing.clone())),
        "ocpp1.6",
        options(),
    )
    .await;
    match result {
        Err(ConnectError::TlsFile { path, .. }) => assert_eq!(path, missing),
        other => panic!("expected a TlsFile error, got {other:?}"),
    }
}

#[tokio::test]
async fn ca_file_without_certificates_is_a_typed_file_error() {
    let dir = temp_dir();
    let empty = write(dir.path(), "empty.pem", "not a certificate\n");
    let result = Connection::connect(
        "wss://127.0.0.1:9/ocpp",
        "CP-001",
        &tls(Some(empty.clone())),
        "ocpp1.6",
        options(),
    )
    .await;
    match result {
        Err(ConnectError::TlsFile { path, reason }) => {
            assert_eq!(path, empty);
            assert!(reason.contains("no PEM certificates"), "{reason}");
        }
        other => panic!("expected a TlsFile error, got {other:?}"),
    }
}

#[tokio::test]
async fn client_certificate_without_key_is_a_configuration_error() {
    let csms = start_tls(false).await;
    let material = csms.tls_material().expect("material").clone();
    let dir = temp_dir();
    let tls = TlsConfig {
        cert_file: Some(write(dir.path(), "client.pem", material.client_cert_pem())),
        ..TlsConfig::default()
    };
    let result = Connection::connect(
        &charge_point_url(&csms),
        "CP-001",
        &tls,
        "ocpp1.6",
        options(),
    )
    .await;
    assert!(matches!(result, Err(ConnectError::Tls(_))), "{result:?}");
    csms.shutdown().await;
}

#[tokio::test]
async fn key_file_without_a_private_key_is_a_typed_file_error() {
    let csms = start_tls(false).await;
    let material = csms.tls_material().expect("material").clone();
    let dir = temp_dir();
    let tls = TlsConfig {
        cert_file: Some(write(dir.path(), "client.pem", material.client_cert_pem())),
        key_file: Some(write(dir.path(), "key.pem", material.client_cert_pem())),
        ..TlsConfig::default()
    };
    let result = Connection::connect(
        &charge_point_url(&csms),
        "CP-001",
        &tls,
        "ocpp1.6",
        options(),
    )
    .await;
    assert!(
        matches!(result, Err(ConnectError::TlsFile { .. })),
        "{result:?}"
    );
    csms.shutdown().await;
}

#[tokio::test]
async fn in_memory_identity_and_installed_roots_work_like_files() {
    let csms = start_tls(true).await;
    let material = csms.tls_material().expect("material").clone();
    let tls = TlsConfig {
        extra_roots_pem: vec![material.ca_cert_pem().to_owned()],
        identity_pem: Some(kirby_ocpp_core::config::IdentityPem {
            chain_pem: material.client_cert_pem().to_owned(),
            key_pem: material.client_key_pem().to_owned(),
        }),
        server_name: Some("localhost".to_owned()),
        ..TlsConfig::default()
    };
    let (connection, _events) = connect_with_tls(&charge_point_url(&csms), &tls, options()).await;
    assert!(connection.is_open());
    csms.wait_for_connection(crate::ws_support::STEP)
        .await
        .expect("connection recorded");
    csms.shutdown().await;
}
