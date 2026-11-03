//! Certificate store of the Security Whitepaper (edition 3): pure logic, no I/O.
//!
//! * Holds the installed `CentralSystemRootCertificate` / `ManufacturerRootCertificate`
//!   entries (PEM, at most `CertificateStoreMaxLength` in total) and the station's own
//!   `ChargePointCertificate` chain (public PEM only; the private key lives in the secrets
//!   file, see `store.rs`).
//! * `CertificateHashData` follows the OCSP `CertID` rules: the hash of the DER encoding of the
//!   issuer name, the hash of the issuer's public key (the contents of the `subjectPublicKey`
//!   BIT STRING) and the serial number (lowercase hex, no leading zeros).
//! * Only self-signed (root) certificates can be installed: for others the issuer key hash
//!   would need the issuer certificate. Signatures are not verified.

use chrono::{DateTime, Utc};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PublicKeyData};
use ring::digest;
use serde::{Deserialize, Serialize};
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::ocpp::messages::{
    CertificateHashData, CertificateUse, DeleteCertificateStatus, HashAlgorithm,
    InstallCertificateStatus,
};

/// Default `CertificateStoreMaxLength`.
pub const DEFAULT_STORE_MAX_LENGTH: u32 = 8;
/// Default `CertificateSignedMaxChainSize` (bytes of `certificateChain`).
pub const DEFAULT_MAX_CHAIN_SIZE: u32 = 10_000;
/// Longest certificate chain that is looked at.
const MAX_CHAIN_CERTS: usize = 8;

/// One installed root certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredRoot {
    pub kind: CertificateUse,
    pub pem: String,
}

/// Everything about certificates that is safe to keep in the main state file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CertificateStore {
    pub roots: Vec<StoredRoot>,
    /// The station's certificate chain, leaf first (public data only).
    pub charge_point_chain: Option<String>,
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
pub struct ParsedCert {
    pub der: Vec<u8>,
    pub subject: String,
    pub serial: String,
    pub not_before: i64,
    pub not_after: i64,
    issuer_raw: Vec<u8>,
    key_bits: Vec<u8>,
    spki_der: Vec<u8>,
    self_signed: bool,
}

impl ParsedCert {
    /// Parses one DER certificate.
    pub fn from_der(der: &[u8]) -> Result<Self, String> {
        let (rest, cert) = X509Certificate::from_der(der).map_err(|e| e.to_string())?;
        if !rest.is_empty() {
            return Err("trailing data after the certificate".to_owned());
        }
        let validity = cert.validity();
        Ok(Self {
            der: der.to_vec(),
            subject: cert.subject().to_string(),
            serial: serial_hex(cert.tbs_certificate.raw_serial()),
            not_before: validity.not_before.timestamp(),
            not_after: validity.not_after.timestamp(),
            issuer_raw: cert.issuer().as_raw().to_vec(),
            key_bits: cert.public_key().subject_public_key.data.to_vec(),
            spki_der: cert.public_key().raw.to_vec(),
            self_signed: cert.subject().as_raw() == cert.issuer().as_raw(),
        })
    }

    /// Not expired and already valid at `now`.
    pub fn valid_at(&self, now: DateTime<Utc>) -> bool {
        let t = now.timestamp();
        self.not_before <= t && t <= self.not_after
    }

    pub fn not_after_utc(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.not_after, 0).unwrap_or(DateTime::UNIX_EPOCH)
    }

    /// `CertificateHashData` of a self-signed certificate (its own key is the issuer key).
    pub fn hash_data(&self, algorithm: HashAlgorithm) -> CertificateHashData {
        let alg = match algorithm {
            HashAlgorithm::Sha256 => &digest::SHA256,
            HashAlgorithm::Sha384 => &digest::SHA384,
            HashAlgorithm::Sha512 => &digest::SHA512,
        };
        CertificateHashData {
            hash_algorithm: algorithm,
            issuer_name_hash: hex(digest::digest(alg, &self.issuer_raw).as_ref()),
            issuer_key_hash: hex(digest::digest(alg, &self.key_bits).as_ref()),
            serial_number: self.serial.clone(),
        }
    }

    pub fn summary(&self, kind: &str) -> CertificateSummary {
        CertificateSummary {
            kind: kind.to_owned(),
            subject: self.subject.clone(),
            serial_number: self.serial.clone(),
            not_after: self.not_after_utc(),
            hash: self.hash_data(HashAlgorithm::Sha256),
        }
    }
}

