//! Security Whitepaper: `SignedUpdateFirmware` + `SignedFirmwareStatusNotification`.

use crate::station_support::{Rig, STEP};
use kirby_ocpp_core::Clock;
use kirby_ocpp_core::ocpp::ErrorCode;
use kirby_ocpp_core::station::{Command, FirmwareOutcome, ResponseRule};
use kirby_ocpp_testkit::Reply;
use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair};
use serde_json::{Value, json};

async fn booted() -> Rig {
    let mut rig = Rig::start().await;
    for action in [
        "SignedFirmwareStatusNotification",
        "SecurityEventNotification",
    ] {
        rig.csms.respond(action, |_| Reply::Result(json!({})));
    }
    rig.connect_booted().await;
    rig
}

struct Ca {
    cert: rcgen::Certificate,
    issuer: rcgen::Issuer<'static, KeyPair>,
}

fn dn(name: &str) -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, name);
    dn
}

fn ca(name: &str) -> Ca {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name = dn(name);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(30);
    let cert = params.self_signed(&key).unwrap();
    Ca {
        cert,
        issuer: rcgen::Issuer::new(params, key),
    }
}

/// A leaf certificate signed by `ca`, valid for the given number of days from now.
fn leaf(ca: &Ca, from_days: i64, to_days: i64) -> String {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name = dn("firmware signer");
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now + time::Duration::days(from_days);
    params.not_after = now + time::Duration::days(to_days);
    params.signed_by(&key, &ca.issuer).unwrap().pem()
}

fn request(id: i32, location: &str, certificate: &str) -> Value {
    json!({"requestId": id, "firmware": {
        "location": location,
        "retrieveDateTime": "2020-01-01T00:00:00Z",
        "signingCertificate": certificate,
        "signature": "c2ln"}})
}

fn statuses(rig: &Rig, mark: usize) -> Vec<(String, Option<i64>)> {
    rig.calls("SignedFirmwareStatusNotification", mark)
        .iter()
        .map(|c| {
            (
                c["status"].as_str().unwrap_or_default().to_owned(),
                c["requestId"].as_i64(),
            )
        })
        .collect()
}

async fn wait_status(rig: &Rig, mark: usize, status: &str, id: i64) {
    rig.csms
        .wait_for_call_after(
            mark,
            "SignedFirmwareStatusNotification",
            |p| p["status"] == status && p["requestId"] == id,
            STEP,
        )
        .await
        .unwrap();
}

async fn wait_event(rig: &Rig, mark: usize, kind: &str) {
    rig.csms
        .wait_for_call_after(
            mark,
            "SecurityEventNotification",
            |p| p["type"] == kind,
            STEP,
        )
        .await
        .unwrap_or_else(|e| panic!("{kind}: {e}"));
}

#[tokio::test]
async fn happy_path_reports_the_whole_sequence() {
    let rig = booted().await;
    let signer = ca("signer");
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "SignedUpdateFirmware",
            request(5, "https://host/fw.bin", &signer.cert.pem()),
            "Accepted",
        )
        .await
        .unwrap();
    wait_status(&rig, mark, "Installed", 5).await;
    let seen: Vec<_> = statuses(&rig, mark).into_iter().map(|(s, _)| s).collect();
    assert_eq!(
        seen,
        [
            "Downloading",
            "Downloaded",
            "SignatureVerified",
            "Installing",
            "InstallRebooting",
            "Installed"
        ]
    );
    assert!(statuses(&rig, mark).iter().all(|(_, id)| *id == Some(5)));
    wait_event(&rig, mark, "FirmwareUpdated").await;
    let snap = rig.station.snapshot();
    assert_eq!(snap.signed_firmware_request_id, Some(5));
    rig.shutdown().await;
}

#[tokio::test]
async fn future_times_add_scheduled_statuses() {
    let rig = booted().await;
    let signer = ca("signer");
    let mut body = request(6, "ftp://host/fw", &signer.cert.pem());
    // The manual clock does not advance: these stay in its future.
    let t0 = rig.clock.now_utc();
    body["firmware"]["retrieveDateTime"] =
        json!((t0 + chrono::Duration::milliseconds(150)).to_rfc3339());
    body["firmware"]["installDateTime"] =
        json!((t0 + chrono::Duration::milliseconds(300)).to_rfc3339());
    let mark = rig.csms.mark();
    rig.csms
        .expect_status("SignedUpdateFirmware", body, "Accepted")
        .await
        .unwrap();
    wait_status(&rig, mark, "Installed", 6).await;
    let seen: Vec<_> = statuses(&rig, mark).into_iter().map(|(s, _)| s).collect();
    assert_eq!(seen[0], "DownloadScheduled");
    assert!(seen.contains(&"InstallScheduled".to_owned()), "{seen:?}");
    rig.shutdown().await;
}

#[tokio::test]
async fn invalid_signature_outcome_sends_the_status_and_a_security_event() {
    let rig = booted().await;
    rig.send(Command::SetFirmwareOutcome {
        outcome: FirmwareOutcome::InvalidSignature,
    })
    .await;
    let signer = ca("signer");
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "SignedUpdateFirmware",
            request(7, "sftp://host/fw", &signer.cert.pem()),
            "Accepted",
        )
        .await
        .unwrap();
    wait_status(&rig, mark, "InvalidSignature", 7).await;
    wait_event(&rig, mark, "InvalidFirmwareSignature").await;
    let seen: Vec<_> = statuses(&rig, mark).into_iter().map(|(s, _)| s).collect();
    assert_eq!(seen, ["Downloading", "Downloaded", "InvalidSignature"]);
    rig.shutdown().await;
}

