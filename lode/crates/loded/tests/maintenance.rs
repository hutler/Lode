//! T019 acceptance through the `loded` binary: `backup` and `fsck` next to a running server.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use assert_cmd::cargo::CommandCargoExt;

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn http_post(addr: &str, path: &str, token: &str, body: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

fn loded(args: &[&str], config: &Path) -> std::process::Output {
    let mut cmd = Command::cargo_bin("loded").unwrap();
    cmd.arg(args[0])
        .arg("--config")
        .arg(config)
        .args(&args[1..]);
    cmd.output().unwrap()
}

#[test]
fn backup_and_fsck_from_the_command_line() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let out = Command::cargo_bin("loded")
        .unwrap()
        .arg("init")
        .arg(&data)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let token = String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|l| l.starts_with("lode_"))
        .unwrap()
        .to_owned();
    let cfg_path = data.join("loded.toml");
    let cfg = std::fs::read_to_string(&cfg_path)
        .unwrap()
        .replace("listen = \"127.0.0.1:7480\"", "listen = \"127.0.0.1:0\"");
    std::fs::write(&cfg_path, cfg).unwrap();

    let child = Command::cargo_bin("loded")
        .unwrap()
        .arg("serve")
        .arg("--config")
        .arg(&cfg_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut child = KillOnDrop(child);
    let mut line = String::new();
    BufReader::new(child.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let addr = line
        .trim()
        .strip_prefix("loded listening on http://")
        .unwrap()
        .to_owned();
    let resp = http_post(&addr, "/api/v1/depots", &token, r#"{"name":"game"}"#);
    assert!(resp.starts_with("HTTP/1.1 201"), "{resp}");

    // Both run beside the live server.
    let out = loded(&["fsck"], &cfg_path);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("game: "));
    let backup_dir = dir.path().join("backup");
    let out = loded(&["backup", backup_dir.to_str().unwrap()], &cfg_path);
    assert!(out.status.success(), "{out:?}");
    drop(child);

    // Damage the original: remove every object of the depot.
    let objects = data.join("depots");
    for depot in std::fs::read_dir(&objects).unwrap().flatten() {
        let _ = std::fs::remove_dir_all(depot.path().join("objects"));
    }
    let out = loded(&["fsck", "--full", "game"], &cfg_path);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("missing"));

    // The backup is a working data directory and is intact.
    let out = loded(&["fsck", "--full"], &backup_dir.join("loded.toml"));
    assert!(out.status.success(), "{out:?}");

    let out = loded(&["fsck", "nope"], &cfg_path);
    assert!(!out.status.success());
}
