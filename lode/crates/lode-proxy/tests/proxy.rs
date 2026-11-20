//! T154 acceptance: the caching proxy in front of an in-process server.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lode_client::{Client, DownloadOpts};
use lode_core::{blob_id, ChunkingHasher, DepotPath, ObjectId};
use lode_proto::admin::{AccessLevel, AclEffect, ObliterateRequest, ObliterateTarget};
use lode_proxy::cache::Key;
use lode_proxy::{BoundProxy, ProxyConfig, ProxyState};
use lode_server::ManualClock;
use lode_testkit::TestServer;

struct Proxy {
    url: String,
    state: ProxyState,
    clock: ManualClock,
    _stop: tokio::sync::oneshot::Sender<()>,
    _dir: tempfile::TempDir,
}

async fn proxy(upstream: &TestServer, max_bytes: u64) -> Proxy {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = ProxyConfig::new(upstream.url(), dir.path().join("cache"));
    cfg.cache_max_bytes = max_bytes;
    let clock = ManualClock::new(1_800_000_000_000);
    let bound = BoundProxy::bind(&cfg, Arc::new(clock.clone()))
        .await
        .unwrap();
    let url = format!("http://{}", bound.local_addr());
    let state = bound.state().clone();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(bound.run(async move {
        let _ = rx.await;
    }));
    Proxy {
        url,
        state,
        clock,
        _stop: tx,
        _dir: dir,
    }
}

/// A client of `server` whose content downloads go through `proxy`.
fn via(server: &TestServer, proxy: &str, token: &str) -> Client {
    Client::builder(server.url())
        .proxy(Some(proxy.to_owned()))
        .build()
        .unwrap()
        .with_token(token)
}

async fn get(c: &Client, id: &ObjectId) -> lode_client::Result<Vec<u8>> {
    let mut out = Vec::new();
    c.get_blob_to("game", id, &mut out).await?;
    Ok(out)
}

fn upstream_content(s: &TestServer) -> usize {
    s.requests().count("GET", "/blobs/")
}

fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(len);
    out
}

#[tokio::test]
async fn content_is_fetched_once_and_info_names_the_proxy() {
    let s = TestServer::start().await;
    s.create_depot("game").await;
    let files: Vec<(String, Vec<u8>)> = (0..5)
        .map(|i| {
            (
                format!("f{i}.txt"),
                format!("file {i} ").repeat(100 + i).into_bytes(),
            )
        })
        .collect();
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, d)| (p.as_str(), d.as_slice()))
        .collect();
    s.commit_files("game", "main", &refs).await;
    let p = proxy(&s, 1 << 30).await;
    s.requests().clear();

    // Two "workspaces" download everything; only the first reaches the server for content.
    for round in 0..2 {
        let c = via(&s, &p.url, s.admin_token());
        for (_, data) in &files {
            let id = blob_id(data.as_slice()).unwrap();
            assert_eq!(&get(&c, &id).await.unwrap(), data);
        }
        assert_eq!(upstream_content(&s), files.len(), "round {round}");
    }
    let m = p.state.metrics();
    assert_eq!((m.hits, m.misses), (5, 5));
    assert!(m.bytes_saved > 0 && m.cache_entries == 5);

    // Concurrent requests for one object share a fetch.
    let data = b"wanted by many".repeat(50);
    s.commit_files("game", "main", &[("many.txt", &data)]).await;
    let id = blob_id(data.as_slice()).unwrap();
    s.requests().clear();
    let c = via(&s, &p.url, s.admin_token());
    let all = futures::future::join_all((0..8).map(|_| get(&c, &id))).await;
    assert!(all.iter().all(|r| r.as_ref().unwrap() == &data));
    assert_eq!(upstream_content(&s), 1);

    // Ranges are served from the cache.
    let resp = reqwest::Client::new()
        .get(format!("{}/api/v1/depots/game/blobs/{id}", p.url))
        .bearer_auth(s.admin_token())
        .header("range", "bytes=5-9")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 206);
    assert_eq!(&resp.bytes().await.unwrap()[..], &data[5..10]);

    // /info is the upstream's plus the proxy; other endpoints point at the server.
    let info = Client::builder(&p.url)
        .build()
        .unwrap()
        .info()
        .await
        .unwrap();
    assert!(info.chunking.is_some());
    assert_eq!(info.proxy.unwrap().upstream, format!("{}/", s.url()));
    let err = Client::builder(&p.url)
        .build()
        .unwrap()
        .with_token(s.admin_token())
        .depots()
        .await
        .unwrap_err();
    assert!(err.to_string().contains("caching proxy"), "{err}");
    let page = reqwest::get(format!("{}/admin/", p.url))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("Hit rate"), "{page}");
}

