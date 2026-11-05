//! Security Whitepaper, certificate half: configuration keys, certificate store commands,
//! `SignCertificate` / `CertificateSigned`, security events and the profile rules.

mod station_support;

use std::sync::{Arc, Mutex};

use kirby_ocpp_core::Station;
use kirby_ocpp_core::station::Command;
use kirby_ocpp_testkit::Reply;
use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair};
use ring::digest;
use serde_json::json;
use station_support::{Rig, STEP, t0, test_config};

fn ca(name: &str) -> (rcgen::Certificate, rcgen::Issuer<'static, KeyPair>) {
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
    (cert, rcgen::Issuer::new(params, key))
}

fn expired_root() -> String {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(10);
    params.not_after = now - time::Duration::days(1);
    params.self_signed(&key).unwrap().pem()
}

/// Signs a CSR with `issuer`; returns leaf + root as a chain.
fn sign(
    csr_pem: &str,
    issuer: &rcgen::Issuer<'static, KeyPair>,
    root: &rcgen::Certificate,
) -> String {
    let csr = rcgen::CertificateSigningRequestParams::from_pem(csr_pem).unwrap();
    let leaf = csr.signed_by(issuer).unwrap();
    format!("{}\n{}", leaf.pem(), root.pem())
}

fn ok_event_reply(rig: &Rig) {
    rig.csms
        .respond("SecurityEventNotification", |_| Reply::Result(json!({})));
}

async fn rig() -> Rig {
    let rig = Rig::start().await;
    ok_event_reply(&rig);
    rig
}

#[tokio::test]
async fn startup_event_follows_the_first_accepted_boot_once() {
    let mut rig = rig().await;
    rig.connect_booted().await;
    let call = rig
        .csms
        .wait_for_call("SecurityEventNotification", STEP)
        .await
        .unwrap();
    assert_eq!(call["type"], "StartupOfTheDevice");
    assert!(call["timestamp"].is_string());
    // A reconnect boots again but is not a new process start.
    let mark = rig.csms.mark();
    rig.send(Command::Reconnect).await;
    rig.wait_registration(kirby_ocpp_core::ocpp::types::RegistrationStatus::Accepted)
        .await;
    rig.csms
        .wait_for_call_count_after(mark, "StatusNotification", 3, |_| true, STEP)
        .await
        .unwrap();
    rig.assert_no_calls("SecurityEventNotification", mark);
    rig.shutdown().await;
}

#[tokio::test]
async fn certificate_store_roundtrip_with_independent_hashes() {
    let mut rig = rig().await;
    rig.connect_booted().await;
    let (root, _) = ca("Root One");
    let pem = root.pem();
    let kind = "CentralSystemRootCertificate";
    let reply = rig
        .csms_call(
            "InstallCertificate",
            json!({"certificateType": kind, "certificate": pem}),
        )
        .await;
    assert_eq!(reply["status"], "Accepted");
    assert_eq!(rig.station.snapshot().certificates.len(), 1);

    let ids = rig
        .csms_call(
            "GetInstalledCertificateIds",
            json!({"certificateType": kind}),
        )
        .await;
    assert_eq!(ids["status"], "Accepted");
    let hash = ids["certificateHashData"][0].clone();
    // Independent computation straight from the DER bytes.
    let der = root.der().to_vec();
    let (_, x509) = x509_parser::parse_x509_certificate(&der).unwrap();
    let hex = |b: &[u8]| {
        use std::fmt::Write as _;
        b.iter().fold(String::new(), |mut out, x| {
            let _ = write!(out, "{x:02x}");
            out
        })
    };
    assert_eq!(
        hash["issuerNameHash"],
        hex(digest::digest(&digest::SHA256, x509.issuer().as_raw()).as_ref())
    );
    assert_eq!(
        hash["issuerKeyHash"],
        hex(digest::digest(&digest::SHA256, &x509.public_key().subject_public_key.data).as_ref())
    );
    assert_eq!(hash["hashAlgorithm"], "SHA256");

    let none = rig
        .csms_call(
            "GetInstalledCertificateIds",
            json!({"certificateType": "ManufacturerRootCertificate"}),
        )
        .await;
    assert_eq!(none["status"], "NotFound");

    let deleted = rig
        .csms_call("DeleteCertificate", json!({"certificateHashData": hash}))
        .await;
    assert_eq!(deleted["status"], "Accepted");
    let again = rig
        .csms_call("DeleteCertificate", json!({"certificateHashData": hash}))
        .await;
    assert_eq!(again["status"], "NotFound");
    assert_eq!(rig.station.snapshot().certificates.len(), 0);
    rig.shutdown().await;
}

