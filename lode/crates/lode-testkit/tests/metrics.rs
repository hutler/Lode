//! T181 acceptance: optional Prometheus metrics at `/metrics`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_core::{blob_id, DepotPath};
use lode_proto::locks::{LockPath, LockRequest};
use lode_server::metrics::check_text_format;
use lode_testkit::{TestServer, TestServerOptions};

async fn get(s: &TestServer, token: Option<&str>) -> (u16, String, String) {
    let url = format!("{}/metrics", s.url().trim_end_matches('/'));
    let mut req = reqwest::Client::new().get(url);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status().as_u16();
    let kind = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    (status, kind, resp.text().await.unwrap())
}

/// The value of the line that starts with `series ` (a name with its labels).
fn value(text: &str, series: &str) -> f64 {
    text.lines()
        .find_map(|l| l.strip_prefix(&format!("{series} ")))
        .unwrap_or_else(|| panic!("no {series} in\n{text}"))
        .parse()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn off_by_default() {
    let s = TestServer::start().await;
    let (status, _, _) = get(&s, Some(s.admin_token())).await;
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_token_with_the_metrics_scope_reads_them() {
    let s = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| c.metrics.enabled = true),
    })
    .await;
    s.create_depot("game").await;
    let content = b"a texture".as_slice();
    let rev = s
        .commit_files("game", "main", &[("hero.psd", content)])
        .await;
    let locker = s
        .token_for("admin", &["read", "write", "admin", "lock"])
        .await;
    s.client(&locker)
        .lock(
            "game",
            &LockRequest {
                branch: "main".into(),
                workspace: "ws".into(),
                comment: None,
                paths: vec![LockPath {
                    path: DepotPath::parse("hero.psd").unwrap(),
                    base_id: blob_id(content).unwrap(),
                }],
            },
        )
        .await
        .unwrap();

    // Without a token, or with one that lacks the scope: refused.
    let (status, _, _) = get(&s, None).await;
    assert_eq!(status, 401);
    let (status, _, _) = get(&s, Some(s.admin_token())).await;
    assert_eq!(status, 403);

    let reader = s.token_for("admin", &["metrics"]).await;
    let (status, kind, text) = get(&s, Some(&reader)).await;
    assert_eq!(status, 200, "{text}");
    assert!(kind.starts_with("text/plain; version=0.0.4"), "{kind}");
    let samples = check_text_format(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert!(samples > 5, "{text}");
    assert_eq!(value(&text, "lode_depots"), 1.0);
    assert_eq!(
        value(&text, "lode_depot_head_revision{depot=\"game\"}"),
        rev as f64
    );
    assert!(value(&text, "lode_commits_total") >= 1.0, "{text}");
    assert!(value(&text, "lode_blob_bytes_received_total") >= content.len() as f64);
    assert_eq!(value(&text, "lode_locks_held{depot=\"game\"}"), 1.0);
    assert!(text.contains("lode_http_requests_total{class=\"commit\",code=\"2"));
    assert!(text.contains("lode_build_info{version="));
    // Identifiers only: no file names.
    assert!(!text.contains("hero.psd"));

    // The refused requests were counted too.
    let (_, _, text) = get(&s, Some(&reader)).await;
    assert_eq!(
        value(
            &text,
            "lode_http_requests_total{class=\"metrics\",code=\"401\"}"
        ),
        1.0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allowed_addresses_need_no_token() {
    let s = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| {
            c.metrics.enabled = true;
            c.metrics.allow_from = vec!["127.0.0.0/8".parse().unwrap()];
        }),
    })
    .await;
    let (status, _, text) = get(&s, None).await;
    assert_eq!(status, 200, "{text}");
    check_text_format(&text).unwrap();

    let s = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| {
            c.metrics.enabled = true;
            c.metrics.allow_from = vec!["10.0.0.0/8".parse().unwrap()];
        }),
    })
    .await;
    assert_eq!(get(&s, None).await.0, 401);
}
