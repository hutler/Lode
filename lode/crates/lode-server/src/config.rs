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
feed = true                                 # events of a depot from a cursor (GET .../events)

[gc]
# Unreferenced objects younger than this are kept (24 hours).
grace_seconds = 86400

# Scheduled backups while the server runs, like `loded backup`. Off unless `every` is set.
# [backup]
# every = "24h"       # s, m, h or d; at least 1m
# keep = 7            # complete backups kept; older ones are removed
# dir = "backups"     # relative to this file; best on another disk

# Single sign-on through your company's identity provider (OpenID Connect): Google,
# Microsoft Entra ID, Okta, Keycloak and others. Register Lode as a web application there
# with the redirect address <public_url>/api/v1/auth/sso/callback.
# [oidc]
# issuer = "https://accounts.google.com"
# client_id = "1234.apps.googleusercontent.com"
# client_secret_file = "oidc-secret.txt"   # or client_secret = "..."
# public_url = "https://lode.example.com"   # how browsers reach this server
# label = "Example Corp"                    # shown on sign-in buttons
# user_claim = "email"                      # the claim that names a user
# allowed_domains = ["example.com"]
# auto_create = false     # true: create a user on first sign-in instead of refusing
# password_login = "everyone"               # "admins": only server admins use passwords

# Where file content is kept. By default it is files under depots/ in the data directory.
# With kind = "s3" it goes to an S3-compatible bucket (AWS S3, Backblaze B2, MinIO, ...);
# the database stays here. Move existing content with `loded migrate-storage --to s3`.
# [storage]
# kind = "s3"
# bucket = "lode-content"
# prefix = "main"                       # keys start with this; may be empty
# region = "eu-central-1"               # or AWS_REGION; default us-east-1
# endpoint = "https://s3.eu-central-003.backblazeb2.com"   # leave out for AWS
# path_style = false                    # true for MinIO and most self-hosted services
# access_key_id = "AKIA..."             # with the secret's file; leave both out to use
# secret_access_key_file = "s3-secret.txt"   # AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY

# Where the metadata (history, users, access rules, module data) is kept. By default it is
# the SQLite file meta.db in the data directory. With kind = "postgres" it is a Postgres
# database (14 or newer), for many concurrent users and for a standby server that takes over
# when this one stops (`loded serve --wait-for-lock`). Move an existing server with
# `loded migrate-db --to postgres --url-file <file>`.
# [database]
# kind = "postgres"
# url_file = "database-url.txt"   # first line: postgres://lode:...@db.example.com/lode?sslmode=verify-full
# connections = 8                 # read connections; writes use one more

# The admin web UI at <server>/admin/ for server administrators: depots, maintenance, people,
# access rules and settings in a browser. Put the server behind TLS before using it from
# another machine; turn it off if nobody uses it.
[admin_ui]
enabled = true

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
    /// Scheduled backups.
    #[serde(default)]
    pub backup: BackupConfig,
    /// Webhooks module settings.
    #[serde(default)]
    pub webhooks: WebhooksConfig,
    /// Single sign-on, when configured.
    #[serde(default)]
    pub oidc: Option<OidcConfig>,
    /// Where object content is kept.
    #[serde(default)]
    pub storage: StorageConfig,
    /// The admin web UI.
    #[serde(default)]
    pub admin_ui: AdminUiConfig,
    /// Where the metadata is kept.
    #[serde(default)]
    pub database: DatabaseConfig,
}

/// The `[database]` table (T153).
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawDatabase")]
pub struct DatabaseConfig {
    /// Which database the server uses.
    pub kind: DatabaseKind,
    /// The Postgres URL, when given in the file. Prefer `url_file`.
    pub url: Option<String>,
    /// A file whose first line is the Postgres URL. Resolved against the config file's
    /// directory.
    pub url_file: Option<PathBuf>,
    /// Read connections.
    pub connections: usize,
}

impl std::fmt::Debug for DatabaseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatabaseConfig")
            .field("kind", &self.kind)
            .field("url", &self.url.as_ref().map(|_| "***"))
            .field("url_file", &self.url_file)
            .field("connections", &self.connections)
            .finish()
    }
}

/// Which database holds the metadata.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseKind {
    /// `meta.db` in the data directory (the default).
    #[default]
    Sqlite,
    /// A Postgres database.
    Postgres,
}

/// Default number of read connections.
pub const DEFAULT_DB_CONNECTIONS: usize = 8;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDatabase {
    #[serde(default)]
    kind: DatabaseKind,
    url: Option<String>,
    url_file: Option<PathBuf>,
    connections: Option<usize>,
}

impl TryFrom<RawDatabase> for DatabaseConfig {
    type Error = String;

    fn try_from(raw: RawDatabase) -> Result<Self, String> {
        let connections = raw.connections.unwrap_or(DEFAULT_DB_CONNECTIONS);
        if connections == 0 {
            return Err("[database] connections must be at least 1".into());
        }
        if raw.url.is_some() && raw.url_file.is_some() {
            return Err("[database] takes url or url_file, not both".into());
        }
        if raw.kind == DatabaseKind::Postgres && raw.url.is_none() && raw.url_file.is_none() {
            return Err("[database] kind = \"postgres\" needs url_file (or url)".into());
        }
        if let Some(url) = &raw.url {
            if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
                return Err("[database] url must start with postgres://".into());
            }
        }
        Ok(Self {
            kind: raw.kind,
            url: raw.url,
            url_file: raw.url_file,
            connections,
        })
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            kind: DatabaseKind::Sqlite,
            url: None,
            url_file: None,
            connections: DEFAULT_DB_CONNECTIONS,
        }
    }
}

