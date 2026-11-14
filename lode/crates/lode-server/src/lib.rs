//! Lode server: configuration, error model, HTTP routes and server lifecycle.
//!
//! Later tasks add authentication, permissions, the commit engine and the module framework.

#![forbid(unsafe_code)]

pub mod acme;
pub mod admin;
pub mod auth;
pub mod blobs;
pub mod branches;
pub mod clock;
pub mod commit;
pub mod config;
pub mod depots;
pub mod error;
pub mod events;
pub mod extract;
pub mod failpoint;
mod idle;
pub mod lru;
pub mod maintenance;
pub mod module;
pub mod perm;
pub mod read;
pub mod routes;
mod serve;
pub mod server;
pub mod state;
pub mod tls;

pub use auth::Caller;
pub use clock::{Clock, ManualClock, SharedClock, SystemClock};
pub use config::{
    AcmeConfig, AdminUiConfig, BackupConfig, ConfigError, ConnectionsConfig, GcConfig, Limits,
    ModulesConfig, OidcConfig, PasswordLogin, S3Settings, ServerConfig, StorageConfig, StorageKind,
    TlsConfig, WebhooksConfig,
};
pub use error::{ApiError, ApiResult};
pub use extract::{ApiJson, ApiPath, ApiQuery};
pub use perm::{BranchAccess, DepotAccess, Level, Permissions};
pub use routes::{core_routes, finish, router, REQUEST_ID_HEADER};
pub use server::{
    init, open_blob_store, open_state, reset_admin, set_admin_password, shutdown_signal,
    BoundServer, ClientInfo, InitReport, ServerError,
};
pub use state::{AppState, BackupRun};
