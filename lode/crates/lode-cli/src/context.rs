//! Global options, user settings, sign-in resolution and the client.

use std::path::{Path, PathBuf};

use lode_client::{read_token_file, Client, CredentialStore, ServerUrl};
use lode_workspace::db::keys;
use lode_workspace::Workspace;
use serde::{Deserialize, Serialize};

use crate::error::{CliError, CliResult, Exit};
use crate::output::Out;

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
    /// Resolves the working and config directories.
    pub fn new(g: Globals) -> CliResult<Self> {
        let base = std::env::current_dir()?;
        let cwd = match &g.dir {
            Some(d) => base.join(d),
            None => base,
        };
        if !cwd.is_dir() {
            return Err(CliError::usage(format!(
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
                    CliError::failed(
                        "no_config_dir",
                        "cannot find a config directory; set LODE_CONFIG_DIR",
                    )
                })?,
        };
        let out = Out { json: g.json };
        Ok(Self {
            g,
            cwd,
            config_dir,
            out,
        })
    }

    /// Stored tokens.
    pub fn credentials(&self) -> CredentialStore {
        CredentialStore::new(self.config_dir.join("credentials"))
    }

    fn settings_path(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }

    /// User settings; missing file means defaults.
    pub fn settings(&self) -> CliResult<Settings> {
        match std::fs::read_to_string(self.settings_path()) {
            Ok(text) => toml::from_str(&text).map_err(|e| {
                CliError::failed(
                    "bad_settings",
                    format!("{}: {e}", self.settings_path().display()),
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Writes user settings.
    pub fn save_settings(&self, s: &Settings) -> CliResult<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        let text =
            toml::to_string(s).map_err(|e| CliError::failed("bad_settings", e.to_string()))?;
        std::fs::write(self.settings_path(), text)?;
        Ok(())
    }

    /// Parses and checks a server URL against `--insecure`.
    pub fn parse_server(&self, url: &str) -> CliResult<ServerUrl> {
        Ok(lode_client::parse_server_url(url, self.g.insecure)?)
    }

    /// The server: `--server`, else the workspace's, else `LODE_SERVER`, else the one from
    /// `lode login`.
    pub fn server_url(&self, ws: Option<&Workspace>) -> CliResult<ServerUrl> {
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
        Err(CliError::usage(
            "no server given: pass --server, set LODE_SERVER, or run `lode login <server>`",
        ))
    }

    /// The token for `server`: `LODE_TOKEN_FILE` first, then the stored login.
    pub fn token(&self, server: &ServerUrl) -> CliResult<Option<String>> {
        if let Some(file) = std::env::var_os("LODE_TOKEN_FILE").filter(|f| !f.is_empty()) {
            let path = PathBuf::from(file);
            return match read_token_file(&path)? {
                Some(t) => Ok(Some(t)),
                None => Err(CliError::new(
                    Exit::Auth,
                    "unauthenticated",
                    format!(
                        "LODE_TOKEN_FILE names {}, which does not exist",
                        path.display()
                    ),
                )),
            };
        }
        Ok(self.credentials().load(server)?)
    }

    /// A client for `server` without a token.
    pub fn anonymous_client(&self, server: &ServerUrl) -> CliResult<Client> {
        Ok(Client::builder(server.as_str())
            .allow_insecure_http(self.g.insecure)
            .product(PRODUCT)
            .build()?)
    }

    /// A signed-in client for `server`.
    pub fn client(&self, server: &ServerUrl) -> CliResult<Client> {
        let token = self.token(server)?.ok_or_else(|| {
            CliError::new(
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
    pub fn workspace(&self) -> CliResult<Workspace> {
        Ok(Workspace::discover(&self.cwd)?)
    }

    /// A path from the command line, relative to the working directory.
    pub fn local(&self, p: &Path) -> PathBuf {
        self.cwd.join(p)
    }
}