impl DatabaseConfig {
    /// Postgres settings with the URL in a file.
    pub fn postgres_file(url_file: impl Into<PathBuf>) -> Self {
        Self {
            kind: DatabaseKind::Postgres,
            url: None,
            url_file: Some(url_file.into()),
            connections: DEFAULT_DB_CONNECTIONS,
        }
    }

    /// The Postgres URL: `url`, or the first line of `url_file`.
    pub fn postgres_url(&self) -> Result<String, String> {
        if let Some(url) = &self.url {
            return Ok(url.clone());
        }
        let Some(file) = &self.url_file else {
            return Err("[database] has no url_file".into());
        };
        let text = std::fs::read_to_string(file)
            .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let url = text.lines().next().unwrap_or("").trim().to_owned();
        if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
            return Err(format!(
                "{} must hold a postgres:// URL on its first line",
                file.display()
            ));
        }
        Ok(url)
    }
}

/// The `[admin_ui]` table (T140).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AdminUiConfig {
    /// Serve the admin web UI at `/admin/`. On by default; off answers 404 there.
    pub enabled: bool,
}

impl Default for AdminUiConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// The `[storage]` table: where object content is kept (T123).
///
/// S3 settings may be filled in while `kind` is still `fs`: `loded migrate-storage --to s3`
/// copies the content into that bucket and then sets `kind = "s3"`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawStorage")]
pub struct StorageConfig {
    /// Which store the server uses.
    pub kind: StorageKind,
    /// S3 settings; required when `kind` is `s3`.
    pub s3: Option<S3Settings>,
}

/// Which object store the server uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StorageKind {
    /// Files under `<data_dir>/depots/` (the default).
    #[default]
    Fs,
    /// An S3-compatible bucket.
    S3,
}

impl StorageConfig {
    /// S3 storage with these settings.
    pub fn s3(settings: S3Settings) -> Self {
        Self {
            kind: StorageKind::S3,
            s3: Some(settings),
        }
    }

    /// The S3 settings when the server uses S3.
    pub fn s3_in_use(&self) -> Option<&S3Settings> {
        match self.kind {
            StorageKind::S3 => self.s3.as_ref(),
            StorageKind::Fs => None,
        }
    }
}

/// S3 storage settings. Credentials come from `access_key_id` with
/// `secret_access_key_file`, or else from the `AWS_*` environment variables.
#[derive(Clone, PartialEq, Eq)]
pub struct S3Settings {
    /// Bucket name.
    pub bucket: String,
    /// Key prefix, without leading or trailing `/`.
    pub prefix: String,
    /// Region; else `AWS_REGION`, `AWS_DEFAULT_REGION`, then `us-east-1`.
    pub region: Option<String>,
    /// Endpoint of a non-AWS service, `http(s)://host[:port]`.
    pub endpoint: Option<String>,
    /// Bucket in the path instead of the host name.
    pub path_style: bool,
    /// Access key id.
    pub access_key_id: Option<String>,
    /// A file whose first line is the secret access key. Resolved against the config file's
    /// directory.
    pub secret_access_key_file: Option<PathBuf>,
    /// A secret given in code, for tests and embedding; never read from `loded.toml`.
    secret_access_key: Option<String>,
}

impl std::fmt::Debug for S3Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Settings")
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("path_style", &self.path_style)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key_file", &self.secret_access_key_file)
            .field(
                "secret_access_key",
                &self.secret_access_key.as_ref().map(|_| "***"),
            )
            .finish()
    }
}

