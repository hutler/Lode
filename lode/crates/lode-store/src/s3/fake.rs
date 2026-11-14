//! An in-process fake of the part of S3 that [`super::S3BlobStore`] uses, for tests
//! (feature `test-util`).
//!
//! Path-style only. It checks every request's Signature Version 4 with the credentials it
//! was started with, and serves: bucket `HEAD` and `PUT`, ListObjectsV2 with continuation
//! tokens (page size set with [`FakeS3::set_page_size`]), and object `PUT` (honouring
//! `If-None-Match: *` atomically), `GET`, `HEAD` and `DELETE`. [`FakeS3::fail_next`] makes
//! the next requests answer `503 SlowDown`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use axum::body::{to_bytes, Body};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use bytes::Bytes;
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use super::sigv4::{self, Credentials, UNSIGNED_PAYLOAD};

#[derive(Clone)]
struct Object {
    data: Bytes,
    modified: SystemTime,
}

struct Inner {
    creds: Credentials,
    region: String,
    buckets: HashMap<String, BTreeMap<String, Object>>,
    page_size: usize,
    fail_next: u32,
    requests: u64,
}

/// A running fake S3 endpoint. Stops when dropped.
pub struct FakeS3 {
    url: String,
    inner: Arc<Mutex<Inner>>,
    _stop: oneshot::Sender<()>,
}

