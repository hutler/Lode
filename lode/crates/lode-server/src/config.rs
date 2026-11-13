//! `loded.toml`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Name of the configuration file `loded init` writes into the data directory.
pub const CONFIG_FILE_NAME: &str = "loded.toml";

/// Address used when the configuration names none.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:7480";

/// The commented default configuration written by `loded init`.
pub const DEFAULT_CONFIG: &str = r#"# Lode server configuration. Unknown keys are an error.

# Address and port to listen on. Without [tls], keep this on a loopback address and put a
# TLS-terminating reverse proxy in front of the server.
listen = "127.0.0.1:7480"

# Data directory holding meta.db and depots/. A relative path is relative to this file.
data_dir = "."

# Serve HTTPS directly. Paths are relative to this file unless absolute.
# [tls]
# cert = "cert.pem"
# key = "key.pem"

[limits]
# Largest JSON request body, in bytes (64 MiB).
max_request_json = 67108864
# Largest single file, in bytes (64 GiB).
max_blob_size = 68719476736
# Most changes in one commit.
max_changes_per_commit = 500000
# Most changed paths listed per revision in a revision list.
max_changes_listed = 1000
# Most file edits in one MCP shelf tool call (create_shelf, update_shelf).
mcp_max_edits = 500
# Most file content, in bytes, inline in one MCP shelf tool call (16 MiB). Larger work uses
# the HTTP blob API.
mcp_max_inline_bytes = 16777216
# Seconds a request body may go without data before the request fails (slow or stalled
# uploads).
body_idle_seconds = 60

# Optional features compiled into loded. Webhooks and checks are off by default.
[modules]
locks = true
shelves = true
mcp = true
webhooks = false
checks = false

[gc]
# Unreferenced objects younger than this are kept (24 hours).
grace_seconds = 86400

# Webhooks module, when enabled in [modules].
[webhooks]
# Allow hooks with plain http:// URLs. Leave off unless the receivers are on a trusted
# network: the signature proves who sent a request, but not that nobody read it.
allow_http = false
"#;

/// Errors reading the configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("cannot read config file {path}: {source}")]
    Read {
        /// The file.
        path: PathBuf,
        /// Cause.
        source: std::io::Error,
    },
    /// The file is not valid TOML or has unknown or ill-typed keys.
    #[error("invalid config file {path}: {message}")]
    Parse {
        /// The file.
        path: PathBuf,
        /// What is wrong.
        message: String,
    },
}

/// The server configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Listen address.
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// Data directory. Resolved against the config file's directory by [`ServerConfig::load`].
    pub data_dir: PathBuf,
    /// Serve HTTPS with this certificate and key.
    #[serde(default)]
    pub tls: Option<TlsConfig>,
    /// Size and count limits.
    #[serde(default)]
    pub limits: Limits,
    /// Which modules are enabled.
    #[serde(default)]
    pub modules: ModulesConfig,
    /// Garbage collection.
    #[serde(default)]
    pub gc: GcConfig,
    /// Webhooks module settings.
    #[serde(default)]
    pub webhooks: WebhooksConfig,
}

/// The `[webhooks]` table, read by the webhooks module.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WebhooksConfig {
    /// Accept `http://` hook URLs. Off by default: hooks must use `https://`.
    pub allow_http: bool,
}

/// The `[gc]` table.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GcConfig {
    /// Unreferenced objects younger than this many seconds are kept, so uploads for a commit
    /// that has not arrived yet survive (ARCHITECTURE §11). Default 24 hours.
    pub grace_seconds: u64,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            grace_seconds: 24 * 60 * 60,
        }
    }
}

/// PEM files for HTTPS.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// Certificate chain, leaf first.
    pub cert: PathBuf,
    /// Private key (PKCS#8, PKCS#1 or SEC1).
    pub key: PathBuf,
}

/// Limits from ARCHITECTURE §6.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    /// Largest JSON request body in bytes.
    pub max_request_json: u64,
    /// Largest single blob in bytes.
    pub max_blob_size: u64,
    /// Most changes in one commit.
    pub max_changes_per_commit: usize,
    /// Most changed paths listed per revision in a revision list.
    pub max_changes_listed: usize,
    /// Most file edits in one MCP shelf tool call.
    pub mcp_max_edits: usize,
    /// Most inline file content, in bytes, in one MCP shelf tool call.
    pub mcp_max_inline_bytes: u64,
    /// Seconds a request body may go without data before the request fails.
    pub body_idle_seconds: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_request_json: lode_proto::consts::MAX_REQUEST_JSON,
            max_blob_size: lode_proto::consts::MAX_BLOB_SIZE,
            max_changes_per_commit: lode_proto::consts::MAX_CHANGES_PER_COMMIT,
            max_changes_listed: lode_proto::consts::MAX_CHANGES_LISTED,
            mcp_max_edits: 500,
            mcp_max_inline_bytes: 16 * 1024 * 1024,
            body_idle_seconds: 60,
        }
    }
}