impl S3Settings {
    /// Settings for `bucket` with everything else unset.
    pub fn new(bucket: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            prefix: String::new(),
            region: None,
            endpoint: None,
            path_style: false,
            access_key_id: None,
            secret_access_key_file: None,
            secret_access_key: None,
        }
    }

    /// Sets the credentials directly, for tests and embedding.
    pub fn with_credentials(mut self, access_key_id: &str, secret_access_key: &str) -> Self {
        self.access_key_id = Some(access_key_id.to_owned());
        self.secret_access_key = Some(secret_access_key.to_owned());
        self
    }

    /// The store settings, with credentials and region resolved. `env` looks up
    /// environment variables (`std::env::var` in the server; a map in tests).
    pub fn store_config(
        &self,
        data_dir: &Path,
        max_object_size: u64,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<lode_store::S3Config, String> {
        let nonempty = |name: &str| env(name).filter(|v| !v.trim().is_empty());
        let credentials = match &self.access_key_id {
            Some(id) => {
                let secret = match (&self.secret_access_key, &self.secret_access_key_file) {
                    (Some(s), _) => s.clone(),
                    (None, Some(file)) => {
                        let text = std::fs::read_to_string(file).map_err(|e| {
                            format!("cannot read secret_access_key_file {}: {e}", file.display())
                        })?;
                        let first = text.lines().next().unwrap_or("").trim().to_owned();
                        if first.is_empty() {
                            return Err(format!("{} is empty", file.display()));
                        }
                        first
                    }
                    (None, None) => {
                        return Err("[storage] access_key_id needs secret_access_key_file".into())
                    }
                };
                lode_store::s3::sigv4::Credentials {
                    access_key_id: id.clone(),
                    secret_access_key: secret,
                    session_token: None,
                }
            }
            None => match (
                nonempty("AWS_ACCESS_KEY_ID"),
                nonempty("AWS_SECRET_ACCESS_KEY"),
            ) {
                (Some(id), Some(secret)) => lode_store::s3::sigv4::Credentials {
                    access_key_id: id.trim().to_owned(),
                    secret_access_key: secret.trim().to_owned(),
                    session_token: nonempty("AWS_SESSION_TOKEN").map(|t| t.trim().to_owned()),
                },
                _ => {
                    return Err("S3 storage needs credentials: set access_key_id and \
                                secret_access_key_file in [storage], or the environment \
                                variables AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY"
                        .into())
                }
            },
        };
        let region = self
            .region
            .clone()
            .or_else(|| nonempty("AWS_REGION"))
            .or_else(|| nonempty("AWS_DEFAULT_REGION"))
            .map(|r| r.trim().to_owned())
            .unwrap_or_else(|| "us-east-1".to_owned());
        Ok(lode_store::S3Config {
            bucket: self.bucket.clone(),
            prefix: self.prefix.clone(),
            region,
            endpoint: self.endpoint.clone(),
            path_style: self.path_style,
            credentials,
            max_object_size,
            tmp_dir: data_dir.join("s3-tmp"),
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStorage {
    #[serde(default)]
    kind: StorageKind,
    bucket: Option<String>,
    prefix: Option<String>,
    region: Option<String>,
    endpoint: Option<String>,
    #[serde(default)]
    path_style: bool,
    access_key_id: Option<String>,
    secret_access_key_file: Option<PathBuf>,
}

impl TryFrom<RawStorage> for StorageConfig {
    type Error = String;

    fn try_from(raw: RawStorage) -> Result<Self, String> {
        let s3_keys = raw.bucket.is_some()
            || raw.prefix.is_some()
            || raw.region.is_some()
            || raw.endpoint.is_some()
            || raw.path_style
            || raw.access_key_id.is_some()
            || raw.secret_access_key_file.is_some();
        if !s3_keys {
            if raw.kind == StorageKind::S3 {
                return Err("[storage] kind = \"s3\" needs a bucket".into());
            }
            return Ok(Self::default());
        }
        let bucket = raw.bucket.unwrap_or_default();
        let bucket_ok = (3..=63).contains(&bucket.len())
            && bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-');
        if !bucket_ok {
            return Err(format!(
                "[storage] bucket {bucket:?} is not a bucket name (3-63 lower-case letters, \
                 digits, dots and dashes)"
            ));
        }
        let prefix = raw.prefix.unwrap_or_default().trim_matches('/').to_owned();
        if prefix.split('/').any(|p| p == "." || p == "..") || prefix.contains("//") {
            return Err("[storage] prefix must not have empty, . or .. parts".into());
        }
        if let Some(e) = &raw.endpoint {
            let ok = reqwest::Url::parse(e).is_ok_and(|u| {
                matches!(u.scheme(), "http" | "https")
                    && u.host_str().is_some()
                    && u.path() == "/"
                    && u.query().is_none()
                    && u.username().is_empty()
            });
            if !ok {
                return Err(format!(
                    "[storage] endpoint {e:?} must be http(s)://host[:port] with nothing after it"
                ));
            }
        }
        if raw.access_key_id.is_some() != raw.secret_access_key_file.is_some() {
            return Err(
                "[storage] give access_key_id and secret_access_key_file together, or \
                        neither to use the AWS_* environment variables"
                    .into(),
            );
        }
        if let Some(r) = &raw.region {
            if r.is_empty() || !r.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                return Err(format!("[storage] region {r:?} is not a region name"));
            }
        }
        Ok(Self {
            kind: raw.kind,
            s3: Some(S3Settings {
                bucket,
                prefix,
                region: raw.region,
                endpoint: raw.endpoint,
                path_style: raw.path_style,
                access_key_id: raw.access_key_id,
                secret_access_key_file: raw.secret_access_key_file,
                secret_access_key: None,
            }),
        })
    }
}

/// Who may still sign in with a password when single sign-on is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PasswordLogin {
    /// Everyone with a password.
    #[default]
    Everyone,
    /// Only server administrators.
    Admins,
}

/// The `[oidc]` table: single sign-on through an OpenID Connect provider (T122).
#[derive(Clone, Deserialize)]
#[serde(try_from = "RawOidc")]
pub struct OidcConfig {
    /// The provider's issuer URL, exactly as its ID tokens name it.
    pub issuer: String,
    /// Lode's client id at the provider.
    pub client_id: String,
    /// The client secret, given inline.
    client_secret: Option<String>,
    /// A file holding the client secret. Resolved against the config file's directory.
    pub client_secret_file: Option<PathBuf>,
    /// How browsers reach this server, such as `https://lode.example.com`. Without it the
    /// request's own address is used.
    pub public_url: Option<String>,
    /// Scopes asked for; always includes `openid`.
    pub scopes: Vec<String>,
    /// The ID token claim that names a user. Default `email`.
    pub user_claim: String,
    /// When not empty, only identities in these domains (after `@`) may sign in.
    pub allowed_domains: Vec<String>,
    /// Create a user on first sign-in. Off by default: an administrator adds users first.
    pub auto_create: bool,
    /// What sign-in buttons call the provider. Default `SSO`.
    pub label: String,
    /// Who may still sign in with a password.
    pub password_login: PasswordLogin,
}

impl std::fmt::Debug for OidcConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The secret never reaches a log.
        f.debug_struct("OidcConfig")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret.as_ref().map(|_| "***"))
            .field("client_secret_file", &self.client_secret_file)
            .field("public_url", &self.public_url)
            .field("scopes", &self.scopes)
            .field("user_claim", &self.user_claim)
            .field("allowed_domains", &self.allowed_domains)
            .field("auto_create", &self.auto_create)
            .field("label", &self.label)
            .field("password_login", &self.password_login)
            .finish()
    }
}

