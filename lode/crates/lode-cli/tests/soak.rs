//! T054 concurrency soak: several clients, each its own `lode` workspace and user, commit,
//! update, lock and shelve against one server for a bounded time. At the end:
//!
//! - revisions are linear: 1 to head, each on `main`, none missing;
//! - no update is lost: a shared counter equals the number of increments that committed,
//!   and each client's own log has one line per commit it made;
//! - locks are exclusive: no two clients ever held the lock at the same time;
//! - every shelf is there, once, with its owner and description.
//!
//! CI runs it for a few seconds. `LODE_SOAK_SECS` and `LODE_SOAK_CLIENTS` run it longer and
//! wider; `docs/HARDENING.md` gives the long form.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lode_proto::admin::{AccessLevel, AclEffect};
use lode_testkit::{TestServer, TestWorkspace};
use serde_json::Value;

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// What one client saw, for the checks at the end.
#[derive(Default)]
struct Tally {
    commits: u64,
    out_of_date: u64,
    locks: u64,
    refused: u64,
    shelves: Vec<(u64, String)>,
}

/// A lock hold as seen by its holder: from `lock` returning to `unlock` being called, which
/// is inside the server's hold.
type Hold = (Instant, Instant, usize);

fn error_code(v: &Value) -> String {
    v["error"]["code"].as_str().unwrap_or_default().to_owned()
}