#[tokio::test]
async fn full_expired_and_garbage_certificates_are_rejected() {
    let mut rig = rig().await;
    rig.connect_booted().await;
    let kind = "ManufacturerRootCertificate";
    for pem in [expired_root(), "garbage".to_owned()] {
        let reply = rig
            .csms_call(
                "InstallCertificate",
                json!({"certificateType": kind, "certificate": pem}),
            )
            .await;
        assert_eq!(reply["status"], "Rejected");
    }
    for n in 0..8 {
        let (root, _) = ca(&format!("Root {n}"));
        let reply = rig
            .csms_call(
                "InstallCertificate",
                json!({"certificateType": kind, "certificate": root.pem()}),
            )
            .await;
        assert_eq!(reply["status"], "Accepted", "{n}");
    }
    let (extra, _) = ca("One too many");
    let reply = rig
        .csms_call(
            "InstallCertificate",
            json!({"certificateType": kind, "certificate": extra.pem()}),
        )
        .await;
    assert_eq!(reply["status"], "Rejected");
    rig.shutdown().await;
}

#[tokio::test]
async fn csr_flow_stores_the_certificate_and_survives_a_restart() {
    let mut rig = rig().await;
    let (root, issuer) = ca("Test CA");
    let chain: Arc<Mutex<Option<String>>> = Arc::default();
    {
        let chain = Arc::clone(&chain);
        rig.csms.respond("SignCertificate", move |payload| {
            let csr = payload["csr"].as_str().unwrap().to_owned();
            *chain.lock().unwrap() = Some(sign(&csr, &issuer, &root));
            Reply::Result(json!({"status": "Accepted"}))
        });
    }
    rig.connect_booted().await;
    let outcome = rig.send(Command::RequestCertificate).await;
    assert!(matches!(
        outcome,
        kirby_ocpp_core::station::Outcome::Response { .. }
    ));
    assert!(rig.station.snapshot().certificate_request_pending);
    let signed = chain.lock().unwrap().clone().expect("CSR was signed");

    // A chain for some other key is refused and reported.
    let (_, other_issuer) = ca("Other CA");
    let (other_root, _) = ca("Other root");
    let (other_csr, _) =
        kirby_ocpp_core::station::certificates::generate_csr("CP-001", "x").unwrap();
    let foreign = sign(&other_csr, &other_issuer, &other_root);
    let mark = rig.csms.mark();
    let reply = rig
        .csms_call("CertificateSigned", json!({"certificateChain": foreign}))
        .await;
    assert_eq!(reply["status"], "Rejected");
    rig.csms
        .wait_for_call_after(
            mark,
            "SecurityEventNotification",
            |p| p["type"] == "InvalidChargePointCertificate",
            STEP,
        )
        .await
        .unwrap();
    assert!(rig.station.snapshot().certificate_request_pending);

    let reply = rig
        .csms_call("CertificateSigned", json!({"certificateChain": signed}))
        .await;
    assert_eq!(reply["status"], "Accepted");
    let snapshot = rig.station.snapshot();
    assert!(!snapshot.certificate_request_pending);
    assert!(
        snapshot
            .certificates
            .iter()
            .any(|c| c.kind == "ChargePointCertificate")
    );

    // The private key is in the secrets file, never in the main state file.
    let state_dir = rig.dir.path().to_path_buf();
    let Rig {
        csms,
        station,
        join,
        dir,
        clock,
        ..
    } = rig;
    station.shutdown().await.unwrap();
    join.unwrap().await.unwrap();
    let mut state = String::new();
    let mut secrets = String::new();
    for entry in std::fs::read_dir(&state_dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with(".station.secrets.json") {
            secrets = std::fs::read_to_string(&path).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        } else if name.ends_with(".station.json") {
            state = std::fs::read_to_string(&path).unwrap();
        }
    }
    assert!(secrets.contains("PRIVATE KEY"), "{secrets}");
    assert!(state.contains("BEGIN CERTIFICATE") && !state.contains("PRIVATE KEY"));

    let config = test_config(csms.url(), &dir);
    let (station, join) = Station::spawn(config, Arc::new(clock.clone()));
    assert!(
        station
            .snapshot()
            .certificates
            .iter()
            .any(|c| c.kind == "ChargePointCertificate")
    );
    station.shutdown().await.unwrap();
    join.await.unwrap();
    csms.shutdown().await;
}

