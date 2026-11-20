//! Global options, user settings, sign-in resolution and the client.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lode_client::{read_token_file, Client, CredentialStore, ServerUrl};
use lode_workspace::db::keys;
use lode_workspace::{BlobCache, Workspace, DEFAULT_SHARED_CACHE_CAP};
use serde::{Deserialize, Serialize};

use crate::error::{Exit, OpError, OpResult};
use crate::output::{Out, Ui};

/// `User-Agent` product of this binary; the server records it as `provenance.client`.
pub const PRODUCT: &str = concat!("lode-cli/", env!("CARGO_PKG_VERSION"));

/// Options every command takes.
#[derive(Debug, Clone, Default)]
pub struct Globals {
    /// `--json`.
    pub json: bool,
    /// `--server`.
    pub server: Option<String>,
    /// `-C <dir>`.
    pub dir: Option<PathBuf>,
    /// `--insecure`.
    pub insecure: bool,
    /// Self-reported agent provenance for commits and shelves (`lode mcp --agent-…`).
    pub agent: Option<lode_proto::commit::AgentProvenance>,
}

/// `<config>/settings.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Server used when nothing else names one; set by `lode login`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Shared download cache directory; `LODE_CACHE_DIR` overrides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_dir: Option<PathBuf>,
    /// Size cap of the shared download cache in GiB (default 20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_max_gib: Option<u64>,
    /// Caching proxy (T154) per server URL: content downloads from that server go through
    /// it. `LODE_PROXY` overrides it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub proxies: BTreeMap<String, String>,
    /// Merge tools by name (T164): what `lode resolve --tool` runs for a name that a depot's
    /// `[merge.tools]` gives a file. Only this file decides what runs.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub merge_tools: BTreeMap<String, MergeToolSetting>,
    /// Validation scripts (T202): which Python, and which depots' scripts are trusted.
    #[serde(
        default,
        skip_serializing_if = "crate::cmd::validate::ValidateSettings::is_empty"
    )]
    pub validate: crate::cmd::validate::ValidateSettings,
}

/// `[merge_tools.NAME]` in `settings.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeToolSetting {
    /// Program and arguments; `{base}`, `{mine}`, `{theirs}` and `{merged}` are replaced by
    /// file paths wherever they appear in an argument. No shell is involved.
    pub command: Vec<String>,
    /// Whether exit status 0 means merged (default true); when false, the merged file must
    /// have changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_exit_code: Option<bool>,
}

/// What a command runs with.
#[derive(Debug)]
pub struct Ctx {
    /// Global options.
    pub g: Globals,
    /// Working directory (after `-C`).
    pub cwd: PathBuf,
    /// The user's lode config directory.
    pub config_dir: PathBuf,
    /// Output.
    pub out: Out,
}

impl Globals {
    /// The provenance a commit or shelf made with these options sends.
    pub fn provenance(&self) -> Option<lode_proto::commit::Provenance> {
        self.agent
            .clone()
            .map(|agent| lode_proto::commit::Provenance {
                agent: Some(agent),
                ..Default::default()
            })
    }
}

impl Ctx {
    /// Resolves the working directory (`-C` against the process's current directory) and
    /// the config directory; output goes to `ui`.
    pub fn new(g: Globals, ui: Arc<dyn Ui>) -> OpResult<Self> {
        let base = std::env::current_dir()?;
        Self::in_dir(g, &base, ui)
    }

    /// Like [`Ctx::new`], with `base` in place of the process's current directory. The
    /// desktop app runs every workspace's operations this way.
    pub fn in_dir(g: Globals, base: &Path, ui: Arc<dyn Ui>) -> OpResult<Self> {
        let base = base.to_path_buf();
        let cwd = match &g.dir {
            Some(d) => base.join(d),
            None => base,
        };
        if !cwd.is_dir() {
            return Err(OpError::usage(format!(
                "{} is not a directory",
                cwd.display()
            )));
        }
        // Canonical, like the workspace root, so command-line paths can be matched to it.
        let cwd = lode_workspace::paths::canonical(&cwd)?;
        let config_dir = match std::env::var_os("LODE_CONFIG_DIR") {
            Some(d) if !d.is_empty() => PathBuf::from(d),
            _ => directories::ProjectDirs::from("", "", "lode")
                .map(|d| d.config_dir().to_path_buf())
                .ok_or_else(|| {
                    OpError::failed(
                        "no_config_dir",
                        "cannot find a config directory; set LODE_CONFIG_DIR",
                    )
                })?,
        };
        let out = Out::new(g.json, ui);
        Ok(Self {
            g,
            cwd,
            config_dir,
            out,
        })
    }

