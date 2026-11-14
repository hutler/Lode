//! Updates of the app itself, from the feed on the project's GitHub releases (T114, T135).
//!
//! The window checks at start and once a day unless the "check for updates" setting is off; the
//! `LODE_DESK_NO_UPDATE` environment variable turns every check off for managed machines. The
//! feed is the `latest.json` the release workflow writes (see [`write_feed`]): a version, its
//! notes, and per platform a download address and a minisign signature. A package is installed
//! only when its signature matches the public key built into the app (`LODE_DESK_UPDATE_PUBKEY`
//! at build time); a build without a key never offers updates.
//!
//! Keys and signatures are minisign's, base64-wrapped the way `cargo packager signer` (and
//! `tauri signer` before it) writes them, so a key made for the earlier Tauri app keeps working.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::desk::ErrorView;

/// Set (to anything but an empty value) to turn update checks off.
pub const NO_UPDATE_ENV: &str = "LODE_DESK_NO_UPDATE";

/// Set to a feed address to check that feed instead of the project's (a test feed). Packages
/// must still be signed with the built-in key.
pub const FEED_ENV: &str = "LODE_DESK_UPDATE_FEED";

/// Where installed copies look for a new version: one fixed release, so a release of another
/// product never hides it.
pub const FEED_URL: &str =
    "https://github.com/hutler/Lode/releases/download/lode-desk-feed/latest.json";

/// The update public key built into this copy; empty when the build had none.
pub fn built_in_pubkey() -> &'static str {
    option_env!("LODE_DESK_UPDATE_PUBKEY").unwrap_or("").trim()
}

/// The app's own settings, kept in `settings.json` in its data directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Look for a new version at start and once a day.
    pub check_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            check_updates: true,
        }
    }
}

fn io_error(what: &str, e: impl std::fmt::Display) -> ErrorView {
    ErrorView {
        code: "io".into(),
        message: format!("{what}: {e}"),
        details: Value::Null,
    }
}

fn failed(message: impl Into<String>) -> ErrorView {
    ErrorView {
        code: "update_failed".into(),
        message: message.into(),
        details: Value::Null,
    }
}

fn bad_signature(why: impl std::fmt::Display) -> ErrorView {
    ErrorView {
        code: "bad_signature".into(),
        message: "the update is not signed with the Lode Desk update key, so it was not installed"
            .into(),
        details: Value::String(why.to_string()),
    }
}

/// The settings in `file`; the defaults when it is missing or unreadable.
pub fn load_settings(file: &Path) -> Settings {
    std::fs::read(file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Writes `settings` to `file`, replacing it whole.
pub fn save_settings(file: &Path, settings: &Settings) -> Result<(), ErrorView> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io_error("cannot save the settings", e))?;
    }
    let text =
        serde_json::to_vec_pretty(settings).map_err(|e| io_error("cannot save the settings", e))?;
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| io_error("cannot save the settings", e))?;
    std::fs::rename(&tmp, file).map_err(|e| io_error("cannot save the settings", e))
}

/// What an update check found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum UpdateCheck {
    /// Checks are turned off.
    Off {
        /// Why, in words.
        reason: String,
    },
    /// This build has no update key, so it never updates itself.
    Unconfigured,
    /// The newest version is running.
    Current {
        /// The running version.
        current: String,
    },
    /// A newer version can be installed.
    Available {
        /// The running version.
        current: String,
        /// The new version.
        version: String,
        /// Its release notes.
        notes: String,
        /// When it was published (RFC 3339), if the feed says.
        date: Option<String>,
    },
}

/// Why checks are off: the environment variable, or (unless the user asked) the setting.
pub fn off_reason(settings: &Settings, env: Option<&OsStr>, asked: bool) -> Option<String> {
    if env.is_some_and(|v| !v.is_empty()) {
        return Some(format!(
            "updates are turned off on this computer ({NO_UPDATE_ENV} is set)"
        ));
    }
    if !asked && !settings.check_updates {
        return Some("checking for updates is turned off in the settings".into());
    }
    None
}

// --- The feed ------------------------------------------------------------------------------

