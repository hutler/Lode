//! Lode server: configuration, error model, HTTP routes and server lifecycle.
//!
//! Later tasks add authentication, permissions, the commit engine and the module framework.

#![forbid(unsafe_code)]

pub mod clock;
pub mod config;
pub mod error;
pub mod extract;
pub mod routes;
pub mod server;
pub mod state;
pub mod tls;

pub use clock::{Clock, ManualClock, SharedClock, SystemClock};
pub use config::{ConfigError, Limits, ModulesConfig, ServerConfig, TlsConfig};
pub use error::{ApiError, ApiResult};
pub use extract::ApiJson;
pub use routes::{core_routes, finish, router, REQUEST_ID_HEADER};
pub use server::{init, open_state, shutdown_signal, BoundServer, InitReport, ServerError};
pub use state::AppState;
