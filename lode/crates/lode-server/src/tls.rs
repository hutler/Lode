//! HTTPS: loading PEM files and a listener that hands finished TLS connections to axum.

use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;

/// How long a client may take to finish the TLS handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

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

/// An `axum::serve` listener that accepts TCP connections and completes each TLS handshake
/// on its own task, so one slow client cannot hold up the others.
pub struct TlsListener {
    rx: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    local: SocketAddr,
    accept_task: tokio::task::JoinHandle<()>,
}

impl TlsListener {
    /// Starts accepting on `tcp`.
    pub fn new(tcp: TcpListener, config: rustls::ServerConfig) -> io::Result<Self> {
        let local = tcp.local_addr()?;
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let (tx, rx) = mpsc::channel(64);
        let accept_task = tokio::spawn(async move {
            loop {
                let (stream, addr) = match tcp.accept().await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(error = %e, "accept failed");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let acceptor = acceptor.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                        Ok(Ok(tls)) => {
                            let _ = tx.send((tls, addr)).await;
                        }
                        Ok(Err(e)) => tracing::debug!(%addr, error = %e, "TLS handshake failed"),
                        Err(_) => tracing::debug!(%addr, "TLS handshake timed out"),
                    }
                });
            }
        });
        Ok(Self {
            rx,
            local,
            accept_task,
        })
    }
}

impl Drop for TlsListener {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.rx.recv().await {
            Some(conn) => conn,
            // The accept task only ends when this listener is dropped, so the channel cannot
            // close while `accept` is being awaited.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(self.local)
    }
}
