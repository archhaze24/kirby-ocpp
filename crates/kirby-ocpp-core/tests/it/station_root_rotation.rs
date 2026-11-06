//! `AdditionalRootCertificateCheck` (Security Whitepaper `M05.FR.08` to `M05.FR.14`): the
//! station-level behaviour of `InstallCertificate` and the fallback root over TLS.

use std::sync::Arc;

use crate::station_support::{Observed, Rig, t0, test_config, wait_state};
use kirby_ocpp_core::config::TlsConfig;
use kirby_ocpp_core::ocpp::types::RegistrationStatus;
use kirby_ocpp_core::station::certificates::StoredRoot;
use kirby_ocpp_core::store::{LoadOutcome, PersistedStation, StationStore};
use kirby_ocpp_core::{ManualClock, Station};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms, Reply, TlsOpts};
use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair};
use serde_json::{Value, json};

const KIND: &str = "CentralSystemRootCertificate";

struct Ca {
    params: CertificateParams,
    key: KeyPair,
    pem: String,
}

fn params(name: &str) -> CertificateParams {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, name);
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(30);
    params
}

fn root(name: &str) -> Ca {
    let params = params(name);
    let key = KeyPair::generate().unwrap();
    let pem = params.clone().self_signed(&key).unwrap().pem();
    Ca { params, key, pem }
}

/// A root whose certificate was issued by `issuer`.
fn cross_signed(name: &str, issuer: &Ca) -> String {
    let issuer = rcgen::Issuer::new(issuer.params.clone(), &issuer.key);
    let key = KeyPair::generate().unwrap();
    params(name).signed_by(&key, &issuer).unwrap().pem()
}

async fn install(rig: &Rig, pem: &str) -> Value {
    rig.csms_call(
        "InstallCertificate",
        json!({"certificateType": KIND, "certificate": pem}),
    )
    .await["status"]
        .clone()
}

fn rig_with_check(check: bool) -> impl std::future::Future<Output = Rig> {
    Rig::with(CsmsOptions::default(), move |mut config| {
        config.additional_root_certificate_check = check;
        config
    })
}