/// The feed file, `latest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feed {
    /// The newest version, `MAJOR.MINOR.PATCH`.
    pub version: String,
    /// Its release notes.
    #[serde(default)]
    pub notes: String,
    /// When it was published (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pub_date: Option<String>,
    /// Per updater platform (such as `windows-x86_64-msi`), where the package is and its
    /// signature.
    pub platforms: std::collections::BTreeMap<String, Package>,
}

/// One downloadable package of the feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    /// The `.sig` file's text, base64-wrapped.
    pub signature: String,
    /// Where to download it.
    pub url: String,
}

/// The release files the feed can point at, by the end of their names
/// (`lode-desk-VERSION-<suffix>`), and the updater platforms each one serves. The first platform
/// of a kind is also the fallback for installs whose kind is not known.
pub const BUNDLES: &[(&str, &[&str])] = &[
    (
        "linux-x86_64.AppImage",
        &["linux-x86_64", "linux-x86_64-appimage"],
    ),
    ("linux-x86_64.deb", &["linux-x86_64-deb"]),
    (
        "windows-x86_64.msi",
        &["windows-x86_64", "windows-x86_64-msi"],
    ),
    ("windows-x86_64-setup.exe", &["windows-x86_64-nsis"]),
    (
        "macos-universal.app.tar.gz",
        &[
            "darwin-x86_64",
            "darwin-aarch64",
            "darwin-x86_64-app",
            "darwin-aarch64-app",
        ],
    ),
];

/// A release version `MAJOR.MINOR.PATCH`, or a running version that may carry `-suffix`.
fn version_numbers(text: &str) -> Option<(u64, u64, u64, bool)> {
    let (core, pre) = match text.split_once(['-', '+']) {
        Some((c, _)) => (c, true),
        None => (text, false),
    };
    let mut parts = core.split('.').map(|p| {
        (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            .then(|| p.parse::<u64>().ok())
            .flatten()
    });
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some((v.0, v.1, v.2, pre))
}

/// Whether `offered` is newer than `running`. A pre-release of a version is older than it.
pub fn is_newer(offered: &str, running: &str) -> bool {
    match (version_numbers(offered), version_numbers(running)) {
        (Some((a, b, c, p)), Some((x, y, z, q))) => {
            (a, b, c) > (x, y, z) || ((a, b, c) == (x, y, z) && q && !p)
        }
        _ => false,
    }
}

/// Writes the feed for the release files in `files` (name to the `.sig` file's text, or `None`
/// when unsigned), which are downloaded from `base_url`. Unsigned files are left out.
pub fn write_feed(
    version: &str,
    base_url: &str,
    notes: &str,
    date: &str,
    files: &std::collections::BTreeMap<String, Option<String>>,
) -> Result<Feed, String> {
    if !matches!(version_numbers(version), Some((_, _, _, false))) {
        return Err(format!("`{version}` is not a release version"));
    }
    let base = base_url.trim_end_matches('/');
    if !base.starts_with("https://") {
        return Err("the download address must be https".into());
    }
    let mut platforms = std::collections::BTreeMap::new();
    for (name, signature) in files {
        let Some(signature) = signature else { continue };
        let Some((_, keys)) = BUNDLES
            .iter()
            .find(|(suffix, _)| *name == format!("lode-desk-{version}-{suffix}"))
        else {
            continue;
        };
        for key in *keys {
            platforms.insert(
                (*key).to_owned(),
                Package {
                    signature: signature.trim().to_owned(),
                    url: format!("{base}/{}", crate::links::encode(name)),
                },
            );
        }
    }
    if platforms.is_empty() {
        return Err("no signed bundle to put in the feed".into());
    }
    Ok(Feed {
        version: version.to_owned(),
        notes: notes.to_owned(),
        pub_date: Some(date.to_owned()),
        platforms,
    })
}

// --- Signatures ----------------------------------------------------------------------------

fn unwrap_base64(what: &str, text: &str) -> Result<String, ErrorView> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .map_err(|e| bad_signature(format!("the {what} is not base64: {e}")))?;
    String::from_utf8(bytes).map_err(|_| bad_signature(format!("the {what} is not text")))
}