/// The `[modules]` table. Defaults follow ARCHITECTURE §8.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ModulesConfig {
    /// File locks.
    pub locks: bool,
    /// Shelves.
    pub shelves: bool,
    /// MCP endpoint.
    pub mcp: bool,
    /// Outgoing webhooks.
    pub webhooks: bool,
    /// Reported checks and the submit gate.
    pub checks: bool,
}

impl Default for ModulesConfig {
    fn default() -> Self {
        Self {
            locks: true,
            shelves: true,
            mcp: true,
            webhooks: false,
            checks: false,
        }
    }
}

impl ModulesConfig {
    /// Whether the module with this name is enabled. Unknown names are not.
    pub fn is_enabled(&self, name: &str) -> bool {
        match name {
            "locks" => self.locks,
            "shelves" => self.shelves,
            "mcp" => self.mcp,
            "webhooks" => self.webhooks,
            "checks" => self.checks,
            _ => false,
        }
    }
}

fn default_listen() -> SocketAddr {
    // The constant is a valid address; a parse failure here would be a typo caught by tests.
    DEFAULT_LISTEN
        .parse()
        .unwrap_or(SocketAddr::from(([127, 0, 0, 1], 7480)))
}

impl ServerConfig {
    /// Parses configuration text. Relative paths are resolved against `base`.
    pub fn parse(text: &str, base: &Path, origin: &Path) -> Result<Self, ConfigError> {
        let mut cfg: ServerConfig = toml::from_str(text).map_err(|e| ConfigError::Parse {
            path: origin.to_owned(),
            message: e.to_string(),
        })?;
        cfg.data_dir = base.join(&cfg.data_dir);
        if let Some(tls) = &mut cfg.tls {
            tls.cert = base.join(&tls.cert);
            tls.key = base.join(&tls.key);
        }
        Ok(cfg)
    }

    /// Reads and parses a configuration file.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        let base = path.parent().unwrap_or(Path::new("."));
        Self::parse(&text, base, path)
    }

    /// A configuration with defaults for everything but the data directory. For tests and
    /// embedding.
    pub fn with_data_dir(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            listen: default_listen(),
            data_dir: data_dir.into(),
            tls: None,
            limits: Limits::default(),
            gc: GcConfig::default(),
            modules: ModulesConfig::default(),
            webhooks: WebhooksConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn default_config_parses_to_defaults() {
        let cfg =
            ServerConfig::parse(DEFAULT_CONFIG, Path::new("/srv/lode"), Path::new("x")).unwrap();
        assert_eq!(cfg.listen.to_string(), DEFAULT_LISTEN);
        assert_eq!(cfg.data_dir, Path::new("/srv/lode").join("."));
        assert!(cfg.tls.is_none());
        assert_eq!(cfg.limits.max_request_json, 64 * 1024 * 1024);
        assert_eq!(cfg.limits.max_blob_size, 64 * 1024 * 1024 * 1024);
        assert_eq!(cfg.limits.max_changes_per_commit, 500_000);
        assert_eq!(cfg.limits.max_changes_listed, 1_000);
        assert_eq!(cfg.limits.mcp_max_edits, 500);
        assert_eq!(cfg.limits.mcp_max_inline_bytes, 16 * 1024 * 1024);
        assert_eq!(cfg.limits.body_idle_seconds, 60);
        assert!(cfg.modules.locks && cfg.modules.shelves && cfg.modules.mcp);
        assert!(!cfg.modules.webhooks && !cfg.modules.checks);
        assert!(!cfg.webhooks.allow_http);
    }

    #[test]
    fn unknown_keys_are_errors() {
        for text in [
            "data_dir = '.'\nlisten_addr = '1.2.3.4:5'",
            "data_dir = '.'\n[limits]\nmax_json = 1",
            "data_dir = '.'\n[modules]\nfrobnicate = true",
            "data_dir = '.'\n[webhooks]\nallow_ftp = true",
            "data_dir = '.'\n[tls]\ncert='a'\nkey='b'\npassword='c'",
        ] {
            let err = ServerConfig::parse(text, Path::new("."), Path::new("x")).unwrap_err();
            assert!(matches!(err, ConfigError::Parse { .. }), "{text}");
        }
    }

    #[test]
    fn data_dir_is_required() {
        assert!(ServerConfig::parse("", Path::new("."), Path::new("x")).is_err());
    }

    #[test]
    fn relative_paths_resolve_against_the_config_directory() {
        let text = "data_dir = 'data'\n[tls]\ncert = 'c.pem'\nkey = '/abs/k.pem'";
        let cfg = ServerConfig::parse(text, Path::new("/etc/lode"), Path::new("x")).unwrap();
        assert_eq!(cfg.data_dir, Path::new("/etc/lode/data"));
        let tls = cfg.tls.unwrap();
        assert_eq!(tls.cert, Path::new("/etc/lode/c.pem"));
        assert_eq!(tls.key, Path::new("/abs/k.pem"));
    }
}
