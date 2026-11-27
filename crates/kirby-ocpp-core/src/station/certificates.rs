//! Certificate store of the Security Whitepaper (edition 3): pure logic, no I/O.
//!
//! * Holds the installed `CentralSystemRootCertificate` / `ManufacturerRootCertificate`
//!   entries (PEM, at most `CertificateStoreMaxLength` in total) and the station's own
//!   `ChargePointCertificate` chain (public PEM only; the private key lives in the secrets
//!   file, see `store.rs`).
//! * `CertificateHashData` follows the OCSP `CertID` rules: the hash of the DER encoding of the
//!   issuer name, the hash of the issuer's public key (the contents of the `subjectPublicKey`
//!   BIT STRING) and the serial number (lowercase hex, no leading zeros).
//! * Normally only self-signed (root) certificates can be installed: for others the issuer key
//!   hash would need the issuer certificate. Signatures are not verified, with one exception:
//!   with `AdditionalRootCertificateCheck` a new `CentralSystemRootCertificate` must be signed
//!   by the installed one (it may be cross-signed, not self-signed) and the old root stays as a
//!   fallback until the station connected with the new one (`M05.FR.08` to `M05.FR.13`). The
//!   hash data of such a root names the certificate itself as its issuer.

#[cfg(not(target_arch = "wasm32"))]
mod native;

#[cfg(not(target_arch = "wasm32"))]
pub use native::{generate_csr, parse_pem_chain, validate_signed_chain};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ocpp::messages::{CertificateHashData, CertificateUse};
#[cfg(target_arch = "wasm32")]
use crate::ocpp::messages::{DeleteCertificateStatus, InstallCertificateStatus};

/// Default `CertificateStoreMaxLength`.
pub const DEFAULT_STORE_MAX_LENGTH: u32 = 8;
/// Default `CertificateSignedMaxChainSize` (bytes of `certificateChain`).
pub const DEFAULT_MAX_CHAIN_SIZE: u32 = 10_000;

/// One installed root certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredRoot {
    pub kind: CertificateUse,
    pub pem: String,
    /// The superseded Central System root kept until the station connected with its successor
    /// (`AdditionalRootCertificateCheck`; added in format version 5).
    #[serde(default)]
    pub fallback: bool,
}

/// Everything about certificates that is safe to keep in the main state file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CertificateStore {
    pub roots: Vec<StoredRoot>,
    /// The station's certificate chain, leaf first (public data only).
    pub charge_point_chain: Option<String>,
    /// The chain that `charge_point_chain` replaced, kept (with its key in the secrets file)
    /// until the new one has connected; see the old/new alternation (`A02.FR.28`). Added in
    /// store version 7.
    pub previous_charge_point_chain: Option<String>,
}

/// A certificate as shown to UIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateSummary {
    /// `CentralSystemRootCertificate`, `ManufacturerRootCertificate` or `ChargePointCertificate`.
    pub kind: String,
    pub subject: String,
    pub serial_number: String,
    pub not_after: DateTime<Utc>,
    /// SHA-256 hash data.
    pub hash: CertificateHashData,
}

/// A parsed certificate (owned copies of what we need).
#[derive(Debug, Clone)]
#[cfg(not(target_arch = "wasm32"))]
pub struct ParsedCert {
    pub der: Vec<u8>,
    pub subject: String,
    pub serial: String,
    pub not_before: i64,
    pub not_after: i64,
    subject_raw: Vec<u8>,
    issuer_raw: Vec<u8>,
    key_bits: Vec<u8>,
    spki_der: Vec<u8>,
    self_signed: bool,
}

impl CertificateStore {
    /// Number of installed root certificates.
    pub fn root_count(&self) -> usize {
        self.roots.len()
    }

    /// Whether a superseded Central System root is kept as a fallback.
    pub fn has_fallback(&self) -> bool {
        self.roots.iter().any(|r| r.fallback)
    }

    /// Forgets the fallback root (`M05.FR.12`). `true` when there was one.
    pub fn drop_fallback(&mut self) -> bool {
        let before = self.roots.len();
        self.roots.retain(|r| !r.fallback);
        self.roots.len() != before
    }

    /// Trust anchors as PEM text, for the TLS layer (without the fallback root).
    pub fn root_pems(&self, kind: CertificateUse) -> Vec<String> {
        self.roots
            .iter()
            .filter(|r| r.kind == kind && !r.fallback)
            .map(|r| r.pem.clone())
            .collect()
    }

    /// The fallback Central System root as PEM text (`M05.FR.13`).
    pub fn fallback_pems(&self) -> Vec<String> {
        self.roots
            .iter()
            .filter(|r| r.fallback)
            .map(|r| r.pem.clone())
            .collect()
    }
}

/// Why a `CertificateSigned` chain was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainRejected(pub String);

