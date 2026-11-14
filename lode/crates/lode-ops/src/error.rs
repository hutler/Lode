//! Operation errors: a stable `code`, a message, details, and the exit code of SPEC §9.

use std::fmt;

use lode_core::ErrorCode;
use lode_workspace::WorkspaceError;
use serde_json::{json, Value};

/// Exit codes of SPEC §9.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The operation failed: conflict, rejected, not found.
    Failed = 1,
    /// Bad command line.
    Usage = 2,
    /// Authentication or permission.
    Auth = 3,
    /// Network or server fault.
    Network = 4,
}

/// An error as the user sees it.
#[derive(Debug, Clone)]
pub struct OpError {
    /// Stable code, such as `out_of_date` or `workspace_in_use`.
    pub code: String,
    /// What happened and, where possible, what to do.
    pub message: String,
    /// Structured data, such as affected paths.
    pub details: Value,
    /// Process exit code.
    pub exit: Exit,
}

impl OpError {
    /// An error with no details.
    pub fn new(exit: Exit, code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            details: Value::Null,
            exit,
        }
    }

    /// A usage error (exit 2).
    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(Exit::Usage, "usage", message)
    }

    /// An operation failure (exit 1).
    pub fn failed(code: &str, message: impl Into<String>) -> Self {
        Self::new(Exit::Failed, code, message)
    }

    /// Adds details.
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    /// The `--json` form.
    pub fn to_json(&self) -> Value {
        let mut error = json!({ "code": self.code, "message": self.message });
        if !self.details.is_null() {
            error["details"] = self.details.clone();
        }
        json!({ "ok": false, "error": error })
    }
}

impl fmt::Display for OpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<lode_client::Error> for OpError {
    fn from(e: lode_client::Error) -> Self {
        use lode_client::Error as E;
        match e {
            E::Api(api) => {
                let exit = match api.known_code() {
                    Some(ErrorCode::Unauthenticated | ErrorCode::PermissionDenied) => Exit::Auth,
                    Some(ErrorCode::Internal) => Exit::Network,
                    _ if api.status >= 500 => Exit::Network,
                    _ => Exit::Failed,
                };
                let message = match api.known_code() {
                    Some(ErrorCode::Unauthenticated) => format!(
                        "{} (sign in with `lode login <server>` or set LODE_TOKEN_FILE)",
                        api.message
                    ),
                    _ => api.message.clone(),
                };
                Self {
                    code: api.code,
                    message,
                    details: api.details,
                    exit,
                }
            }
            E::Transport(t) => Self::new(
                Exit::Network,
                "network",
                format!("cannot reach the server: {t}"),
            ),
            E::InsecureUrl(_) => Self::new(
                Exit::Usage,
                "insecure_url",
                format!("{e}; pass --insecure to allow it"),
            ),
            E::InvalidUrl { .. } => Self::new(Exit::Usage, "invalid_url", e.to_string()),
            E::HashMismatch { .. } => Self::new(Exit::Network, "hash_mismatch", e.to_string()),
            E::Protocol(_) => Self::new(Exit::Network, "protocol", e.to_string()),
            E::Io(io) => Self::failed("io", io.to_string()),
        }
    }
}

impl From<WorkspaceError> for OpError {
    fn from(e: WorkspaceError) -> Self {
        let code = match &e {
            WorkspaceError::InUse => "workspace_in_use",
            WorkspaceError::NotAWorkspace(_) => "not_a_workspace",
            WorkspaceError::AlreadyAWorkspace(_) => "already_a_workspace",
            WorkspaceError::OutsideWorkspace(_) | WorkspaceError::InvalidName { .. } => {
                return Self::new(Exit::Usage, "invalid_path", e.to_string())
            }
            WorkspaceError::Glob(_) => {
                return Self::new(Exit::Usage, "invalid_pattern", e.to_string())
            }
            WorkspaceError::Io { .. } => "io",
            _ => "workspace",
        };
        Self::failed(code, e.to_string())
    }
}

impl From<std::io::Error> for OpError {
    fn from(e: std::io::Error) -> Self {
        Self::failed("io", e.to_string())
    }
}

/// Shorthand.
pub type OpResult<T> = Result<T, OpError>;