#[tokio::test]
async fn other_failure_outcomes() {
    let rig = booted().await;
    let signer = ca("signer");
    for (outcome, last, id) in [
        (FirmwareOutcome::DownloadFailure, "DownloadFailed", 10),
        (
            FirmwareOutcome::InstallationFailure,
            "InstallationFailed",
            11,
        ),
        (
            FirmwareOutcome::InstallVerificationFailed,
            "InstallVerificationFailed",
            12,
        ),
    ] {
        rig.send(Command::SetFirmwareOutcome { outcome }).await;
        let mark = rig.csms.mark();
        rig.csms
            .expect_status(
                "SignedUpdateFirmware",
                request(id, "https://host/fw", &signer.cert.pem()),
                "Accepted",
            )
            .await
            .unwrap();
        wait_status(&rig, mark, last, i64::from(id)).await;
    }
    rig.shutdown().await;
}

#[tokio::test]
async fn invalid_certificates_are_refused() {
    let rig = booted().await;
    let signer = ca("signer");
    let mark = rig.csms.mark();
    for pem in [
        "not a certificate".to_owned(),
        leaf(&signer, -10, -1),
        leaf(&signer, 1, 10),
    ] {
        rig.csms
            .expect_status(
                "SignedUpdateFirmware",
                request(1, "https://host/fw", &pem),
                "InvalidCertificate",
            )
            .await
            .unwrap();
    }
    wait_event(&rig, mark, "InvalidFirmwareSigningCertificate").await;
    rig.assert_no_calls("SignedFirmwareStatusNotification", mark);
    rig.shutdown().await;
}

#[tokio::test]
async fn an_installed_manufacturer_root_becomes_the_trust_anchor() {
    let rig = booted().await;
    let maker = ca("maker");
    let other = ca("other");
    rig.csms
        .expect_status(
            "InstallCertificate",
            json!({"certificateType": "ManufacturerRootCertificate", "certificate": maker.cert.pem()}),
            "Accepted",
        )
        .await
        .unwrap();
    rig.csms
        .expect_status(
            "SignedUpdateFirmware",
            request(1, "https://host/fw", &leaf(&other, -1, 10)),
            "InvalidCertificate",
        )
        .await
        .unwrap();
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "SignedUpdateFirmware",
            request(2, "https://host/fw", &leaf(&maker, -1, 10)),
            "Accepted",
        )
        .await
        .unwrap();
    wait_status(&rig, mark, "Installed", 2).await;
    rig.shutdown().await;
}

#[tokio::test]
async fn a_second_request_cancels_the_running_update() {
    let rig = booted().await;
    let signer = ca("signer");
    rig.send(Command::SetFirmwareOutcome {
        outcome: FirmwareOutcome::DownloadFailure,
    })
    .await;
    let mark = rig.csms.mark();
    let mut first = request(1, "https://host/fw", &signer.cert.pem());
    first["retries"] = json!(5);
    first["retryInterval"] = json!(1);
    rig.csms
        .expect_status("SignedUpdateFirmware", first, "Accepted")
        .await
        .unwrap();
    wait_status(&rig, mark, "Downloading", 1).await;
    rig.send(Command::SetFirmwareOutcome {
        outcome: FirmwareOutcome::Success,
    })
    .await;
    rig.csms
        .expect_status(
            "SignedUpdateFirmware",
            request(2, "https://host/fw", &signer.cert.pem()),
            "AcceptedCanceled",
        )
        .await
        .unwrap();
    wait_status(&rig, mark, "Installed", 2).await;
    let seen = statuses(&rig, mark);
    let first_new = seen.iter().position(|(_, id)| *id == Some(2)).unwrap();
    assert!(
        seen[first_new..].iter().all(|(_, id)| *id == Some(2)),
        "{seen:?}"
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn bad_locations_are_rejected() {
    let rig = booted().await;
    let signer = ca("signer");
    let mark = rig.csms.mark();
    for location in ["file:///fw", "not a url", "ws://host/fw"] {
        rig.csms
            .expect_status(
                "SignedUpdateFirmware",
                request(1, location, &signer.cert.pem()),
                "Rejected",
            )
            .await
            .unwrap();
    }
    rig.assert_no_calls("SignedFirmwareStatusNotification", mark);
    rig.shutdown().await;
}

#[tokio::test]
async fn schema_violations_are_call_errors() {
    let rig = booted().await;
    rig.csms
        .expect_call_error(
            "SignedUpdateFirmware",
            json!({"firmware": {"location": "https://h/f"}}),
            &ErrorCode::OccurenceConstraintViolation,
        )
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn rule_rejects_and_extended_trigger_reports_the_signed_status() {
    let rig = booted().await;
    let signer = ca("signer");
    rig.send(Command::SetResponseRule {
        rule: "SignedUpdateFirmware=reject*1"
            .parse::<ResponseRule>()
            .unwrap(),
    })
    .await;
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "SignedUpdateFirmware",
            request(1, "https://host/fw", &signer.cert.pem()),
            "Rejected",
        )
        .await
        .unwrap();
    rig.assert_no_calls("SignedFirmwareStatusNotification", mark);
    rig.csms
        .expect_status(
            "SignedUpdateFirmware",
            request(2, "https://host/fw", &signer.cert.pem()),
            "Accepted",
        )
        .await
        .unwrap();
    wait_status(&rig, mark, "Installed", 2).await;
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "ExtendedTriggerMessage",
            json!({"requestedMessage": "FirmwareStatusNotification"}),
            "Accepted",
        )
        .await
        .unwrap();
    let call = rig
        .csms
        .wait_for_call_after(mark, "SignedFirmwareStatusNotification", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(call["status"], "Installed");
    assert_eq!(call["requestId"], 2);
    rig.assert_no_calls("FirmwareStatusNotification", mark);
    rig.shutdown().await;
}