#[tokio::test]
async fn rejected_sign_certificate_keeps_nothing_pending() {
    let mut rig = rig().await;
    rig.csms.respond("SignCertificate", |_| {
        Reply::Result(json!({"status": "Rejected"}))
    });
    rig.connect_booted().await;
    let outcome = rig.send(Command::RequestCertificate).await;
    assert!(matches!(
        outcome,
        kirby_ocpp_core::station::Outcome::Response { .. }
    ));
    assert!(!rig.station.snapshot().certificate_request_pending);
    // And a CertificateSigned without a request is refused.
    let (root, issuer) = ca("CA");
    let (csr, _) = kirby_ocpp_core::station::certificates::generate_csr("CP-001", "x").unwrap();
    let chain = sign(&csr, &issuer, &root);
    let reply = rig
        .csms_call("CertificateSigned", json!({"certificateChain": chain}))
        .await;
    assert_eq!(reply["status"], "Rejected");
    rig.shutdown().await;
}

#[tokio::test]
async fn authorization_key_is_write_only_and_never_logged() {
    let mut rig = rig().await;
    rig.connect_booted().await;
    let secret = "0123456789abcdefSECRETKEY";
    rig.cfg("AuthorizationKey", secret).await;
    for request in [json!({}), json!({"key": ["AuthorizationKey"]})] {
        let reply = rig.csms_call("GetConfiguration", request).await;
        let text = reply.to_string();
        assert!(!text.contains(secret), "{text}");
        let entry = reply["configurationKey"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["key"] == "AuthorizationKey")
            .cloned();
        if let Some(entry) = entry {
            assert!(entry.get("value").is_none(), "{entry}");
        }
    }
    let snapshot = rig.station.snapshot();
    let item = snapshot
        .config
        .iter()
        .find(|c| c.key == "AuthorizationKey")
        .unwrap();
    assert_eq!(item.value, "********");
    rig.observed.drain();
    assert!(
        rig.observed.logs.iter().all(|l| !l.contains(secret)),
        "{:#?}",
        rig.observed.logs
    );

    let dir = rig.dir.path().to_path_buf();
    rig.station.shutdown().await.unwrap();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        let is_secrets = path.to_string_lossy().ends_with(".station.secrets.json");
        assert_eq!(text.contains(secret), is_secrets, "{}", path.display());
    }
    // Restored from the secrets file after a restart.
    let config = test_config(rig.csms.url(), &rig.dir);
    let (station, join) = Station::spawn(config, Arc::new(rig.clock.clone()));
    assert_eq!(
        station
            .snapshot()
            .config
            .iter()
            .find(|c| c.key == "AuthorizationKey")
            .unwrap()
            .value,
        "********"
    );
    station.shutdown().await.unwrap();
    join.await.unwrap();
}

