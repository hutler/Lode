//! Extractors whose rejections use the API error shape.

use std::error::Error;

use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::StatusCode;
use lode_core::ErrorCode;
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// Like `axum::Json`, but a bad body is answered with `invalid_request` and an oversized one
/// with `too_large`, both in the usual error shape.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiJson<T>(pub T);

impl<T, S> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(v)) => Ok(Self(v)),
            Err(rejection) => Err(json_rejection(rejection)),
        }
    }
}

/// Decode a tool's arguments with the same field paths as typed HTTP JSON bodies.
pub fn tool_input<T: DeserializeOwned>(input: serde_json::Value) -> Result<T, ApiError> {
    serde_path_to_error::deserialize(input).map_err(|e| input_error(&e))
}

type InputError = serde_path_to_error::Error<serde_json::Error>;

fn input_error(error: &InputError) -> ApiError {
    use serde_path_to_error::Segment::{Map, Seq};

    // Serde knows which enum variants are valid for a supplied value. When the field is
    // absent it reports only the field name, so supply the choices for a shelf/commit action.
    let mut path = error.path().iter();
    let (Some(Map { key }), Some(Seq { index })) = (path.next(), path.next()) else {
        return ApiError::invalid_request(error.to_string());
    };
    let reason = match (key.as_str(), path.next()) {
        ("edits" | "changes", None)
            if error
                .inner()
                .to_string()
                .starts_with("missing field `action`") =>
        {
            "missing required field"
        }
        ("edits" | "changes", Some(Map { key })) if key == "action" => "invalid field",
        _ => return ApiError::invalid_request(error.to_string()),
    };
    ApiError::invalid_request(format!(
        r#"{key}[{index}]: {reason} "action" (one of: {})"#,
        lode_core::ChangeAction::NAMES.join(", ")
    ))
}

fn json_rejection(r: JsonRejection) -> ApiError {
    if r.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return ApiError::new(ErrorCode::TooLarge, "request body is too large");
    }
    if let JsonRejection::JsonDataError(ref data) = r {
        let mut source = data.source();
        while let Some(err) = source {
            if let Some(input) = err.downcast_ref::<InputError>() {
                return input_error(input);
            }
            source = err.source();
        }
    }
    ApiError::invalid_request(r.body_text())
}

/// Like `axum::extract::Path`, but a malformed segment is answered with `invalid_request`
/// in the usual error shape.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiPath<T>(pub T);

impl<T, S> FromRequestParts<S> for ApiPath<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Path::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Path(v)) => Ok(Self(v)),
            Err(r) => Err(path_rejection(r)),
        }
    }
}

fn path_rejection(r: PathRejection) -> ApiError {
    ApiError::invalid_request(r.body_text())
}

/// The query string as decoded `(key, value)` pairs, keeping repeated keys such as
/// `include=a&include=b`.
#[derive(Debug, Clone, Default)]
pub struct ApiQuery(pub Vec<(String, String)>);

impl<S: Send + Sync> FromRequestParts<S> for ApiQuery {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let raw = parts.uri.query().unwrap_or("");
        let mut out = Vec::new();
        for pair in raw.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            out.push((percent_decode(k)?, percent_decode(v)?));
        }
        Ok(Self(out))
    }
}

fn percent_decode(s: &str) -> Result<String, ApiError> {
    let bad = || ApiError::invalid_request("malformed query string");
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = s.get(i + 1..i + 3).ok_or_else(bad)?;
                out.push(u8::from_str_radix(hex, 16).map_err(|_| bad())?);
                i += 2;
            }
            _ => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).map_err(|_| bad())
}

impl ApiQuery {
    /// The last value of `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Every value of `key`, in order.
    pub fn all(&self, key: &str) -> Vec<String> {
        self.0
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .collect()
    }

    /// `key` as a number, if present.
    pub fn u64(&self, key: &str) -> Result<Option<u64>, ApiError> {
        self.get(key)
            .map(|v| {
                v.parse()
                    .map_err(|_| ApiError::invalid_request(format!("`{key}` must be a number")))
            })
            .transpose()
    }

    /// `key` as a flag: absent, `false` or `0` is false; empty, `true` or `1` is true.
    pub fn flag(&self, key: &str) -> Result<bool, ApiError> {
        match self.get(key) {
            None | Some("false" | "0") => Ok(false),
            Some("" | "true" | "1") => Ok(true),
            Some(_) => Err(ApiError::invalid_request(format!(
                "`{key}` must be true or false"
            ))),
        }
    }

    /// `key` as a depot path, if present.
    pub fn path(&self, key: &str) -> Result<Option<lode_core::DepotPath>, ApiError> {
        self.get(key)
            .map(|v| {
                lode_core::DepotPath::parse(v)
                    .map_err(|e| ApiError::new(ErrorCode::InvalidPath, format!("`{key}`: {e}")))
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_and_keeps_repeats() {
        assert_eq!(percent_decode("a%2Fb+c").ok().as_deref(), Some("a/b c"));
        assert!(percent_decode("%zz").is_err());
        assert!(percent_decode("%4").is_err());
        let q = ApiQuery(vec![
            ("include".into(), "a/**".into()),
            ("include".into(), "b/**".into()),
            ("rev".into(), "7".into()),
        ]);
        assert_eq!(q.all("include"), vec!["a/**", "b/**"]);
        assert_eq!(q.u64("rev").ok().flatten(), Some(7));
        assert!(q.flag("own").is_ok_and(|f| !f));
    }
}
