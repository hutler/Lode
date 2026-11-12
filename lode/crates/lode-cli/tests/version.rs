use assert_cmd::Command;

#[test]
fn prints_workspace_version() {
    let out = Command::cargo_bin("lode")
        .unwrap()
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(text.trim(), concat!("lode ", env!("CARGO_PKG_VERSION")));
}
