//! T153 acceptance through the `loded` binary: `migrate-db` moves a fresh server into
//! Postgres; a second `loded serve` on the same database refuses to start; with
//! `--wait-for-lock` it takes over within seconds after the first is killed. Runs only with
//! `LODE_TEST_POSTGRES`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use assert_cmd::cargo::CommandCargoExt;

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn http_get(addr: &str, path: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

fn loded() -> Command {
    let mut c = Command::cargo_bin("loded").unwrap();
    // The data directory below is set up with SQLite and moved explicitly.
    c.env_remove("LODE_TEST_POSTGRES");
    c
}

/// Starts `loded serve` and returns the child and a receiver of its first stdout line.
fn serve(config: &std::path::Path, extra: &[&str]) -> (KillOnDrop, mpsc::Receiver<String>) {
    let mut child = loded()
        .arg("serve")
        .arg("--config")
        .arg(config)
        .args(extra)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        if BufReader::new(stdout).read_line(&mut line).is_ok() {
            let _ = tx.send(line.trim().to_owned());
        }
    });
    (KillOnDrop(child), rx)
}

fn address(line: &str) -> String {
    line.strip_prefix("loded listening on http://")
        .unwrap_or_else(|| panic!("unexpected first line: {line:?}"))
        .to_owned()
}

#[test]
fn one_active_server_and_a_standby_that_takes_over() {
    let Some(url) = lode_meta::test_postgres_url() else {
        return;
    };
    let url = url.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let out = loded().arg("init").arg(&data).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let config = data.join("loded.toml");
    let text = std::fs::read_to_string(&config)
        .unwrap()
        .replace("listen = \"127.0.0.1:7480\"", "listen = \"127.0.0.1:0\"");
    std::fs::write(&config, text).unwrap();
    let url_file = dir.path().join("database-url.txt");
    std::fs::write(&url_file, format!("{url}\n")).unwrap();
    let out = loded()
        .args(["migrate-db", "--to", "postgres", "--config"])
        .arg(&config)
        .arg("--url-file")
        .arg(&url_file)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("rows into Postgres"));

    let (first, first_line) = serve(&config, &[]);
    let addr = address(&first_line.recv_timeout(Duration::from_secs(30)).unwrap());
    assert!(http_get(&addr, "/api/v1/info").starts_with("HTTP/1.1 200"));

    // A second server refuses while the first is active.
    let out = loded()
        .args(["serve", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("in use by another server"), "{err}");
    assert!(err.contains("--wait-for-lock"), "{err}");

    // A standby waits, then takes over when the first dies.
    let (_standby, standby_line) = serve(&config, &["--wait-for-lock"]);
    assert!(standby_line.recv_timeout(Duration::from_secs(3)).is_err());
    drop(first);
    let addr = address(&standby_line.recv_timeout(Duration::from_secs(20)).unwrap());
    assert!(http_get(&addr, "/api/v1/info").starts_with("HTTP/1.1 200"));
}
