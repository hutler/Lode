//! Compiles the Slint screens, works out the build version, and on Windows gives the program its
//! icon and a manifest.
//!
//! The build version (`DESK_BUILD_VERSION`, shown in About) follows Desk's own releases, tagged
//! `desk-v<version>`, the way the Lode programs follow `lode-v*`:
//!
//! - `DESK_BUILD_VERSION`, when set, is used as it is (packaging that builds without Git).
//! - On the commit tagged `desk-v<version>`: the version alone, such as `0.3.0`.
//! - After it: the version, the commits since the newest `desk-v*` tag and the commit, such as
//!   `0.3.0+5.g4a33c89`; without a reachable tag `0.3.0+g4a33c89`; without Git the version alone.
//!
//! The manifest asks for Common Controls version 6 (the folder picker's dialogs need it) and
//! tells Windows the app handles display scaling and long paths. Tests and examples get it
//! too, so they start the way the app does.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    build_version();
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    #[cfg(windows)]
    if target_os == "windows" {
        println!("cargo:rerun-if-changed=packaging/windows/lode-desk.rc");
        println!("cargo:rerun-if-changed=packaging/windows/lode-desk.manifest");
        println!("cargo:rerun-if-changed=icons/icon.ico");
        if let Err(e) = embed_resource::compile_for_everything(
            "packaging/windows/lode-desk.rc",
            embed_resource::NONE,
        )
        .manifest_required()
        {
            println!("cargo:warning=cannot embed the icon and manifest: {e}");
            std::process::exit(1);
        }
    }
    // Windows 11 style everywhere (Slint's `native` style would need Qt); macOS gets its own.
    let style = if target_os == "macos" {
        "cupertino"
    } else {
        "fluent"
    };
    // Debug builds keep the element names the UI tests find controls by.
    let debug = std::env::var("PROFILE").map_or(true, |p| p != "release");
    let config = slint_build::CompilerConfiguration::new()
        .with_style(style.into())
        .with_debug_info(debug);
    if let Err(e) = slint_build::compile_with_config("ui/slint/app.slint", config) {
        println!("cargo:warning=cannot compile the Slint screens: {e}");
        std::process::exit(1);
    }
}

fn build_version() {
    println!("cargo:rerun-if-env-changed=DESK_BUILD_VERSION");
    let base = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let version = match std::env::var("DESK_BUILD_VERSION") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_owned(),
        _ => {
            watch_git();
            from_git(&base).unwrap_or(base)
        }
    };
    println!("cargo:rustc-env=DESK_BUILD_VERSION={version}");
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
        "desk-v*",
        "HEAD",
    ]) else {
        return Some(format!("{base}+g{sha}"));
    };
    // `desk-v0.3.0-5-g4a33c89`; the tag itself may contain dashes (`desk-v1.0.0-rc1`).
    let mut parts = described.rsplitn(3, '-');
    let _hash = parts.next()?;
    let count: u64 = parts.next()?.parse().ok()?;
    let tag = parts.next()?;
    if count == 0 && tag.strip_prefix("desk-v") == Some(base) {
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
