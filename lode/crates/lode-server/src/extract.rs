//! Extractors whose rejections use the API error shape.

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request};
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