fn read_counter(ws: &TestWorkspace) -> u64 {
    String::from_utf8(ws.read("counter.txt"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn client_loop(i: usize, ws: &TestWorkspace, until: Instant, holds: &Mutex<Vec<Hold>>) -> Tally {
    let mut t = Tally::default();
    let mut step = 0u64;
    while Instant::now() < until {
        step += 1;
        match (step + i as u64) % 4 {
            // Increment the shared counter and log it in this client's own file.
            0 | 1 => {
                ws.json(&["update"]);
                let n = read_counter(ws);
                ws.write("counter.txt", format!("{}\n", n + 1));
                let own = format!("c{i}.txt");
                let mut log = ws.read(&own);
                log.extend_from_slice(format!("{}\n", n + 1).as_bytes());
                std::fs::write(ws.root().join(&own), log).unwrap();
                let (code, v) = ws.json_status(&["commit", "-m", &format!("c{i} +1")]);
                if code == 0 {
                    t.commits += 1;
                } else {
                    assert_eq!(error_code(&v), "out_of_date", "client {i}: {v}");
                    t.out_of_date += 1;
                    ws.json(&["revert", "--all"]);
                }
            }
            // Take the lock, hold it a moment, give it back.
            2 => {
                let (code, v) = ws.json_status(&["lock", "guarded.txt"]);
                if code == 0 {
                    let from = Instant::now();
                    std::thread::sleep(Duration::from_millis(5));
                    let to = Instant::now();
                    holds.lock().unwrap().push((from, to, i));
                    ws.json(&["unlock", "guarded.txt"]);
                    t.locks += 1;
                } else {
                    assert_eq!(error_code(&v), "locked", "client {i}: {v}");
                    t.refused += 1;
                }
            }
            // Shelve a new file, bring it back, drop it.
            _ => {
                let file = format!("s{i}.txt");
                let content = format!("shelved by {i} at step {step}\n");
                ws.write(&file, &content);
                ws.json(&["add", &file]);
                let desc = format!("c{i}-{step}");
                let s = ws.json(&["shelve", "-m", &desc]);
                let id = s["shelf"].as_u64().unwrap();
                assert!(!ws.exists(&file), "client {i}: shelving left {file}");
                ws.json(&["unshelve", &id.to_string()]);
                assert_eq!(ws.read(&file), content.as_bytes(), "client {i}");
                ws.json(&["revert", "--all"]);
                ws.json(&["clean"]);
                t.shelves.push((id, desc));
            }
        }
    }
    t
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clients_soak_and_invariants_hold() {
    let secs = env_u64("LODE_SOAK_SECS", 6);
    let clients = usize::try_from(env_u64("LODE_SOAK_CLIENTS", 4)).unwrap();
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let mut seed: Vec<(String, Vec<u8>)> = vec![
        ("counter.txt".into(), b"0\n".to_vec()),
        ("guarded.txt".into(), b"guarded\n".to_vec()),
    ];
    for i in 0..clients {
        seed.push((format!("c{i}.txt"), Vec::new()));
    }
    let refs: Vec<(&str, &[u8])> = seed
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_slice()))
        .collect();
    let first = server.commit_files("game", "main", &refs).await;

    let mut workspaces = Vec::new();
    for i in 0..clients {
        let name = format!("user{i}");
        let token = server
            .create_user(&name, &["read", "write", "lock", "shelve"])
            .await;
        server
            .acl(
                "game",
                &name,
                "*",
                "**",
                AclEffect::Allow,
                AccessLevel::Write,
            )
            .await;
        let ws = TestWorkspace::new(&server).with_token(&token);
        ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
        workspaces.push(ws);
    }

    let until = Instant::now() + Duration::from_secs(secs);
    let holds = Arc::new(Mutex::new(Vec::new()));
    let mut tasks = Vec::new();
    for (i, ws) in workspaces.into_iter().enumerate() {
        let holds = holds.clone();
        tasks.push(tokio::task::spawn_blocking(move || {
            let t = client_loop(i, &ws, until, &holds);
            (ws, t)
        }));
    }
    let mut done = Vec::new();
    for task in tasks {
        done.push(task.await.unwrap());
    }

    // Linear revisions: every one from the seed to head is there, in order, on main.
    let head = server
        .admin()
        .branch("game", "main")
        .await
        .unwrap()
        .head_rev;
    let total: u64 = done.iter().map(|(_, t)| t.commits).sum();
    assert_eq!(head, first + total, "one revision per successful commit");
    for rev in first..=head {
        let r = server.admin().revision("game", rev).await.unwrap();
        assert_eq!(r.rev, rev);
        assert_eq!(r.branch, "main");
    }

    // No lost update.
    let (ws0, _) = &done[0];
    ws0.json(&["update"]);
    assert_eq!(read_counter(ws0), total);
    let mut seen = BTreeSet::new();
    for (i, (_, t)) in done.iter().enumerate() {
        let log = String::from_utf8(ws0.read(&format!("c{i}.txt"))).unwrap();
        let values: Vec<u64> = log.lines().map(|l| l.parse().unwrap()).collect();
        assert_eq!(values.len() as u64, t.commits, "client {i}");
        for v in values {
            assert!(seen.insert(v), "counter value {v} committed twice");
        }
    }
    assert_eq!(seen, (1..=total).collect::<BTreeSet<_>>());

    // Exclusive locks.
    let mut holds = holds.lock().unwrap().clone();
    holds.sort();
    for w in holds.windows(2) {
        assert!(
            w[0].1 <= w[1].0,
            "clients {} and {} held the lock together",
            w[0].2,
            w[1].2
        );
    }

    // Every shelf, once, as its owner left it.
    let mut ids = BTreeSet::new();
    for (i, (_, t)) in done.iter().enumerate() {
        for (id, desc) in &t.shelves {
            assert!(ids.insert(*id), "shelf {id} twice");
            let view = server.admin().shelf("game", *id, false).await.unwrap();
            assert_eq!(&view.description, desc);
            assert_eq!(view.owner.name, format!("user{i}"));
        }
    }

    let stats: Vec<String> = done
        .iter()
        .map(|(_, t)| {
            format!(
                "{} commits, {} out of date, {} locks, {} refused, {} shelves",
                t.commits,
                t.out_of_date,
                t.locks,
                t.refused,
                t.shelves.len()
            )
        })
        .collect();
    eprintln!("soak over {secs}s: {stats:#?}");
    assert!(total > 0, "no commit got through: {stats:?}");
}
