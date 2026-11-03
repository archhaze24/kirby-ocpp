use std::sync::Arc;

use rustls::ServerConfig;
use rustls::crypto::ring::default_provider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

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
