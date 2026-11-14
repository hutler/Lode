//! Errors: what the server said ([`ApiError`]) kept apart from failing to talk to it.

use std::io;

use lode_core::{ErrorCode, ObjectId};
use reqwest::StatusCode;

/// An error response from the server, decoded from its `{code, message, details}` body.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message} ({code})")]
pub struct ApiError {
    /// HTTP status of the response.
    pub status: u16,
    /// Stable code, such as `out_of_date`. Kept as text so codes of modules this client
    /// does not know still arrive intact.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
    /// Code-specific data, such as the paths that are out of date; `null` when absent.
    pub details: serde_json::Value,
}

impl ApiError {
    /// The code, if it is one of the codes this client knows.
    pub fn known_code(&self) -> Option<ErrorCode> {
        self.code.parse().ok()
    }

    /// Whether the code is `code`.
    pub fn is(&self, code: ErrorCode) -> bool {
        self.code == code.as_str()
    }

    /// `details.paths`, as most path-related rejections carry it; empty when absent.
    pub fn paths(&self) -> Vec<String> {
        self.details
            .get("paths")
            .and_then(|p| p.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Builds the error for a response whose body is not the error shape, such as a page
    /// from a reverse proxy. The code is the one the status implies.
    pub(crate) fn from_status(status: StatusCode, body: &[u8]) -> Self {
        let code = match status.as_u16() {
            401 => ErrorCode::Unauthenticated,
            403 => ErrorCode::PermissionDenied,
            404 => ErrorCode::NotFound,
            409 => ErrorCode::Conflict,
            413 => ErrorCode::TooLarge,
            422 => ErrorCode::HashMismatch,
            s if s >= 500 => ErrorCode::Internal,
            _ => ErrorCode::InvalidRequest,
        };
        let text = String::from_utf8_lossy(&body[..body.len().min(200)]);
        let text = text.trim();
        let message = if text.is_empty() {
            format!("HTTP {status}")
        } else {
            format!("HTTP {status}: {text}")
        };
        Self {
            status: status.as_u16(),
            code: code.as_str().to_owned(),
            message,
            details: serde_json::Value::Null,
        }
    }
}

/// Everything a client call can fail with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server answered with an error.
    #[error(transparent)]
    Api(#[from] ApiError),
    /// The request did not complete: connection, TLS, timeout or a broken body.
    #[error("cannot reach the server: {0}")]
    Transport(#[source] reqwest::Error),
    /// Plain HTTP to a host that is not this machine, without explicit permission.
    #[error(
        "refusing plain HTTP to {0}: tokens would travel unencrypted; use https:// or allow insecure connections explicitly"
    )]
    InsecureUrl(String),
    /// The server URL is not usable.
    #[error("invalid server URL `{url}`: {reason}")]
    InvalidUrl {
        /// The URL as given.
        url: String,
        /// What is wrong with it.
        reason: String,
    },
    /// Downloaded content does not hash to the id it was requested by.
    #[error("downloaded content of {expected} hashes to {actual}")]
    HashMismatch {
        /// The id that was requested.
        expected: ObjectId,
        /// What the content hashes to.
        actual: ObjectId,
    },
    /// The server's answer is not what the API defines.
    #[error("unexpected response from the server: {0}")]
    Protocol(String),
    /// Local I/O, such as writing a download.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// A single sign-on did not complete: the provider or the server refused it, or the
    /// browser did not come back in time.
    #[error("single sign-on failed: {0}")]
    SignIn(String),
}

impl Error {
    /// The server's error, if this is one.
    pub fn api(&self) -> Option<&ApiError> {
        match self {
            Error::Api(e) => Some(e),
            _ => None,
        }
    }

    /// Whether the server rejected the request with `code`.
    pub fn is_code(&self, code: ErrorCode) -> bool {
        self.api().is_some_and(|e| e.is(code))
    }

    /// Whether trying again later may succeed: transport failures and gateway or
    /// availability errors. Never a 4xx.
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Transport(e) => transient_transport(e),
            Error::Api(e) => StatusCode::from_u16(e.status).is_ok_and(retryable_status),
            Error::Io(e) => matches!(
                e.kind(),
                io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::UnexpectedEof
            ),
            _ => false,
        }
    }
}

/// Statuses worth retrying: the server or a proxy in front of it is briefly unavailable.
pub(crate) fn retryable_status(s: StatusCode) -> bool {
    matches!(
        s,
        StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT
    )
}

pub(crate) fn transient_transport(e: &reqwest::Error) -> bool {
    e.is_connect() || e.is_timeout() || e.is_request() || e.is_body()
}

/// Shorthand for client results.
pub type Result<T, E = Error> = std::result::Result<T, E>;