/// Checks that `signature` (a `.sig` file's text, base64-wrapped) signs `data` with `pubkey`
/// (a public key file's text, base64-wrapped).
pub fn verify(pubkey: &str, signature: &str, data: &[u8]) -> Result<(), ErrorView> {
    let key = minisign_verify::PublicKey::decode(&unwrap_base64("update key", pubkey)?)
        .map_err(|e| bad_signature(format!("the update key cannot be read: {e}")))?;
    let sig = minisign_verify::Signature::decode(&unwrap_base64("signature", signature)?)
        .map_err(|e| bad_signature(format!("the signature cannot be read: {e}")))?;
    key.verify(data, &sig, true).map_err(bad_signature)
}

// --- How this copy was installed -----------------------------------------------------------

/// How the running copy was installed, which decides the package it updates from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    /// A Linux AppImage at this path (`$APPIMAGE`).
    AppImage(PathBuf),
    /// The Linux `.deb` package.
    Deb,
    /// The Windows MSI (for all users, in Program Files).
    Msi,
    /// The Windows setup program (for the current user).
    Nsis,
    /// The macOS app bundle at this path.
    MacApp(PathBuf),
    /// Not from an installer (a build from source, or a copied program).
    Unknown,
}

/// How a copy at `exe` was installed, on `os` (`std::env::consts::OS`), with `appimage` the
/// `APPIMAGE` environment variable.
pub fn install_kind(exe: &Path, os: &str, appimage: Option<&OsStr>) -> InstallKind {
    match os {
        "linux" => match appimage.filter(|v| !v.is_empty()) {
            Some(p) => InstallKind::AppImage(PathBuf::from(p)),
            None if exe.starts_with("/usr/") => InstallKind::Deb,
            None => InstallKind::Unknown,
        },
        "windows" => {
            let dir = exe.parent();
            if dir.is_some_and(|d| d.join("uninstall.exe").is_file()) {
                InstallKind::Nsis
            } else if exe
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("\\program files")
            {
                InstallKind::Msi
            } else {
                InstallKind::Unknown
            }
        }
        "macos" => exe
            .ancestors()
            .find(|a| a.extension().is_some_and(|e| e == "app"))
            .map_or(InstallKind::Unknown, |b| {
                InstallKind::MacApp(b.to_path_buf())
            }),
        _ => InstallKind::Unknown,
    }
}

/// The running copy's install kind.
pub fn this_install() -> InstallKind {
    match std::env::current_exe() {
        Ok(exe) => install_kind(
            &exe,
            std::env::consts::OS,
            std::env::var_os("APPIMAGE").as_deref(),
        ),
        Err(_) => InstallKind::Unknown,
    }
}

/// The feed platforms that fit `kind` on `os` and `arch`, best first.
pub fn platforms(kind: &InstallKind, os: &str, arch: &str) -> Vec<String> {
    let os = if os == "macos" { "darwin" } else { os };
    let base = format!("{os}-{arch}");
    let own = match kind {
        InstallKind::AppImage(_) => Some("appimage"),
        InstallKind::Deb => Some("deb"),
        InstallKind::Msi => Some("msi"),
        InstallKind::Nsis => Some("nsis"),
        InstallKind::MacApp(_) => Some("app"),
        InstallKind::Unknown => None,
    };
    let mut keys: Vec<String> = own.map(|k| format!("{base}-{k}")).into_iter().collect();
    keys.push(base);
    keys
}

// --- Checking and downloading ---------------------------------------------------------------

/// A newer version found in the feed, with the package for this copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The new version.
    pub version: String,
    /// The package and its signature.
    pub package: Package,
}

/// Where and how this copy looks for updates.
#[derive(Debug, Clone)]
pub struct Updater {
    /// The public key packages must be signed with; empty for none.
    pub pubkey: String,
    /// The feed address.
    pub feed: String,
    /// The running version.
    pub current: String,
    /// The feed platforms that fit this copy, best first.
    pub platforms: Vec<String>,
    /// `settings.json`.
    pub settings_file: PathBuf,
}

