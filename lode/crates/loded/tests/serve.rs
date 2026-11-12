//! T010 acceptance through the `loded` binary: `init`, then `serve`, then `/info`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
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

fn http_get_auth(addr: &str, path: &str, token: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn init_then_serve_answers_info() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let out = Command::cargo_bin("loded")
        .unwrap()
        .arg("init")
        .arg(&data)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(stdout.contains("Initialized"));
    let admin_token = stdout
        .lines()
        .find(|l| l.starts_with("lode_"))
        .expect("init prints the admin token")
        .to_owned();

    // Use a free port instead of the default one.
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
        .unwrap_or_else(|| panic!("unexpected first line: {line:?}"))
        .to_owned();

    let response = http_get(&addr, "/api/v1/info");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let body = response.split("\r\n\r\n").nth(1).unwrap();
    let info: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(info["server_version"], env!("CARGO_PKG_VERSION"));
    // Modules on by default that this build has.
    assert_eq!(info["modules"], serde_json::json!(["locks"]));
    assert!(response.to_ascii_lowercase().contains("x-request-id:"));

    let response = http_get_auth(&addr, "/api/v1/auth/whoami", &admin_token);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"name\":\"admin\""));

    // reset-admin works offline next to a running server and revokes the old token.
    let out = Command::cargo_bin("loded")
        .unwrap()
        .args(["reset-admin", "--config"])
        .arg(&cfg_path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let fresh = stdout.lines().find(|l| l.starts_with("lode_")).unwrap();
    assert!(http_get_auth(&addr, "/api/v1/auth/whoami", &admin_token).starts_with("HTTP/1.1 401"));
    assert!(http_get_auth(&addr, "/api/v1/auth/whoami", fresh).starts_with("HTTP/1.1 200"));

    let response = http_get(&addr, "/api/v1/does-not-exist");
    assert!(response.starts_with("HTTP/1.1 404"), "{response}");
    assert!(response.contains("\"code\":\"not_found\""));
}

#[test]
fn serve_refuses_missing_or_uninitialized_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("loded.toml");

    std::fs::write(&cfg_path, "listen = '127.0.0.1:0'\ndata_dir = 'nowhere'\n").unwrap();
    let out = Command::cargo_bin("loded")
        .unwrap()
        .args(["serve", "--config"])
        .arg(&cfg_path)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("does not exist") && err.contains("loded init"),
        "{err}"
    );

    std::fs::create_dir(dir.path().join("empty")).unwrap();
    std::fs::write(&cfg_path, "listen = '127.0.0.1:0'\ndata_dir = 'empty'\n").unwrap();
    let out = Command::cargo_bin("loded")
        .unwrap()
        .args(["serve", "--config"])
        .arg(&cfg_path)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not initialized"), "{err}");
}

#[test]
fn serve_rejects_unknown_config_keys() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("loded.toml");
    std::fs::write(&cfg_path, "data_dir = '.'\nlisten_port = 1\n").unwrap();
    let out = Command::cargo_bin("loded")
        .unwrap()
        .args(["serve", "--config"])
        .arg(&cfg_path)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("listen_port"));
}
