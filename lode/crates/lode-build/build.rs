//! Works out the build version from Git, so that every commit on `main` (every push) builds a
//! program that says which commit it came from.
//!
//! - `LODE_BUILD_VERSION`, when set, is used as it is (packaging that builds without Git).
//! - On the commit tagged `lode-v<version>`: the version alone, such as `0.2.0`. Releases keep
//!   their plain numbers.
//! - After it: the version, the number of commits since the newest `lode-v*` tag and the commit,
//!   as SemVer build metadata, such as `0.2.0+5.g4a33c89`.
//! - Without a reachable tag (a shallow clone): `0.2.0+g4a33c89`.
//! - Without Git or outside a repository (a source archive): the version alone.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=LODE_BUILD_VERSION");
    let base = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let version = match std::env::var("LODE_BUILD_VERSION") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_owned(),
        _ => {
            watch_git();
            from_git(&base).unwrap_or(base)
        }
    };
    println!("cargo:rustc-env=LODE_BUILD_VERSION={version}");
}

fn git(args: &[&str]) -> Option<String> {
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let out = Command::new("git")
        .args(args)
        .current_dir(manifest_dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn from_git(base: &str) -> Option<String> {
    let sha = git(&["rev-parse", "--short=7", "HEAD"])?;
    let Some(described) = git(&[
        "describe",
        "--tags",
        "--long",
        "--abbrev=7",
        "--match",
        "lode-v*",
        "HEAD",
    ]) else {
        return Some(format!("{base}+g{sha}"));
    };
    // `lode-v0.2.0-5-g4a33c89`; the tag itself may contain dashes (`lode-v1.0.0-rc1`).
    let mut parts = described.rsplitn(3, '-');
    let _hash = parts.next()?;
    let count: u64 = parts.next()?.parse().ok()?;
    let tag = parts.next()?;
    if count == 0 && tag.strip_prefix("lode-v") == Some(base) {
        return Some(base.to_owned());
    }
    Some(format!("{base}+{count}.g{sha}"))
}

/// Rebuilds when HEAD moves (a commit, a checkout, a pull) or tags change.
fn watch_git() {
    let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]).map(PathBuf::from) else {
        return;
    };
    // In a worktree, refs and tags live in the common directory.
    let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.clone());
    watch(&dir.join("HEAD"));
    watch(&common.join("packed-refs"));
    watch(&common.join("refs").join("tags"));
    if let Some(head_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watch(&common.join(head_ref));
    }
}

fn watch(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
