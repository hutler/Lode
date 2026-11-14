//! `lode self-update [--check]` (T084): replaces this `lode` with the newest release for
//! this platform.
//!
//! The release list comes from GitHub (`LODE_UPDATE_URL` points elsewhere, for mirrors and
//! tests). Each release carries the bare executable for every platform,
//! `lode-<version>-<target>[.exe]`, and a `SHA256SUMS` file; the download must match its
//! line there or nothing changes. The new executable is written next to the old one and
//! renamed over it (on Windows the running one is first renamed to `lode.old.exe`, which the
//! next update removes). A `lode` installed by a package manager is left to that manager.

use std::io::Write;
use std::path::{Path, PathBuf};

use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::context::Ctx;
use crate::error::{OpError, OpResult};

/// Where releases are listed unless `LODE_UPDATE_URL` says otherwise.
pub const RELEASES_URL: &str = "https://api.github.com/repos/hutler/Lode/releases?per_page=100";

/// Largest executable accepted.
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

/// `lode self-update` options.
#[derive(Debug, clap::Args)]
pub struct SelfUpdateArgs {
    /// Only report whether a newer release exists.
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// How this `lode` was installed, when something other than `self-update` owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    /// A Debian package.
    Apt,
    /// Another system package (RPM and the like).
    SystemPackage,
    /// Homebrew.
    Homebrew,
    /// The Windows installer (directly or through winget).
    WindowsInstaller,
}

impl Manager {
    fn advice(self) -> &'static str {
        match self {
            Manager::Apt => "it was installed with apt; update it with `sudo apt update && sudo apt install --only-upgrade lode`, or rerun the install script",
            Manager::SystemPackage => "it was installed by the system package manager; update it with `sudo dnf upgrade lode` (or your distribution's equivalent), or rerun the install script",
            Manager::Homebrew => "it was installed with Homebrew; update it with `brew upgrade lode`",
            Manager::WindowsInstaller => "it was installed with the Windows installer; update it with `winget upgrade SimpleDevOps.Lode`, or install the newer MSI",
        }
    }
}

/// The package manager that owns the executable at `exe`, if any. `program_files` is the
/// Windows Program Files folder.
pub fn manager_of(exe: &Path, program_files: Option<&Path>) -> Option<Manager> {
    let text = exe.to_string_lossy().replace('\\', "/");
    if text.contains("/Cellar/")
        || text.starts_with("/opt/homebrew/")
        || text.contains("/linuxbrew/")
    {
        return Some(Manager::Homebrew);
    }
    if let Some(pf) = program_files {
        if exe.parent() == Some(pf.join("Lode").as_path()) {
            return Some(Manager::WindowsInstaller);
        }
    }
    if text.starts_with("/usr/bin/") || text.starts_with("/bin/") {
        let dpkg = Path::new("/var/lib/dpkg/info/lode.list");
        return Some(if dpkg.is_file() {
            Manager::Apt
        } else {
            Manager::SystemPackage
        });
    }
    None
}

/// The release target name of this build, as the release assets use it.
pub fn release_target() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        _ => return None,
    })
}

/// `1.2.3` as numbers; anything else is not a release version.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.split('.');
    let n = |p: Option<&str>| p.and_then(|s| s.parse::<u64>().ok());
    let version = (n(parts.next())?, n(parts.next())?, n(parts.next())?);
    parts.next().is_none().then_some(version)
}

fn failed(code: &str, message: impl Into<String>) -> OpError {
    OpError::failed(code, message)
}

fn http_client() -> OpResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("lode/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| failed("update_failed", format!("cannot start an HTTP client: {e}")))
}

/// Refuses plain HTTP except to this machine.
fn check_url(url: &str) -> OpResult<reqwest::Url> {
    let u = reqwest::Url::parse(url)
        .map_err(|e| failed("update_failed", format!("bad update URL `{url}`: {e}")))?;
    let loopback = matches!(
        u.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
    );
    if u.scheme() != "https" && !(u.scheme() == "http" && loopback) {
        return Err(failed(
            "update_failed",
            format!("refusing to update over `{url}`: use https"),
        ));
    }
    Ok(u)
}

