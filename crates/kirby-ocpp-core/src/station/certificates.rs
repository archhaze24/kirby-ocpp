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
    subject_raw: Vec<u8>,
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
            subject_raw: cert.subject().as_raw().to_vec(),
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
        self.summary_with(kind, self.hash_data(HashAlgorithm::Sha256))
    }

    fn summary_with(&self, kind: &str, hash: CertificateHashData) -> CertificateSummary {
        CertificateSummary {
            kind: kind.to_owned(),
            subject: self.subject.clone(),
            serial_number: self.serial.clone(),
            not_after: self.not_after_utc(),
            hash,
        }
    }

    /// Hash data of an installed root: the certificate counts as its own issuer even when it
    /// is cross-signed (identical to [`Self::hash_data`] for self-signed ones).
    pub fn root_hash_data(&self, algorithm: HashAlgorithm) -> CertificateHashData {
        let mut data = self.hash_data(algorithm);
        if !self.self_signed {
            let alg = match algorithm {
                HashAlgorithm::Sha256 => &digest::SHA256,
                HashAlgorithm::Sha384 => &digest::SHA384,
                HashAlgorithm::Sha512 => &digest::SHA512,
            };
            data.issuer_name_hash = hex(digest::digest(alg, &self.subject_raw).as_ref());
        }
        data
    }

    /// Whether `issuer`'s key made this certificate's signature.
    pub fn signed_by(&self, issuer: &Self) -> bool {
        let Ok((_, cert)) = X509Certificate::from_der(&self.der) else {
            return false;
        };
        let Ok((_, key)) = X509Certificate::from_der(&issuer.der) else {
            return false;
        };
        cert.verify_signature(Some(key.public_key())).is_ok()
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
    ///
    /// With `additional_check` (`AdditionalRootCertificateCheck`) and an installed Central System
    /// root, a new `CentralSystemRootCertificate` must be signed by that root (else `Rejected`);
    /// it then replaces it, the old root stays as the fallback and an older fallback goes. It
    /// need not be self-signed in that case.
    pub fn install(
        &mut self,
        kind: CertificateUse,
        pem: &str,
        now: DateTime<Utc>,
        max_len: usize,
        additional_check: bool,
    ) -> InstallCertificateStatus {
        use InstallCertificateStatus::{Accepted, Rejected};
        if pem.len() > crate::ocpp::messages::MAX_CERTIFICATE_LEN {
            return Rejected;
        }
        let Ok(mut parsed) = parse_pem_chain(pem) else {
            return Rejected;
        };
        if parsed.len() != 1 {
            return Rejected;
        }
        let cert = parsed.remove(0);
        if !cert.valid_at(now) {
            return Rejected;
        }
        let new_hash = cert.root_hash_data(HashAlgorithm::Sha256);
        let parsed_roots = self.parsed_roots();
        if parsed_roots.iter().any(|(k, c)| {
            *k == kind && same_hash(&c.root_hash_data(HashAlgorithm::Sha256), &new_hash)
        }) {
            return Accepted;
        }
        let central = CertificateUse::CentralSystemRootCertificate;
        let primaries: Vec<usize> = if additional_check && kind == central {
            self.roots
                .iter()
                .enumerate()
                .filter(|(_, r)| r.kind == central && !r.fallback)
                .map(|(index, _)| index)
                .collect()
        } else {
            Vec::new()
        };
        if primaries.is_empty() {
            if !cert.self_signed {
                return Rejected;
            }
            if self.roots.len() >= max_len {
                return Rejected;
            }
            self.roots.push(StoredRoot {
                kind,
                pem: pem.trim().to_owned(),
                fallback: false,
            });
            return Accepted;
        }
        // M05.FR.09 / FR.10: the new root must be signed by the root it replaces.
        let signer = primaries.iter().copied().find(|&index| {
            self.roots
                .get(index)
                .and_then(|r| parse_pem_chain(&r.pem).ok())
                .and_then(|c| c.into_iter().next())
                .is_some_and(|old| cert.signed_by(&old))
        });
        let Some(signer) = signer else {
            return Rejected;
        };
        // M05.FR.08: one root plus one fallback; everything else of this kind goes.
        let kept = self.roots.iter().filter(|r| r.kind != central).count();
        if kept + 2 > max_len {
            return Rejected;
        }
        let mut index = 0;
        self.roots.retain_mut(|r| {
            let keep = r.kind != central || index == signer;
            if keep && r.kind == central {
                r.fallback = true;
            }
            index += 1;
            keep
        });
        self.roots.push(StoredRoot {
            kind,
            pem: pem.trim().to_owned(),
            fallback: false,
        });
        Accepted
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

    /// `DeleteCertificate`. With `keep_last_central_root` the last Central System root cannot go
    /// (answer `Failed`).
    pub fn delete(
        &mut self,
        wanted: &CertificateHashData,
        keep_last_central_root: bool,
    ) -> DeleteCertificateStatus {
        let found = self.roots.iter().enumerate().find_map(|(index, r)| {
            let cert = parse_pem_chain(&r.pem).ok()?.into_iter().next()?;
            same_hash(&cert.root_hash_data(wanted.hash_algorithm), wanted)
                .then_some((index, r.kind))
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
        if !self
            .roots
            .iter()
            .any(|r| r.kind == CertificateUse::CentralSystemRootCertificate && !r.fallback)
        {
            // The successor is gone: the fallback is the only root left.
            for r in &mut self.roots {
                r.fallback = false;
            }
        }
        DeleteCertificateStatus::Accepted
    }

    /// `GetInstalledCertificateIds` (SHA-256 hash data).
    pub fn ids(&self, kind: CertificateUse) -> Vec<CertificateHashData> {
        self.parsed_roots()
            .into_iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, c)| c.root_hash_data(HashAlgorithm::Sha256))
            .collect()
    }

    /// Summaries of every certificate (roots first, then the station's own leaf).
    pub fn summaries(&self) -> Vec<CertificateSummary> {
        let mut out: Vec<CertificateSummary> = self
            .parsed_roots()
            .into_iter()
            .map(|(k, c)| c.summary_with(k.as_str(), c.root_hash_data(HashAlgorithm::Sha256)))
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

    /// Whether some certificate of `chain` is an installed manufacturer root or was issued by one
    /// (issuer name equals the root's subject; the signature is not verified). `None` when no
    /// manufacturer root is installed.
    pub fn chains_to_manufacturer_root(&self, chain: &[ParsedCert]) -> Option<bool> {
        let roots: Vec<ParsedCert> = self
            .parsed_roots()
            .into_iter()
            .filter(|(kind, _)| *kind == CertificateUse::ManufacturerRootCertificate)
            .map(|(_, cert)| cert)
            .collect();
        if roots.is_empty() {
            return None;
        }
        Some(chain.iter().any(|cert| {
            roots
                .iter()
                .any(|root| cert.der == root.der || cert.issuer_raw == root.subject_raw)
        }))
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
