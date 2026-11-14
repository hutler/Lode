//! Updates of the app itself, from the feed on the project's GitHub releases.
//!
//! The window checks at start and once a day unless the "check for updates" setting is off; the
//! `LODE_DESK_NO_UPDATE` environment variable turns every check off for managed machines. A
//! package is installed only when its minisign signature matches the public key built into the
//! app (`plugins.updater.pubkey`); a build without a key never offers updates.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(feature = "web")]
use tauri::{AppHandle, Runtime};
#[cfg(feature = "web")]
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::desk::ErrorView;

/// Set (to anything but an empty value) to turn update checks off.
pub const NO_UPDATE_ENV: &str = "LODE_DESK_NO_UPDATE";

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
pub fn off_reason(
    settings: &Settings,
    env: Option<&std::ffi::OsStr>,
    asked: bool,
) -> Option<String> {
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

#[cfg(feature = "web")]
/// The update public key built into the app; empty when there is none.
fn pubkey<R: Runtime>(app: &AppHandle<R>) -> String {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|u| u.get("pubkey"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

#[cfg(feature = "web")]
/// An updater error as the window shows it. A signature that does not match the key is
/// `bad_signature`.
pub fn error_view(e: tauri_plugin_updater::Error) -> ErrorView {
    use tauri_plugin_updater::Error as E;
    match e {
        E::Minisign(_)
        | E::SignatureUtf8(_)
        | E::Base64(_)
        | E::SignedVersionMismatch { .. }
        | E::MissingSignedVersion => ErrorView {
            code: "bad_signature".into(),
            message:
                "the update is not signed with the Lode Desk update key, so it was not installed"
                    .into(),
            details: Value::String(e.to_string()),
        },
        other => ErrorView {
            code: "update_failed".into(),
            message: format!("cannot update Lode Desk: {other}"),
            details: Value::Null,
        },
    }
}

#[cfg(feature = "web")]
/// Asks the feed for a newer version, returning it with what the window shows.
pub async fn find<R: Runtime>(
    app: &AppHandle<R>,
    settings_file: &Path,
    asked: bool,
) -> Result<(UpdateCheck, Option<Update>), ErrorView> {
    let env = std::env::var_os(NO_UPDATE_ENV);
    if let Some(reason) = off_reason(&load_settings(settings_file), env.as_deref(), asked) {
        return Ok((UpdateCheck::Off { reason }, None));
    }
    if pubkey(app).is_empty() {
        return Ok((UpdateCheck::Unconfigured, None));
    }
    let current = app.package_info().version.to_string();
    let found = app
        .updater()
        .map_err(error_view)?
        .check()
        .await
        .map_err(error_view)?;
    Ok(match found {
        None => (UpdateCheck::Current { current }, None),
        Some(u) => (
            UpdateCheck::Available {
                current,
                version: u.version.clone(),
                notes: u.body.clone().unwrap_or_default(),
                date: u.date.and_then(|d| {
                    d.format(&time::format_description::well_known::Rfc3339)
                        .ok()
                }),
            },
            Some(u),
        ),
    })
}

#[cfg(feature = "web")]
/// Downloads `update` and checks its signature; the bytes can then be installed.
pub async fn download(update: &Update) -> Result<Vec<u8>, ErrorView> {
    update.download(|_, _| {}, || {}).await.map_err(error_view)
}
