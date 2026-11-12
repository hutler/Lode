//! T011 acceptance: token values never appear in logs or in responses other than creation.
//!
//! In its own test binary: a thread-scoped tracing subscriber misses events when other tests
//! register the same callsites concurrently without one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use http_body_util::BodyExt;
use lode_server::{init, open_state, ClientInfo, ManualClock, ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn send(
    router: &axum::Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (Value, String) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    let mut req = match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let addr: SocketAddr = "10.0.0.1:5000".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(ClientInfo(addr)));
    let resp = router.clone().oneshot(req).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (serde_json::from_slice(&bytes).unwrap_or(Value::Null), text)
}

/// Collects everything the server logs.
#[derive(Clone, Default)]
struct LogSink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn token_values_never_leak() {
    let sink = LogSink::default();
    let writer = sink.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let dir = tempfile::tempdir().unwrap();
    let admin = init(dir.path()).await.unwrap().admin_token;
    let state = open_state(
        ServerConfig::with_data_dir(dir.path()),
        Arc::new(ManualClock::new(1_800_000_000_000)),
        vec![],
    )
    .await
    .unwrap();
    let r = lode_server::router(state);

    let user = json!({ "kind": "user", "name": "ana", "password": "correct horse" });
    send(
        &r,
        "POST",
        "/api/v1/admin/principals",
        Some(&admin),
        Some(user),
    )
    .await;
    let creds = json!({ "name": "ana", "password": "correct horse" });
    let (login, _) = send(&r, "POST", "/api/v1/auth/login", None, Some(creds)).await;
    let ana = login["token"].as_str().unwrap().to_owned();
    let req = json!({ "name": "t", "scopes": ["read"] });
    let (created, _) = send(&r, "POST", "/api/v1/tokens", Some(&ana), Some(req)).await;
    let minted = created["token"].as_str().unwrap().to_owned();
    let mut seen = String::new();
    for t in [&admin, &ana, &minted] {
        seen.push_str(
            &send(&r, "GET", "/api/v1/auth/whoami", Some(t), None)
                .await
                .1,
        );
        seen.push_str(&send(&r, "GET", "/api/v1/tokens", Some(t), None).await.1);
    }
    seen.push_str(
        &send(&r, "GET", "/api/v1/admin/principals", Some(&admin), None)
            .await
            .1,
    );
    send(
        &r,
        "GET",
        "/api/v1/auth/whoami",
        Some("lode_wrongwrongwrong"),
        None,
    )
    .await;

    let logs = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("request"),
        "the subscriber captured nothing: {logs}"
    );
    for t in [&admin, &ana, &minted] {
        assert!(!logs.contains(t.as_str()), "token in logs");
        assert!(
            !seen.contains(t.as_str()),
            "token in a non-creation response"
        );
    }
    assert!(!logs.contains("lode_wrongwrongwrong"));
    assert!(!logs.contains("correct horse"));
}
