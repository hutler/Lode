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

# Address and port to listen on. Without [tls], keep this on a loopback address (or put a
# TLS-terminating reverse proxy in front of the server).
listen = "127.0.0.1:7480"

# Data directory holding meta.db and depots/. A relative path is relative to this file.
data_dir = "."

# Serve HTTPS directly, with certificate files (paths relative to this file unless
# absolute)...
# [tls]
# cert = "cert.pem"
# key = "key.pem"
#
# ...or with a certificate obtained and renewed automatically from Let's Encrypt. The domain
# must resolve to this server and port 443 must reach `listen`. Setting acme_domain accepts
# the certificate authority's terms of service.
# [tls]
# acme_domain = "lode.example.com"
# acme_email = "admin@example.com"

[connections]
# Seconds a client has to send a request's headers, and that an idle kept-alive connection
# stays open waiting for its next request. Slower connections are closed.
header_timeout_seconds = 30
# Most open connections from one client address (an IPv6 address counts by its /64
# network). Further connections are closed at once.
max_per_address = 64

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
    /// Connection timeouts and limits.
    #[serde(default)]
    pub connections: ConnectionsConfig,
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

/// The `[connections]` table.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ConnectionsConfig {
    /// Seconds allowed for a request's headers, and for the next request on an idle
    /// kept-alive connection. Default 30.
    pub header_timeout_seconds: u64,
    /// Most open connections from one address (an IPv6 /64 network). Default 64.
    pub max_per_address: usize,
}

impl Default for ConnectionsConfig {
    fn default() -> Self {
        Self {
            header_timeout_seconds: 30,
            max_per_address: 64,
        }
    }
}

/// The ACME directory of Let's Encrypt, used when `acme_directory` is not set.
pub const LETS_ENCRYPT_DIRECTORY: &str = "https://acme-v02.api.letsencrypt.org/directory";

/// The `[tls]` table: certificate files, or a certificate obtained over ACME.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RawTls")]
pub enum TlsConfig {
    /// PEM files.
    Files {
        /// Certificate chain, leaf first.
        cert: PathBuf,
        /// Private key (PKCS#8, PKCS#1 or SEC1).
        key: PathBuf,
    },
    /// A certificate obtained and renewed from an ACME server such as Let's Encrypt.
    Acme(AcmeConfig),
}

/// Obtaining a certificate over ACME (TLS-ALPN-01 on the HTTPS port).
#[derive(Debug, Clone)]
pub struct AcmeConfig {
    /// The server's DNS name, which must resolve to this server.
    pub domain: String,
    /// Contact address given to the certificate authority (expiry notices).
    pub email: Option<String>,
    /// Directory URL of the ACME server; Let's Encrypt production by default.
    pub directory: String,
    /// Extra root certificate (PEM) to trust for the ACME server itself, for a private one.
    pub ca: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTls {
    cert: Option<PathBuf>,
    key: Option<PathBuf>,
    acme_domain: Option<String>,
    acme_email: Option<String>,
    acme_directory: Option<String>,
    acme_ca: Option<PathBuf>,
}

impl TryFrom<RawTls> for TlsConfig {
    type Error = String;