    /// Uses `dir` as the config directory (stored sign-ins and settings) instead of
    /// `LODE_CONFIG_DIR` or the platform's.
    pub fn with_config_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.config_dir = dir.into();
        self
    }

    /// Stored tokens.
    pub fn credentials(&self) -> CredentialStore {
        CredentialStore::new(self.config_dir.join("credentials"))
    }

    fn settings_path(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }

    /// User settings; missing file means defaults.
    pub fn settings(&self) -> OpResult<Settings> {
        match std::fs::read_to_string(self.settings_path()) {
            Ok(text) => toml::from_str(&text).map_err(|e| {
                OpError::failed(
                    "bad_settings",
                    format!("{}: {e}", self.settings_path().display()),
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Writes user settings.
    pub fn save_settings(&self, s: &Settings) -> OpResult<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        let text =
            toml::to_string(s).map_err(|e| OpError::failed("bad_settings", e.to_string()))?;
        std::fs::write(self.settings_path(), text)?;
        Ok(())
    }

    /// Parses and checks a server URL against `--insecure`.
    pub fn parse_server(&self, url: &str) -> OpResult<ServerUrl> {
        Ok(lode_client::parse_server_url(url, self.g.insecure)?)
    }

    /// The server: `--server`, else the workspace's, else `LODE_SERVER`, else the one from
    /// `lode login`.
    pub fn server_url(&self, ws: Option<&Workspace>) -> OpResult<ServerUrl> {
        if let Some(s) = &self.g.server {
            return self.parse_server(s);
        }
        if let Some(ws) = ws {
            if let Some(s) = ws.db().config(keys::SERVER_URL)? {
                return self.parse_server(&s);
            }
        }
        if let Some(s) = std::env::var("LODE_SERVER").ok().filter(|s| !s.is_empty()) {
            return self.parse_server(&s);
        }
        if let Some(s) = self.settings()?.server {
            return self.parse_server(&s);
        }
        Err(OpError::usage(
            "no server given: pass --server, set LODE_SERVER, or run `lode login <server>`",
        ))
    }

    /// The token for `server`: `LODE_TOKEN_FILE` first, then the stored login.
    pub fn token(&self, server: &ServerUrl) -> OpResult<Option<String>> {
        if let Some(file) = std::env::var_os("LODE_TOKEN_FILE").filter(|f| !f.is_empty()) {
            let path = PathBuf::from(file);
            // The path is only reported, never stored in the workspace.
            return match read_token_file(&path) {
                Ok(Some(t)) => Ok(Some(t)),
                Ok(None) => Err(OpError::new(
                    Exit::Auth,
                    "unauthenticated",
                    format!(
                        "LODE_TOKEN_FILE names {}, which does not exist",
                        path.display()
                    ),
                )),
                Err(e) => Err(OpError::new(
                    Exit::Auth,
                    "unauthenticated",
                    format!("cannot read LODE_TOKEN_FILE {}: {e}", path.display()),
                )),
            };
        }
        Ok(self.credentials().load(server)?)
    }

    /// The shared download cache: `LODE_CACHE_DIR`, else the `cache_dir` setting; none when
    /// neither is set.
    pub fn shared_cache(&self) -> OpResult<Option<BlobCache>> {
        let settings = self.settings()?;
        let dir = match std::env::var_os("LODE_CACHE_DIR").filter(|d| !d.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => match settings.cache_dir {
                Some(d) if !d.as_os_str().is_empty() => d,
                _ => return Ok(None),
            },
        };
        let cap = settings
            .cache_max_gib
            .map_or(DEFAULT_SHARED_CACHE_CAP, |g| g.saturating_mul(1 << 30));
        Ok(Some(BlobCache::new(dir, cap)))
    }

    /// The caching proxy for `server` and where it was set: `LODE_PROXY` (empty or `none`
    /// for none), else the `[proxies]` setting.
    pub fn proxy_for(&self, server: &ServerUrl) -> OpResult<Option<(String, &'static str)>> {
        if let Some(p) = std::env::var_os("LODE_PROXY") {
            let p = p.to_string_lossy().trim().to_owned();
            if p.is_empty() || p.eq_ignore_ascii_case("none") {
                return Ok(None);
            }
            return Ok(Some((p, "LODE_PROXY")));
        }
        Ok(self
            .settings()?
            .proxies
            .get(server.as_str())
            .filter(|p| !p.is_empty())
            .map(|p| (p.clone(), "settings")))
    }

    /// A client for `server` without a token. Content downloads go through the caching
    /// proxy when one is set; if it fails, a warning is shown once and the server is used.
    pub fn anonymous_client(&self, server: &ServerUrl) -> OpResult<Client> {
        let out = self.out.clone();
        Ok(Client::builder(server.as_str())
            .allow_insecure_http(self.g.insecure)
            .product(PRODUCT)
            .proxy(self.proxy_for(server)?.map(|(p, _)| p))
            .on_proxy_failure(Arc::new(move |m| out.warn(m)))
            .build()?)
    }

    /// A signed-in client for `server`.
    pub fn client(&self, server: &ServerUrl) -> OpResult<Client> {
        let token = self.token(server)?.ok_or_else(|| {
            OpError::new(
                Exit::Auth,
                "unauthenticated",
                format!(
                    "not signed in to {server}: run `lode login {server}` or set LODE_TOKEN_FILE"
                ),
            )
        })?;
        Ok(self.anonymous_client(server)?.with_token(token))
    }

    /// The workspace containing the working directory.
    pub fn workspace(&self) -> OpResult<Workspace> {
        Ok(Workspace::discover(&self.cwd)?)
    }

    /// A path from the command line, relative to the working directory.
    pub fn local(&self, p: &Path) -> PathBuf {
        self.cwd.join(p)
    }
}