#[tokio::test]
async fn security_profile_can_only_go_up_and_needs_its_prerequisites() {
    let mut rig = rig().await;
    rig.connect_booted().await;
    let status = |key: &'static str, value: &'static str| {
        let csms = &rig.csms;
        async move {
            csms.send_call("ChangeConfiguration", json!({"key": key, "value": value}))
                .await
                .unwrap()
        }
    };
    let get = |frame: kirby_ocpp_core::ocpp::Frame| match frame {
        kirby_ocpp_core::ocpp::Frame::CallResult { payload, .. } => {
            payload["status"].as_str().unwrap().to_owned()
        }
        other => panic!("{}", other.to_json_string()),
    };
    // No key yet, ws:// URL: 1 needs a key, 2 and 3 need TLS.
    assert_eq!(get(status("SecurityProfile", "1").await), "Rejected");
    assert_eq!(get(status("SecurityProfile", "2").await), "Rejected");
    assert_eq!(get(status("SecurityProfile", "3").await), "Rejected");
    assert_eq!(get(status("SecurityProfile", "9").await), "Rejected");
    assert_eq!(
        get(status("AuthorizationKey", "0123456789abcdef").await),
        "Accepted"
    );
    assert_eq!(get(status("SecurityProfile", "1").await), "Accepted");
    assert_eq!(rig.station.snapshot().security_profile, 1);
    assert_eq!(get(status("SecurityProfile", "0").await), "Rejected");
    assert_eq!(rig.station.snapshot().security_profile, 1);
    // Reconfiguration is reported.
    rig.csms
        .wait_for_call_after(
            0,
            "SecurityEventNotification",
            |p| p["type"] == "ReconfigurationOfSecurityParameters",
            STEP,
        )
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn offline_security_events_are_queued_validated_and_sent_after_boot() {
    let mut rig = rig().await;
    let bad = rig
        .station
        .send(Command::SendSecurityEvent {
            event_type: "NotAnEvent".into(),
            tech_info: None,
        })
        .await;
    assert!(bad.is_err());
    rig.send(Command::SendSecurityEvent {
        event_type: "tamperdetectionactivated".into(),
        tech_info: Some("lid opened".into()),
    })
    .await;
    assert_eq!(rig.station.snapshot().pending_security_events, 1);
    rig.connect_booted().await;
    let call = rig
        .csms
        .wait_for_call_after(
            0,
            "SecurityEventNotification",
            |p| p["type"] == "TamperDetectionActivated",
            STEP,
        )
        .await
        .unwrap();
    assert_eq!(call["techInfo"], "lid opened");
    station_support::wait_state(&rig.station, "queue drained", |s| {
        s.pending_security_events == 0
    })
    .await;
    rig.shutdown().await;
}

#[tokio::test]
async fn reset_reports_reset_or_reboot() {
    let mut rig = rig().await;
    rig.connect_booted().await;
    rig.csms
        .expect_status("Reset", json!({"type": "Soft"}), "Accepted")
        .await
        .unwrap();
    rig.csms
        .wait_for_call_after(
            0,
            "SecurityEventNotification",
            |p| p["type"] == "ResetOrReboot",
            STEP,
        )
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn profile_one_sends_basic_auth_with_the_authorization_key_and_401_is_reported() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let head: Arc<Mutex<String>> = Arc::default();
    let seen = Arc::clone(&head);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0_u8; 4096];
            let n = socket.read(&mut buf).await.unwrap_or(0);
            *seen.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).into_owned();
            let _ = socket
                .write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n")
                .await;
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config(&format!("ws://127.0.0.1:{port}/ocpp"), &dir);
    config.reconnect.enabled = false;
    let (station, join) = Station::spawn(config, Arc::new(kirby_ocpp_core::ManualClock::new(t0())));
    station
        .send(Command::SetConfiguration {
            key: "AuthorizationKey".into(),
            value: "0123456789abcdef".into(),
        })
        .await
        .unwrap();
    station
        .send(Command::SetConfiguration {
            key: "SecurityProfile".into(),
            value: "1".into(),
        })
        .await
        .unwrap();
    let before = station.snapshot().pending_security_events;
    station.connect().await.unwrap();
    station_support::wait_state(&station, "401 reported", |s| {
        s.pending_security_events > before
    })
    .await;
    let text = head.lock().unwrap().clone();
    let expected = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode("CP-001:0123456789abcdef")
    };
    assert!(
        text.contains(&format!("Authorization: Basic {expected}")),
        "{text}"
    );
    station.shutdown().await.unwrap();
    join.await.unwrap();
}