#[cfg(target_arch = "wasm32")]
impl CertificateStore {
    /// Certificate installation is unsupported on this target.
    pub fn install(
        &mut self,
        _kind: CertificateUse,
        _pem: &str,
        _now: DateTime<Utc>,
        _max_len: usize,
        _additional_check: bool,
    ) -> InstallCertificateStatus {
        InstallCertificateStatus::Failed
    }

    /// Certificate deletion by hash is unsupported on this target.
    pub fn delete(
        &mut self,
        _wanted: &CertificateHashData,
        _keep_last_central_root: bool,
    ) -> DeleteCertificateStatus {
        DeleteCertificateStatus::Failed
    }

    /// No parsed certificate identifiers are available on this target.
    pub fn ids(&self, _kind: CertificateUse) -> Vec<CertificateHashData> {
        Vec::new()
    }

    /// No parsed certificate summaries are available on this target.
    pub fn summaries(&self) -> Vec<CertificateSummary> {
        Vec::new()
    }
}

#[cfg(target_arch = "wasm32")]
pub fn generate_csr(_common_name: &str, _organization: &str) -> Result<(String, String), String> {
    Err(crate::UNSUPPORTED_IN_BROWSER.to_owned())
}

#[cfg(target_arch = "wasm32")]
pub fn validate_signed_chain(
    _chain_pem: &str,
    _key_pem: &str,
    _now: DateTime<Utc>,
    _max_size: usize,
) -> Result<(), ChainRejected> {
    Err(ChainRejected(crate::UNSUPPORTED_IN_BROWSER.to_owned()))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair};
    use ring::digest;
    use x509_parser::prelude::{FromDer, X509Certificate};

    use super::native::{hex, serial_hex};
    use super::*;
    use crate::ocpp::messages::{DeleteCertificateStatus, HashAlgorithm, InstallCertificateStatus};

    fn root(name: &str, days: i64) -> (String, KeyPair, rcgen::Certificate) {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, name);
        params.distinguished_name = dn;
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::days(1);
        params.not_after = now + time::Duration::days(days);
        let cert = params.self_signed(&key).unwrap();
        (cert.pem(), key, cert)
    }

    #[test]
    fn install_list_delete_roundtrip_with_independent_hashes() {
        let (pem, _, cert) = root("Test Root", 30);
        let now = Utc::now();
        let mut store = CertificateStore::default();
        assert_eq!(
            store.install(
                CertificateUse::CentralSystemRootCertificate,
                &pem,
                now,
                2,
                false
            ),
            InstallCertificateStatus::Accepted
        );
        // Re-install is idempotent.
        assert_eq!(
            store.install(
                CertificateUse::CentralSystemRootCertificate,
                &pem,
                now,
                2,
                false
            ),
            InstallCertificateStatus::Accepted
        );
        assert_eq!(store.root_count(), 1);
        let ids = store.ids(CertificateUse::CentralSystemRootCertificate);
        assert_eq!(ids.len(), 1);
        assert_eq!(
            store.ids(CertificateUse::ManufacturerRootCertificate).len(),
            0
        );

        // Independent computation with x509-parser + sha256 over the raw parts.
        let der = cert.der().to_vec();
        let (_, x509) = X509Certificate::from_der(&der).unwrap();
        let name_hash = digest::digest(&digest::SHA256, x509.issuer().as_raw());
        let key_hash = digest::digest(&digest::SHA256, &x509.public_key().subject_public_key.data);
        assert_eq!(ids[0].issuer_name_hash, hex(name_hash.as_ref()));
        assert_eq!(ids[0].issuer_key_hash, hex(key_hash.as_ref()));
        assert_eq!(ids[0].serial_number, serial_hex(x509.raw_serial()));

        // Last central root is protected on request, otherwise deleted; then NotFound.
        let mut upper = ids[0].clone();
        upper.issuer_name_hash = upper.issuer_name_hash.to_uppercase();
        assert_eq!(store.delete(&upper, true), DeleteCertificateStatus::Failed);
        assert_eq!(
            store.delete(&upper, false),
            DeleteCertificateStatus::Accepted
        );
        assert_eq!(
            store.delete(&ids[0], false),
            DeleteCertificateStatus::NotFound
        );
    }

    #[test]
    fn sha512_hash_data_can_delete() {
        let (pem, _, _) = root("R", 30);
        let now = Utc::now();
        let mut store = CertificateStore::default();
        store.install(
            CertificateUse::ManufacturerRootCertificate,
            &pem,
            now,
            2,
            false,
        );
        let cert = parse_pem_chain(&pem).unwrap().remove(0);
        let wanted = cert.hash_data(HashAlgorithm::Sha512);
        assert_eq!(wanted.issuer_key_hash.len(), 128);
        assert_eq!(
            store.delete(&wanted, true),
            DeleteCertificateStatus::Accepted
        );
    }

    #[test]
    fn full_store_expired_and_garbage_are_rejected() {
        let now = Utc::now();
        let mut store = CertificateStore::default();
        let (a, _, _) = root("A", 30);
        let (b, _, _) = root("B", 30);
        let (expired, _, _) = root("E", -1);
        let kind = CertificateUse::CentralSystemRootCertificate;
        assert_eq!(
            store.install(kind, &a, now, 1, false),
            InstallCertificateStatus::Accepted
        );
        assert_eq!(
            store.install(kind, &b, now, 1, false),
            InstallCertificateStatus::Rejected
        );
        assert_eq!(
            store.install(kind, &expired, now, 5, false),
            InstallCertificateStatus::Rejected
        );
        assert_eq!(
            store.install(kind, "not a pem", now, 5, false),
            InstallCertificateStatus::Rejected
        );
        let two = format!("{a}\n{b}");
        assert_eq!(
            store.install(kind, &two, now, 5, false),
            InstallCertificateStatus::Rejected
        );
        let huge = "A".repeat(6000);
        assert_eq!(
            store.install(kind, &huge, now, 5, false),
            InstallCertificateStatus::Rejected
        );
    }

    #[test]
    fn signed_chain_must_match_the_pending_key() {
        let (csr_pem, key_pem) = generate_csr("CP-1", "Org").unwrap();
        let csr = rcgen::CertificateSigningRequestParams::from_pem(&csr_pem).unwrap();
        let (_, ca_key, ca_cert) = root("CA", 30);
        let issuer = rcgen::Issuer::new(
            {
                let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
                let mut dn = DistinguishedName::new();
                dn.push(DnType::CommonName, "CA");
                p.distinguished_name = dn;
                p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
                p
            },
            ca_key,
        );
        let leaf = csr.signed_by(&issuer).unwrap();
        let chain = format!("{}\n{}", leaf.pem(), ca_cert.pem());
        let now = Utc::now();
        assert_eq!(validate_signed_chain(&chain, &key_pem, now, 10_000), Ok(()));
        let (_, other_key) = generate_csr("CP-1", "Org").unwrap();
        assert!(validate_signed_chain(&chain, &other_key, now, 10_000).is_err());
        assert!(validate_signed_chain(&chain, &key_pem, now, 10).is_err());
        assert!(validate_signed_chain("junk", &key_pem, now, 10_000).is_err());
        // The next certificate did not sign the leaf.
        let (_, _, other_ca) = root("Other CA", 30);
        let broken = format!("{}\n{}", leaf.pem(), other_ca.pem());
        assert!(validate_signed_chain(&broken, &key_pem, now, 10_000).is_err());
        // A single-certificate chain has no link to check.
        assert_eq!(
            validate_signed_chain(leaf.pem().as_str(), &key_pem, now, 10_000),
            Ok(())
        );
    }

    #[test]
    fn a_forged_self_signed_looking_root_is_rejected() {
        let now = Utc::now();
        let (real_params, real_key, _) = ca("Root A");
        // Same subject and issuer name, but signed by another key.
        let (_, _, genuine) = ca("Root A");
        let forged = cross_signed("Root A", (&real_params, &real_key));
        let mut store = CertificateStore::default();
        assert_eq!(
            store.install(CS, &forged, now, 8, false),
            InstallCertificateStatus::Rejected
        );
        assert_eq!(
            store.install(CS, &genuine.pem(), now, 8, false),
            InstallCertificateStatus::Accepted
        );
    }

    /// A CA (self-signed) and the pieces to sign others with.
    fn ca(name: &str) -> (CertificateParams, KeyPair, rcgen::Certificate) {
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
        (params, key, cert)
    }

    /// A root `name` whose certificate is issued by `issuer` (cross-signed, not self-signed).
    fn cross_signed(name: &str, issuer: (&CertificateParams, &KeyPair)) -> String {
        let (params, key, _) = ca(name);
        let issuer = rcgen::Issuer::new(issuer.0.clone(), issuer.1);
        params.signed_by(&key, &issuer).unwrap().pem()
    }

    const CS: CertificateUse = CertificateUse::CentralSystemRootCertificate;

    #[test]
    fn additional_check_needs_the_new_root_signed_by_the_old_one() {
        let now = Utc::now();
        let (a_params, a_key, a) = ca("Root A");
        let (_, _, c) = ca("Root C");
        let b_pem = cross_signed("Root B", (&a_params, &a_key));
        let mut store = CertificateStore::default();
        assert_eq!(
            store.install(CS, &a.pem(), now, 8, true),
            InstallCertificateStatus::Accepted
        );
        // Unrelated self-signed root and a root signed by nobody installed: refused, unchanged.
        assert_eq!(
            store.install(CS, &c.pem(), now, 8, true),
            InstallCertificateStatus::Rejected
        );
        assert_eq!(store.root_count(), 1);
        // Signed by A: accepted although it is not self-signed; A becomes the fallback.
        assert_eq!(
            store.install(CS, &b_pem, now, 8, true),
            InstallCertificateStatus::Accepted
        );
        assert_eq!(store.root_count(), 2);
        assert!(store.has_fallback());
        assert_eq!(store.root_pems(CS), vec![b_pem.trim().to_owned()]);
        assert_eq!(store.fallback_pems(), vec![a.pem().trim().to_owned()]);
        // Both are still reported as installed.
        assert_eq!(store.ids(CS).len(), 2);
        assert_eq!(store.summaries().len(), 2);
        // Installing either again changes nothing (M05.FR.14).
        assert_eq!(
            store.install(CS, &b_pem, now, 8, true),
            InstallCertificateStatus::Accepted
        );
        assert_eq!(
            store.install(CS, &a.pem(), now, 8, true),
            InstallCertificateStatus::Accepted
        );
        assert_eq!(store.root_count(), 2);
        // The fallback goes after a good connection.
        assert!(store.drop_fallback());
        assert!(!store.drop_fallback());
        assert_eq!(store.ids(CS).len(), 1);
    }

    #[test]
    fn without_the_check_only_self_signed_roots_are_installed() {
        let now = Utc::now();
        let (a_params, a_key, a) = ca("Root A");
        let (_, _, c) = ca("Root C");
        let b_pem = cross_signed("Root B", (&a_params, &a_key));
        let mut store = CertificateStore::default();
        assert_eq!(
            store.install(CS, &a.pem(), now, 8, false),
            InstallCertificateStatus::Accepted
        );
        assert_eq!(
            store.install(CS, &b_pem, now, 8, false),
            InstallCertificateStatus::Rejected
        );
        // Several roots side by side, no fallback.
        assert_eq!(
            store.install(CS, &c.pem(), now, 8, false),
            InstallCertificateStatus::Accepted
        );
        assert_eq!(store.root_count(), 2);
        assert!(!store.has_fallback());
        assert_eq!(store.root_pems(CS).len(), 2);
        // With the check on and no usable single root, a cross-signed first root is refused.
        let mut empty = CertificateStore::default();
        assert_eq!(
            empty.install(CS, &b_pem, now, 8, true),
            InstallCertificateStatus::Rejected
        );
    }

    #[test]
    fn rotating_again_drops_the_older_fallback_and_respects_the_store_size() {
        let now = Utc::now();
        let (a_params, a_key, a) = ca("Root A");
        let (b_params, b_key, _) = ca("Root B");
        let b_pem = {
            let issuer = rcgen::Issuer::new(a_params.clone(), &a_key);
            b_params.signed_by(&b_key, &issuer).unwrap().pem()
        };
        let d_pem = cross_signed("Root D", (&b_params, &b_key));
        let mut store = CertificateStore::default();
        store.install(CS, &a.pem(), now, 8, true);
        // Two entries do not fit into a store of one.
        assert_eq!(
            store.install(CS, &b_pem, now, 1, true),
            InstallCertificateStatus::Rejected
        );
        assert_eq!(store.root_count(), 1);
        assert!(!store.has_fallback());
        assert_eq!(
            store.install(CS, &b_pem, now, 2, true),
            InstallCertificateStatus::Accepted
        );
        // D is signed by B's key, but B's certificate is B; D replaces B, A is dropped.
        assert_eq!(
            store.install(CS, &d_pem, now, 2, true),
            InstallCertificateStatus::Accepted
        );
        assert_eq!(store.root_count(), 2);
        assert_eq!(store.root_pems(CS), vec![d_pem.trim().to_owned()]);
        assert_eq!(store.fallback_pems(), vec![b_pem.trim().to_owned()]);
    }

    #[test]
    fn deleting_the_successor_promotes_the_fallback() {
        let now = Utc::now();
        let (a_params, a_key, a) = ca("Root A");
        let b_pem = cross_signed("Root B", (&a_params, &a_key));
        let mut store = CertificateStore::default();
        store.install(CS, &a.pem(), now, 8, true);
        store.install(CS, &b_pem, now, 8, true);
        let b_hash = parse_pem_chain(&b_pem).unwrap()[0].root_hash_data(HashAlgorithm::Sha256);
        assert_eq!(
            store.delete(&b_hash, true),
            DeleteCertificateStatus::Accepted
        );
        assert!(!store.has_fallback());
        assert_eq!(store.root_pems(CS).len(), 1);
    }

    #[test]
    fn stores_without_the_fallback_flag_still_load() {
        let json = r#"{"roots":[{"kind":"CentralSystemRootCertificate","pem":"x"}]}"#;
        let store: CertificateStore = serde_json::from_str(json).unwrap();
        assert!(!store.has_fallback());
    }
}
