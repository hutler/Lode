//! Initializing a data directory, opening it, and serving HTTP or HTTPS.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use lode_meta::{MetaConfig, MetaStore, MigrationSet, NewPrincipal, PrincipalKind, Reads};
use lode_store::{BlobStore, FsBlobStore, FsBlobStoreConfig, S3BlobStore};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::acme::{Acme, AcmeError};
use crate::auth::{generate_token, hash_token, ScopeRegistry, ADMIN_USER};
use crate::clock::{Clock, SharedClock, SystemClock};
use crate::config::{ServerConfig, TlsConfig, CONFIG_FILE_NAME, DEFAULT_CONFIG};
use crate::module::Modules;
use crate::serve::{self, Settings};
use crate::state::AppState;
use crate::tls::{acme_server_config, load_server_config, TlsError};

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
    /// A password shorter than the minimum.
    #[error(
        "the password must have at least {} characters",
        crate::auth::MIN_PASSWORD_LEN
    )]
    WeakPassword,
    /// The database has no built-in `admin` user.
    #[error("the database has no `admin` user")]
    NoAdmin,
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
    /// The `[storage]` settings cannot be used.
    #[error("{0}")]
    Storage(String),
    /// TLS setup error.
    #[error(transparent)]
    Tls(#[from] TlsError),
    /// ACME certificate setup error.
    #[error(transparent)]
    Acme(#[from] AcmeError),
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
    /// The one-time token of the built-in `admin` user. Shown once; only its hash is stored.
    pub admin_token: String,
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
    let meta = open_meta(db, Vec::new()).await?;
    FsBlobStore::open(data_dir, FsBlobStoreConfig::default()).await?;

    let token = generate_token(&mut rand::rngs::OsRng);
    let hash = hash_token(&token);
    let now = SystemClock.now_ms();
    meta.write(move |tx| {
        let admin = tx.create_principal(
            &NewPrincipal {
                kind: PrincipalKind::User,
                name: ADMIN_USER.to_owned(),
                display_name: Some("Administrator".to_owned()),
                owner_id: None,
                password_hash: None,
            },
            now,
        )?;
        tx.create_token(
            admin,
            "init",
            &hash,
            &ScopeRegistry::core().all(),
            None,
            now,
        )?;
        Ok(())
    })
    .await?;

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
        admin_token: token,
    })
}

async fn open_meta(db: PathBuf, modules: Vec<MigrationSet>) -> Result<MetaStore, ServerError> {
    tokio::task::spawn_blocking(move || MetaStore::open(db, &modules, MetaConfig::default()))
        .await
        .map_err(|e| ServerError::Io {
            context: "opening the database".into(),
            source: io::Error::other(e),
        })?
        .map_err(ServerError::from)
}

fn check_initialized(dir: &Path) -> Result<PathBuf, ServerError> {
    if !dir.is_dir() {
        return Err(ServerError::DataDirMissing(dir.to_owned()));
    }
    let db = dir.join(META_DB_FILE);
    if !db.is_file() {
        return Err(ServerError::NotInitialized(dir.to_owned()));
    }
    Ok(db)
}

/// Offline recovery: revokes every token of the built-in `admin` user, re-enables it, and
/// returns a new token with all core scopes.
pub async fn reset_admin(config: &ServerConfig) -> Result<String, ServerError> {
    let db = check_initialized(&config.data_dir)?;
    let meta = open_meta(db, Vec::new()).await?;
    let token = generate_token(&mut rand::rngs::OsRng);
    let hash = hash_token(&token);
    let now = SystemClock.now_ms();
    let found = meta
        .write(move |tx| {
            let Some(admin) = tx
                .principal_by_name(ADMIN_USER)?
                .filter(|p| p.kind == PrincipalKind::User)
            else {
                return Ok(false);
            };
            tx.delete_tokens_of(admin.id)?;
            tx.set_principal_disabled(admin.id, false)?;
            let scopes = ScopeRegistry::core().all();
            tx.create_token(admin.id, "reset-admin", &hash, &scopes, None, now)?;
            Ok(true)
        })
        .await?;
    if !found {
        return Err(ServerError::NoAdmin);
    }
    Ok(token)
}