async fn get(client: &reqwest::Client, url: &str) -> OpResult<reqwest::Response> {
    let resp = client
        .get(check_url(url)?)
        .send()
        .await
        .map_err(|e| failed("update_failed", format!("cannot reach {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(failed(
            "update_failed",
            format!("{url} answered {}", resp.status()),
        ));
    }
    Ok(resp)
}

/// The newest release with a parsable `lode-v<version>` tag that is not a draft or a
/// pre-release.
fn newest(releases: Vec<Release>) -> Option<((u64, u64, u64), Release)> {
    releases
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| {
            let v = parse_version(r.tag_name.strip_prefix("lode-v")?)?;
            Some((v, r))
        })
        .max_by_key(|(v, _)| *v)
}

/// Downloads `url` into `dest`, returning the SHA-256 of what arrived.
async fn download(ctx: &Ctx, client: &reqwest::Client, url: &str, dest: &Path) -> OpResult<String> {
    let resp = get(client, url).await?;
    let mut progress = ctx
        .out
        .progress("Downloading", resp.content_length().unwrap_or(0));
    let mut file = std::fs::File::create(dest)?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut body = resp.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk =
            chunk.map_err(|e| failed("update_failed", format!("download interrupted: {e}")))?;
        total += chunk.len() as u64;
        if total > MAX_DOWNLOAD {
            return Err(failed("update_failed", "the download is too large"));
        }
        hasher.update(&chunk);
        file.write_all(&chunk)?;
        progress.step(chunk.len() as u64);
    }
    file.sync_all()?;
    progress.finish();
    Ok(hex::encode(hasher.finalize()))
}

/// The hash `SHA256SUMS` gives for `name`.
fn expected_hash(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        (file == name).then(|| hash.to_ascii_lowercase())
    })
}

/// Puts `new` in place of `exe`.
fn replace(exe: &Path, new: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(new, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(new, exe)
    }
    #[cfg(not(unix))]
    {
        // A running executable cannot be overwritten on Windows, but it can be renamed.
        let old = old_path(exe);
        let _ = std::fs::remove_file(&old);
        std::fs::rename(exe, &old)?;
        if let Err(e) = std::fs::rename(new, exe) {
            let _ = std::fs::rename(&old, exe);
            return Err(e);
        }
        Ok(())
    }
}

/// Where the replaced executable is left on Windows until the next update.
fn old_path(exe: &Path) -> PathBuf {
    exe.with_extension("old.exe")
}

