//! The error body every endpoint uses.

use lode_core::ErrorCode;
use serde::{Deserialize, Serialize};

/// `{ "code": "out_of_date", "message": "…", "details": { … } }`.
///
/// `code` is kept as a string so a client can carry codes added by modules it does not
/// know; [`ErrorBody::known_code`] maps it to [`ErrorCode`] when possible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Stable machine-readable code.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
    /// Code-specific data, such as the paths that are out of date.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub details: serde_json::Value,
}

impl ErrorBody {
    /// A body with no details.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code.as_str().to_owned(),
            message: message.into(),
            details: serde_json::Value::Null,
        }
    }

    /// Adds details.
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = details;
        self
    }

    /// The code, if it is one of the core codes.
    pub fn known_code(&self) -> Option<ErrorCode> {
        self.code.parse().ok()
    }
}