impl OidcConfig {
    /// A configuration for `issuer` and `client_id` with every other setting at its
    /// default. For tests and embedding.
    pub fn new(issuer: &str, client_id: &str) -> Result<Self, String> {
        Self::try_from(RawOidc {
            issuer: issuer.to_owned(),
            client_id: client_id.to_owned(),
            ..RawOidc::default()
        })
    }

    /// Sets the inline client secret.
    pub fn with_client_secret(mut self, secret: impl Into<String>) -> Self {
        self.client_secret = Some(secret.into());
        self
    }

    /// The client secret: the inline one, or the first line of `client_secret_file`, read
    /// each time so that a changed file needs no restart. `None` for a public client.
    pub fn client_secret(&self) -> std::io::Result<Option<String>> {
        if let Some(s) = &self.client_secret {
            return Ok(Some(s.clone()));
        }
        match &self.client_secret_file {
            Some(path) => {
                let text = std::fs::read_to_string(path)?;
                let line = text.lines().next().unwrap_or("").trim().to_owned();
                if line.is_empty() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("{} is empty", path.display()),
                    ));
                }
                Ok(Some(line))
            }
            None => Ok(None),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOidc {
    issuer: String,
    client_id: String,
    client_secret: Option<String>,
    client_secret_file: Option<PathBuf>,
    public_url: Option<String>,
    scopes: Option<Vec<String>>,
    user_claim: Option<String>,
    #[serde(default)]
    allowed_domains: Vec<String>,
    #[serde(default)]
    auto_create: bool,
    label: Option<String>,
    #[serde(default)]
    password_login: PasswordLogin,
}

/// Whether `url` may be used to reach an identity provider: `https`, or `http` to this
/// machine (for tests and a provider on the same host).
pub(crate) fn provider_url_ok(url: &reqwest::Url) -> bool {
    match url.scheme() {
        "https" => url.host_str().is_some(),
        "http" => url.host_str().is_some_and(is_loopback_host),
        _ => false,
    }
}

/// `localhost` or a loopback IP address (IPv6 in brackets, as URLs write it).
pub(crate) fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

impl TryFrom<RawOidc> for OidcConfig {
    type Error = String;

    fn try_from(raw: RawOidc) -> Result<Self, String> {
        let issuer = reqwest::Url::parse(&raw.issuer)
            .map_err(|e| format!("[oidc] issuer is not a URL: {e}"))?;
        if !provider_url_ok(&issuer) {
            return Err("[oidc] issuer must be an https:// URL".into());
        }
        if raw.client_id.trim().is_empty() {
            return Err("[oidc] client_id is required".into());
        }
        if raw.client_secret.is_some() && raw.client_secret_file.is_some() {
            return Err("[oidc] give client_secret or client_secret_file, not both".into());
        }
        let public_url = match raw.public_url {
            Some(u) => {
                let parsed = reqwest::Url::parse(&u)
                    .map_err(|e| format!("[oidc] public_url is not a URL: {e}"))?;
                if !matches!(parsed.scheme(), "http" | "https") || parsed.host().is_none() {
                    return Err("[oidc] public_url must be an http:// or https:// address".into());
                }
                Some(u.trim_end_matches('/').to_owned())
            }
            None => None,
        };
        let mut scopes = raw
            .scopes
            .unwrap_or_else(|| vec!["openid".into(), "email".into(), "profile".into()]);
        if !scopes.iter().any(|s| s == "openid") {
            scopes.insert(0, "openid".into());
        }
        if scopes
            .iter()
            .any(|s| s.is_empty() || s.contains(char::is_whitespace))
        {
            return Err("[oidc] scopes must be single words".into());
        }
        let user_claim = raw.user_claim.unwrap_or_else(|| "email".into());
        if user_claim.trim().is_empty() {
            return Err("[oidc] user_claim must not be empty".into());
        }
        let allowed_domains = raw
            .allowed_domains
            .into_iter()
            .map(|d| d.trim().trim_start_matches('@').to_ascii_lowercase())
            .collect::<Vec<_>>();
        if allowed_domains.iter().any(String::is_empty) {
            return Err("[oidc] allowed_domains has an empty entry".into());
        }
        let label = raw
            .label
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| "SSO".into());
        if label.chars().count() > 60 {
            return Err("[oidc] label is too long (at most 60 characters)".into());
        }
        Ok(Self {
            issuer: raw.issuer,
            client_id: raw.client_id,
            client_secret: raw.client_secret,
            client_secret_file: raw.client_secret_file,
            public_url,
            scopes,
            user_claim,
            allowed_domains,
            auto_create: raw.auto_create,
            label,
            password_login: raw.password_login,
        })
    }
}

