//! T056 benchmarks for the numeric targets of SPEC §10. They are ignored by default, since they
//! take minutes and mean something only in a release build:
//!
//! ```text
//! cargo build --release -p loded
//! cargo test --release -p lode-cli --test perf -- --ignored --nocapture --test-threads 1
//! ```
//!
//! `LODE_PERF_STATUS_FILES` (default 200000) and `LODE_PERF_COMMIT_FILES` (default 10000)
//! change the sizes. Results go in `docs/BENCHMARKS.md`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use lode_testkit::{TestServer, TestWorkspace};

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Writes `n` small files with distinct contents, 100 to 1,100 bytes, 100 per directory.
fn write_small_files(root: &Path, n: usize, tag: &str) {
    for i in 0..n {
        let dir = root.join(format!("d{:04}", i / 100));
        if i % 100 == 0 {
            std::fs::create_dir_all(&dir).unwrap();
        }
        let line = format!("{tag} file {i}\n");
        let body = line.repeat(1 + (100 + (i * 37) % 1000) / line.len());
        std::fs::write(dir.join(format!("f{i}.txt")), body).unwrap();
    }
}

fn timed(ws: &TestWorkspace, args: &[&str]) -> Duration {
    let started = Instant::now();
    ws.json(args);
    started.elapsed()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmark: run in release with --ignored"]
async fn commit_of_small_files() {
    let n = env_usize("LODE_PERF_COMMIT_FILES", 10_000);
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    write_small_files(ws.root(), n, "commit");

    let took = timed(&ws, &["commit", "-A", "-m", "many small files"]);
    eprintln!(
        "BENCH commit {n} small files: {:.2} s (target < 30 s)",
        took.as_secs_f64()
    );
    // And the same files once more, all modified.
    write_small_files(ws.root(), n, "again");
    let again = timed(&ws, &["commit", "-m", "modify them all"]);
    eprintln!(
        "BENCH commit {n} modified small files: {:.2} s",
        again.as_secs_f64()
    );
    assert!(took < Duration::from_secs(30), "{took:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmark: run in release with --ignored"]
async fn status_of_a_large_unchanged_workspace() {
    let n = env_usize("LODE_PERF_STATUS_FILES", 200_000);
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    write_small_files(ws.root(), n, "status");
    let commit = timed(&ws, &["commit", "-A", "-m", "a large tree"]);
    eprintln!(
        "BENCH commit {n} small files (setup): {:.1} s",
        commit.as_secs_f64()
    );

    // Warm: the first run fills the OS file cache; then the median of three.
    let cold = timed(&ws, &["status"]);
    let mut runs: Vec<Duration> = (0..3).map(|_| timed(&ws, &["status"])).collect();
    runs.sort();
    let warm = runs[1];
    eprintln!(
        "BENCH status {n} unchanged files: first {:.2} s, warm median {:.2} s (target < 5 s)",
        cold.as_secs_f64(),
        warm.as_secs_f64()
    );
    let st = ws.json(&["status"]);
    assert_eq!(st["files"].as_array().map_or(0, Vec::len), 0, "{st}");
    assert!(warm < Duration::from_secs(5), "{warm:?}");
}

/// A `loded` process of the release build, on a temporary data directory.
struct Loded {
    child: Child,
    url: String,
    token: String,
    _dir: tempfile::TempDir,
}

impl Drop for Loded {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn loded_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.parent()?;
    let name = format!("loded{}", std::env::consts::EXE_SUFFIX);
    let bin = dir.join(name);
    bin.is_file().then_some(bin)
}

fn start_loded(bin: &Path) -> Loded {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let out = Command::new(bin).arg("init").arg(&data).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let token = text
        .lines()
        .find(|l| l.starts_with("lode_"))
        .unwrap()
        .to_owned();
    let config = data.join("loded.toml");
    let cfg = std::fs::read_to_string(&config)
        .unwrap()
        .replace("listen = \"127.0.0.1:7480\"", "listen = \"127.0.0.1:0\"");
    std::fs::write(&config, cfg).unwrap();
    let mut child = Command::new(bin)
        .args(["serve", "--config"])
        .arg(&config)
        .env("LODED_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let url = line
        .trim()
        .strip_prefix("loded listening on ")
        .unwrap()
        .to_owned();
    Loded {
        child,
        url,
        token,
        _dir: dir,
    }
}

/// Peak resident memory of a process, in KiB (Linux only).
fn peak_rss_kib(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find(|l| l.starts_with("VmHWM:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// Writes `mib` MiB of incompressible bytes.
fn write_noise(path: &Path, mib: usize) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15 ^ mib as u64;
    let mut buf = vec![0u8; 1024 * 1024];
    for _ in 0..mib {
        for chunk in buf.chunks_mut(8) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            chunk.copy_from_slice(&x.to_le_bytes()[..chunk.len()]);
        }
        f.write_all(&buf).unwrap();
    }
    f.flush().unwrap();
}

#[test]
#[ignore = "benchmark: run in release with --ignored (Linux, needs the release loded)"]
fn server_memory_does_not_grow_with_file_size() {
    let Some(bin) = loded_binary() else {
        panic!("build the server first: cargo build --release -p loded");
    };
    if peak_rss_kib(std::process::id()).is_none() {
        eprintln!("BENCH memory: skipped, needs /proc (Linux)");
        return;
    }
    let mut peaks = Vec::new();
    for mib in [16usize, 256, 1024] {
        // A fresh server for each size, so each peak belongs to that size alone.
        let loded = start_loded(&bin);
        let pid = loded.child.id();
        let base = peak_rss_kib(pid).unwrap();
        let home = tempfile::tempdir().unwrap();
        let token_file = home.path().join("token");
        std::fs::write(&token_file, format!("{}\n", loded.token)).unwrap();
        let lode = |dir: &Path, args: &[&str]| {
            let lode_bin = loded_binary()
                .unwrap()
                .with_file_name(format!("lode{}", std::env::consts::EXE_SUFFIX));
            let out = Command::new(lode_bin)
                .current_dir(dir)
                .env("LODE_SERVER", &loded.url)
                .env("LODE_TOKEN_FILE", &token_file)
                .env("LODE_CONFIG_DIR", home.path().join("config"))
                .env("HOME", home.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "lode {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        lode(home.path(), &["admin", "depot", "create", "big"]);
        let ws = home.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        lode(&ws, &["checkout", &format!("{}/big", loded.url), "."]);
        write_noise(&ws.join("big.bin"), mib);
        let started = Instant::now();
        lode(&ws, &["commit", "-A", "-m", "big"]);
        let up = started.elapsed();
        let after_upload = peak_rss_kib(pid).unwrap();
        let other = home.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        let started = Instant::now();
        lode(&other, &["checkout", &format!("{}/big", loded.url), "."]);
        let down = started.elapsed();
        let after_download = peak_rss_kib(pid).unwrap();
        std::fs::remove_file(ws.join("big.bin")).unwrap();
        eprintln!(
            "BENCH memory {mib} MiB file: server peak RSS {} MiB at start, {} MiB after upload \
             ({:.1} s), {} MiB after download ({:.1} s)",
            base / 1024,
            after_upload / 1024,
            up.as_secs_f64(),
            after_download / 1024,
            down.as_secs_f64()
        );
        peaks.push(after_download);
    }
    // Independent of file size: the 1 GiB peak is within 64 MiB of the 16 MiB one.
    let (small, large) = (peaks[0], peaks[2]);
    assert!(
        large < small + 64 * 1024,
        "peak RSS grew from {small} KiB to {large} KiB"
    );
}
