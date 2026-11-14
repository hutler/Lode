//! T151 acceptance: large files travel in chunks, and only the chunks that changed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_testkit::{TestServer, TestWorkspace};
use serde_json::Value;

const MIB: usize = 1024 * 1024;

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

fn n(v: &Value, key: &str) -> u64 {
    v[key].as_u64().unwrap_or_else(|| panic!("no {key} in {v}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edit_to_a_large_file_sends_only_changed_chunks() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let url = format!("{}/game", server.url());
    let a = TestWorkspace::new(&server).with_token(server.admin_token());
    a.json(&["checkout", &url, "."]);
    let mut data = noise(1, 64 * MIB);
    a.write("Content/level.umap", &data);
    a.json(&["add", "."]);
    let c1 = a.json(&["commit", "-m", "Add level"]);
    assert_eq!(n(&c1, "bytes_uploaded"), 64 * MIB as u64, "{c1}");
    assert!(server.requests().count("PUT", "/chunks") == 1);

    // A second workspace at the first version.
    let b = TestWorkspace::new(&server).with_token(server.admin_token());
    b.json(&["checkout", &url, "."]);
    assert_eq!(b.read("Content/level.umap"), data);

    // Change 1 MiB in the middle.
    let mid = data.len() / 2;
    data[mid..mid + MIB].copy_from_slice(&noise(2, MIB));
    a.write("Content/level.umap", &data);
    let c2 = a.json(&["commit", "-m", "Move a light"]);
    assert!(n(&c2, "bytes_uploaded") < 10 * MIB as u64, "{c2}");
    assert!(n(&c2, "bytes_reused") > 50 * MIB as u64, "{c2}");

    let u = b.json(&["update"]);
    assert!(n(&u, "bytes_downloaded") < 10 * MIB as u64, "{u}");
    assert!(n(&u, "bytes_reused") > 50 * MIB as u64, "{u}");
    assert!(
        b.read("Content/level.umap") == data,
        "content differs after update"
    );

    // `cat` reads the chunked file whole.
    let out = a
        .cmd()
        .args(["cat", &format!("Content/level.umap@{}", n(&c2, "rev"))])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout == data, "cat differs");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_transfers_resume() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let url = format!("{}/game", server.url());
    let a = TestWorkspace::new(&server).with_token(server.admin_token());
    a.json(&["checkout", &url, "."]);
    let data = noise(3, 40 * MIB);
    a.write("big.bin", &data);
    a.json(&["add", "."]);

    // Killed after three chunks went up.
    let status = a
        .std_cmd()
        .args(["commit", "-m", "Big", "--json"])
        .env("LODE_TEST_EXIT_AFTER_CHUNKS", "3")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(86));
    let c = a.json(&["commit", "-m", "Big"]);
    let total = 40 * MIB as u64;
    assert!(n(&c, "bytes_uploaded") < total, "{c}");
    assert!(n(&c, "bytes_reused") >= 3 * 256 * 1024, "{c}");

    // A checkout killed after three chunks came down resumes with the rest.
    let b = TestWorkspace::new(&server).with_token(server.admin_token());
    let status = b
        .std_cmd()
        .args(["checkout", &url, ".", "--json"])
        .env("LODE_TEST_EXIT_AFTER_CHUNKS", "3")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(86));
    let co = b.json(&["checkout", &url, "."]);
    assert!(n(&co, "bytes_downloaded") < total, "{co}");
    assert!(b.read("big.bin") == data, "content differs after resume");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whole_files_when_chunking_is_off() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let url = format!("{}/game", server.url());
    let a = TestWorkspace::new(&server).with_token(server.admin_token());
    a.json(&["checkout", &url, "."]);
    let data = noise(4, 20 * MIB);
    a.write("big.bin", &data);
    a.json(&["add", "."]);
    let out = a
        .std_cmd()
        .args(["commit", "-m", "Big", "--json"])
        .env("LODE_TEST_NO_CHUNKING", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(server.requests().count("PUT", "/chunks"), 0);
    let b = TestWorkspace::new(&server).with_token(server.admin_token());
    b.json(&["checkout", &url, "."]);
    assert!(b.read("big.bin") == data);
    // Stored whole: the new client asks for a list, hears `not_chunked`, and downloads whole.
    assert_eq!(server.requests().count("GET", "/chunks"), 1);
}