impl Updater {
    /// This copy: the built-in key, the project's feed (or `LODE_DESK_UPDATE_FEED`), its
    /// version and install kind.
    pub fn for_this_copy(settings_file: PathBuf) -> Self {
        Self {
            pubkey: built_in_pubkey().to_owned(),
            feed: std::env::var(FEED_ENV)
                .ok()
                .filter(|f| !f.trim().is_empty())
                .unwrap_or_else(|| FEED_URL.to_owned()),
            current: env!("CARGO_PKG_VERSION").to_owned(),
            platforms: platforms(
                &this_install(),
                std::env::consts::OS,
                std::env::consts::ARCH,
            ),
            settings_file,
        }
    }

    /// Asks the feed for a newer version. `asked` is a check the user asked for, which the
    /// setting does not stop; `env` is `LODE_DESK_NO_UPDATE`, which stops every check.
    pub async fn check(
        &self,
        asked: bool,
        env: Option<&OsStr>,
    ) -> Result<(UpdateCheck, Option<Found>), ErrorView> {
        if let Some(reason) = off_reason(&load_settings(&self.settings_file), env, asked) {
            return Ok((UpdateCheck::Off { reason }, None));
        }
        if self.pubkey.is_empty() {
            return Ok((UpdateCheck::Unconfigured, None));
        }
        let body = get(&self.feed, Duration::from_secs(30)).await?;
        let feed: Feed = serde_json::from_slice(&body)
            .map_err(|e| failed(format!("the update feed cannot be read: {e}")))?;
        let current = self.current.clone();
        if !is_newer(&feed.version, &current) {
            return Ok((UpdateCheck::Current { current }, None));
        }
        let Some(package) = self
            .platforms
            .iter()
            .find_map(|p| feed.platforms.get(p).cloned())
        else {
            // A version without a package for this system is not offered.
            return Ok((UpdateCheck::Current { current }, None));
        };
        Ok((
            UpdateCheck::Available {
                current,
                version: feed.version.clone(),
                notes: feed.notes,
                date: feed.pub_date,
            },
            Some(Found {
                version: feed.version,
                package,
            }),
        ))
    }

    /// Downloads the package `found` names and checks its signature against the key.
    pub async fn download(&self, found: &Found) -> Result<Vec<u8>, ErrorView> {
        let bytes = get(&found.package.url, Duration::from_secs(30 * 60)).await?;
        verify(&self.pubkey, &found.package.signature, &bytes)?;
        Ok(bytes)
    }
}

/// Whether `url` may be fetched: https, or http to this computer (a test feed).
fn allowed(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', '?']).next().unwrap_or("");
    let name = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => host.split(':').next().unwrap_or(""),
    };
    matches!(name, "127.0.0.1" | "localhost" | "::1")
}

