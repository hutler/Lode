//! T178 acceptance: `lode doctor` names which step on the way to a server fails.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_testkit::{TestServer, TestWorkspace};
use serde_json::Value;

fn status<'a>(v: &'a Value, name: &str) -> &'a str {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no {name} in {v}"))["status"]
        .as_str()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_reports_each_step() {
    let server = TestServer::start().await;
    let url = server.url().to_owned();

    // Signed in: everything answers.
    let ws = TestWorkspace::detached().with_token(server.admin_token());
    let (code, v) = ws.json_status(&["doctor", &url]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["status"], "ok");
    assert_eq!(status(&v, "dns"), "skip");
    assert_eq!(status(&v, "tls"), "skip");
    assert_eq!(status(&v, "info"), "ok");
    assert_eq!(status(&v, "versions"), "ok");
    assert_eq!(status(&v, "identity"), "ok");

    // Not signed in: a warning, exit 2.
    let anon = TestWorkspace::detached();
    let (code, v) = anon.json_status(&["doctor", &url]);
    assert_eq!(code, 2, "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(status(&v, "identity"), "warn");

    // A token the server does not know.
    let bad = TestWorkspace::detached().with_token("lode_nope");
    let (code, v) = bad.json_status(&["doctor", &url]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["ok"], false);
    assert_eq!(status(&v, "identity"), "fail");

    // Nothing listens: the request fails, the later steps are skipped.
    let (code, v) = anon.json_status(&["doctor", "http://127.0.0.1:1"]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(status(&v, "info"), "fail");
    assert_eq!(status(&v, "identity"), "skip");

    // Human output: one line per check.
    let out = anon.cmd().args(["doctor", &url]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.lines().any(|l| l.starts_with("ok    info")), "{text}");
    assert!(
        text.lines().any(|l| l.starts_with("warn  identity")),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_certificate_fails_the_tls_step() {
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let mut params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(2021, 1, 1);
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    std::fs::write(dir.path().join("cert.pem"), cert.pem()).unwrap();
    std::fs::write(dir.path().join("key.pem"), key.serialize_pem()).unwrap();
    let cfg = dir.path().join("loded.toml");
    std::fs::write(
        &cfg,
        "listen = '127.0.0.1:0'\ndata_dir = 'data'\n[tls]\ncert = 'cert.pem'\nkey = 'key.pem'\n",
    )
    .unwrap();
    lode_server::init(&dir.path().join("data")).await.unwrap();
    let config = lode_server::ServerConfig::load(&cfg).unwrap();
    let state = lode_server::open_state(config.clone(), Arc::new(lode_server::SystemClock), vec![])
        .await
        .unwrap();
    let server = lode_server::BoundServer::bind(&config, lode_server::router(state))
        .await
        .unwrap();
    let port = server.local_addr().port();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let running = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));

    let ws = TestWorkspace::detached();
    let (code, v) = ws.json_status(&["doctor", &format!("https://localhost:{port}")]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(status(&v, "dns"), "ok", "{v}");
    assert_eq!(status(&v, "tls"), "fail", "{v}");
    assert_eq!(status(&v, "info"), "skip", "{v}");
    let _ = stop.send(());
    let _ = running.await;
}
