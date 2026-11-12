//! T010 acceptance: server skeleton.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use http_body_util::BodyExt;
use lode_server::{
    core_routes, finish, init, open_state, ApiJson, AppState, BoundServer, ServerConfig,
    ServerError, SystemClock,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

async fn state_in(dir: &Path, cfg: impl FnOnce(&mut ServerConfig)) -> AppState {
    init(dir).await.unwrap();
    let mut config = ServerConfig::with_data_dir(dir);
    cfg(&mut config);
    open_state(config, Arc::new(SystemClock), Vec::new())
        .await
        .unwrap()
}

async fn call(
    router: Router,
    req: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, headers, json)
}

fn get_req(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn info_reports_version_and_no_modules() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_in(dir.path(), |_| {}).await;
    let (status, headers, body) = call(lode_server::router(state), get_req("/api/v1/info")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["server_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["api_version"], "1");
    assert_eq!(body["modules"], serde_json::json!([]));
    let id = headers.get("x-request-id").expect("request id header");
    assert!(!id.is_empty());
}

#[tokio::test]
async fn request_ids_differ_per_request() {
    let dir = tempfile::tempdir().unwrap();
    let router = lode_server::router(state_in(dir.path(), |_| {}).await);
    let (_, a, _) = call(router.clone(), get_req("/api/v1/info")).await;
    let (_, b, _) = call(router, get_req("/api/v1/info")).await;
    assert_ne!(a.get("x-request-id"), b.get("x-request-id"));
}

#[tokio::test]
async fn unknown_route_is_not_found_json() {
    let dir = tempfile::tempdir().unwrap();
    let router = lode_server::router(state_in(dir.path(), |_| {}).await);
    let (status, headers, body) = call(router.clone(), get_req("/api/v1/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
    assert!(body["message"].is_string());
    assert!(headers.get("x-request-id").is_some());

    let req = Request::delete("/api/v1/info").body(Body::empty()).unwrap();
    let (status, _, body) = call(router, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
}

fn echo_routes() -> Router<AppState> {
    async fn echo(ApiJson(v): ApiJson<serde_json::Value>) -> axum::Json<serde_json::Value> {
        axum::Json(v)
    }
    core_routes().route("/test/echo", post(echo))
}

fn json_post(uri: &str, body: String) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn oversized_json_is_too_large() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_in(dir.path(), |c| c.limits.max_request_json = 64).await;
    let router = finish(echo_routes(), state);

    let small = serde_json::json!({ "a": 1 }).to_string();
    let (status, _, body) = call(router.clone(), json_post("/test/echo", small)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["a"], 1);

    let big = serde_json::json!({ "a": "x".repeat(200) }).to_string();
    let (status, _, body) = call(router, json_post("/test/echo", big)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["code"], "too_large");
}

#[tokio::test]
async fn malformed_json_is_invalid_request() {
    let dir = tempfile::tempdir().unwrap();
    let router = finish(echo_routes(), state_in(dir.path(), |_| {}).await);
    let (status, _, body) = call(router, json_post("/test/echo", "{nope".into())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_request");
}

#[tokio::test]
async fn handler_panic_becomes_internal_without_details() {
    async fn boom() -> &'static str {
        panic!("secret detail 1234");
    }
    let dir = tempfile::tempdir().unwrap();
    let routes = core_routes().route("/test/boom", get(boom));
    let router = finish(routes, state_in(dir.path(), |_| {}).await);
    let (status, headers, body) = call(router, get_req("/test/boom")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["code"], "internal");
    assert!(!body.to_string().contains("secret"));
    assert!(headers.get("x-request-id").is_some());
}

#[tokio::test]
async fn refuses_missing_or_uninitialized_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing");
    let err = open_state(
        ServerConfig::with_data_dir(&missing),
        Arc::new(SystemClock),
        vec![],
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(err, ServerError::DataDirMissing(_)), "{err}");
    assert!(err.to_string().contains("does not exist"));

    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let err = open_state(
        ServerConfig::with_data_dir(&empty),
        Arc::new(SystemClock),
        vec![],
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(err, ServerError::NotInitialized(_)), "{err}");
    assert!(err.to_string().contains("not initialized"));
    // Refusing must not create a database as a side effect.
    assert!(!empty.join("meta.db").exists());
}

#[tokio::test]
async fn init_creates_layout_and_refuses_twice() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let report = init(&data).await.unwrap();
    assert!(data.join("meta.db").is_file());
    assert!(data.join("depots").is_dir());
    let cfg_path = report.config_written.unwrap();
    let cfg = ServerConfig::load(&cfg_path).unwrap();
    assert_eq!(cfg.data_dir, data.join("."));
    let text = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(text.contains("# Lode server configuration"));

    let err = init(&data).await.unwrap_err();
    assert!(matches!(err, ServerError::AlreadyInitialized(_)));
}

async fn raw_get<S>(mut stream: S, path: &str) -> String
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    stream.read_to_end(&mut out).await.unwrap();
    String::from_utf8(out).unwrap()
}

#[tokio::test]
async fn graceful_shutdown_finishes_in_flight_requests() {
    async fn slow() -> &'static str {
        tokio::time::sleep(Duration::from_millis(300)).await;
        "done"
    }
    let dir = tempfile::tempdir().unwrap();
    let state = state_in(dir.path(), |c| c.listen = "127.0.0.1:0".parse().unwrap()).await;
    let config = state.config().clone();
    let router = finish(core_routes().route("/test/slow", get(slow)), state);
    let server = BoundServer::bind(&config, router).await.unwrap();
    let addr = server.local_addr();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let running = tokio::spawn(server.run(async {
        let _ = stop_rx.await;
    }));

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = tokio::spawn(raw_get(stream, "/test/slow"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    stop_tx.send(()).unwrap();

    let response = request.await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("done"));
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("server stops after shutdown")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn tls_mode_serves_https() {
    let dir = tempfile::tempdir().unwrap();
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let cert_pem = cert.cert.pem();
    std::fs::write(dir.path().join("cert.pem"), &cert_pem).unwrap();
    std::fs::write(dir.path().join("key.pem"), cert.signing_key.serialize_pem()).unwrap();

    let data = dir.path().join("data");
    let text =
        "listen = '127.0.0.1:0'\ndata_dir = 'data'\n[tls]\ncert = 'cert.pem'\nkey = 'key.pem'\n";
    let cfg_path = dir.path().join("loded.toml");
    std::fs::write(&cfg_path, text).unwrap();
    init(&data).await.unwrap();
    let config = ServerConfig::load(&cfg_path).unwrap();
    let state = open_state(config.clone(), Arc::new(SystemClock), vec![])
        .await
        .unwrap();
    let server = BoundServer::bind(&config, lode_server::router(state))
        .await
        .unwrap();
    assert_eq!(server.scheme(), "https");
    let addr = server.local_addr();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let running = tokio::spawn(server.run(async {
        let _ = stop_rx.await;
    }));

    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.cert.der().clone()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let tls = connector.connect(name, tcp).await.unwrap();
    let response = raw_get(tls, "/api/v1/info").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"api_version\":\"1\""), "{response}");

    // Plain HTTP to the TLS port gets no HTTP answer.
    let mut plain = tokio::net::TcpStream::connect(addr).await.unwrap();
    plain
        .write_all(b"GET /api/v1/info HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), plain.read_to_end(&mut buf)).await;
    assert!(!String::from_utf8_lossy(&buf).contains("api_version"));

    stop_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
