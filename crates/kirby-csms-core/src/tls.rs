use std::sync::Arc;

use rustls::crypto::ring::default_provider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};

use crate::error::CsmsError;

/// Builds a server-auth-only rustls configuration from PEM text: a certificate chain
/// (leaf first) and one private key (PKCS#8, PKCS#1 or SEC1). Read the files yourself and
/// pass their bytes. Uses the ring provider explicitly, so no process-wide default
/// provider is needed. For mTLS or other needs build a `ServerConfig` directly.
pub fn server_config_from_pem(
    cert_chain_pem: &[u8],
    private_key_pem: &[u8],
) -> Result<Arc<ServerConfig>, CsmsError> {
    let chain = CertificateDer::pem_slice_iter(cert_chain_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| CsmsError::Tls(format!("certificate chain: {e}")))?;
    if chain.is_empty() {
        return Err(CsmsError::Tls("no certificate in the PEM data".to_owned()));
    }
    let key = PrivateKeyDer::from_pem_slice(private_key_pem)
        .map_err(|e| CsmsError::Tls(format!("private key: {e}")))?;
    let config = ServerConfig::builder_with_provider(Arc::new(default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| CsmsError::Tls(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|e| CsmsError::Tls(e.to_string()))?;
    Ok(Arc::new(config))
}

/// Like [`server_config_from_pem`] but requires every client to present a certificate that
/// chains to one of the CA certificates in `client_ca_pem` (Security Profile 3). Clients
/// without a valid certificate fail the TLS handshake.
pub fn server_config_with_client_ca(
    cert_chain_pem: &[u8],
    private_key_pem: &[u8],
    client_ca_pem: &[u8],
) -> Result<Arc<ServerConfig>, CsmsError> {
    let chain = CertificateDer::pem_slice_iter(cert_chain_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| CsmsError::Tls(format!("certificate chain: {e}")))?;
    if chain.is_empty() {
        return Err(CsmsError::Tls("no certificate in the PEM data".to_owned()));
    }
    let key = PrivateKeyDer::from_pem_slice(private_key_pem)
        .map_err(|e| CsmsError::Tls(format!("private key: {e}")))?;
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(client_ca_pem) {
        let cert = cert.map_err(|e| CsmsError::Tls(format!("client CA: {e}")))?;
        roots
            .add(cert)
            .map_err(|e| CsmsError::Tls(format!("client CA: {e}")))?;
    }
    if roots.is_empty() {
        return Err(CsmsError::Tls(
            "no certificate in the client CA PEM data".to_owned(),
        ));
    }
    let provider = Arc::new(default_provider());
    let verifier =
        WebPkiClientVerifier::builder_with_provider(Arc::new(roots), Arc::clone(&provider))
            .build()
            .map_err(|e| CsmsError::Tls(format!("client verifier: {e}")))?;
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| CsmsError::Tls(e.to_string()))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(chain, key)
        .map_err(|e| CsmsError::Tls(e.to_string()))?;
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_pem_is_a_tls_error() {
        assert!(matches!(
            server_config_from_pem(b"nope", b"nope"),
            Err(CsmsError::Tls(_))
        ));
    }
}
