//! Initializing a data directory, opening it, and serving HTTP or HTTPS.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use lode_meta::{MetaConfig, MetaStore};
use lode_store::{FsBlobStore, FsBlobStoreConfig};
use tokio::net::TcpListener;

use crate::clock::SharedClock;
use crate::config::{ServerConfig, CONFIG_FILE_NAME, DEFAULT_CONFIG};
use crate::state::AppState;
use crate::tls::{load_server_config, TlsError, TlsListener};

/// Name of the metadata database inside the data directory.
pub const META_DB_FILE: &str = "meta.db";

/// Name of the object directory inside the data directory.
pub const DEPOTS_DIR: &str = "depots";

/// Errors starting or initializing a server.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// The configured data directory does not exist.
    #[error("data directory {0} does not exist; create it with `loded init <data-dir>`")]
    DataDirMissing(PathBuf),
    /// The data directory exists but was never initialized.
    #[error("data directory {0} is not initialized (no meta.db); run `loded init <data-dir>`")]
    NotInitialized(PathBuf),
    /// `init` was run on a directory that already holds a server.
    #[error("data directory {0} is already initialized")]
    AlreadyInitialized(PathBuf),
    /// Filesystem error.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: String,
        /// Cause.
        source: io::Error,
    },
    /// Metadata store error.
    #[error("metadata store: {0}")]
    Meta(#[from] lode_meta::MetaError),
    /// Blob store error.
    #[error("blob store: {0}")]
    Store(#[from] lode_store::StoreError),
    /// TLS setup error.
    #[error(transparent)]
    Tls(#[from] TlsError),
}

fn io_err(context: impl Into<String>) -> impl FnOnce(io::Error) -> ServerError {
    let context = context.into();
    move |source| ServerError::Io { context, source }
}

/// What `init` created.
#[derive(Debug, Clone)]
pub struct InitReport {
    /// The data directory.
    pub data_dir: PathBuf,
    /// The configuration file, if `init` wrote it; `None` if one was already there.
    pub config_written: Option<PathBuf>,
}

/// Creates the directory layout, the database and a commented default `loded.toml`.
///
/// Refuses a directory that already has a database. An existing `loded.toml` is kept.
pub async fn init(data_dir: &Path) -> Result<InitReport, ServerError> {
    let db = data_dir.join(META_DB_FILE);
    if db.exists() {
        return Err(ServerError::AlreadyInitialized(data_dir.to_owned()));
    }
    tokio::fs::create_dir_all(data_dir)
        .await
        .map_err(io_err(format!("cannot create {}", data_dir.display())))?;
    let meta_path = db.clone();
    tokio::task::spawn_blocking(move || MetaStore::open(meta_path, &[], MetaConfig::default()))
        .await
        .map_err(|e| ServerError::Io {
            context: "opening the database".into(),
            source: io::Error::other(e),
        })??;
    FsBlobStore::open(data_dir, FsBlobStoreConfig::default()).await?;

    let config_path = data_dir.join(CONFIG_FILE_NAME);
    let config_written = if config_path.exists() {
        None
    } else {
        tokio::fs::write(&config_path, DEFAULT_CONFIG)
            .await
            .map_err(io_err(format!("cannot write {}", config_path.display())))?;
        Some(config_path)
    };
    Ok(InitReport {
        data_dir: data_dir.to_owned(),
        config_written,
    })
}

/// Opens an initialized data directory and builds the shared state.
///
/// `modules` names the enabled, registered modules (reported by `/info`).
pub async fn open_state(
    config: ServerConfig,
    clock: SharedClock,
    modules: Vec<String>,
) -> Result<AppState, ServerError> {
    let dir = config.data_dir.clone();
    if !dir.is_dir() {
        return Err(ServerError::DataDirMissing(dir));
    }
    let db = dir.join(META_DB_FILE);
    if !db.is_file() {
        return Err(ServerError::NotInitialized(dir));
    }
    let meta = tokio::task::spawn_blocking(move || MetaStore::open(db, &[], MetaConfig::default()))
        .await
        .map_err(|e| ServerError::Io {
            context: "opening the database".into(),
            source: io::Error::other(e),
        })??;
    let blobs = FsBlobStore::open(
        &dir,
        FsBlobStoreConfig {
            max_object_size: config.limits.max_blob_size,
        },
    )
    .await?;
    Ok(AppState::new(meta, Arc::new(blobs), config, clock, modules))
}

enum Listener {
    Plain(TcpListener),
    Tls(TlsListener),
}

/// A server bound to its address, ready to run.
pub struct BoundServer {
    listener: Listener,
    local: SocketAddr,
    router: Router,
}

impl BoundServer {
    /// Binds the configured address and loads TLS material if configured.
    pub async fn bind(config: &ServerConfig, router: Router) -> Result<Self, ServerError> {
        let tcp = TcpListener::bind(config.listen)
            .await
            .map_err(io_err(format!("cannot listen on {}", config.listen)))?;
        let local = tcp
            .local_addr()
            .map_err(io_err("cannot read the listen address"))?;
        let listener = match &config.tls {
            Some(tls) => {
                let rustls_config = load_server_config(&tls.cert, &tls.key)?;
                Listener::Tls(
                    TlsListener::new(tcp, rustls_config)
                        .map_err(io_err("cannot start the TLS listener"))?,
                )
            }
            None => {
                if !local.ip().is_loopback() {
                    tracing::warn!(
                        %local,
                        "serving plain HTTP on a non-loopback address; configure [tls] or put \
                         a TLS-terminating proxy in front"
                    );
                }
                Listener::Plain(tcp)
            }
        };
        Ok(Self {
            listener,
            local,
            router,
        })
    }

    /// The bound address (useful when the configured port was 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// `https` or `http`.
    pub fn scheme(&self) -> &'static str {
        match self.listener {
            Listener::Plain(_) => "http",
            Listener::Tls(_) => "https",
        }
    }

    /// Serves until `shutdown` completes, then stops accepting and waits for in-flight
    /// requests to finish.
    pub async fn run<F>(self, shutdown: F) -> io::Result<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        match self.listener {
            Listener::Plain(l) => {
                axum::serve(l, self.router)
                    .with_graceful_shutdown(shutdown)
                    .await
            }
            Listener::Tls(l) => {
                axum::serve(l, self.router)
                    .with_graceful_shutdown(shutdown)
                    .await
            }
        }
    }
}

/// Completes on Ctrl-C, and on SIGTERM where that exists.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "cannot listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = term => {},
    }
    tracing::info!("shutting down; finishing in-flight requests");
}
