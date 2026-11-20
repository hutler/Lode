//! `proxy.toml`: the listener (as in `loded.toml`) and the `[proxy]` table.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use lode_server::config::{ConnectionsConfig, TlsConfig};
use lode_server::ServerConfig;
use serde::Deserialize;

/// Default `cache_max_gib`.
pub const DEFAULT_CACHE_MAX_GIB: f64 = 100.0;

/// A starting `proxy.toml`, as `loded setup --proxy` would write it.
pub const EXAMPLE_CONFIG: &str = r#"# Lode caching proxy (`loded proxy --config proxy.toml`). It keeps copies of file content
# near the people who download it; everything else goes to the main server. Access stays
# with the main server: the proxy asks it once a minute per person and depot.

listen = "0.0.0.0:7480"

# HTTPS, as in loded.toml: certificate files, or a Let's Encrypt certificate.
# [tls]
# acme_domain = "lode-office.example.com"
# acme_email = "admin@example.com"

[proxy]
upstream = "https://lode.example.com"
cache_dir = "cache"
cache_max_gib = 100
"#;

/// Why the configuration cannot be used.
#[derive(Debug, thiserror::Error)]
#[error("{path}: {message}")]
pub struct ConfigError {
    /// The file.
    pub path: PathBuf,
    /// What is wrong.
    pub message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    listen: Option<SocketAddr>,
    data_dir: Option<PathBuf>,
    tls: Option<TlsConfig>,
    #[serde(default)]
    connections: ConnectionsConfig,
    proxy: RawProxy,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProxy {
    upstream: String,
    cache_dir: PathBuf,
    cache_max_gib: Option<f64>,
    #[serde(default)]
    allow_http_upstream: bool,
}

/// A caching proxy's configuration.
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// The listener: address, TLS and connection limits. Its data directory holds the
    /// ACME account and certificate when Let's Encrypt is used.
    pub listener: ServerConfig,
    /// The main server, such as `https://lode.example.com`.
    pub upstream: String,
    /// Allow a plain `http://` upstream on another machine.
    pub allow_http_upstream: bool,
    /// Where cached content is kept.
    pub cache_dir: PathBuf,
    /// The cache's size cap in bytes.
    pub cache_max_bytes: u64,
}

impl ProxyConfig {
    /// A configuration for tests and embedding: listen on `127.0.0.1:0`, plain HTTP.
    pub fn new(upstream: impl Into<String>, cache_dir: impl Into<PathBuf>) -> Self {
        let cache_dir = cache_dir.into();
        let mut listener = ServerConfig::with_data_dir(&cache_dir);
        listener.listen = SocketAddr::from(([127, 0, 0, 1], 0));
        Self {
            listener,
            upstream: upstream.into(),
            allow_http_upstream: false,
            cache_dir,
            cache_max_bytes: gib(DEFAULT_CACHE_MAX_GIB),
        }
    }

    /// Parses `text`; relative paths are resolved against `base`.
    pub fn parse(text: &str, base: &Path, origin: &Path) -> Result<Self, ConfigError> {
        let err = |message: String| ConfigError {
            path: origin.to_owned(),
            message,
        };
        let raw: RawFile = toml::from_str(text).map_err(|e| err(e.to_string()))?;
        let max_gib = raw.proxy.cache_max_gib.unwrap_or(DEFAULT_CACHE_MAX_GIB);
        if !(max_gib.is_finite() && max_gib > 0.0) {
            return Err(err("[proxy] cache_max_gib must be above 0".into()));
        }
        if raw.connections.header_timeout_seconds == 0 || raw.connections.max_per_address == 0 {
            return Err(err("[connections] values must be at least 1".into()));
        }
        let data_dir = base.join(raw.data_dir.unwrap_or_else(|| PathBuf::from(".")));
        let mut listener = ServerConfig::with_data_dir(data_dir);
        if let Some(l) = raw.listen {
            listener.listen = l;
        }
        listener.connections = raw.connections;
        listener.tls = raw.tls.map(|t| match t {
            TlsConfig::Files { cert, key } => TlsConfig::Files {
                cert: base.join(cert),
                key: base.join(key),
            },
            TlsConfig::Acme(mut a) => {
                a.ca = a.ca.map(|c| base.join(c));
                TlsConfig::Acme(a)
            }
        });
        lode_client::parse_server_url(&raw.proxy.upstream, raw.proxy.allow_http_upstream)
            .map_err(|e| err(format!("[proxy] upstream: {e}")))?;
        Ok(Self {
            listener,
            upstream: raw.proxy.upstream,
            allow_http_upstream: raw.proxy.allow_http_upstream,
            cache_dir: base.join(raw.proxy.cache_dir),
            cache_max_bytes: gib(max_gib),
        })
    }

    /// Reads and parses a configuration file.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError {
            path: path.to_owned(),
            message: e.to_string(),
        })?;
        Self::parse(&text, path.parent().unwrap_or(Path::new(".")), path)
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn gib(g: f64) -> u64 {
    (g * (1u64 << 30) as f64) as u64
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn example_parses() {
        let c = ProxyConfig::parse(EXAMPLE_CONFIG, Path::new("/srv/p"), Path::new("x")).unwrap();
        assert_eq!(c.upstream, "https://lode.example.com");
        assert_eq!(c.cache_dir, Path::new("/srv/p/cache"));
        assert_eq!(c.cache_max_bytes, 100 << 30);
        assert_eq!(c.listener.listen.to_string(), "0.0.0.0:7480");
        let bad = EXAMPLE_CONFIG.replace("https://lode", "http://lode");
        assert!(ProxyConfig::parse(&bad, Path::new("/"), Path::new("x")).is_err());
    }
}
