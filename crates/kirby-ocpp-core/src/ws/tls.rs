//! TLS client settings for `wss://` URLs.
//!
//! * Trust: without `ca_file` the platform root store is used. With `ca_file` the
//!   certificates in that file **replace** the system roots, like Node's `ca` option.
//! * `skip_verify` accepts any server certificate and name (testing only). Handshake
//!   signatures are still verified through the crypto provider.
//! * `cert_file` + `key_file` give a client identity (mutual TLS).
//! * `server_name` overrides the SNI value and the name the certificate is checked against.
//!
//! The provider is always ring, passed explicitly (no process-wide default is installed).

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use url::Url;

use super::ConnectError;
use crate::config::TlsConfig;

/// Builds the rustls client configuration for `tls`, reading the PEM files now.
pub(super) async fn client_config(tls: &TlsConfig) -> Result<Arc<ClientConfig>, ConnectError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| ConnectError::Tls(e.to_string()))?;

    let builder = if tls.skip_verify {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipVerification {
                algorithms: provider.signature_verification_algorithms,
            }))
    } else {
        builder.with_root_certificates(trust_roots(tls.ca_file.as_deref()).await?)
    };

    let config = match client_identity(tls).await? {
        Some((chain, key)) => builder
            .with_client_auth_cert(chain, key)
            .map_err(|e| ConnectError::Tls(format!("client certificate rejected: {e}")))?,
        None => builder.with_no_client_auth(),
    };
    Ok(Arc::new(config))
}

/// The name the server certificate is checked against and sent as SNI: the
/// `server_name` override, else the URL host (without IPv6 brackets).
pub(super) fn server_name(url: &Url, tls: &TlsConfig) -> Result<ServerName<'static>, ConnectError> {
    let name = tls.server_name.clone().unwrap_or_else(|| {
        url.host_str()
            .unwrap_or_default()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned()
    });
    ServerName::try_from(name.clone())
        .map_err(|_| ConnectError::Tls(format!("invalid TLS server name `{name}`")))
}

async fn trust_roots(ca_file: Option<&Path>) -> Result<RootCertStore, ConnectError> {
    let mut roots = RootCertStore::empty();
    if let Some(path) = ca_file {
        for certificate in read_certificates(path).await? {
            roots
                .add(certificate)
                .map_err(|e| file_error(path, format!("not a usable CA certificate: {e}")))?;
        }
    } else {
        let loaded = rustls_native_certs::load_native_certs();
        roots.add_parsable_certificates(loaded.certs);
        if roots.is_empty() {
            return Err(ConnectError::Tls(
                "no system root certificates found; set tls ca_file".to_owned(),
            ));
        }
    }
    Ok(roots)
}

async fn client_identity(
    tls: &TlsConfig,
) -> Result<Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>, ConnectError> {
    match (&tls.cert_file, &tls.key_file) {
        (None, None) => Ok(None),
        (Some(cert_path), Some(key_path)) => {
            let chain = read_certificates(cert_path).await?;
            let bytes = tokio::fs::read(key_path)
                .await
                .map_err(|e| file_error(key_path, e.to_string()))?;
            let key = PrivateKeyDer::from_pem_slice(&bytes)
                .map_err(|e| file_error(key_path, format!("no usable private key: {e}")))?;
            Ok(Some((chain, key)))
        }
        _ => Err(ConnectError::Tls(
            "tls cert_file and key_file must be given together".to_owned(),
        )),
    }
}

async fn read_certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>, ConnectError> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| file_error(path, e.to_string()))?;
    let certificates = CertificateDer::pem_slice_iter(&bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| file_error(path, format!("invalid PEM certificate: {e}")))?;
    if certificates.is_empty() {
        return Err(file_error(path, "no PEM certificates found"));
    }
    Ok(certificates)
}

fn file_error(path: &Path, reason: impl Into<String>) -> ConnectError {
    ConnectError::TlsFile {
        path: path.to_owned(),
        reason: reason.into(),
    }
}

/// Accepts any certificate chain and name; signatures are still checked.
struct SkipVerification {
    algorithms: WebPkiSupportedAlgorithms,
}

impl fmt::Debug for SkipVerification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SkipVerification").finish_non_exhaustive()
    }
}

impl ServerCertVerifier for SkipVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tls_with_server_name(name: Option<&str>) -> TlsConfig {
        TlsConfig {
            server_name: name.map(str::to_owned),
            ..TlsConfig::default()
        }
    }

    #[test]
    fn server_name_defaults_to_the_url_host() {
        let url = Url::parse("wss://127.0.0.1:9443/ocpp").unwrap();
        let name = server_name(&url, &tls_with_server_name(None)).unwrap();
        assert_eq!(name.to_str(), "127.0.0.1");
    }

    #[test]
    fn server_name_strips_ipv6_brackets() {
        let url = Url::parse("wss://[::1]:9443/ocpp").unwrap();
        let name = server_name(&url, &tls_with_server_name(None)).unwrap();
        assert_eq!(name.to_str(), "::1");
    }

    #[test]
    fn server_name_override_wins() {
        let url = Url::parse("wss://127.0.0.1:9443/ocpp").unwrap();
        let name = server_name(&url, &tls_with_server_name(Some("central.example"))).unwrap();
        assert_eq!(name.to_str(), "central.example");
    }

    #[test]
    fn invalid_server_name_is_a_tls_error() {
        let url = Url::parse("wss://h/ocpp").unwrap();
        let error = server_name(&url, &tls_with_server_name(Some("not a name!"))).unwrap_err();
        assert!(matches!(error, ConnectError::Tls(_)), "{error}");
    }
}