#[tokio::test]
async fn access_stays_with_the_server() {
    let s = TestServer::start().await;
    s.create_depot("game").await;
    s.commit_files("game", "main", &[("a.txt", b"secret stuff")])
        .await;
    let id = blob_id(&b"secret stuff"[..]).unwrap();
    let p = proxy(&s, 1 << 30).await;
    // Warm the cache with someone who may read it.
    get(&via(&s, &p.url, s.admin_token()), &id).await.unwrap();

    let bob = s.create_user("bob", &["read"]).await;
    let err = get(&via(&s, &p.url, &bob), &id).await.unwrap_err();
    assert_eq!(err.api().map(|a| a.status), Some(404), "{err}");
    let err = get(&via(&s, &p.url, "not-a-token"), &id).await.unwrap_err();
    assert_eq!(err.api().map(|a| a.status), Some(401), "{err}");

    let carol = s.create_user("carol", &["read"]).await;
    let rule = s
        .acl(
            "game",
            "carol",
            "**",
            "**",
            AclEffect::Allow,
            AccessLevel::Read,
        )
        .await;
    let c = via(&s, &p.url, &carol);
    assert_eq!(get(&c, &id).await.unwrap(), b"secret stuff");
    s.admin().delete_acl_rule(rule).await.unwrap();
    // The answer is kept for a minute...
    p.clock.advance_ms(30_000);
    assert!(get(&c, &id).await.is_ok());
    // ...then asked again.
    p.clock.advance_ms(31_000);
    let err = get(&c, &id).await.unwrap_err();
    assert_eq!(err.api().map(|a| a.status), Some(404), "{err}");
    assert!(c.proxy_active(), "a refusal is not a proxy failure");
}

#[tokio::test]
async fn obliterated_content_is_refused_and_removed() {
    let s = TestServer::start().await;
    s.create_depot("game").await;
    let leaked = s
        .commit_files(
            "game",
            "main",
            &[("leak.txt", b"api key 1234"), ("ok.txt", b"fine")],
        )
        .await;
    let id = blob_id(&b"api key 1234"[..]).unwrap();
    let p = proxy(&s, 1 << 30).await;
    let c = via(&s, &p.url, s.admin_token());
    get(&c, &id).await.unwrap();
    assert!(p.state.cache().lookup(&Key::content("game", id)).is_some());

    s.commit_files("game", "main", &[("leak.txt", b"api key ****")])
        .await;
    s.admin()
        .obliterate(
            "game",
            &ObliterateRequest {
                targets: vec![ObliterateTarget {
                    path: DepotPath::parse("leak.txt").unwrap(),
                    rev: Some(leaked),
                }],
                reason: "leaked key".into(),
                dry_run: false,
            },
        )
        .await
        .unwrap();
    p.clock.advance_ms(61_000);
    let err = get(&c, &id).await.unwrap_err();
    assert!(err.is_code(lode_core::ErrorCode::Obliterated), "{err}");
    assert!(p.state.cache().lookup(&Key::content("game", id)).is_none());
    assert!(c.proxy_active());
    // Other content still comes through.
    assert_eq!(
        get(&c, &blob_id(&b"fine"[..]).unwrap()).await.unwrap(),
        b"fine"
    );
}