/// Parses the certificates of a PEM text (all `CERTIFICATE` blocks).
pub fn parse_pem_chain(pem: &str) -> Result<Vec<ParsedCert>, String> {
    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::pem::PemObject;
    let mut out = Vec::new();
    for item in CertificateDer::pem_slice_iter(pem.as_bytes()) {
        let der = item.map_err(|e| format!("invalid PEM: {e}"))?;
        if out.len() >= MAX_CHAIN_CERTS {
            return Err(format!("more than {MAX_CHAIN_CERTS} certificates"));
        }
        out.push(ParsedCert::from_der(der.as_ref())?);
    }
    if out.is_empty() {
        return Err("no PEM certificate found".to_owned());
    }
    Ok(out)
}

fn serial_hex(raw: &[u8]) -> String {
    let trimmed: &[u8] = {
        let start = raw.iter().position(|b| *b != 0).unwrap_or(raw.len());
        raw.get(start..).unwrap_or_default()
    };
    if trimmed.is_empty() {
        "0".to_owned()
    } else {
        let text = hex(trimmed);
        text.trim_start_matches('0').to_owned()
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn same_hash(a: &CertificateHashData, b: &CertificateHashData) -> bool {
    a.hash_algorithm == b.hash_algorithm
        && a.issuer_name_hash.eq_ignore_ascii_case(&b.issuer_name_hash)
        && a.issuer_key_hash.eq_ignore_ascii_case(&b.issuer_key_hash)
        && a.serial_number
            .trim_start_matches('0')
            .eq_ignore_ascii_case(b.serial_number.trim_start_matches('0'))
}

impl CertificateStore {
    fn parsed_roots(&self) -> Vec<(CertificateUse, ParsedCert)> {
        self.roots
            .iter()
            .filter_map(|r| {
                parse_pem_chain(&r.pem)
                    .ok()
                    .and_then(|mut c| (c.len() == 1).then(|| c.remove(0)))
                    .map(|c| (r.kind, c))
            })
            .collect()
    }

    /// Number of installed root certificates.
    pub fn root_count(&self) -> usize {
        self.roots.len()
    }

    /// `InstallCertificate`. Re-installing a certificate that is present is `Accepted` and does
    /// not use more space; a full store answers `Rejected` (as OCPP 2.0.1 does).
    pub fn install(
        &mut self,
        kind: CertificateUse,
        pem: &str,
        now: DateTime<Utc>,
        max_len: usize,
    ) -> InstallCertificateStatus {
        if pem.len() > crate::ocpp::messages::MAX_CERTIFICATE_LEN {
            return InstallCertificateStatus::Rejected;
        }
        let Ok(mut parsed) = parse_pem_chain(pem) else {
            return InstallCertificateStatus::Rejected;
        };
        if parsed.len() != 1 {
            return InstallCertificateStatus::Rejected;
        }
        let cert = parsed.remove(0);
        if !cert.valid_at(now) || !cert.self_signed {
            return InstallCertificateStatus::Rejected;
        }
        let new_hash = cert.hash_data(HashAlgorithm::Sha256);
        let existing = self.parsed_roots().into_iter().position(|(k, c)| {
            k == kind && same_hash(&c.hash_data(HashAlgorithm::Sha256), &new_hash)
        });
        if existing.is_some() {
            return InstallCertificateStatus::Accepted;
        }
        if self.roots.len() >= max_len {
            return InstallCertificateStatus::Rejected;
        }
        self.roots.push(StoredRoot {
            kind,
            pem: pem.trim().to_owned(),
        });
        InstallCertificateStatus::Accepted
    }

    /// `DeleteCertificate`. With `keep_last_central_root` the last Central System root cannot go
    /// (answer `Failed`).
    pub fn delete(
        &mut self,
        wanted: &CertificateHashData,
        keep_last_central_root: bool,
    ) -> DeleteCertificateStatus {
        let found = self.roots.iter().enumerate().find_map(|(index, r)| {
            let cert = parse_pem_chain(&r.pem).ok()?.into_iter().next()?;
            same_hash(&cert.hash_data(wanted.hash_algorithm), wanted).then_some((index, r.kind))
        });
        let Some((index, kind)) = found else {
            return DeleteCertificateStatus::NotFound;
        };
        if keep_last_central_root
            && kind == CertificateUse::CentralSystemRootCertificate
            && self
                .roots
                .iter()
                .filter(|r| r.kind == CertificateUse::CentralSystemRootCertificate)
                .count()
                <= 1
        {
            return DeleteCertificateStatus::Failed;
        }
        self.roots.remove(index);
        DeleteCertificateStatus::Accepted
    }

    /// `GetInstalledCertificateIds` (SHA-256 hash data).
    pub fn ids(&self, kind: CertificateUse) -> Vec<CertificateHashData> {
        self.parsed_roots()
            .into_iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, c)| c.hash_data(HashAlgorithm::Sha256))
            .collect()
    }

    /// Summaries of every certificate (roots first, then the station's own leaf).
    pub fn summaries(&self) -> Vec<CertificateSummary> {
        let mut out: Vec<CertificateSummary> = self
            .parsed_roots()
            .into_iter()
            .map(|(k, c)| c.summary(k.as_str()))
            .collect();
        if let Some(leaf) = self
            .charge_point_chain
            .as_deref()
            .and_then(|pem| parse_pem_chain(pem).ok())
            .and_then(|c| c.into_iter().next())
        {
            out.push(leaf.summary("ChargePointCertificate"));
        }
        out
    }

    /// Trust anchors as PEM text, for the TLS layer.
    pub fn root_pems(&self, kind: CertificateUse) -> Vec<String> {
        self.roots
            .iter()
            .filter(|r| r.kind == kind)
            .map(|r| r.pem.clone())
            .collect()
    }
}