#[tokio::test]
async fn the_check_decides_which_roots_can_be_installed() {
    let mut rig = rig_with_check(true).await;
    rig.csms
        .respond("SecurityEventNotification", |_| Reply::Result(json!({})));
    rig.connect_booted().await;
    let a = root("Root A");
    let c = root("Root C");
    let b = cross_signed("Root B", &a);

    let config = rig
        .csms_call(
            "GetConfiguration",
            json!({"key": ["AdditionalRootCertificateCheck"]}),
        )
        .await;
    assert_eq!(config["configurationKey"][0]["value"], "true");
    // The key is read-only for the CSMS.
    let changed = rig
        .csms
        .send_call(
            "ChangeConfiguration",
            json!({"key": "AdditionalRootCertificateCheck", "value": "false"}),
        )
        .await
        .unwrap();
    assert!(changed.to_json_string().contains("Rejected"));

    assert_eq!(install(&rig, &a.pem).await, "Accepted");
    assert_eq!(install(&rig, &c.pem).await, "Rejected", "unrelated root");
    assert_eq!(
        install(&rig, &b).await,
        "Accepted",
        "signed by the old root"
    );
    assert_eq!(install(&rig, &b).await, "Accepted", "already present");
    assert_eq!(rig.station.snapshot().certificates.len(), 2);
    let ids = rig
        .csms_call(
            "GetInstalledCertificateIds",
            json!({"certificateType": KIND}),
        )
        .await;
    assert_eq!(ids["certificateHashData"].as_array().unwrap().len(), 2);

    // The fallback is part of the persisted file.
    rig.station.shutdown().await.unwrap();
    let store = StationStore::new(rig.dir.path(), &rig.station.config().charge_point_id);
    let saved: Value = serde_json::from_slice(&std::fs::read(store.path()).unwrap()).unwrap();
    let roots = saved["certificates"]["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 2);
    assert_eq!(roots.iter().filter(|r| r["fallback"] == true).count(), 1);
    assert_eq!(saved["version"], 9);
    rig.join.take().unwrap().await.unwrap();
    rig.csms.shutdown().await;
}

#[tokio::test]
async fn without_the_check_only_self_signed_roots_are_accepted() {
    let mut rig = rig_with_check(false).await;
    rig.csms
        .respond("SecurityEventNotification", |_| Reply::Result(json!({})));
    rig.connect_booted().await;
    let a = root("Root A");
    let c = root("Root C");
    let b = cross_signed("Root B", &a);
    assert_eq!(install(&rig, &a.pem).await, "Accepted");
    assert_eq!(install(&rig, &b).await, "Rejected", "not self-signed");
    assert_eq!(install(&rig, &c.pem).await, "Accepted");
    assert_eq!(rig.station.snapshot().certificates.len(), 2);
    rig.shutdown().await;
}

/// A TLS mock CSMS and a station whose state file already holds `roots(ca_pem)`.
async fn tls_station(
    roots: impl FnOnce(&str) -> Vec<StoredRoot>,
) -> (
    MockCsms,
    kirby_ocpp_core::station::StationHandle,
    Observed,
    tempfile::TempDir,
) {
    tls_station_with(roots, false).await
}

/// As [`tls_station`]; with `ca_file` the station also trusts the mock CA via `--tls-ca`.
async fn tls_station_with(
    roots: impl FnOnce(&str) -> Vec<StoredRoot>,
    ca_file: bool,
) -> (
    MockCsms,
    kirby_ocpp_core::station::StationHandle,
    Observed,
    tempfile::TempDir,
) {
    let csms = MockCsms::start(CsmsOptions {
        tls: Some(TlsOpts::default()),
        ..CsmsOptions::default()
    })
    .await
    .unwrap();
    csms.respond("SecurityEventNotification", |_| Reply::Result(json!({})));
    let ca_pem = csms.tls_material().unwrap().ca_cert_pem().to_owned();
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config(csms.url(), &dir);
    config.tls = TlsConfig {
        server_name: Some("localhost".to_owned()),
        ca_file: ca_file.then(|| {
            let path = dir.path().join("ca.pem");
            std::fs::write(&path, &ca_pem).unwrap();
            path
        }),
        ..TlsConfig::default()
    };
    let mut state = PersistedStation::new(config.charge_point_id.clone());
    state.certificates.roots = roots(&ca_pem);
    StationStore::new(dir.path(), &config.charge_point_id)
        .save(&state)
        .unwrap();
    let (station, _join) = Station::spawn(config, Arc::new(ManualClock::new(t0())));
    let observed = Observed::new(station.subscribe());
    (csms, station, observed, dir)
}

fn stored(pem: &str, fallback: bool) -> StoredRoot {
    StoredRoot {
        kind: kirby_ocpp_core::ocpp::messages::CertificateUse::CentralSystemRootCertificate,
        pem: pem.trim().to_owned(),
        fallback,
    }
}

#[tokio::test]
async fn the_fallback_goes_after_a_connection_with_the_new_root() {
    let old = root("Old root");
    let (csms, station, mut observed, dir) =
        tls_station(|ca| vec![stored(&old.pem, true), stored(ca, false)]).await;
    assert_eq!(station.snapshot().certificates.len(), 2);
    station.connect().await.unwrap();
    wait_state(&station, "accepted", |s| {
        s.registration == Some(RegistrationStatus::Accepted)
    })
    .await;
    observed.wait_log("fallback root was removed").await;
    assert_eq!(station.snapshot().certificates.len(), 1);
    // And it was written down.
    station.shutdown().await.unwrap();
    let store = StationStore::new(dir.path(), &station.config().charge_point_id);
    let LoadOutcome::Loaded(state) = store.load().unwrap() else {
        panic!("state file not loaded")
    };
    assert_eq!(state.certificates.roots.len(), 1);
    assert!(!state.certificates.has_fallback());
    csms.shutdown().await;
}

#[tokio::test]
async fn a_server_the_new_root_does_not_vouch_for_is_tried_with_the_fallback() {
    let unrelated = root("New root that does not match");
    let (csms, station, mut observed, _dir) =
        tls_station(|ca| vec![stored(ca, true), stored(&unrelated.pem, false)]).await;
    station.connect().await.unwrap();
    wait_state(&station, "accepted via fallback", |s| {
        s.registration == Some(RegistrationStatus::Accepted)
    })
    .await;
    observed.drain();
    assert!(
        observed
            .logs
            .iter()
            .any(|l| l.contains("connection failed") && l.contains("invalid peer certificate")),
        "{:#?}",
        observed.logs
    );
    // Connecting through the fallback proves nothing about the new root: both stay.
    assert_eq!(station.snapshot().certificates.len(), 2);
    station.shutdown().await.unwrap();
    csms.shutdown().await;
}

#[tokio::test]
async fn the_fallback_stays_when_no_installed_root_was_in_the_trust_set() {
    let old = root("Old root");
    // Only the fallback is installed; the connection is verified through `ca_file` alone.
    let (csms, station, mut observed, _dir) =
        tls_station_with(|_| vec![stored(&old.pem, true)], true).await;
    station.connect().await.unwrap();
    wait_state(&station, "accepted", |s| {
        s.registration == Some(RegistrationStatus::Accepted)
    })
    .await;
    observed.drain();
    assert!(
        !observed
            .logs
            .iter()
            .any(|l| l.contains("fallback root was removed")),
        "{:#?}",
        observed.logs
    );
    assert_eq!(station.snapshot().certificates.len(), 1);
    station.shutdown().await.unwrap();
    csms.shutdown().await;
}