#[tokio::test]
async fn damaged_entries_are_fetched_again_and_the_cache_keeps_its_cap() {
    let s = TestServer::start().await;
    s.create_depot("game").await;
    let files: Vec<Vec<u8>> = (0..6u64).map(|i| noise(i + 1, 1000)).collect();
    let refs: Vec<(String, &[u8])> = files
        .iter()
        .enumerate()
        .map(|(i, d)| (format!("f{i}.bin"), d.as_slice()))
        .collect();
    let refs: Vec<(&str, &[u8])> = refs.iter().map(|(p, d)| (p.as_str(), *d)).collect();
    s.commit_files("game", "main", &refs).await;
    let p = proxy(&s, 3500).await;
    let c = via(&s, &p.url, s.admin_token());
    let first = blob_id(files[0].as_slice()).unwrap();
    get(&c, &first).await.unwrap();

    let path = p
        .state
        .cache()
        .lookup(&Key::content("game", first))
        .unwrap();
    std::fs::write(&path, noise(99, 1000)).unwrap();
    s.requests().clear();
    assert_eq!(get(&c, &first).await.unwrap(), files[0]);
    assert_eq!(
        upstream_content(&s),
        1,
        "the damaged copy was fetched again"
    );

    for f in &files {
        assert_eq!(&get(&c, &blob_id(f.as_slice()).unwrap()).await.unwrap(), f);
        assert!(p.state.metrics().cache_bytes <= 3500);
    }
    assert_eq!(p.state.metrics().cache_entries, 3);
}

#[tokio::test]
async fn with_the_proxy_down_the_client_warns_and_uses_the_server() {
    let s = TestServer::start().await;
    s.create_depot("game").await;
    s.commit_files("game", "main", &[("a.txt", b"hello"), ("b.txt", b"world")])
        .await;
    // A port nobody listens on.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let warnings = Arc::new(Mutex::new(Vec::<String>::new()));
    let w = warnings.clone();
    let c = Client::builder(s.url())
        .proxy(Some(dead.clone()))
        .on_proxy_failure(Arc::new(move |m| w.lock().unwrap().push(m)))
        .build()
        .unwrap()
        .with_token(s.admin_token());
    let dir = tempfile::tempdir().unwrap();
    for (name, data) in [("a", &b"hello"[..]), ("b", &b"world"[..])] {
        let dest = dir.path().join(name);
        c.download_blob("game", &blob_id(data).unwrap(), &dest)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
    }
    assert_eq!(warnings.lock().unwrap().len(), 1);
    assert!(warnings.lock().unwrap()[0].contains(&dead));
    assert!(!c.proxy_active());
    assert_eq!(
        c.proxy_url()
            .map(|u| u.as_str().trim_end_matches('/').to_owned()),
        Some(dead)
    );
}

#[tokio::test]
async fn chunked_files_reuse_cached_chunks() {
    let s = TestServer::start().await;
    s.create_depot("game").await;
    let data = noise(7, 20 * 1024 * 1024);
    let mut h = ChunkingHasher::new();
    h.update(&data);
    let cut = h.finish();
    let list = cut.chunks.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("big.bin");
    std::fs::write(&src, &data).unwrap();
    s.admin()
        .upload_file("game", &cut.id, &src, data.len() as u64)
        .await
        .unwrap();
    let p = proxy(&s, 1 << 30).await;
    s.requests().clear();

    let opts = DownloadOpts::default();
    for round in 0..2 {
        let c = via(&s, &p.url, s.admin_token());
        let dest = dir.path().join(format!("out{round}"));
        c.download_file("game", &cut.id, &dest, data.len() as u64, &opts)
            .await
            .unwrap();
        assert!(std::fs::read(&dest).unwrap() == data);
        assert_eq!(s.requests().count("GET", "/chunks"), 1, "round {round}");
        assert_eq!(
            upstream_content(&s) - 1,
            list.chunks.len(),
            "each chunk fetched once (round {round})"
        );
    }
    // The whole file, and a range across a chunk boundary, assembled from cached chunks.
    let c = via(&s, &p.url, s.admin_token());
    let whole = get(&c, &cut.id).await.unwrap();
    assert!(whole == data);
    let b = u64::from(list.chunks[0].size);
    let resp = reqwest::Client::new()
        .get(format!("{}/api/v1/depots/game/blobs/{}", p.url, cut.id))
        .bearer_auth(s.admin_token())
        .header("range", format!("bytes={}-{}", b - 3, b + 3))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 206);
    let b = usize::try_from(b).unwrap();
    assert_eq!(&resp.bytes().await.unwrap()[..], &data[b - 3..=b + 3]);
    assert_eq!(
        upstream_content(&s) - 1,
        list.chunks.len(),
        "nothing more fetched"
    );
}