async fn get(url: &str, timeout: Duration) -> Result<Vec<u8>, ErrorView> {
    if !allowed(url) {
        return Err(failed(format!(
            "`{url}` is not an https address, so updates are not taken from it"
        )));
    }
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(concat!("lode-desk/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| failed(format!("cannot check for updates: {e}")))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| failed(format!("cannot reach {url}: {e}")))?;
    if !response.status().is_success() {
        return Err(failed(format!(
            "{url} answered {}",
            response.status().as_u16()
        )));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| failed(format!("cannot download {url}: {e}")))?;
    Ok(bytes.to_vec())
}

// --- Installing ----------------------------------------------------------------------------

/// Replaces the AppImage at `path` with `bytes`, keeping it runnable.
pub fn replace_appimage(path: &Path, bytes: &[u8]) -> Result<(), ErrorView> {
    let new = path.with_extension("AppImage.new");
    std::fs::write(&new, bytes).map_err(|e| io_error("cannot write the update", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| io_error("cannot write the update", e))?;
    }
    std::fs::rename(&new, path).map_err(|e| io_error("cannot replace the app", e))
}

fn temp_file(name: &str, bytes: &[u8]) -> Result<PathBuf, ErrorView> {
    let dir = std::env::temp_dir().join("lode-desk-update");
    std::fs::create_dir_all(&dir).map_err(|e| io_error("cannot write the update", e))?;
    let file = dir.join(name);
    std::fs::write(&file, bytes).map_err(|e| io_error("cannot write the update", e))?;
    Ok(file)
}

fn started(r: std::io::Result<std::process::Child>) -> Result<(), ErrorView> {
    r.map(|_| ())
        .map_err(|e| failed(format!("cannot start the installer: {e}")))
}

/// Installs the checked package `bytes` for an install of `kind`, then starts the new version.
/// On Windows the installer runs after this returns, so the caller exits at once.
pub fn install(kind: &InstallKind, bytes: &[u8]) -> Result<(), ErrorView> {
    let exe = std::env::current_exe().map_err(|e| failed(format!("cannot find the app: {e}")))?;
    match kind {
        InstallKind::AppImage(path) => {
            replace_appimage(path, bytes)?;
            started(std::process::Command::new(path).spawn())
        }
        InstallKind::Deb => {
            let deb = temp_file("lode-desk.deb", bytes)?;
            let status = std::process::Command::new("pkexec")
                .arg("dpkg")
                .arg("-i")
                .arg(&deb)
                .status()
                .map_err(|e| failed(format!("cannot start the installer: {e}")))?;
            if !status.success() {
                return Err(failed(
                    "the package was not installed (pkexec dpkg -i did not finish)",
                ));
            }
            started(std::process::Command::new(&exe).spawn())
        }
        InstallKind::Msi | InstallKind::Nsis => windows_install(kind, bytes, &exe),
        InstallKind::MacApp(bundle) => mac_install(bundle, bytes),
        InstallKind::Unknown => Err(failed(
            "this copy was not installed by a Lode Desk installer, so it cannot update itself; \
             download the new version from the project's releases",
        )),
    }
}

#[cfg(windows)]
fn windows_install(kind: &InstallKind, bytes: &[u8], exe: &Path) -> Result<(), ErrorView> {
    use std::os::windows::process::CommandExt;
    if *kind == InstallKind::Nsis {
        // Passive, and the setup program starts the new version when it is done.
        let setup = temp_file("lode-desk-setup.exe", bytes)?;
        return started(std::process::Command::new(setup).args(["/P", "/R"]).spawn());
    }
    // `cmd /S /C "…"` drops exactly the outer quotes. The MSI installs once the app has had two
    // seconds to exit, then the new version starts. No console window shows.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let msi = temp_file("lode-desk.msi", bytes)?;
    let line = format!(
        "/S /C \"ping -n 3 127.0.0.1 >nul & msiexec /i \"{}\" /passive /norestart & start \"\" \"{}\"\"",
        msi.display(),
        exe.display()
    );
    started(
        std::process::Command::new("cmd.exe")
            .raw_arg(line)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn(),
    )
}

#[cfg(not(windows))]
fn windows_install(_kind: &InstallKind, _bytes: &[u8], _exe: &Path) -> Result<(), ErrorView> {
    Err(failed("Windows installers run on Windows only"))
}

/// Unpacks the `.app.tar.gz` next to `bundle`, swaps it in, and opens the new app.
fn mac_install(bundle: &Path, bytes: &[u8]) -> Result<(), ErrorView> {
    let archive = temp_file("lode-desk.app.tar.gz", bytes)?;
    let unpack = std::env::temp_dir().join("lode-desk-update").join("app");
    let _ = std::fs::remove_dir_all(&unpack);
    std::fs::create_dir_all(&unpack).map_err(|e| io_error("cannot unpack the update", e))?;
    let ok = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&unpack)
        .status()
        .is_ok_and(|s| s.success());
    let new = std::fs::read_dir(&unpack)
        .ok()
        .and_then(|mut d| d.find_map(|e| e.ok().map(|e| e.path())))
        .filter(|p| p.extension().is_some_and(|e| e == "app"));
    let (true, Some(new)) = (ok, new) else {
        return Err(failed("the update does not hold the app"));
    };
    let old = bundle.with_extension("app.old");
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(bundle, &old).map_err(|e| io_error("cannot replace the app", e))?;
    if let Err(e) = std::fs::rename(&new, bundle) {
        let _ = std::fs::rename(&old, bundle);
        return Err(io_error("cannot replace the app", e));
    }
    let _ = std::fs::remove_dir_all(&old);
    started(
        std::process::Command::new("open")
            .arg("-n")
            .arg(bundle)
            .spawn(),
    )
}
