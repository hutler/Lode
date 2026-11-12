use assert_cmd::Command;

#[test]
fn prints_workspace_version() {
    let out = Command::cargo_bin("loded")
        .unwrap()
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(text.trim(), concat!("loded ", env!("CARGO_PKG_VERSION")));
}
