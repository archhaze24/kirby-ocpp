use chrono::{DateTime, Utc};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PublicKeyData};
use ring::digest;
use x509_parser::prelude::{FromDer, X509Certificate};

use super::{CertificateStore, CertificateSummary, ChainRejected, ParsedCert, StoredRoot};
use crate::ocpp::messages::{
    CertificateHashData, CertificateUse, DeleteCertificateStatus, HashAlgorithm,
    InstallCertificateStatus,
};

/// Longest certificate chain that is looked at.
const MAX_CHAIN_CERTS: usize = 8;

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
            // Subject equal to issuer is not enough: the signature must verify with its own key.
            self_signed: cert.subject().as_raw() == cert.issuer().as_raw()
                && cert.verify_signature(None).is_ok(),
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

pub(super) fn serial_hex(raw: &[u8]) -> String {
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

pub(super) fn hex(bytes: &[u8]) -> String {
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

/// Checks a `CertificateSigned` chain against the pending private key: it must parse, be within
/// `max_size`, have a valid leaf, the leaf's public key must be the pending key's, and every
/// certificate must be signed by the next one of the chain (no trust anchor or name checks).
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
    if !chain.windows(2).all(|pair| pair[0].signed_by(&pair[1])) {
        return reject("a certificate of the chain is not signed by the next one");
    }
    Ok(())
}
