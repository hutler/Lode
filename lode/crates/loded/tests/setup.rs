//! T082 acceptance: `loded setup` with flags gives a server that starts and accepts the chosen
//! admin password; `loded service install --dry-run` shows the systemd unit.
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

fn post_json(addr: &str, path: &str, body: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn setup_with_flags_gives_a_working_server() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("lode data");
    let pw = dir.path().join("pw.txt");
    std::fs::write(&pw, "correct horse battery\n").unwrap();
    let out = Command::cargo_bin("loded")
        .unwrap()
        .args([
            "setup",
            "--yes",
            "--local",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
        ])
        .arg(&data)
        .arg("--admin-password-file")
        .arg(&pw)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Lode is set up"), "{stdout}");
    assert!(
        !stdout.lines().any(|l| l.starts_with("lode_")),
        "one secret: no admin token without --print-admin-token: {stdout}"
    );
    let config = data.join("loded.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains("listen = \"127.0.0.1:0\""));
    assert!(
        text.contains("[backup]\nevery = \"24h\""),
        "daily backups by default"
    );
    assert!(data.join("backups").is_dir());

    // Running it again is refused.
    let again = Command::cargo_bin("loded")
        .unwrap()
        .args(["setup", "--yes", "--local", "--data-dir"])
        .arg(&data)
        .arg("--admin-password-file")
        .arg(&pw)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("already set up"));

    // The server starts and the admin signs in with the password.
    let child = Command::cargo_bin("loded")
        .unwrap()
        .arg("serve")
        .arg("--config")
        .arg(&config)
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
    let ok = post_json(
        &addr,
        "/api/v1/auth/login",
        r#"{"name":"admin","password":"correct horse battery"}"#,
    );
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
    assert!(ok.contains("lode_"), "a token comes back: {ok}");
    let bad = post_json(
        &addr,
        "/api/v1/auth/login",
        r#"{"name":"admin","password":"wrong password"}"#,
    );
    assert!(bad.starts_with("HTTP/1.1 401"), "{bad}");
}

#[test]
fn setup_refuses_a_short_password_and_a_domain_sets_up_https() {
    let dir = tempfile::tempdir().unwrap();
    let pw = dir.path().join("pw.txt");
    std::fs::write(&pw, "short\n").unwrap();
    let out = Command::cargo_bin("loded")
        .unwrap()
        .args(["setup", "--yes", "--local", "--data-dir"])
        .arg(dir.path().join("a"))
        .arg("--admin-password-file")
        .arg(&pw)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!dir.path().join("a").exists(), "nothing written");

    std::fs::write(&pw, "long enough password\n").unwrap();
    let data = dir.path().join("b");
    let out = Command::cargo_bin("loded")
        .unwrap()
        .args([
            "setup",
            "--yes",
            "--domain",
            "lode.example.com",
            "--email",
            "ops@example.com",
            "--no-backup",
            "--admin-password-file",
            "-",
            "--data-dir",
        ])
        .arg(&data)
        .stdin(std::fs::File::open(&pw).unwrap())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let text = std::fs::read_to_string(data.join("loded.toml")).unwrap();
    assert!(text.contains("listen = \"0.0.0.0:443\""));
    assert!(text
        .contains("[tls]\nacme_domain = \"lode.example.com\"\nacme_email = \"ops@example.com\"\n"));
    assert!(!text.contains("\n[backup]"));
    assert!(String::from_utf8_lossy(&out.stdout).contains("https://lode.example.com"));
}

/// Starts `loded serve` and returns it with its address.
fn serve(config: &std::path::Path) -> (KillOnDrop, String) {
    let child = Command::cargo_bin("loded")
        .unwrap()
        .arg("serve")
        .arg("--config")
        .arg(config)
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
    (child, addr)
}

/// The line after `heading` in setup's output.
fn printed_after(stdout: &str, heading: &str) -> String {
    let mut lines = stdout.lines();
    lines.find(|l| l.starts_with(heading)).unwrap();
    lines.next().unwrap().trim().to_owned()
}

#[test]
fn generated_admin_password_works_only_to_choose_a_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    // No terminal and no password file: setup generates one instead of failing.
    let out = Command::cargo_bin("loded")
        .unwrap()
        .args([
            "setup",
            "--yes",
            "--local",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
        ])
        .arg(&data)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let first = printed_after(&stdout, "Admin password (shown only now");
    assert_eq!(first.len(), 20, "{stdout}");
    assert!(!stdout.lines().any(|l| l.starts_with("lode_")), "{stdout}");

    let (_child, addr) = serve(&data.join("loded.toml"));
    let login = |pw: &str| {
        post_json(
            &addr,
            "/api/v1/auth/login",
            &format!(r#"{{"name":"admin","password":"{pw}"}}"#),
        )
    };
    let refused = login(&first);
    assert!(refused.starts_with("HTTP/1.1 401"), "{refused}");
    assert!(refused.contains("password_change_required"), "{refused}");

    let same = post_json(
        &addr,
        "/api/v1/auth/password",
        &format!(r#"{{"name":"admin","password":"{first}","new_password":"{first}"}}"#),
    );
    assert!(same.starts_with("HTTP/1.1 400"), "{same}");
    let changed = post_json(
        &addr,
        "/api/v1/auth/password",
        &format!(r#"{{"name":"admin","password":"{first}","new_password":"my own password"}}"#),
    );
    assert!(changed.starts_with("HTTP/1.1 200"), "{changed}");
    assert!(login(&first).starts_with("HTTP/1.1 401"), "old one is gone");
    let ok = login("my own password");
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
}

#[test]
fn print_admin_token_keeps_the_old_output_and_reset_admin_generates() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let out = Command::cargo_bin("loded")
        .unwrap()
        .args([
            "setup",
            "--yes",
            "--local",
            "--generate-admin-password",
            "--print-admin-token",
            "--data-dir",
        ])
        .arg(&data)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.lines().any(|l| l.starts_with("lode_")), "{stdout}");

    let out = Command::cargo_bin("loded")
        .unwrap()
        .args(["reset-admin", "--generate-password", "--config"])
        .arg(data.join("loded.toml"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(printed_after(&stdout, "New admin password").len(), 20);
    assert!(!stdout.lines().any(|l| l.starts_with("lode_")), "{stdout}");
}

#[cfg(target_os = "linux")]
#[test]
fn service_install_dry_run_shows_the_systemd_unit() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let config = data.join("loded.toml");
    std::fs::write(
        &config,
        "listen = '0.0.0.0:443'\ndata_dir = '.'\n[backup]\nevery = '24h'\ndir = '/mnt/lode-backups'\n",
    )
    .unwrap();
    let out = Command::cargo_bin("loded")
        .unwrap()
        .args([
            "service",
            "install",
            "--dry-run",
            "--user",
            "lodesvc",
            "--config",
        ])
        .arg(&config)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let exe = assert_cmd::cargo::cargo_bin("loded");
    assert!(
        stdout.contains(&format!(
            "ExecStart={} serve --config {}\n",
            exe.display(),
            config.display()
        )),
        "{stdout}"
    );
    assert!(stdout.contains("User=lodesvc\n"));
    assert!(stdout.contains(&format!(
        "ReadWritePaths={} /mnt/lode-backups\n",
        data.display()
    )));
    assert!(stdout.contains("AmbientCapabilities=CAP_NET_BIND_SERVICE"));
    assert!(stdout.contains("systemctl enable loded"));
    assert!(
        !std::path::Path::new("/mnt/lode-backups").exists(),
        "dry run changes nothing"
    );
}
