use std::fmt;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose, SanType,
};
use rustls::crypto::ring::default_provider;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};

use crate::error::{TestkitError, TestkitResult};

/// TLS options for a `wss://` mock CSMS.
///
/// Certificates are generated in memory when the server starts: a self-signed CA,
/// a server certificate for `localhost` and `127.0.0.1`, and a client certificate
/// issued by that CA. Read the PEM back with [`crate::MockCsms::tls_material`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TlsOpts {
    /// Require a client certificate signed by the generated CA (mTLS). Clients without
    /// one fail the TLS handshake.
    pub require_client_cert: bool,
}

/// PEM material generated for a TLS mock CSMS.
#[derive(Clone, PartialEq, Eq)]
pub struct TlsMaterial {
    ca_cert: String,
    client_cert: String,
    client_key: String,
}

impl TlsMaterial {
    /// Certificate (PEM) of the CA that signed the server and client certificates. Trust it
    /// as a root in the charge point's configuration.
    pub fn ca_cert_pem(&self) -> &str {
        &self.ca_cert
    }

    /// Client certificate (PEM) issued by the CA. Valid for mTLS.
    pub fn client_cert_pem(&self) -> &str {
        &self.client_cert
    }

    /// PKCS#8 private key (PEM) of [`TlsMaterial::client_cert_pem`]. Secret: write it only
    /// to a temporary directory.
    pub fn client_key_pem(&self) -> &str {
        &self.client_key
    }
}

// The client key is deliberately not printed.
impl fmt::Debug for TlsMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsMaterial")
            .field("ca_cert_bytes", &self.ca_cert.len())
            .finish_non_exhaustive()
    }
}

fn tls_error(error: impl fmt::Display) -> TestkitError {
    TestkitError::Tls(error.to_string())
}

/// Generates the CA, server and client certificates and the server TLS configuration.
///
/// Uses the ring provider explicitly, so no process-wide default provider is needed.
pub(crate) fn build(options: TlsOpts) -> TestkitResult<(Arc<ServerConfig>, TlsMaterial)> {
    let provider = Arc::new(default_provider());

    let ca_key = KeyPair::generate().map_err(tls_error)?;
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).map_err(tls_error)?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "kirby-ocpp-testkit CA");
    let ca = CertifiedIssuer::self_signed(ca_params, ca_key).map_err(tls_error)?;

    let server_key = KeyPair::generate().map_err(tls_error)?;
    let mut server_params =
        CertificateParams::new(vec!["localhost".to_owned()]).map_err(tls_error)?;
    server_params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    server_params
        .distinguished_name
        .push(DnType::CommonName, "localhost");
    let server_cert = server_params
        .signed_by(&server_key, &ca)
        .map_err(tls_error)?;

    let client_key = KeyPair::generate().map_err(tls_error)?;
    let mut client_params = CertificateParams::new(Vec::<String>::new()).map_err(tls_error)?;
    client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    client_params
        .distinguished_name
        .push(DnType::CommonName, "kirby-ocpp-testkit client");
    let client_cert = client_params
        .signed_by(&client_key, &ca)
        .map_err(tls_error)?;

    let mut roots = RootCertStore::empty();
    roots.add(ca.der().clone()).map_err(tls_error)?;
    let builder = ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(tls_error)?;
    let builder = if options.require_client_cert {
        let verifier =
            WebPkiClientVerifier::builder_with_provider(Arc::new(roots), Arc::clone(&provider))
                .build()
                .map_err(tls_error)?;
        builder.with_client_cert_verifier(verifier)
    } else {
        builder.with_no_client_auth()
    };
    let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(server_key.serialize_der()));
    let config = builder
        .with_single_cert(vec![server_cert.der().clone()], key)
        .map_err(tls_error)?;

    let material = TlsMaterial {
        ca_cert: ca.pem(),
        client_cert: client_cert.pem(),
        client_key: client_key.serialize_pem(),
    };
    Ok((Arc::new(config), material))
}
