//! T054: the client stopped abruptly in `checkout`, `update` and `commit`, then run again.
//! Every rerun converges: the workspace ends up matching the server, nothing is committed
//! twice, and nothing is lost.
//!
//! Two kinds of stop: named crash points (`LODE_TEST_CRASH_AT`), which hit the steps between
//! durable writes exactly, and kills after a random delay, which land anywhere. The random
//! test runs a few rounds by default; `LODE_CRASH_ROUNDS` and `LODE_CRASH_SEED` make it longer
//! or replay a failure.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use lode_testkit::{TestServer, TestWorkspace};
use serde_json::Value;

/// Exit code of a simulated crash (`lode-cli` `crash::CRASH_EXIT`).
const CRASH_EXIT: i32 = 86;

async fn server_with_depot() -> TestServer {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
}

fn ws_for(server: &TestServer) -> TestWorkspace {
    TestWorkspace::new(server).with_token(server.admin_token())
}

fn checkout(server: &TestServer) -> TestWorkspace {
    let ws = ws_for(server);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws
}

/// Files under `dir` with their contents; `.lode` left out.
fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            if e.file_name() == ".lode" && dir == base {
                continue;
            }
            let path = e.path();
            if e.file_type().unwrap().is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path.strip_prefix(base).unwrap().to_string_lossy();
                out.insert(rel.replace('\\', "/"), std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// Runs `lode args…` with a crash at `point`, which must be reached.
fn crash_at(ws: &TestWorkspace, point: &str, args: &[&str]) {
    let out = ws
        .cmd()
        .env("LODE_TEST_CRASH_AT", point)
        .args(args)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(CRASH_EXIT),
        "{point}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The files of the branch head, from a fresh checkout nobody interrupted.
fn server_files(server: &TestServer) -> BTreeMap<String, Vec<u8>> {
    let fresh = checkout(server);
    files(fresh.root())
}

async fn head(server: &TestServer) -> u64 {
    server
        .admin()
        .branch("game", "main")
        .await
        .unwrap()
        .head_rev
}

fn clean_status(ws: &TestWorkspace) {
    let st = ws.json(&["status"]);
    let changed: Vec<&Value> = st["files"]
        .as_array()
        .map(|a| a.iter().filter(|f| f["status"] != "unchanged").collect())
        .unwrap_or_default();
    assert!(changed.is_empty(), "{st}");
}

async fn seed(server: &TestServer, n: usize) {
    let contents: Vec<(String, Vec<u8>)> = (0..n)
        .map(|i| {
            (
                format!("d{}/f{i}.txt", i % 4),
                format!("file {i}\n").repeat(1 + i % 7).into_bytes(),
            )
        })
        .collect();
    let refs: Vec<(&str, &[u8])> = contents
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_slice()))
        .collect();
    server.commit_files("game", "main", &refs).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn named_crash_points_converge() {
    let server = server_with_depot().await;
    seed(&server, 12).await;
    let url = format!("{}/game", server.url());

    // Checkout, stopped after planning and after fetching.
    for point in ["checkout.created", "checkout.planned", "checkout.fetched"] {
        let ws = ws_for(&server);
        crash_at(&ws, point, &["checkout", &url, "."]);
        let v = ws.json(&["checkout", &url, "."]);
        assert_eq!(v["resumed"], true, "{point}");
        assert_eq!(files(ws.root()), server_files(&server), "{point}");
        clean_status(&ws);
    }

    // Stopped before the workspace database had any configuration: an empty database.
    let ws = ws_for(&server);
    std::fs::create_dir_all(ws.root().join(".lode")).unwrap();
    std::fs::write(ws.root().join(".lode/workspace.db"), b"").unwrap();
    let v = ws.json(&["checkout", &url, "."]);
    assert_eq!(v["resumed"], true);
    assert_eq!(files(ws.root()), server_files(&server));
    // A finished checkout is not one to resume.
    let (code, err) = ws.json_status(&["checkout", &url, "."]);
    assert_ne!(code, 0);
    assert_eq!(err["error"]["code"], "already_a_workspace");

    // Commit, stopped after uploading, after storing the request, after the server answered.
    let a = checkout(&server);
    for (i, point) in ["commit.uploaded", "commit.stored", "commit.sent"]
        .into_iter()
        .enumerate()
    {
        a.write("d0/f0.txt", format!("edit {i}\n"));
        a.write(&format!("new{i}.txt"), format!("new {i}\n"));
        a.json(&["add", &format!("new{i}.txt")]);
        let before = head(&server).await;
        crash_at(&a, point, &["commit", "-m", point]);
        let sent = point == "commit.sent";
        assert_eq!(head(&server).await, before + u64::from(sent), "{point}");

        let v = a.json(&["commit", "-m", point]);
        assert_eq!(v["rev"], before + 1, "{point}: {v}");
        assert_eq!(v["resumed"], point != "commit.uploaded", "{point}: {v}");
        assert_eq!(v["replayed"], sent, "{point}: {v}");
        assert_eq!(head(&server).await, before + 1, "{point}");
        clean_status(&a);
        assert_eq!(files(a.root()), server_files(&server), "{point}");
    }

    // Update, stopped after planning, after deleting and after fetching.
    let b = checkout(&server);
    for (i, point) in ["update.planned", "update.deleted", "update.fetched"]
        .into_iter()
        .enumerate()
    {
        b.write("d1/f1.txt", format!("from b {i}\n"));
        b.write(&format!("b{i}.txt"), "b\n");
        b.json(&["add", &format!("b{i}.txt")]);
        b.json(&["rm", &format!("d2/f{}.txt", 2 + 4 * i)]);
        b.json(&["commit", "-m", point]);
        crash_at(&a, point, &["update"]);
        a.json(&["update"]);
        clean_status(&a);
        assert_eq!(files(a.root()), server_files(&server), "{point}");
        assert_eq!(files(a.root()), files(b.root()), "{point}");
    }
}

/// A small deterministic generator, so a failing seed can be replayed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Starts `lode args…` and kills it after `delay`, unless it finished first.
fn kill_after(ws: &TestWorkspace, args: &[&str], delay: Duration) {
    let mut child = ws
        .std_cmd()
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(delay);
    let _ = child.kill();
    child.wait().unwrap();
}

/// Runs `lode --json args…` until it succeeds or fails with one of `done_codes`, which also
/// count as converged (such as `nothing_to_commit` after a commit that was finished).
fn rerun(ws: &TestWorkspace, args: &[&str], done_codes: &[&str]) -> Value {
    let (code, v) = ws.json_status(args);
    if code == 0 {
        return v;
    }
    let err = v["error"]["code"].as_str().unwrap_or_default();
    assert!(done_codes.contains(&err), "lode {args:?} after a kill: {v}");
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn random_kills_converge() {
    let seed_value = env_u64("LODE_CRASH_SEED", 0x5eed_c0de);
    let rounds = env_u64("LODE_CRASH_ROUNDS", 4);
    let mut rng = Rng(seed_value | 1);
    let server = server_with_depot().await;
    seed(&server, 40).await;
    let url = format!("{}/game", server.url());
    let other = checkout(&server);
    let ws = checkout(&server);

    for round in 0..rounds {
        let ctx = format!("seed {seed_value:#x} round {round}");
        let delay = |rng: &mut Rng| Duration::from_millis(rng.below(250));

        // Someone else changes a few files, adds some and removes one.
        for k in 0..5 {
            let n = rng.below(40);
            other.write(
                &format!("d{}/f{n}.txt", n % 4),
                format!("other {round} {k}\n"),
            );
        }
        for k in 0..3 {
            let p = format!("o{round}/n{k}.txt");
            other.write(&p, "x".repeat(1 + rng.below(5000) as usize));
            other.json(&["add", &p]);
        }
        if round > 0 {
            other.json(&["rm", &format!("o{}/n0.txt", round - 1)]);
        }
        other.json(&["commit", "-m", &format!("other {round}")]);

        // Update, killed somewhere.
        kill_after(&ws, &["update"], delay(&mut rng));
        rerun(&ws, &["update"], &[]);
        clean_status(&ws);
        assert_eq!(files(ws.root()), files(other.root()), "{ctx}: update");

        // Commit, killed somewhere: the change lands exactly once.
        let before = head(&server).await;
        for k in 0..4 {
            let n = rng.below(40);
            ws.write(
                &format!("d{}/f{n}.txt", n % 4),
                format!("mine {round} {k}\n"),
            );
        }
        let p = format!("m{round}.txt");
        ws.write(&p, "mine\n");
        ws.json(&["add", &p]);
        kill_after(&ws, &["commit", "-m", "mine"], delay(&mut rng));
        rerun(&ws, &["commit", "-m", "mine"], &["nothing_to_commit"]);
        assert_eq!(head(&server).await, before + 1, "{ctx}: commit");
        clean_status(&ws);
        other.json(&["update"]);
        assert_eq!(files(ws.root()), files(other.root()), "{ctx}: commit");

        // A fresh checkout, killed somewhere.
        let fresh = ws_for(&server);
        kill_after(&fresh, &["checkout", &url, "."], delay(&mut rng));
        rerun(&fresh, &["checkout", &url, "."], &[]);
        clean_status(&fresh);
        assert_eq!(files(fresh.root()), files(ws.root()), "{ctx}: checkout");
    }
}
