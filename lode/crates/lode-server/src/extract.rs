//! Extractors whose rejections use the API error shape.

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

fn json_rejection(r: JsonRejection) -> ApiError {
    if r.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return ApiError::new(ErrorCode::TooLarge, "request body is too large");
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
