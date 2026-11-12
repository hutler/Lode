//! Stored sign-ins: one token file per server in the user's config directory
//! (ARCHITECTURE §10.1), never in a workspace.
//!
//! Protection: on Unix the directory is `0700` and each file `0600`. On Windows the files
//! live under the user's profile (`%APPDATA%`), whose ACL already limits access to that user
//! and administrators; no ACL is set by this crate.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use reqwest::Url;

/// Token files, one per server URL.
#[derive(Debug, Clone)]
pub struct CredentialStore {
    dir: PathBuf,
}

impl CredentialStore {
    /// A store in `dir`. Tests and `LODE_CONFIG_DIR`-style overrides use this.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The store in the user's config directory: `~/.config/lode/credentials` on Linux,
    /// `~/Library/Application Support/lode/credentials` on macOS,
    /// `%APPDATA%\lode\config\credentials` on Windows. `None` when the platform reports no
    /// home directory.
    pub fn user_default() -> Option<Self> {
        let dirs = directories::ProjectDirs::from("", "", "lode")?;
        Some(Self::new(dirs.config_dir().join("credentials")))
    }

    /// The directory holding the token files.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file that holds (or would hold) the token for `server`. Its name is readable and
    /// unique per normalized URL: scheme, host, port and path prefix.
    pub fn token_file(&self, server: &Url) -> PathBuf {
        let key = server_key(server);
        let mut name: String = key
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        name.truncate(80);
        let hash = blake3::hash(key.as_bytes()).to_hex();
        self.dir.join(format!("{name}-{}.token", &hash[..12]))
    }

    /// The stored token for `server`, if any.
    pub fn load(&self, server: &Url) -> io::Result<Option<String>> {
        read_token_file(&self.token_file(server))
    }

    /// Stores `token` for `server`, replacing any earlier one. Returns the file written.
    pub fn save(&self, server: &Url, token: &str) -> io::Result<PathBuf> {
        let token = token.trim();
        if token.is_empty() || token.contains(['\n', '\r']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a token is one non-empty line",
            ));
        }
        create_private_dir(&self.dir)?;
        let path = self.token_file(server);
        // tempfile creates the file owner-only (0600) on Unix; it is renamed into place so a
        // reader never sees half a token.
        let mut tmp = tempfile::Builder::new()
            .prefix(".token-")
            .tempfile_in(&self.dir)?;
        writeln!(tmp, "{token}")?;
        tmp.as_file().sync_all()?;
        tmp.persist(&path).map_err(|e| e.error)?;
        restrict_file(&path)?;
        Ok(path)
    }

    /// Forgets the token for `server`. Returns whether there was one.
    pub fn remove(&self, server: &Url) -> io::Result<bool> {
        match fs::remove_file(self.token_file(server)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// Reads a token file such as `LODE_TOKEN_FILE` names: the first line, trimmed. A missing
/// file is `None`; an empty one is an error.
pub fn read_token_file(path: &Path) -> io::Result<Option<String>> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let token = text.lines().next().unwrap_or("").trim();
    if token.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} holds no token", path.display()),
        ));
    }
    Ok(Some(token.to_owned()))
}

/// `https://host:port/prefix` with the default port left out and no trailing slash, so the
/// same server written two ways shares one file.
fn server_key(url: &Url) -> String {
    let host = url
        .host_str()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let port = match url.port() {
        Some(p) => format!(":{p}"),
        None => String::new(),
    };
    let path = url.path().trim_end_matches('/');
    format!("{}://{host}{port}{path}", url.scheme())
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn save_load_remove() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("creds"));
        let a = url("https://Lode.Example.com/");
        assert_eq!(store.load(&a).unwrap(), None);
        let path = store.save(&a, "secret-1\n").unwrap();
        assert_eq!(store.load(&a).unwrap().as_deref(), Some("secret-1"));
        // Same server written differently shares the file.
        assert_eq!(store.token_file(&url("https://lode.example.com:443")), path);
        // Different servers do not.
        assert_ne!(
            store.token_file(&url("https://lode.example.com:8443")),
            path
        );
        assert_ne!(store.token_file(&url("http://lode.example.com")), path);
        assert_ne!(store.token_file(&url("https://lode.example.com/b")), path);
        store.save(&a, "secret-2").unwrap();
        assert_eq!(store.load(&a).unwrap().as_deref(), Some("secret-2"));
        assert!(store.remove(&a).unwrap());
        assert!(!store.remove(&a).unwrap());
        assert!(store.save(&a, "two\nlines").is_err());
        // No temp files left behind.
        assert_eq!(fs::read_dir(store.dir()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("creds"));
        let path = store.save(&url("https://h"), "t").unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(store.dir()), 0o700);
    }

    #[test]
    fn token_file_reading() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("tok");
        assert_eq!(read_token_file(&p).unwrap(), None);
        fs::write(&p, "  abc  \nignored\n").unwrap();
        assert_eq!(read_token_file(&p).unwrap().as_deref(), Some("abc"));
        fs::write(&p, "\n").unwrap();
        assert!(read_token_file(&p).is_err());
    }
}
