//! `wss://` behaviour: handshake against the generated CA, and mTLS.

use crate::common::{STEP, cp_call, heartbeat, result_payload, tls_client_config, wait_closed};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms, TlsOpts};
use rustls::RootCertStore;
use serde_json::json;

#[tokio::test]
async fn tls_handshake_with_generated_ca_roundtrip() {
    let csms = MockCsms::start_tls(TlsOpts::default())
        .await
        .expect("start TLS mock CSMS");
    assert!(csms.url().starts_with("wss://127.0.0.1:"));
    let material = csms.tls_material().expect("TLS material");
    assert!(material.ca_cert_pem().contains("BEGIN CERTIFICATE"));
    assert!(material.client_key_pem().contains("BEGIN PRIVATE KEY"));

    // Write the CA to a temporary directory and load it back, as a test would for an emulator.
    let dir = tempfile::tempdir().expect("tempdir");
    let ca_path = dir.path().join("ca.pem");
    std::fs::write(&ca_path, material.ca_cert_pem()).expect("write CA");
    let ca_pem = std::fs::read_to_string(&ca_path).expect("read CA");

    let config = tls_client_config(&ca_pem, None);
    let mut cp = crate::common::connect_tls(csms.addr(), "/ocpp/CP-TLS", config)
        .await
        .expect("TLS connect with CA trust");
    let beat = result_payload(cp_call(&mut cp, "hb", "Heartbeat", heartbeat()).await);
    assert!(beat["currentTime"].is_string());

    let connection = csms.wait_for_connection(STEP).await.expect("connection");
    assert_eq!(connection.charge_point_id.as_deref(), Some("CP-TLS"));
    csms.wait_for_call("Heartbeat", STEP)
        .await
        .expect("recorded");
    csms.shutdown().await;
    assert!(wait_closed(&mut cp).await);
}

#[tokio::test]
async fn tls_client_without_the_ca_is_rejected() {
    let csms = MockCsms::start_tls(TlsOpts::default())
        .await
        .expect("start TLS mock CSMS");

    // An empty trust store: the server certificate cannot be verified.
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let result =
        crate::common::connect_tls(csms.addr(), "/ocpp/CP-X", std::sync::Arc::new(config)).await;
    assert!(result.is_err(), "unverifiable server certificate must fail");
    assert_eq!(csms.connections(), 0, "failed TLS is never a connection");
}

#[tokio::test]
async fn mtls_rejects_a_client_without_certificate() {
    let csms = MockCsms::start(CsmsOptions {
        tls: Some(TlsOpts {
            require_client_cert: true,
        }),
        ..CsmsOptions::default()
    })
    .await
    .expect("start mTLS mock CSMS");
    let ca = csms
        .tls_material()
        .expect("material")
        .ca_cert_pem()
        .to_owned();

    let no_identity = tls_client_config(&ca, None);
    let result = crate::common::connect_tls(csms.addr(), "/ocpp/CP-NOCERT", no_identity).await;
    assert!(
        result.is_err(),
        "mTLS server must refuse a client without a certificate"
    );
    assert_eq!(
        csms.connections(),
        0,
        "rejected handshakes are not recorded"
    );
}

#[tokio::test]
async fn mtls_accepts_a_client_with_a_ca_issued_certificate() {
    let csms = MockCsms::start_tls(TlsOpts {
        require_client_cert: true,
    })
    .await
    .expect("start mTLS mock CSMS");
    let material = csms.tls_material().expect("material");
    let config = tls_client_config(
        material.ca_cert_pem(),
        Some((material.client_cert_pem(), material.client_key_pem())),
    );

    let mut cp = crate::common::connect_tls(csms.addr(), "/ocpp/CP-MTLS", config)
        .await
        .expect("mTLS connect");
    let reply = result_payload(
        cp_call(
            &mut cp,
            "boot",
            "BootNotification",
            json!({"chargePointVendor": "kirby-ocpp", "chargePointModel": "TUI-16"}),
        )
        .await,
    );
    assert_eq!(reply["status"], "Accepted");
    csms.wait_for_call("BootNotification", STEP)
        .await
        .expect("boot recorded");
}
