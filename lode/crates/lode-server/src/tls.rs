//! HTTPS: loading PEM files into a rustls configuration.

use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// Errors setting up TLS.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// The certificate file could not be read or holds no certificate.
    #[error("cannot load TLS certificate {path}: {message}")]
    Cert {
        /// File.
        path: String,
        /// What is wrong.
        message: String,
    },
    /// The key file could not be read or holds no key.
    #[error("cannot load TLS key {path}: {message}")]
    Key {
        /// File.
        path: String,
        /// What is wrong.
        message: String,
    },
    /// rustls rejected the certificate and key.
    #[error("invalid TLS certificate or key: {0}")]
    Config(#[from] rustls::Error),
}

/// Builds a rustls server configuration from PEM files.
pub fn load_server_config(cert: &Path, key: &Path) -> Result<rustls::ServerConfig, TlsError> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .and_then(|it| it.collect::<Result<Vec<_>, _>>())
        .map_err(|e| TlsError::Cert {
            path: cert.display().to_string(),
            message: e.to_string(),
        })?;
    if certs.is_empty() {
        return Err(TlsError::Cert {
            path: cert.display().to_string(),
            message: "no certificate found".into(),
        });
    }
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| TlsError::Key {
        path: key.display().to_string(),
        message: e.to_string(),
    })?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

/// A rustls server configuration whose certificate comes from ACME (T081). It also offers
/// the `acme-tls/1` protocol, which only the certificate authority's validation uses.
pub(crate) fn acme_server_config(
    resolver: Arc<crate::acme::CertResolver>,
) -> Result<rustls::ServerConfig, TlsError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    config.alpn_protocols = vec![
        b"h2".to_vec(),
        b"http/1.1".to_vec(),
        crate::acme::ACME_TLS_ALPN.to_vec(),
    ];
    Ok(config)
}
