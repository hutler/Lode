//! T177 acceptance: `loded completions <shell>` writes a script for each shell that names the
//! subcommands.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;

#[test]
fn every_shell_gets_a_script_naming_the_subcommands() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let out = Command::cargo_bin("loded")
            .unwrap()
            .args(["completions", shell])
            .output()
            .unwrap();
        assert!(out.status.success(), "{shell}: {out:?}");
        let script = String::from_utf8(out.stdout).unwrap();
        for sub in ["setup", "serve", "reset-admin", "backup", "completions"] {
            assert!(script.contains(sub), "{shell} script lacks {sub}");
        }
    }
    let bad = Command::cargo_bin("loded")
        .unwrap()
        .args(["completions", "cmd.exe"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
}