/// The `[webhooks]` table, read by the webhooks module.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WebhooksConfig {
    /// Accept `http://` hook URLs. Off by default: hooks must use `https://`.
    pub allow_http: bool,
}

/// The `[backup]` table.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RawBackup")]
pub struct BackupConfig {
    /// Time between backups; `None` turns scheduled backups off.
    pub every: Option<std::time::Duration>,
    /// Complete backups kept. Default 7.
    pub keep: usize,
    /// Where backups go. Resolved against the config file's directory by
    /// [`ServerConfig::load`]. Default `backups`.
    pub dir: PathBuf,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            every: None,
            keep: 7,
            dir: PathBuf::from("backups"),
        }
    }
}

impl BackupConfig {
    /// The schedule, when backups are on.
    pub fn schedule(&self) -> Option<crate::maintenance::BackupSchedule> {
        self.every.map(|every| crate::maintenance::BackupSchedule {
            every,
            keep: self.keep,
            dir: self.dir.clone(),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBackup {
    every: Option<String>,
    keep: Option<usize>,
    dir: Option<PathBuf>,
}

impl TryFrom<RawBackup> for BackupConfig {
    type Error = String;

    fn try_from(raw: RawBackup) -> Result<Self, String> {
        let defaults = Self::default();
        let every = raw.every.as_deref().map(parse_interval).transpose()?;
        let keep = raw.keep.unwrap_or(defaults.keep);
        if keep == 0 {
            return Err("[backup] keep must be at least 1".into());
        }
        Ok(Self {
            every,
            keep,
            dir: raw.dir.unwrap_or(defaults.dir),
        })
    }
}

/// An interval as `[backup] every` takes it: `7d`, `24h`, `30m` or `90s`.
fn interval_text(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    match secs {
        s if s > 0 && s % 86_400 == 0 => format!("{}d", s / 86_400),
        s if s > 0 && s % 3600 == 0 => format!("{}h", s / 3600),
        s if s > 0 && s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

/// `"90s"`, `"30m"`, `"24h"` or `"7d"`; at least one minute.
pub fn parse_interval(text: &str) -> Result<std::time::Duration, String> {
    let bad = || format!("{text:?} is not an interval such as \"24h\" (s, m, h or d)");
    let t = text.trim();
    let split = t.find(|c: char| !c.is_ascii_digit()).ok_or_else(bad)?;
    let (n, unit) = t.split_at(split);
    let n: u64 = n.parse().map_err(|_| bad())?;
    let secs = match unit {
        "s" => Some(n),
        "m" => n.checked_mul(60),
        "h" => n.checked_mul(3600),
        "d" => n.checked_mul(86_400),
        _ => None,
    }
    .ok_or_else(bad)?;
    if secs < 60 {
        return Err(format!("{text:?} is shorter than one minute"));
    }
    Ok(std::time::Duration::from_secs(secs))
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
    /// The event feed (`GET /depots/{d}/events`, `wait_for_events`).
    pub feed: bool,
}

impl Default for ModulesConfig {
    fn default() -> Self {
        Self {
            locks: true,
            shelves: true,
            mcp: true,
            webhooks: false,
            checks: false,
            feed: true,
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
            "feed" => self.feed,
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
        cfg.backup.dir = base.join(&cfg.backup.dir);
        if let Some(oidc) = &mut cfg.oidc {
            if let Some(f) = &mut oidc.client_secret_file {
                *f = base.join(&*f);
            }
        }
        if let Some(s3) = &mut cfg.storage.s3 {
            if let Some(f) = &mut s3.secret_access_key_file {
                *f = base.join(&*f);
            }
        }
        if let Some(f) = &mut cfg.database.url_file {
            *f = base.join(&*f);
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

    /// The configuration as the server uses it, as `loded.toml` text: every setting with its
    /// effective value, paths resolved, and secrets shown as `***` (T143: the admin web
    /// UI's settings page).
    pub fn effective_toml(&self) -> String {
        use toml::{Table, Value};
        fn s(v: impl Into<String>) -> Value {
            Value::String(v.into())
        }
        fn p(v: &Path) -> Value {
            Value::String(v.display().to_string())
        }
        fn n(v: impl TryInto<i64>) -> Value {
            Value::Integer(v.try_into().unwrap_or(i64::MAX))
        }
        fn list(v: &[String]) -> Value {
            Value::Array(v.iter().cloned().map(Value::String).collect())
        }
        const HIDDEN: &str = "***";
        let mut root = Table::new();
        root.insert("listen".into(), s(self.listen.to_string()));
        root.insert("data_dir".into(), p(&self.data_dir));
        match &self.tls {
            Some(TlsConfig::Files { cert, key }) => {
                let mut t = Table::new();
                t.insert("cert".into(), p(cert));
                t.insert("key".into(), p(key));
                root.insert("tls".into(), Value::Table(t));
            }
            Some(TlsConfig::Acme(a)) => {
                let mut t = Table::new();
                t.insert("acme_domain".into(), s(&a.domain));
                if let Some(e) = &a.email {
                    t.insert("acme_email".into(), s(e));
                }
                t.insert("acme_directory".into(), s(&a.directory));
                if let Some(ca) = &a.ca {
                    t.insert("acme_ca".into(), p(ca));
                }
                root.insert("tls".into(), Value::Table(t));
            }
            None => {}
        }
        let c = &self.connections;
        let mut t = Table::new();
        t.insert("header_timeout_seconds".into(), n(c.header_timeout_seconds));
        t.insert("max_per_address".into(), n(c.max_per_address));
        root.insert("connections".into(), Value::Table(t));
        let l = &self.limits;
        let mut t = Table::new();
        t.insert("max_request_json".into(), n(l.max_request_json));
        t.insert("max_blob_size".into(), n(l.max_blob_size));
        t.insert("max_changes_per_commit".into(), n(l.max_changes_per_commit));
        t.insert("max_changes_listed".into(), n(l.max_changes_listed));
        t.insert("mcp_max_edits".into(), n(l.mcp_max_edits));
        t.insert("mcp_max_inline_bytes".into(), n(l.mcp_max_inline_bytes));
        t.insert("body_idle_seconds".into(), n(l.body_idle_seconds));
        root.insert("limits".into(), Value::Table(t));
        let m = &self.modules;
        let mut t = Table::new();
        for (name, on) in [
            ("locks", m.locks),
            ("shelves", m.shelves),
            ("mcp", m.mcp),
            ("webhooks", m.webhooks),
            ("checks", m.checks),
            ("feed", m.feed),
        ] {
            t.insert(name.into(), Value::Boolean(on));
        }
        root.insert("modules".into(), Value::Table(t));
        let mut t = Table::new();
        t.insert("grace_seconds".into(), n(self.gc.grace_seconds));
        root.insert("gc".into(), Value::Table(t));
        let mut t = Table::new();
        if let Some(every) = self.backup.every {
            t.insert("every".into(), s(interval_text(every)));
        }
        t.insert("keep".into(), n(self.backup.keep));
        t.insert("dir".into(), p(&self.backup.dir));
        root.insert("backup".into(), Value::Table(t));
        if let Some(o) = &self.oidc {
            let mut t = Table::new();
            t.insert("issuer".into(), s(&o.issuer));
            t.insert("client_id".into(), s(&o.client_id));
            if o.client_secret.is_some() {
                t.insert("client_secret".into(), s(HIDDEN));
            }
            if let Some(f) = &o.client_secret_file {
                t.insert("client_secret_file".into(), p(f));
            }
            if let Some(u) = &o.public_url {
                t.insert("public_url".into(), s(u));
            }
            t.insert("scopes".into(), list(&o.scopes));
            t.insert("user_claim".into(), s(&o.user_claim));
            t.insert("allowed_domains".into(), list(&o.allowed_domains));
            t.insert("auto_create".into(), Value::Boolean(o.auto_create));
            t.insert("label".into(), s(&o.label));
            let login = match o.password_login {
                PasswordLogin::Everyone => "everyone",
                PasswordLogin::Admins => "admins",
            };
            t.insert("password_login".into(), s(login));
            root.insert("oidc".into(), Value::Table(t));
        }
        let mut t = Table::new();
        let kind = match self.storage.kind {
            StorageKind::Fs => "fs",
            StorageKind::S3 => "s3",
        };
        t.insert("kind".into(), s(kind));
        if let Some(s3) = &self.storage.s3 {
            t.insert("bucket".into(), s(&s3.bucket));
            t.insert("prefix".into(), s(&s3.prefix));
            if let Some(r) = &s3.region {
                t.insert("region".into(), s(r));
            }
            if let Some(e) = &s3.endpoint {
                t.insert("endpoint".into(), s(e));
            }
            t.insert("path_style".into(), Value::Boolean(s3.path_style));
            if s3.access_key_id.is_some() {
                t.insert("access_key_id".into(), s(HIDDEN));
            }
            if let Some(f) = &s3.secret_access_key_file {
                t.insert("secret_access_key_file".into(), p(f));
            }
        }
        root.insert("storage".into(), Value::Table(t));
        let mut t = Table::new();
        t.insert(
            "allow_http".into(),
            Value::Boolean(self.webhooks.allow_http),
        );
        root.insert("webhooks".into(), Value::Table(t));
        let mut t = Table::new();
        t.insert("enabled".into(), Value::Boolean(self.admin_ui.enabled));
        root.insert("admin_ui".into(), Value::Table(t));
        root.to_string()
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
            backup: BackupConfig::default(),
            modules: ModulesConfig::default(),
            webhooks: WebhooksConfig::default(),
            oidc: None,
            storage: StorageConfig::default(),
            admin_ui: AdminUiConfig::default(),
            database: DatabaseConfig::default(),
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
        assert!(cfg.admin_ui.enabled);
    }

    #[test]
    fn the_effective_settings_hide_secrets_and_parse_back() {
        let text = "data_dir = 'd'\n[backup]\nevery = '24h'\n[oidc]\nissuer = 'https://id.example.com'\n\
                    client_id = 'lode'\nclient_secret = 'oidc-secret-value'\n[storage]\nbucket = 'lode-content'\n\
                    access_key_id = 'AKIAEXAMPLE'\nsecret_access_key_file = 'k.txt'\n";
        let cfg = ServerConfig::parse(text, Path::new("/srv"), Path::new("x")).unwrap();
        let shown = cfg.effective_toml();
        assert!(!shown.contains("oidc-secret-value"), "{shown}");
        assert!(!shown.contains("AKIAEXAMPLE"), "{shown}");
        assert!(shown.contains("client_secret = \"***\""), "{shown}");
        assert!(shown.contains("every = \"1d\""), "{shown}");
        // Apart from the hidden values, the text is a configuration the server reads.
        let back = ServerConfig::parse(
            &shown.replace("\"***\"", "\"x\""),
            Path::new("/"),
            Path::new("x"),
        )
        .unwrap();
        assert_eq!(back.backup.every, cfg.backup.every);
        assert_eq!(back.data_dir, cfg.data_dir);
        let plain = ServerConfig::parse(DEFAULT_CONFIG, Path::new("/srv"), Path::new("x")).unwrap();
        ServerConfig::parse(&plain.effective_toml(), Path::new("/"), Path::new("x")).unwrap();
    }

    #[test]
    fn the_admin_ui_can_be_turned_off() {
        let cfg = ServerConfig::parse(
            "data_dir = '.'\n[admin_ui]\nenabled = false",
            Path::new("/srv"),
            Path::new("x"),
        )
        .unwrap();
        assert!(!cfg.admin_ui.enabled);
        assert!(ServerConfig::parse(
            "data_dir = '.'\n[admin_ui]\nport = 1",
            Path::new("/srv"),
            Path::new("x")
        )
        .is_err());
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
    fn backup_intervals_and_defaults() {
        let parse = |text: &str| {
            ServerConfig::parse(
                &format!("data_dir = '.'\n{text}"),
                Path::new("/srv/lode"),
                Path::new("x"),
            )
        };
        let cfg = parse("").unwrap();
        assert!(cfg.backup.schedule().is_none());
        assert_eq!(cfg.backup.dir, Path::new("/srv/lode/backups"));
        let cfg = parse("[backup]\nevery = '24h'\nkeep = 3\ndir = '/mnt/b'").unwrap();
        let s = cfg.backup.schedule().unwrap();
        assert_eq!(s.every, std::time::Duration::from_secs(86_400));
        assert_eq!((s.keep, s.dir), (3, PathBuf::from("/mnt/b")));
        assert_eq!(parse_interval("90s").unwrap().as_secs(), 90);
        assert_eq!(parse_interval("30m").unwrap().as_secs(), 1_800);
        assert_eq!(parse_interval("7d").unwrap().as_secs(), 604_800);
        for bad in ["10s", "1w", "h", "", "-1h", "1.5h"] {
            assert!(parse_interval(bad).is_err(), "{bad}");
        }
        assert!(parse("[backup]\nevery = '1h'\nkeep = 0").is_err());
        assert!(parse("[backup]\nwhen = '1h'").is_err());
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

    #[test]
    fn oidc_settings_defaults_and_mistakes() {
        let parse = |oidc: &str| {
            ServerConfig::parse(
                &format!("data_dir = '.'\n[oidc]\n{oidc}"),
                Path::new("/etc/lode"),
                Path::new("x"),
            )
        };
        let cfg = parse(
            "issuer = 'https://idp.example.com'\nclient_id = 'lode'\nclient_secret = 'shh'\n\
             allowed_domains = ['@Example.com']\nscopes = ['email']\npassword_login = 'admins'",
        )
        .unwrap();
        let o = cfg.oidc.unwrap();
        assert_eq!(o.scopes, ["openid", "email"], "openid is always asked for");
        assert_eq!(o.user_claim, "email");
        assert_eq!(o.label, "SSO");
        assert_eq!(o.allowed_domains, ["example.com"]);
        assert!(!o.auto_create);
        assert_eq!(o.password_login, PasswordLogin::Admins);
        assert_eq!(o.client_secret().unwrap().as_deref(), Some("shh"));
        assert!(
            !format!("{o:?}").contains("shh"),
            "the secret is not printed"
        );

        let o = parse(
            "issuer = 'http://127.0.0.1:9000'\nclient_id = 'lode'\nclient_secret_file = 's.txt'",
        )
        .unwrap()
        .oidc
        .unwrap();
        assert_eq!(
            o.client_secret_file.as_deref(),
            Some(Path::new("/etc/lode/s.txt"))
        );
        assert_eq!(o.password_login, PasswordLogin::Everyone);

        for bad in [
            "issuer = 'http://idp.example.com'\nclient_id = 'lode'",
            "issuer = 'not a url'\nclient_id = 'lode'",
            "issuer = 'https://idp'\nclient_id = ''",
            "issuer = 'https://idp'\nclient_id = 'x'\nclient_secret = 'a'\nclient_secret_file = 'b'",
            "issuer = 'https://idp'\nclient_id = 'x'\npublic_url = 'ftp://x'",
            "issuer = 'https://idp'\nclient_id = 'x'\npassword_login = 'nobody'",
            "issuer = 'https://idp'\nclient_id = 'x'\nsurprise = 1",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_client_secret_file_is_read_when_used() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret.txt");
        let mut o = OidcConfig::new("https://idp", "lode").unwrap();
        o.client_secret_file = Some(file.clone());
        assert!(o.client_secret().is_err(), "missing file");
        std::fs::write(&file, "first\n").unwrap();
        assert_eq!(o.client_secret().unwrap().as_deref(), Some("first"));
        std::fs::write(&file, "second\r\n").unwrap();
        assert_eq!(o.client_secret().unwrap().as_deref(), Some("second"));
        std::fs::write(&file, "\n").unwrap();
        assert!(o.client_secret().is_err(), "empty file");
    }

    #[test]
    fn storage_settings() {
        let parse = |storage: &str| {
            ServerConfig::parse(
                &format!("data_dir = '.'\n[storage]\n{storage}"),
                Path::new("/etc/lode"),
                Path::new("x"),
            )
        };
        let plain = ServerConfig::parse("data_dir = '.'", Path::new("/"), Path::new("x")).unwrap();
        assert_eq!(plain.storage, StorageConfig::default());
        assert_eq!(plain.storage.kind, StorageKind::Fs);
        assert_eq!(
            parse("kind = 'fs'").unwrap().storage,
            StorageConfig::default()
        );
        let cfg = parse(
            "kind = 's3'\nbucket = 'lode-data'\nprefix = '/main/'\nendpoint = 'http://127.0.0.1:9000'\n\
             path_style = true\naccess_key_id = 'AK'\nsecret_access_key_file = 's3.txt'",
        )
        .unwrap();
        let s3 = cfg.storage.s3_in_use().unwrap();
        assert_eq!(s3.bucket, "lode-data");
        assert_eq!(s3.prefix, "main");
        assert!(s3.path_style);
        assert_eq!(
            s3.secret_access_key_file.as_deref(),
            Some(Path::new("/etc/lode/s3.txt"))
        );
        for bad in [
            "bucket = 'b'",
            "kind = 'gcs'",
            "kind = 's3'",
            "kind = 's3'\nbucket = 'Upper'",
            "kind = 's3'\nbucket = 'ok-name'\nendpoint = 'ftp://x'",
            "kind = 's3'\nbucket = 'ok-name'\nendpoint = 'https://x/path'",
            "kind = 's3'\nbucket = 'ok-name'\nendpoint = 'https://user:pw@x'",
            "kind = 's3'\nbucket = 'ok-name'\naccess_key_id = 'AK'",
            "kind = 's3'\nbucket = 'ok-name'\nsecret_access_key = 'inline'",
            "kind = 's3'\nbucket = 'ok-name'\nprefix = 'a/../b'",
            "kind = 's3'\nbucket = 'ok-name'\nregion = 'eu central'",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        // Prepared for `loded migrate-storage`: S3 settings, still on files.
        let prepared = parse("bucket = 'lode-data'\nregion = 'eu-west-1'")
            .unwrap()
            .storage;
        assert_eq!(prepared.kind, StorageKind::Fs);
        assert!(prepared.s3_in_use().is_none());
        assert_eq!(prepared.s3.unwrap().bucket, "lode-data");
    }

    #[test]
    fn s3_credentials_and_region_are_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                vars.iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        let none = env(&[]);
        let aws = env(&[
            ("AWS_ACCESS_KEY_ID", "ENVKEY"),
            ("AWS_SECRET_ACCESS_KEY", "envsecret"),
            ("AWS_SESSION_TOKEN", "tok"),
            ("AWS_DEFAULT_REGION", "ap-south-1"),
        ]);

        let mut s = S3Settings::new("bucket");
        assert!(s
            .store_config(dir.path(), 10, &none)
            .unwrap_err()
            .contains("AWS_ACCESS_KEY_ID"));
        let c = s.store_config(dir.path(), 10, &aws).unwrap();
        assert_eq!(c.credentials.access_key_id, "ENVKEY");
        assert_eq!(c.credentials.secret_access_key, "envsecret");
        assert_eq!(c.credentials.session_token.as_deref(), Some("tok"));
        assert_eq!(c.region, "ap-south-1");
        assert_eq!(c.tmp_dir, dir.path().join("s3-tmp"));
        assert_eq!(c.max_object_size, 10);

        let file = dir.path().join("secret.txt");
        s.access_key_id = Some("FILEKEY".into());
        s.secret_access_key_file = Some(file.clone());
        s.region = Some("eu-west-1".into());
        let err = s.store_config(dir.path(), 10, &aws).unwrap_err();
        assert!(err.contains("secret.txt"), "{err}");
        std::fs::write(&file, "filesecret\r\nignored\n").unwrap();
        let c = s.store_config(dir.path(), 10, &aws).unwrap();
        assert_eq!(c.credentials.access_key_id, "FILEKEY");
        assert_eq!(c.credentials.secret_access_key, "filesecret");
        assert_eq!(c.credentials.session_token, None);
        assert_eq!(c.region, "eu-west-1");

        let s = S3Settings::new("bucket").with_credentials("K", "inline-secret");
        let c = s.store_config(dir.path(), 10, &none).unwrap();
        assert_eq!(c.credentials.secret_access_key, "inline-secret");
        assert_eq!(c.region, "us-east-1");
        assert!(!format!("{s:?}").contains("inline-secret"));
        assert!(!format!("{c:?}").contains("inline-secret"));
    }
}
