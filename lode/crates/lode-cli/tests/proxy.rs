//! T154 acceptance: `lode` checkouts through a caching proxy.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use lode_proxy::{BoundProxy, ProxyConfig};
use lode_server::SystemClock;
use lode_testkit::{TestServer, TestWorkspace};

async fn start_proxy(
    upstream: &TestServer,
    dir: &std::path::Path,
) -> (String, tokio::sync::oneshot::Sender<()>) {
    let cfg = ProxyConfig::new(upstream.url(), dir.join("cache"));
    let bound = BoundProxy::bind(&cfg, Arc::new(SystemClock)).await.unwrap();
    let url = format!("http://{}", bound.local_addr());
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(bound.run(async move {
        let _ = rx.await;
    }));
    (url, tx)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_workspace_checks_out_without_content_from_the_server() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let files: Vec<(String, Vec<u8>)> = (0..6)
        .map(|i| {
            (
                format!("dir{}/f{i}.txt", i % 2),
                format!("content {i}\n").repeat(50 + i).into_bytes(),
            )
        })
        .collect();
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, d)| (p.as_str(), d.as_slice()))
        .collect();
    server.commit_files("game", "main", &refs).await;
    let dir = tempfile::tempdir().unwrap();
    let (proxy, _stop) = start_proxy(&server, dir.path()).await;
    let url = format!("{}/game", server.url());

    // The first person sets the proxy with `lode proxy set`.
    let a = TestWorkspace::new(&server).with_token(server.admin_token());
    let v = a.json(&["--server", server.url(), "proxy", "set", &proxy]);
    assert_eq!(v["proxy"], format!("{proxy}/"));
    let v = a.json(&["--server", server.url(), "proxy", "show"]);
    assert_eq!(v["reachable"], true);
    assert_eq!(v["upstream"], format!("{}/", server.url()));
    server.requests().clear();
    a.json(&["checkout", &url]);
    assert_eq!(server.requests().count("GET", "/blobs/"), files.len());

    // The second uses LODE_PROXY; nothing but metadata comes from the server.
    let b = TestWorkspace::new(&server).with_token(server.admin_token());
    server.requests().clear();
    let out = b
        .cmd()
        .env("LODE_PROXY", &proxy)
        .args(["--json", "checkout", &url])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(server.requests().count("GET", "/blobs/"), 0);
    for (p, d) in &files {
        assert_eq!(&b.read(&format!("game/{p}")), d);
    }

    // `status --remote` names the proxy.
    let out = a
        .cmd()
        .current_dir(a.root().join("game"))
        .args(["--json", "status", "--remote"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["remote"]["proxy"], format!("{proxy}/"), "{v}");

    // A server is not accepted as a proxy; clearing goes back to the server.
    let (code, v) = a.json_status(&["--server", server.url(), "proxy", "set", server.url()]);
    assert_eq!(code, 2, "{v}");
    a.json(&["--server", server.url(), "proxy", "clear"]);
    let v = a.json(&["--server", server.url(), "proxy", "show"]);
    assert_eq!(v["proxy"], serde_json::Value::Null);
}

#[tokio::test(flavor = "multi_thread")]
async fn with_the_proxy_down_checkout_warns_and_uses_the_server() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files("game", "main", &[("a.txt", b"one"), ("b.txt", b"two")])
        .await;
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    let out = ws
        .cmd()
        .env("LODE_PROXY", &dead)
        .args(["checkout", &format!("{}/game", server.url())])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.matches("caching proxy").count(), 1, "{stderr}");
    assert_eq!(ws.read("game/b.txt"), b"two");
}
