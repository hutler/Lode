//! The one error type every handler returns, and its JSON rendering.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use lode_core::ErrorCode;
use lode_proto::error::ErrorBody;

/// An error answered to the client as `{ code, message, details }` with the code's status.
///
/// Core codes come from [`ErrorCode`]; modules can add their own with [`ApiError::custom`].
#[derive(Debug, Clone)]
pub struct ApiError {
    status: StatusCode,
    body: ErrorBody,
}

impl ApiError {
    /// An error with a core code.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::from_u16(code.http_status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            body: ErrorBody::new(code, message),
        }
    }

    /// An error with a code defined by a module, such as `checks_required`.
    pub fn custom(code: &str, status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ErrorBody {
                code: code.to_owned(),
                message: message.into(),
                details: serde_json::Value::Null,
            },
        }
    }

    /// Adds code-specific details.
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.body.details = details;
        self
    }

    /// `not_found`.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    /// `invalid_request`.
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidRequest, message)
    }

    /// `internal`. The message is fixed so nothing about the failure reaches the client;
    /// the cause belongs in the server log.
    pub fn internal() -> Self {
        Self::new(ErrorCode::Internal, "internal server error")
    }

    /// The HTTP status.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The body sent to the client.
    pub fn body(&self) -> &ErrorBody {
        &self.body
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.body.code, self.body.message)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

impl From<lode_meta::MetaError> for ApiError {
    fn from(e: lode_meta::MetaError) -> Self {
        tracing::error!(error = %e, "metadata store error");
        Self::internal()
    }
}

impl From<lode_store::StoreError> for ApiError {
    fn from(e: lode_store::StoreError) -> Self {
        use lode_store::StoreError as E;
        match &e {
            E::Io(io) if io.kind() == std::io::ErrorKind::InvalidData => {
                ApiError::invalid_request(format!("invalid request body: {io}"))
            }
            E::Io(io) if io.kind() == std::io::ErrorKind::ConnectionAborted => {
                ApiError::invalid_request("the request body ended early")
            }
            _ => {
                let code = e.code();
                if code == ErrorCode::Internal {
                    tracing::error!(error = %e, "blob store error");
                    ApiError::internal()
                } else {
                    ApiError::new(code, e.to_string())
                }
            }
        }
    }
}

/// Result type for handlers.
pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_core_code_maps_to_its_status() {
        let cases = [
            (ErrorCode::Unauthenticated, 401),
            (ErrorCode::PermissionDenied, 403),
            (ErrorCode::NotFound, 404),
            (ErrorCode::InvalidRequest, 400),
            (ErrorCode::InvalidPath, 400),
            (ErrorCode::OutOfDate, 409),
            (ErrorCode::Conflict, 409),
            (ErrorCode::Locked, 409),
            (ErrorCode::LockRequired, 409),
            (ErrorCode::MissingObjects, 409),
            (ErrorCode::BranchArchived, 409),
            (ErrorCode::HashMismatch, 422),
            (ErrorCode::TooLarge, 413),
            (ErrorCode::Internal, 500),
            (ErrorCode::ChecksRequired, 409),
            (ErrorCode::Obliterated, 410),
        ];
        for (code, status) in cases {
            let e = ApiError::new(code, "m");
            assert_eq!(e.status().as_u16(), status, "{code}");
            assert_eq!(e.body().code, code.as_str());
        }
    }
}