impl FakeS3 {
    /// Starts on a free loopback port with one empty bucket.
    pub async fn start(creds: Credentials, region: &str, bucket: &str) -> Self {
        let mut buckets = HashMap::new();
        buckets.insert(bucket.to_owned(), BTreeMap::new());
        let inner = Arc::new(Mutex::new(Inner {
            creds,
            region: region.to_owned(),
            buckets,
            page_size: 1000,
            fail_next: 0,
            requests: 0,
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("address");
        let app = Router::new()
            .fallback(handle)
            .layer(axum::extract::DefaultBodyLimit::disable())
            .with_state(inner.clone());
        let (tx, rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await;
        });
        Self {
            url: format!("http://{addr}"),
            inner,
            _stop: tx,
        }
    }

    /// `http://127.0.0.1:<port>`, the endpoint to configure.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The most keys one listing page returns (S3's own limit is 1000).
    pub fn set_page_size(&self, n: usize) {
        self.inner.lock().unwrap().page_size = n.max(1);
    }

    /// Answers the next `n` requests with `503 SlowDown`.
    pub fn fail_next(&self, n: u32) {
        self.inner.lock().unwrap().fail_next = n;
    }

    /// Requests received so far, refused ones included.
    pub fn requests(&self) -> u64 {
        self.inner.lock().unwrap().requests
    }

    /// Every key in a bucket, sorted.
    pub fn keys(&self, bucket: &str) -> Vec<String> {
        let g = self.inner.lock().unwrap();
        g.buckets
            .get(bucket)
            .map(|b| b.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// An object's bytes, read directly.
    pub fn get_raw(&self, bucket: &str, key: &str) -> Option<Vec<u8>> {
        let g = self.inner.lock().unwrap();
        g.buckets.get(bucket)?.get(key).map(|o| o.data.to_vec())
    }

    /// Removes an object directly.
    pub fn remove_raw(&self, bucket: &str, key: &str) -> bool {
        let mut g = self.inner.lock().unwrap();
        g.buckets
            .get_mut(bucket)
            .is_some_and(|b| b.remove(key).is_some())
    }

    /// Writes an object directly, replacing any.
    pub fn put_raw(&self, bucket: &str, key: &str, data: Vec<u8>) {
        let mut g = self.inner.lock().unwrap();
        g.buckets.entry(bucket.to_owned()).or_default().insert(
            key.to_owned(),
            Object {
                data: data.into(),
                modified: SystemTime::now(),
            },
        );
    }
}

fn s3_error(status: StatusCode, code: &str) -> Response {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>{code}</Code><Message>{code}</Message></Error>"
    );
    (status, [("content-type", "application/xml")], body).into_response()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn rfc3339(t: SystemTime) -> String {
    time::OffsetDateTime::from(t)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Checks the `Authorization` header against the request as received.
fn signature_ok(
    inner: &Inner,
    method: &Method,
    path: &str,
    query: &[(String, String)],
    headers: &HeaderMap,
    body: &[u8],
) -> bool {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let Some(auth) = header("authorization") else {
        return false;
    };
    let Some(signed) = auth
        .split(", ")
        .find_map(|p| p.strip_prefix("SignedHeaders="))
        .or_else(|| {
            auth.split(',')
                .find_map(|p| p.trim().strip_prefix("SignedHeaders="))
        })
    else {
        return false;
    };
    let (Some(payload_hash), Some(amz_date)) =
        (header("x-amz-content-sha256"), header("x-amz-date"))
    else {
        return false;
    };
    if payload_hash != UNSIGNED_PAYLOAD && hex::encode(Sha256::digest(body)) != payload_hash {
        return false;
    }
    let mut signed_headers = Vec::new();
    for name in signed.split(';') {
        let Some(v) = header(name) else {
            return false;
        };
        signed_headers.push((name, v));
    }
    if !signed_headers.iter().any(|(n, _)| *n == "host") {
        return false;
    }
    let q: Vec<(&str, &str)> = query
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let want = sigv4::authorization(
        &inner.creds,
        &sigv4::Request {
            method: method.as_str(),
            path,
            query: &q,
            headers: &signed_headers,
            payload_hash,
            amz_date,
            region: &inner.region,
            service: "s3",
        },
    );
    want == auth
}

/// `%XX`-decodes a path segment (S3 keys are UTF-8).
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

async fn handle(State(state): State<Arc<Mutex<Inner>>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let Ok(body) = to_bytes(body, usize::MAX).await else {
        return s3_error(StatusCode::BAD_REQUEST, "IncompleteBody");
    };
    let path = parts.uri.path().to_owned();
    let query: Vec<(String, String)> = match parts.uri.query() {
        Some(q) => reqwest::Url::parse(&format!("http://x/?{q}"))
            .map(|u| {
                u.query_pairs()
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect()
            })
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let mut g = state.lock().unwrap();
    g.requests += 1;
    if g.fail_next > 0 {
        g.fail_next -= 1;
        return s3_error(StatusCode::SERVICE_UNAVAILABLE, "SlowDown");
    }
    if !signature_ok(&g, &parts.method, &path, &query, &parts.headers, &body) {
        return s3_error(StatusCode::FORBIDDEN, "SignatureDoesNotMatch");
    }
    let Some(rest) = path.strip_prefix('/') else {
        return s3_error(StatusCode::BAD_REQUEST, "InvalidURI");
    };
    let (bucket, key) = match rest.split_once('/') {
        Some((b, k)) => (b.to_owned(), Some(k)),
        None => (rest.to_owned(), None),
    };
    let key = match key.map(percent_decode) {
        Some(Some(k)) if !k.is_empty() => Some(k),
        Some(Some(_)) | None => None,
        Some(None) => return s3_error(StatusCode::BAD_REQUEST, "InvalidURI"),
    };
    let page_size = g.page_size;
    let method = parts.method.clone();
    match key {
        None => match method {
            Method::HEAD if g.buckets.contains_key(&bucket) => StatusCode::OK.into_response(),
            Method::HEAD => StatusCode::NOT_FOUND.into_response(),
            Method::PUT if g.buckets.contains_key(&bucket) => {
                s3_error(StatusCode::CONFLICT, "BucketAlreadyOwnedByYou")
            }
            Method::PUT => {
                g.buckets.insert(bucket, BTreeMap::new());
                StatusCode::OK.into_response()
            }
            Method::GET => {
                let Some(objects) = g.buckets.get(&bucket) else {
                    return s3_error(StatusCode::NOT_FOUND, "NoSuchBucket");
                };
                let get = |name: &str| {
                    query
                        .iter()
                        .find(|(k, _)| k == name)
                        .map(|(_, v)| v.as_str())
                };
                if get("list-type") != Some("2") {
                    return s3_error(StatusCode::NOT_IMPLEMENTED, "NotImplemented");
                }
                let prefix = get("prefix").unwrap_or("");
                let max = get("max-keys")
                    .and_then(|m| m.parse::<usize>().ok())
                    .unwrap_or(1000)
                    .min(page_size);
                let after = get("continuation-token").unwrap_or("");
                let mut items = objects
                    .range::<str, _>((std::ops::Bound::Excluded(after), std::ops::Bound::Unbounded))
                    .filter(|(k, _)| k.starts_with(prefix))
                    .take(max + 1)
                    .collect::<Vec<_>>();
                let truncated = items.len() > max;
                items.truncate(max);
                let mut xml = String::from(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
                );
                xml.push_str(&format!(
                    "<Name>{}</Name><Prefix>{}</Prefix><KeyCount>{}</KeyCount><MaxKeys>{max}</MaxKeys><IsTruncated>{truncated}</IsTruncated>",
                    xml_escape(&bucket),
                    xml_escape(prefix),
                    items.len()
                ));
                for (k, o) in &items {
                    xml.push_str(&format!(
                        "<Contents><Key>{}</Key><LastModified>{}</LastModified><Size>{}</Size></Contents>",
                        xml_escape(k),
                        rfc3339(o.modified),
                        o.data.len()
                    ));
                }
                if truncated {
                    if let Some((last, _)) = items.last() {
                        xml.push_str(&format!(
                            "<NextContinuationToken>{}</NextContinuationToken>",
                            xml_escape(last)
                        ));
                    }
                }
                xml.push_str("</ListBucketResult>");
                (StatusCode::OK, [("content-type", "application/xml")], xml).into_response()
            }
            _ => s3_error(StatusCode::METHOD_NOT_ALLOWED, "MethodNotAllowed"),
        },
        Some(key) => {
            let Some(objects) = g.buckets.get_mut(&bucket) else {
                return s3_error(StatusCode::NOT_FOUND, "NoSuchBucket");
            };
            match method {
                Method::PUT => {
                    let conditional = parts
                        .headers
                        .get("if-none-match")
                        .is_some_and(|v| v.as_bytes() == b"*");
                    if conditional && objects.contains_key(&key) {
                        return s3_error(StatusCode::PRECONDITION_FAILED, "PreconditionFailed");
                    }
                    objects.insert(
                        key,
                        Object {
                            data: body,
                            modified: SystemTime::now(),
                        },
                    );
                    StatusCode::OK.into_response()
                }
                Method::GET => match objects.get(&key) {
                    Some(o) => (StatusCode::OK, Body::from(o.data.clone())).into_response(),
                    None => s3_error(StatusCode::NOT_FOUND, "NoSuchKey"),
                },
                Method::HEAD => match objects.get(&key) {
                    Some(o) => (
                        StatusCode::OK,
                        [("content-length", o.data.len().to_string())],
                    )
                        .into_response(),
                    None => StatusCode::NOT_FOUND.into_response(),
                },
                Method::DELETE => {
                    objects.remove(&key);
                    StatusCode::NO_CONTENT.into_response()
                }
                _ => s3_error(StatusCode::METHOD_NOT_ALLOWED, "MethodNotAllowed"),
            }
        }
    }
}