    fn try_from(raw: RawTls) -> Result<Self, String> {
        let acme_keys =
            raw.acme_email.is_some() || raw.acme_directory.is_some() || raw.acme_ca.is_some();
        match (raw.cert, raw.key, raw.acme_domain) {
            (Some(cert), Some(key), None) if !acme_keys => Ok(Self::Files { cert, key }),
            (None, None, Some(domain)) => {
                if !is_dns_name(&domain) {
                    return Err(format!("[tls] acme_domain {domain:?} is not a DNS name"));
                }
                let directory = raw
                    .acme_directory
                    .unwrap_or_else(|| LETS_ENCRYPT_DIRECTORY.to_owned());
                if !directory.starts_with("https://") {
                    return Err("[tls] acme_directory must be an https:// URL".into());
                }
                Ok(Self::Acme(AcmeConfig {
                    domain: domain.to_ascii_lowercase(),
                    email: raw.acme_email,
                    directory,
                    ca: raw.acme_ca,
                }))
            }
            (Some(_) | None, Some(_) | None, Some(_)) => {
                Err("[tls] takes either cert and key, or acme_domain, not both".into())
            }
            _ if acme_keys => {
                Err("[tls] acme_email, acme_directory and acme_ca need acme_domain".into())
            }
            _ => Err("[tls] needs cert and key, or acme_domain".into()),
        }
    }
}

/// A host name of letters, digits and hyphens in dot-separated labels, at least two labels.
fn is_dns_name(name: &str) -> bool {
    let labels: Vec<&str> = name.split('.').collect();
    name.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
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
        let invalid = |message: &str| ConfigError::Parse {
            path: origin.to_owned(),
            message: message.to_owned(),
        };
        if cfg.connections.header_timeout_seconds == 0 {
            return Err(invalid(
                "[connections] header_timeout_seconds must be at least 1",
            ));
        }
        if cfg.connections.max_per_address == 0 {
            return Err(invalid("[connections] max_per_address must be at least 1"));
        }
        cfg.data_dir = base.join(&cfg.data_dir);
        match &mut cfg.tls {
            Some(TlsConfig::Files { cert, key }) => {
                *cert = base.join(&*cert);
                *key = base.join(&*key);
            }
            Some(TlsConfig::Acme(acme)) => {
                if let Some(ca) = &mut acme.ca {
                    *ca = base.join(&*ca);
                }
            }
            None => {}
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
            connections: ConnectionsConfig::default(),
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
        assert_eq!(cfg.connections.header_timeout_seconds, 30);
        assert_eq!(cfg.connections.max_per_address, 64);
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
            "data_dir = '.'\n[connections]\nidle_seconds = 1",
            "data_dir = '.'\n[connections]\nheader_timeout_seconds = 0",
            "data_dir = '.'\n[connections]\nmax_per_address = 0",
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
        let Some(TlsConfig::Files { cert, key }) = cfg.tls else {
            panic!("files expected");
        };
        assert_eq!(cert, Path::new("/etc/lode/c.pem"));
        assert_eq!(key, Path::new("/abs/k.pem"));
    }

    #[test]
    fn tls_takes_files_or_acme() {
        let parse = |tls: &str| {
            ServerConfig::parse(
                &format!("data_dir = '.'\n[tls]\n{tls}"),
                Path::new("/etc/lode"),
                Path::new("x"),
            )
        };
        let Some(TlsConfig::Acme(acme)) = parse("acme_domain = 'Lode.Example.com'").unwrap().tls
        else {
            panic!("acme expected");
        };
        assert_eq!(acme.domain, "lode.example.com");
        assert_eq!(acme.directory, LETS_ENCRYPT_DIRECTORY);
        assert!(acme.email.is_none() && acme.ca.is_none());
        let text = "acme_domain = 'lode.example.com'\nacme_email = 'ops@example.com'\n\
                    acme_directory = 'https://localhost:14000/dir'\nacme_ca = 'ca.pem'";
        let Some(TlsConfig::Acme(acme)) = parse(text).unwrap().tls else {
            panic!("acme expected");
        };
        assert_eq!(acme.email.as_deref(), Some("ops@example.com"));
        assert_eq!(acme.directory, "https://localhost:14000/dir");
        assert_eq!(acme.ca.unwrap(), Path::new("/etc/lode/ca.pem"));
        for bad in [
            "cert = 'c'\nkey = 'k'\nacme_domain = 'a.example'",
            "cert = 'c'\nacme_domain = 'a.example'",
            "cert = 'c'",
            "cert = 'c'\nkey = 'k'\nacme_email = 'x@example.com'",
            "acme_email = 'x@example.com'",
            "acme_domain = 'localhost'",
            "acme_domain = 'bad_name.example'",
            "acme_domain = 'a.example'\nacme_directory = 'http://x/dir'",
            "",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