#[tokio::test]
async fn timed_out_sign_certificate_does_not_restore_a_stale_key_over_a_newer_request() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut rig = Rig::with(kirby_ocpp_testkit::CsmsOptions::default(), |c| {
        c.with_call_timeout_ms(300)
    })
    .await;
    ok_event_reply(&rig);
    let (root, issuer) = ca("Test CA");
    let second_chain: Arc<Mutex<Option<String>>> = Arc::default();
    let calls = Arc::new(AtomicUsize::new(0));
    {
        let second_chain = Arc::clone(&second_chain);
        rig.csms.respond("SignCertificate", move |payload| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Reply::Silence;
            }
            let csr = payload["csr"].as_str().unwrap().to_owned();
            *second_chain.lock().unwrap() = Some(sign(&csr, &issuer, &root));
            Reply::Result(json!({"status": "Accepted"}))
        });
    }
    rig.connect_booted().await;
    // Two overlapping requests: the first one times out while the second is queued.
    let first = rig.station.send(Command::RequestCertificate);
    let second = rig.station.send(Command::RequestCertificate);
    let (first, second) = tokio::join!(first, second);
    assert!(first.is_err(), "{first:?}");
    assert!(second.is_ok(), "{second:?}");
    let signed = second_chain
        .lock()
        .unwrap()
        .clone()
        .expect("second CSR signed");
    let reply = rig
        .csms_call("CertificateSigned", json!({"certificateChain": signed}))
        .await;
    assert_eq!(reply["status"], "Accepted");
    rig.shutdown().await;
}

#[tokio::test]
async fn repeated_security_events_are_all_reported_and_control_characters_are_stripped() {
    let mut rig = rig().await;
    for _ in 0..2 {
        rig.send(Command::SendSecurityEvent {
            event_type: "FailedToAuthenticateAtCentralSystem".into(),
            tech_info: Some("HTTP\n401\u{1b}[31m".into()),
        })
        .await;
    }
    assert_eq!(rig.station.snapshot().pending_security_events, 2);
    rig.connect_booted().await;
    let is_event = |p: &serde_json::Value| p["type"] == "FailedToAuthenticateAtCentralSystem";
    rig.csms
        .wait_for_call_after(0, "SecurityEventNotification", is_event, STEP)
        .await
        .expect("first event delivered");
    let first = rig
        .csms
        .calls_since(0)
        .iter()
        .position(|call| call.action == "SecurityEventNotification" && is_event(&call.payload))
        .expect("first event recorded");
    rig.csms
        .wait_for_call_after(first + 1, "SecurityEventNotification", is_event, STEP)
        .await
        .expect("second event delivered");
    let mut calls = rig.calls("SecurityEventNotification", 0);
    calls.retain(is_event);
    let sent: Vec<_> = calls
        .iter()
        .filter(|p| p["type"] == "FailedToAuthenticateAtCentralSystem")
        .collect();
    assert_eq!(sent.len(), 2, "{calls:?}");
    for call in sent {
        assert_eq!(call["techInfo"], "HTTP 401 [31m");
    }
    rig.shutdown().await;
}