/// Runs `lode self-update`.
pub async fn self_update(ctx: &Ctx, args: SelfUpdateArgs) -> OpResult<Value> {
    let exe = std::env::current_exe()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    if cfg!(windows) {
        let _ = std::fs::remove_file(old_path(&exe));
    }
    let current = env!("CARGO_PKG_VERSION");
    let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
    if let Some(m) = manager_of(&exe, program_files.as_deref()) {
        if !args.check {
            return Err(failed(
                "package_managed",
                format!(
                    "lode at {} is not updated by itself: {}",
                    exe.display(),
                    m.advice()
                ),
            ));
        }
    }
    let target = release_target()
        .ok_or_else(|| failed("update_failed", "no releases are built for this platform"))?;

    let url = std::env::var("LODE_UPDATE_URL").unwrap_or_else(|_| RELEASES_URL.to_owned());
    let client = http_client()?;
    let releases: Vec<Release> = get(&client, &url)
        .await?
        .json()
        .await
        .map_err(|e| failed("update_failed", format!("unexpected release list: {e}")))?;
    let Some((latest, release)) = newest(releases) else {
        return Err(failed("update_failed", "no Lode release found"));
    };
    let latest_text = format!("{}.{}.{}", latest.0, latest.1, latest.2);
    let newer = parse_version(current).is_none_or(|c| latest > c);
    if args.check || !newer {
        ctx.out.line(if newer {
            format!("Lode {latest_text} is available (this is {current}); run `lode self-update`.")
        } else {
            format!("lode {current} is up to date.")
        });
        return Ok(json!({
            "current": current,
            "latest": latest_text,
            "update_available": newer,
            "updated": false,
            "path": exe,
        }));
    }

    let name = format!(
        "lode-{latest_text}-{target}{}",
        std::env::consts::EXE_SUFFIX
    );
    let asset = |n: &str| {
        release
            .assets
            .iter()
            .find(|a| a.name == n)
            .map(|a| a.browser_download_url.clone())
            .ok_or_else(|| {
                failed(
                    "update_failed",
                    format!("release {} has no {n}", release.tag_name),
                )
            })
    };
    let (bin_url, sums_url) = (asset(&name)?, asset("SHA256SUMS")?);
    let sums = get(&client, &sums_url)
        .await?
        .text()
        .await
        .map_err(|e| failed("update_failed", format!("cannot read SHA256SUMS: {e}")))?;
    let want = expected_hash(&sums, &name)
        .ok_or_else(|| failed("update_failed", format!("SHA256SUMS does not list {name}")))?;

    let dir = exe
        .parent()
        .ok_or_else(|| failed("update_failed", "cannot tell where lode is"))?;
    let tmp = dir.join(format!(".lode-update-{}.tmp", std::process::id()));
    let got = match download(ctx, &client, &bin_url, &tmp).await {
        Ok(h) => h,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    if got != want {
        let _ = std::fs::remove_file(&tmp);
        return Err(failed(
            "checksum_mismatch",
            format!("the download of {name} does not match SHA256SUMS (got {got}, expected {want}); nothing was changed"),
        ));
    }
    if let Err(e) = replace(&exe, &tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(failed(
            "update_failed",
            format!("cannot replace {}: {e}", exe.display()),
        ));
    }
    ctx.out.line(format!(
        "Updated lode {current} to {latest_text} ({}).",
        exe.display()
    ));
    Ok(json!({
        "current": current,
        "latest": latest_text,
        "update_available": true,
        "updated": true,
        "path": exe,
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn versions_and_sums_parse() {
        assert_eq!(parse_version("0.10.2"), Some((0, 10, 2)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3-rc1"), None);
        let sums = "abc  lode-0.2.0-x86_64-unknown-linux-musl\nDEF *lode-0.2.0-x86_64-pc-windows-msvc.exe\n";
        assert_eq!(
            expected_hash(sums, "lode-0.2.0-x86_64-pc-windows-msvc.exe").as_deref(),
            Some("def")
        );
        assert_eq!(expected_hash(sums, "lode-0.2.0").as_deref(), None);
    }

    #[test]
    fn package_managers_are_recognized() {
        let pf = Path::new("C:/Program Files");
        assert_eq!(
            manager_of(Path::new("C:/Program Files/Lode/lode.exe"), Some(pf)),
            Some(Manager::WindowsInstaller)
        );
        assert_eq!(
            manager_of(Path::new("/opt/homebrew/Cellar/lode/0.1.0/bin/lode"), None),
            Some(Manager::Homebrew)
        );
        assert!(matches!(
            manager_of(Path::new("/usr/bin/lode"), None),
            Some(Manager::Apt | Manager::SystemPackage)
        ));
        assert_eq!(manager_of(Path::new("/home/ana/bin/lode"), None), None);
        assert_eq!(manager_of(Path::new("C:/Tools/lode.exe"), Some(pf)), None);
    }

    #[test]
    fn the_newest_stable_lode_release_wins() {
        let r = |tag: &str, pre: bool| Release {
            tag_name: tag.into(),
            draft: false,
            prerelease: pre,
            assets: Vec::new(),
        };
        let (v, rel) = newest(vec![
            r("lode-v0.2.0", false),
            r("lode-v0.10.0", false),
            r("lode-v0.11.0", true),
            r("umpalumpa-v9.0.0", false),
        ])
        .unwrap();
        assert_eq!((v, rel.tag_name.as_str()), ((0, 10, 0), "lode-v0.10.0"));
    }
}