/// A fresh key pair and a PKCS#10 request for `common_name` / `organization`: `(csr_pem, key_pem)`.
pub fn generate_csr(common_name: &str, organization: &str) -> Result<(String, String), String> {
    let key = KeyPair::generate().map_err(|e| e.to_string())?;
    let mut params = CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, common_name);
    dn.push(DnType::OrganizationName, organization);
    params.distinguished_name = dn;
    let csr = params.serialize_request(&key).map_err(|e| e.to_string())?;
    Ok((csr.pem().map_err(|e| e.to_string())?, key.serialize_pem()))
}

/// Why a `CertificateSigned` chain was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainRejected(pub String);

/// Checks a `CertificateSigned` chain against the pending private key: it must parse, be within
/// `max_size`, have a valid leaf, and the leaf's public key must be the pending key's.
pub fn validate_signed_chain(
    chain_pem: &str,
    key_pem: &str,
    now: DateTime<Utc>,
    max_size: usize,
) -> Result<(), ChainRejected> {
    let reject = |why: &str| Err(ChainRejected(why.to_owned()));
    if chain_pem.len() > max_size {
        return reject("chain is larger than CertificateSignedMaxChainSize");
    }
    let chain = parse_pem_chain(chain_pem).map_err(ChainRejected)?;
    let Some(leaf) = chain.first() else {
        return reject("empty chain");
    };
    if !chain.iter().all(|c| c.valid_at(now)) {
        return reject("a certificate is expired or not yet valid");
    }
    let key = KeyPair::from_pem(key_pem).map_err(|e| ChainRejected(format!("pending key: {e}")))?;
    if key.subject_public_key_info() != leaf.spki_der {
        return reject("the leaf certificate does not match the requested key");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{BasicConstraints, IsCa};

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
            store.install(CertificateUse::CentralSystemRootCertificate, &pem, now, 2),
            InstallCertificateStatus::Accepted
        );
        // Re-install is idempotent.
        assert_eq!(
            store.install(CertificateUse::CentralSystemRootCertificate, &pem, now, 2),
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
        store.install(CertificateUse::ManufacturerRootCertificate, &pem, now, 2);
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
            store.install(kind, &a, now, 1),
            InstallCertificateStatus::Accepted
        );
        assert_eq!(
            store.install(kind, &b, now, 1),
            InstallCertificateStatus::Rejected
        );
        assert_eq!(
            store.install(kind, &expired, now, 5),
            InstallCertificateStatus::Rejected
        );
        assert_eq!(
            store.install(kind, "not a pem", now, 5),
            InstallCertificateStatus::Rejected
        );
        let two = format!("{a}\n{b}");
        assert_eq!(
            store.install(kind, &two, now, 5),
            InstallCertificateStatus::Rejected
        );
        let huge = "A".repeat(6000);
        assert_eq!(
            store.install(kind, &huge, now, 5),
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
    }
}
