//! Requests from the admin pages to the API.
//!
//! A page acts by sending an ordinary API request, with the session's token, to the router
//! that answers clients; the answer is the API's JSON or its error. Nothing goes over the
//! network.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use lode_proto::error::ErrorBody;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tower::ServiceExt;

use super::{Admin, Ctx};

/// Largest API answer a page reads.
const MAX_ANSWER: usize = 64 * 1024 * 1024;

/// The API, as one administrator.
pub(crate) struct Api<'a> {
    ctx: &'a Ctx,
    token: &'a str,
}

impl Ctx {
    /// The API as `admin`.
    pub(crate) fn api<'a>(&'a self, admin: &'a Admin) -> Api<'a> {
        Api {
            ctx: self,
            token: admin.token(),
        }
    }
}

fn failed(message: impl Into<String>) -> ErrorBody {
    ErrorBody {
        code: "internal".to_owned(),
        message: message.into(),
        details: serde_json::Value::Null,
    }
}

impl Api<'_> {
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<(StatusCode, bytes::Bytes), ErrorBody> {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token));
        if body.is_some() {
            req = req.header(header::CONTENT_TYPE, "application/json");
        }
        let req = req
            .body(body.map_or_else(Body::empty, Body::from))
            .map_err(|e| failed(format!("cannot build the request: {e}")))?;
        let resp = match self.ctx.api.clone().oneshot(req).await {
            Ok(r) => r,
            Err(never) => match never {},
        };
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), MAX_ANSWER)
            .await
            .map_err(|e| failed(format!("cannot read the answer: {e}")))?;
        if status.is_success() {
            Ok((status, bytes))
        } else {
            Err(serde_json::from_slice::<ErrorBody>(&bytes)
                .unwrap_or_else(|_| failed(format!("the server answered {status}"))))
        }
    }

    async fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<T, ErrorBody> {
        let (_, bytes) = self.send(method, path, body).await?;
        serde_json::from_slice(&bytes).map_err(|e| failed(format!("unexpected answer: {e}")))
    }

    fn encode<B: Serialize>(body: &B) -> Result<Vec<u8>, ErrorBody> {
        serde_json::to_vec(body).map_err(|e| failed(format!("cannot encode the request: {e}")))
    }

    /// `GET path`.
    pub(crate) async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ErrorBody> {
        self.json(Method::GET, path, None).await
    }

    /// `POST path` with a JSON body, answering JSON.
    pub(crate) async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ErrorBody> {
        self.json(Method::POST, path, Some(Self::encode(body)?))
            .await
    }

    /// `POST path` without a body, answering JSON.
    pub(crate) async fn post_empty<T: DeserializeOwned>(&self, path: &str) -> Result<T, ErrorBody> {
        self.json(Method::POST, path, None).await
    }

    /// `POST path` with a JSON body, ignoring the answer's body.
    pub(crate) async fn post_ok<B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), ErrorBody> {
        self.send(Method::POST, path, Some(Self::encode(body)?))
            .await
            .map(|_| ())
    }

    /// `PATCH path` with a JSON body, ignoring the answer's body.
    pub(crate) async fn patch<B: Serialize>(&self, path: &str, body: &B) -> Result<(), ErrorBody> {
        self.send(Method::PATCH, path, Some(Self::encode(body)?))
            .await
            .map(|_| ())
    }

    /// `PUT path` without a body.
    pub(crate) async fn put(&self, path: &str) -> Result<(), ErrorBody> {
        self.send(Method::PUT, path, None).await.map(|_| ())
    }

    /// `DELETE path`.
    pub(crate) async fn delete(&self, path: &str) -> Result<(), ErrorBody> {
        self.send(Method::DELETE, path, None).await.map(|_| ())
    }

    /// `DELETE path` with a JSON body, answering JSON.
    pub(crate) async fn delete_with<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ErrorBody> {
        self.json(Method::DELETE, path, Some(Self::encode(body)?))
            .await
    }
}

/// An API path from a template such as `/api/v1/depots/{d}/branches`, with each `{…}`
/// replaced by the next value, percent-encoded.
pub(crate) fn path(template: &str, values: &[&str]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut values = values.iter();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        if let Some(v) = values.next() {
            out.push_str(&super::html::encode(v));
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_filled_and_encoded() {
        assert_eq!(
            path(lode_proto::consts::paths::BRANCH, &["game", "feature/x y"]),
            "/api/v1/depots/game/branches/feature%2Fx%20y"
        );
        assert_eq!(path("/a/{x}", &[]), "/a/");
    }
}