/// Offline: sets the built-in `admin` user's password, so the administrator can sign in with
/// `lode login` (used by `loded setup`). The password must have at least
/// [`MIN_PASSWORD_LEN`](crate::auth::MIN_PASSWORD_LEN) characters.
pub async fn set_admin_password(config: &ServerConfig, password: &str) -> Result<(), ServerError> {
    if password.chars().count() < crate::auth::MIN_PASSWORD_LEN {
        return Err(ServerError::WeakPassword);
    }
    let db = check_initialized(&config.data_dir)?;
    let meta = open_meta(db, Vec::new()).await?;
    let password = password.to_owned();
    let hash = tokio::task::spawn_blocking(move || crate::auth::hash_password(&password))
        .await
        .map_err(|e| ServerError::Io {
            context: "hashing the password".into(),
            source: io::Error::other(e),
        })?
        .map_err(|e| ServerError::Io {
            context: "hashing the password".into(),
            source: io::Error::other(e.to_string()),
        })?;
    let found = meta
        .write(move |tx| {
            let Some(admin) = tx
                .principal_by_name(ADMIN_USER)?
                .filter(|p| p.kind == PrincipalKind::User)
            else {
                return Ok(false);
            };
            tx.set_password_hash(admin.id, Some(&hash))?;
            Ok(true)
        })
        .await?;
    if !found {
        return Err(ServerError::NoAdmin);
    }
    Ok(())
}

/// Opens an initialized data directory and builds the shared state.
///
/// `modules` are the enabled modules (see [`crate::module::select_enabled`]); their
/// migrations run here, and `/info` reports their names.
pub async fn open_state(
    config: ServerConfig,
    clock: SharedClock,
    modules: Modules,
) -> Result<AppState, ServerError> {
    let dir = config.data_dir.clone();
    let db = check_initialized(&dir)?;
    // Only enabled modules migrate, so a disabled module adds no tables.
    let sets = modules
        .iter()
        .map(|m| MigrationSet {
            namespace: m.name(),
            migrations: m.migrations(),
        })
        .collect();
    let meta = open_meta(db, sets).await?;
    let blobs = open_blob_store(&config).await?;
    let state = AppState::new(meta, blobs, config, clock, modules);
    state.permissions().reload(state.meta()).await?;
    Ok(state)
}

/// Opens the configured object store: files under the data directory, or an S3 bucket,
/// which must answer a listing with these credentials.
pub async fn open_blob_store(config: &ServerConfig) -> Result<Arc<dyn BlobStore>, ServerError> {
    match config.storage.s3_in_use() {
        None => Ok(Arc::new(
            FsBlobStore::open(
                &config.data_dir,
                FsBlobStoreConfig {
                    max_object_size: config.limits.max_blob_size,
                },
            )
            .await?,
        )),
        Some(s3) => {
            let env = |name: &str| std::env::var(name).ok();
            let cfg = s3
                .store_config(&config.data_dir, config.limits.max_blob_size, &env)
                .map_err(ServerError::Storage)?;
            let store = S3BlobStore::open(cfg).await?;
            // Fail at start, with S3's answer, rather than on the first upload.
            store.check().await?;
            Ok(Arc::new(store))
        }
    }
}

/// The remote address of a connection, available to handlers as
/// `ConnectInfo<ClientInfo>` for plain and TLS listeners alike.
#[derive(Debug, Clone, Copy)]
pub struct ClientInfo(pub SocketAddr);

/// A server bound to its address, ready to run.
pub struct BoundServer {
    tcp: TcpListener,
    tls: Option<TlsAcceptor>,
    acme: Option<Acme>,
    local: SocketAddr,
    router: Router,
    settings: Settings,
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
        let mut acme = None;
        let tls = match &config.tls {
            Some(TlsConfig::Files { cert, key }) => {
                let rustls_config = load_server_config(cert, key)?;
                Some(TlsAcceptor::from(Arc::new(rustls_config)))
            }
            Some(TlsConfig::Acme(acme_config)) => {
                let manager = Acme::new(acme_config.clone(), &config.data_dir)?;
                let rustls_config = acme_server_config(manager.resolver())?;
                acme = Some(manager);
                Some(TlsAcceptor::from(Arc::new(rustls_config)))
            }
            None => {
                if !local.ip().is_loopback() {
                    tracing::warn!(
                        %local,
                        "serving plain HTTP on a non-loopback address; configure [tls] or put \
                         a TLS-terminating proxy in front"
                    );
                }
                None
            }
        };
        let settings = Settings {
            header_timeout: Duration::from_secs(config.connections.header_timeout_seconds),
            max_per_address: config.connections.max_per_address,
        };
        Ok(Self {
            tcp,
            tls,
            acme,
            local,
            router,
            settings,
        })
    }

    /// The bound address (useful when the configured port was 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// `https` or `http`.
    pub fn scheme(&self) -> &'static str {
        if self.tls.is_some() {
            "https"
        } else {
            "http"
        }
    }

    /// Serves until `shutdown` completes, then stops accepting and waits for in-flight
    /// requests to finish.
    pub async fn run<F>(self, shutdown: F) -> io::Result<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        // The certificate is obtained and renewed while the server runs.
        let acme = self.acme.map(|a| tokio::spawn(a.maintain()));
        serve::serve(self.tcp, self.tls, self.router, self.settings, shutdown).await;
        if let Some(task) = acme {
            task.abort();
        }
        Ok(())
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
