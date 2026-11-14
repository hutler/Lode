//! T084 acceptance: `lode self-update` against a local test release server replaces the
//! binary with the release's, and refuses a download whose checksum does not match.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use lode_testkit::TestWorkspace;
use sha2::{Digest, Sha256};

fn target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("windows", _) => "x86_64-pc-windows-msvc",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        _ => "aarch64-apple-darwin",
    }
}

struct Release {
    tag: String,
    files: HashMap<String, Vec<u8>>,
}

/// A release server shaped like the GitHub API, serving one release.
async fn serve(release: Release) -> String {
    let release = Arc::new(release);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let list_base = base.clone();
    let app = Router::new()
        .route(
            "/releases",
            get(move |State(r): State<Arc<Release>>| async move {
                let assets: Vec<_> = r
                    .files
                    .keys()
                    .map(|n| {
                        serde_json::json!({
                            "name": n,
                            "browser_download_url": format!("{list_base}/dl/{n}"),
                        })
                    })
                    .collect();
                Json(serde_json::json!([
                    { "tag_name": "umpalumpa-v9.0.0", "assets": [] },
                    { "tag_name": r.tag, "draft": false, "prerelease": false, "assets": assets },
                ]))
            }),
        )
        .route(
            "/dl/{name}",
            get(
                |State(r): State<Arc<Release>>, UrlPath(name): UrlPath<String>| async move {
                    r.files.get(&name).cloned().ok_or(StatusCode::NOT_FOUND)
                },
            ),
        )
        .with_state(release);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("{base}/releases")
}

/// A copy of the built `lode` in its own folder, as an unpacked release archive would be.
fn installed_copy() -> (tempfile::TempDir, PathBuf, Vec<u8>) {
    let original = TestWorkspace::detached();
    let bin = original.cmd().get_program().to_owned();
    let dir = tempfile::tempdir().unwrap();
    let exe = dir
        .path()
        .join(format!("lode{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(&bin, &exe).unwrap();
    let bytes = std::fs::read(&exe).unwrap();
    (dir, exe, bytes)
}

fn release(version: &str, binary: &[u8], sums_hash: Option<&str>) -> Release {
    let name = format!(
        "lode-{version}-{}{}",
        target(),
        std::env::consts::EXE_SUFFIX
    );
    let hash = sums_hash
        .map(str::to_owned)
        .unwrap_or_else(|| hex::encode(Sha256::digest(binary)));
    let sums = format!("{hash}  {name}\n0000  lode-{version}-other-target\n");
    Release {
        tag: format!("lode-v{version}"),
        files: HashMap::from([
            (name, binary.to_vec()),
            ("SHA256SUMS".to_owned(), sums.into_bytes()),
        ]),
    }
}

fn run(exe: &std::path::Path, url: &str, args: &[&str]) -> (i32, serde_json::Value) {
    let ws = TestWorkspace::detached().with_binary(exe);
    let out = ws
        .cmd()
        .env("LODE_UPDATE_URL", url)
        .arg("--json")
        .arg("self-update")
        .args(args)
        .output()
        .unwrap();
    let v = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stderr)));
    (out.status.code().unwrap(), v)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn self_update_replaces_the_binary_with_the_release() {
    let (dir, exe, before) = installed_copy();
    // The "new" lode: the same program with bytes appended, so it still runs.
    let mut newer = before.clone();
    newer.extend_from_slice(b"\0release 9.8.7 marker");
    let url = serve(release("9.8.7", &newer, None)).await;

    let (code, v) = run(&exe, &url, &["--check"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        (v["latest"].as_str(), v["update_available"].as_bool()),
        (Some("9.8.7"), Some(true))
    );
    assert_eq!(
        std::fs::read(&exe).unwrap(),
        before,
        "--check changes nothing"
    );

    let (code, v) = run(&exe, &url, &[]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["updated"], true);
    assert_eq!(std::fs::read(&exe).unwrap(), newer);
    let ok = std::process::Command::new(&exe)
        .arg("--version")
        .output()
        .unwrap();
    assert!(ok.status.success(), "the new binary runs");
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".lode-update-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn self_update_refuses_a_bad_checksum_and_an_older_release() {
    let (dir, exe, before) = installed_copy();
    let mut newer = before.clone();
    newer.extend_from_slice(b"\0tampered");
    let wrong = "0".repeat(64);
    let url = serve(release("9.8.7", &newer, Some(&wrong))).await;
    let (code, v) = run(&exe, &url, &[]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (1, Some("checksum_mismatch")),
        "{v}"
    );
    assert_eq!(std::fs::read(&exe).unwrap(), before, "nothing changed");
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "no leftovers"
    );

    // A release that is not newer leaves it alone.
    let url = serve(release("0.0.1", &newer, None)).await;
    let (code, v) = run(&exe, &url, &[]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        (v["update_available"].as_bool(), v["updated"].as_bool()),
        (Some(false), Some(false))
    );
    assert_eq!(std::fs::read(&exe).unwrap(), before);
}