#[tokio::test]
async fn setting_the_current_profile_again_is_rejected_for_profiles_two_and_three() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config("wss://127.0.0.1:9/ocpp", &dir);
    config.reconnect.enabled = false;
    let (station, join) = Station::spawn(config, Arc::new(kirby_ocpp_core::ManualClock::new(t0())));
    let set = |key: &str, value: &str| {
        station.send(Command::SetConfiguration {
            key: key.into(),
            value: value.into(),
        })
    };
    set("AuthorizationKey", "0123456789abcdef").await.unwrap();
    set("SecurityProfile", "1").await.unwrap();
    // 1 -> 1 stays accepted (A05.FR.01 only covers values above 1).
    set("SecurityProfile", "1").await.unwrap();
    set("SecurityProfile", "2").await.unwrap();
    assert!(set("SecurityProfile", "2").await.is_err());
    assert_eq!(station.snapshot().security_profile, 2);
    station.shutdown().await.unwrap();
    join.await.unwrap();
}

#[tokio::test]
async fn new_security_parameters_make_the_station_reconnect() {
    let mut rig = rig().await;
    rig.connect_booted().await;
    assert_eq!(rig.csms.connections(), 1);
    rig.cfg("AuthorizationKey", "0123456789abcdef").await;
    // The reply came first (cfg waited for it); then the station reconnects by itself.
    rig.csms
        .wait_for_connection_count(2, STEP)
        .await
        .expect("reconnect after AuthorizationKey");
    rig.wait_registration(kirby_ocpp_core::ocpp::types::RegistrationStatus::Accepted)
        .await;
    rig.shutdown().await;
}

#[tokio::test]
async fn the_edition_four_event_types_can_be_sent() {
    let mut rig = rig().await;
    rig.connect_booted().await;
    for name in [
        "MaintenanceLoginAccepted",
        "maintenanceloginfailed",
        "DiscardedRenewedClientCertificate",
    ] {
        rig.send(Command::SendSecurityEvent {
            event_type: name.into(),
            tech_info: Some(r#"{"user": "ops", "origin": "touch screen"}"#.into()),
        })
        .await;
    }
    for name in [
        "MaintenanceLoginAccepted",
        "MaintenanceLoginFailed",
        "DiscardedRenewedClientCertificate",
    ] {
        rig.csms
            .wait_for_call_after(0, "SecurityEventNotification", |p| p["type"] == name, STEP)
            .await
            .unwrap_or_else(|_| panic!("{name} delivered"));
    }
    rig.shutdown().await;
}

#[tokio::test]
async fn downgrade_from_three_to_two_needs_the_allow_key() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config("wss://127.0.0.1:9/ocpp", &dir);
    config.reconnect.enabled = false;
    // Any path satisfies the "client identity available" check for profile 3.
    config.tls.cert_file = Some(dir.path().join("client.pem"));
    config.tls.key_file = Some(dir.path().join("client.key"));
    let (station, join) = Station::spawn(config, Arc::new(kirby_ocpp_core::ManualClock::new(t0())));
    let set = |key: &str, value: &str| {
        station.send(Command::SetConfiguration {
            key: key.into(),
            value: value.into(),
        })
    };
    assert!(
        station
            .snapshot()
            .config
            .iter()
            .any(|c| c.key == "AllowSecurityProfileDowngrade" && c.value == "false" && !c.readonly)
    );
    set("AuthorizationKey", "0123456789abcdef").await.unwrap();
    set("SecurityProfile", "3").await.unwrap();
    assert!(set("SecurityProfile", "2").await.is_err(), "no allow key");
    set("AllowSecurityProfileDowngrade", "true").await.unwrap();
    // Never to 1 or 0, and the same profile again stays refused.
    assert!(set("SecurityProfile", "1").await.is_err());
    assert!(set("SecurityProfile", "0").await.is_err());
    assert!(set("SecurityProfile", "3").await.is_err());
    set("SecurityProfile", "2").await.unwrap();
    assert_eq!(station.snapshot().security_profile, 2);
    // From 2 only 1 and 0 are lower: still refused with the key set.
    assert!(set("SecurityProfile", "1").await.is_err());
    station.shutdown().await.unwrap();
    join.await.unwrap();
}
