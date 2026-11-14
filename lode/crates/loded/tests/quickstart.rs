//! T087 acceptance: the quick start, end to end with the real programs. `loded setup` with
//! flags, the server started from its configuration, `lode init` with a template on one
//! machine, a second user created by the admin, and that user checking out the depot.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use assert_cmd::cargo::CommandCargoExt;
use lode_testkit::TestWorkspace;

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Every file under `root` except `.lode/`, by relative path.
fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            let rel = e
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel == ".lode" {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                todo.push(e.path());
            } else {
                out.insert(rel, std::fs::read(e.path()).unwrap());
            }
        }
    }
    out
}

fn login(ws: &TestWorkspace, url: &str, name: &str, password: &str) {
    ws.cmd()
        .args(["login", url, "--name", name, "--password-stdin"])
        .write_stdin(format!("{password}\n"))
        .assert()
        .success();
}

#[test]
fn quick_start_from_setup_to_a_second_checkout() {
    // The server: `loded setup` with flags, then `loded serve`.
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let pw = dir.path().join("pw.txt");
    std::fs::write(&pw, "admin password 1\n").unwrap();
    let out = Command::cargo_bin("loded")
        .unwrap()
        .args(["setup", "--yes", "--local", "--listen", "127.0.0.1:0"])
        .arg("--data-dir")
        .arg(&data)
        .arg("--admin-password-file")
        .arg(&pw)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let mut server = KillOnDrop(
        Command::cargo_bin("loded")
            .unwrap()
            .arg("serve")
            .arg("--config")
            .arg(data.join("loded.toml"))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(server.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let url = line
        .trim()
        .strip_prefix("loded listening on ")
        .unwrap_or_else(|| panic!("unexpected first line: {line:?}"))
        .to_owned();

    // The admin's machine: an existing Unreal project becomes the depot.
    let admin = TestWorkspace::detached();
    login(&admin, &url, "admin", "admin password 1");
    admin.write("Game.uproject", "{}\n");
    admin.write("Source/Game/Game.cpp", "int main() {}\n");
    admin.write("Content/Maps/Start.umap", vec![7u8; 4096]);
    admin.write("Binaries/Win64/Game.exe", "built");
    admin.write("Intermediate/Build/x.obj", "built");
    let init = admin.json(&["init", &format!("{url}/game"), "--template", "unreal"]);
    assert_eq!(init["rev"], 2, "{init}");
    assert_eq!(
        init["files"], 5,
        "three project files and the two template files: {init}"
    );

    // The admin adds a user, as in the guide.
    admin
        .cmd()
        .args(["admin", "user", "create", "alice", "--password-stdin"])
        .write_stdin("alice password 1\n")
        .assert()
        .success();
    admin.json(&[
        "admin",
        "acl",
        "add",
        "allow",
        "write",
        "user:alice",
        "--depot",
        "game",
    ]);

    // Alice, on another machine, checks it out and gets the same tree.
    let alice = TestWorkspace::detached();
    login(&alice, &url, "alice", "alice password 1");
    alice.json(&["checkout", &format!("{url}/game"), "."]);
    let mut expected = files(admin.root());
    expected.retain(|p, _| !p.starts_with("Binaries/") && !p.starts_with("Intermediate/"));
    assert_eq!(expected.len(), 5);
    assert_eq!(files(alice.root()), expected);

    // And her change reaches the admin.
    alice.write("Source/Game/Game.cpp", "int main() { return 0; }\n");
    alice.json(&["commit", "-m", "First change"]);
    admin.json(&["update"]);
    assert_eq!(
        admin.read("Source/Game/Game.cpp"),
        b"int main() { return 0; }\n"
    );
}
