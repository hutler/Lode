//! Test helpers for end-to-end tests (T027).
//!
//! - [`TestServer`] runs `lode-server` in this process on a temporary data directory and a
//!   free loopback port, with a controllable clock, an admin token, request counting,
//!   failpoints and helpers to create users, agents, depots and access rules.
//! - [`TestWorkspace`] runs the real `lode` binary with its own home and config directories,
//!   so tests never touch the developer's credentials and can run in parallel.
//!
//! - [`oidc::TestProvider`] is an OpenID Connect provider for single sign-on tests.
//!
//! Every server and workspace lives in its own temporary directory and is removed on drop.

#![forbid(unsafe_code)]

pub mod oidc;
pub mod web;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use lode_client::Client;
use lode_core::{blob_id, Change, ChangeAction, DepotPath, EntryKind};
use lode_proto::admin::{AccessLevel, AclEffect, AclRule, AclSubject, CreatePrincipalRequest};
use lode_proto::auth::CreateTokenRequest;
use lode_proto::commit::CommitRequest;
use lode_proto::PrincipalKind;
use lode_server::failpoint::Failpoints;
use lode_server::{init, open_state, router, AppState, BoundServer, ManualClock, ServerConfig};
use tokio::sync::oneshot;

pub use lode_server::failpoint;

/// One request as the server saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedRequest {
    /// `GET`, `PUT`, …
    pub method: String,
    /// The path, without the query.
    pub path: String,
}

/// Requests the server received, in order.
#[derive(Debug, Clone, Default)]
pub struct RequestLog(Arc<Mutex<Vec<LoggedRequest>>>);

impl RequestLog {
    /// Every request so far.
    pub fn all(&self) -> Vec<LoggedRequest> {
        self.0.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// Requests with `method` whose path contains `fragment`, such as `("PUT", "/blobs/")`.
    pub fn count(&self, method: &str, fragment: &str) -> usize {
        self.all()
            .iter()
            .filter(|r| r.method == method && r.path.contains(fragment))
            .count()
    }

    /// Forgets the requests so far.
    pub fn clear(&self) {
        if let Ok(mut v) = self.0.lock() {
            v.clear();
        }
    }
}

async fn record(log: RequestLog, req: Request, next: Next) -> Response {
    if let Ok(mut v) = log.0.lock() {
        v.push(LoggedRequest {
            method: req.method().to_string(),
            path: req.uri().path().to_owned(),
        });
    }
    next.run(req).await
}

/// Every module `loded` is built with; `[modules]` in the configuration picks the enabled
/// ones, as in `loded`.
pub fn available_modules() -> lode_server::module::Modules {
    vec![
        lode_mod_locks::Locks::module(),
        lode_mod_shelves::Shelves::module(),
        lode_mod_mcp::Mcp::module(),
        lode_mod_webhooks::Webhooks::module(),
        lode_mod_checks::Checks::module(),
    ]
}

/// Options for [`TestServer::start_with`].
pub struct TestServerOptions {
    /// Changes to the configuration before the server starts.
    pub configure: Box<dyn FnOnce(&mut ServerConfig) + Send>,
}

impl Default for TestServerOptions {
    fn default() -> Self {
        Self {
            configure: Box::new(|_| {}),
        }
    }
}

/// A Lode server running in this process.
pub struct TestServer {
    dir: tempfile::TempDir,
    url: String,
    admin_token: String,
    state: AppState,
    clock: ManualClock,
    requests: RequestLog,
    shutdown: Option<oneshot::Sender<()>>,
    config: ServerConfig,
    task: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    background: Background,
}

/// The event dispatcher and module workers of a running test server.
struct Background {
    stop: tokio::sync::watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Background {
    fn start(state: &AppState) -> Self {
        let (stop, signal) = lode_server::module::Stop::new();
        let mut tasks = lode_server::module::spawn_workers(state, &signal);
        let mut wait = signal;
        tasks.push(lode_server::events::Dispatcher::spawn(
            state.clone(),
            async move { wait.wait().await },
        ));
        Self { stop, tasks }
    }

    /// Stops everything at once, as a crash would.
    fn abort(&mut self) {
        for t in self.tasks.drain(..) {
            t.abort();
        }
    }
}

impl std::fmt::Debug for TestServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestServer")
            .field("url", &self.url)
            .field("data_dir", &self.dir.path())
            .finish_non_exhaustive()
    }
}

/// Fails a test with a message; the kit is test-only, so a panic is the right report.
#[track_caller]
#[allow(clippy::panic)]
fn must<T, E: std::fmt::Display>(r: Result<T, E>, what: &str) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("{what}: {e}"),
    }
}

impl TestServer {
    /// Starts a server with the default configuration. Needs a Tokio runtime.
    pub async fn start() -> Self {
        Self::start_with(TestServerOptions::default()).await
    }

    /// Starts a server, letting `opts` adjust its configuration first.
    pub async fn start_with(opts: TestServerOptions) -> Self {
        let dir = must(tempfile::tempdir(), "temp dir");
        let data = dir.path().join("data");
        let admin_token = must(init(&data).await, "loded init").admin_token;
        let mut config = ServerConfig::with_data_dir(&data);
        config.listen = must("127.0.0.1:0".parse(), "listen address");
        (opts.configure)(&mut config);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let clock = ManualClock::new(now);
        let requests = RequestLog::default();
        let (state, url, tx, task) = launch(&config, &clock, &requests).await;
        if let Some(addr) = url.rsplit("//").next().and_then(|a| a.parse().ok()) {
            config.listen = addr;
        }
        let background = Background::start(&state);
        Self {
            background,
            dir,
            url,
            admin_token,
            state,
            clock,
            requests,
            shutdown: Some(tx),
            config,
            task: Some(task),
        }
    }

    /// Stops the server abruptly, as a crash would (no graceful shutdown; in-flight requests
    /// are dropped), then starts it again on the same data directory, port and clock. Armed
    /// failpoints do not survive.
    pub async fn restart(&mut self) {
        self.shutdown.take();
        self.background.abort();
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
        let mut last = None;
        for _ in 0..50 {
            match try_launch(&self.config, &self.clock, &self.requests).await {
                Ok((state, url, tx, task)) => {
                    self.background = Background::start(&state);
                    self.state = state;
                    self.url = url;
                    self.shutdown = Some(tx);
                    self.task = Some(task);
                    return;
                }
                Err(e) => {
                    last = Some(e);
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
        must(Err(last.unwrap_or_default()), "restart the server")
    }

    /// Base URL, such as `http://127.0.0.1:41234`.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The built-in admin's token.
    pub fn admin_token(&self) -> &str {
        &self.admin_token
    }

    /// The server's data directory.
    pub fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    /// Server internals, for assertions.
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// The server's clock; advance it to test expiry and grace periods.
    pub fn clock(&self) -> &ManualClock {
        &self.clock
    }

    /// Failpoints of the commit path.
    pub fn failpoints(&self) -> &Failpoints {
        self.state.failpoints()
    }

    /// Requests received so far.
    pub fn requests(&self) -> &RequestLog {
        &self.requests
    }

    /// A client without a token.
    pub fn anonymous(&self) -> Client {
        must(Client::builder(&self.url).build(), "client")
    }

    /// A client with `token`.
    pub fn client(&self, token: &str) -> Client {
        must(Client::builder(&self.url).token(token).build(), "client")
    }

    /// A client signed in as the admin.
    pub fn admin(&self) -> Client {
        self.client(&self.admin_token)
    }

    /// Creates a depot.
    pub async fn create_depot(&self, name: &str) {
        must(self.admin().create_depot(name).await, "create depot");
    }

    /// Creates a user without a password and returns a token for them with `scopes`
    /// (`read`, `write`, …).
    pub async fn create_user(&self, name: &str, scopes: &[&str]) -> String {
        self.create_principal(PrincipalKind::User, name, None).await;
        self.token_for(name, scopes).await
    }

    /// Creates an agent owned by `owner` and returns a token for it with `scopes`.
    pub async fn create_agent(&self, name: &str, owner: &str, scopes: &[&str]) -> String {
        self.create_principal(PrincipalKind::Agent, name, Some(owner))
            .await;
        self.token_for(name, scopes).await
    }

    async fn create_principal(&self, kind: PrincipalKind, name: &str, owner: Option<&str>) {
        let req = CreatePrincipalRequest {
            kind,
            name: name.to_owned(),
            display_name: None,
            owner: owner.map(str::to_owned),
            password: None,
            sso: None,
        };
        must(
            self.admin().create_principal(&req).await,
            "create principal",
        );
    }

    /// A new token for an existing principal.
    pub async fn token_for(&self, name: &str, scopes: &[&str]) -> String {
        let req = CreateTokenRequest {
            name: "testkit".into(),
            scopes: scopes.iter().map(|s| (*s).to_owned()).collect(),
            expires_at: None,
        };
        must(
            self.admin().create_token_for(name, &req).await,
            "create token",
        )
        .token
    }

    /// Adds an access rule: `effect` `level` for `user` on `branch` glob and `path` glob in
    /// `depot`. Returns the rule id.
    pub async fn acl(
        &self,
        depot: &str,
        user: &str,
        branch: &str,
        path: &str,
        effect: AclEffect,
        level: AccessLevel,
    ) -> i64 {
        let rule = AclRule {
            id: None,
            depot: Some(depot.to_owned()),
            subject: AclSubject::User(user.to_owned()),
            branch: branch.to_owned(),
            path: path.to_owned(),
            effect,
            level,
        };
        must(self.admin().add_acl_rule(&rule).await, "add rule")
            .id
            .unwrap_or_default()
    }

    /// Commits `files` (path, content) as additions or modifications to `branch` with the
    /// admin token, uploading what is missing. Returns the revision.
    pub async fn commit_files(&self, depot: &str, branch: &str, files: &[(&str, &[u8])]) -> u64 {
        let c = self.admin();
        let head = must(c.branch(depot, branch).await, "branch").head_rev;
        let mut changes = Vec::new();
        for (path, content) in files {
            let id = must(blob_id(*content), "hash");
            must(
                c.put_blob(depot, &id, std::io::Cursor::new(content.to_vec()))
                    .await,
                "upload",
            );
            let path = must(DepotPath::parse(path), "path");
            let existing = if head > 0 {
                c.file(depot, head, &path).await.ok().map(|f| f.id)
            } else {
                None
            };
            changes.push(Change {
                path,
                action: if existing.is_some() {
                    ChangeAction::Modify
                } else {
                    ChangeAction::Add
                },
                base_id: existing,
                new_id: Some(id),
                kind: Some(EntryKind::File),
                executable: false,
                size: Some(content.len() as u64),
                copy_from: None,
            });
        }
        let req = CommitRequest {
            request_id: format!("testkit-{}", unique()),
            message: "seed".into(),
            changes,
            merged_from: vec![],
            provenance: None,
            extensions: Default::default(),
        };
        must(c.commit(depot, branch, &req).await, "commit").rev
    }
}

/// The built `lode` binary. Cargo names it in `CARGO_BIN_EXE_lode` only for tests of
/// `lode-cli`; other tests look for it next to (or above) their own executable, in the
/// target profile directory, wherever this cargo puts test binaries below it.
fn lode_binary() -> std::io::Result<PathBuf> {
    if let Some(p) = std::env::var_os("CARGO_BIN_EXE_lode").filter(|p| !p.is_empty()) {
        return PathBuf::from(p).canonicalize();
    }
    let name = format!("lode{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe()?;
    exe.ancestors()
        .skip(1)
        .map(|dir| dir.join(&name))
        .find(|p| p.is_file())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no {name} above {}", exe.display()),
            )
        })?
        .canonicalize()
}

type Launched = (
    AppState,
    String,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<std::io::Result<()>>,
);

async fn try_launch(
    config: &ServerConfig,
    clock: &ManualClock,
    requests: &RequestLog,
) -> Result<Launched, String> {
    let modules = lode_server::module::select_enabled(&config.modules, available_modules());
    let state = open_state(config.clone(), Arc::new(clock.clone()), modules)
        .await
        .map_err(|e| format!("open server state: {e}"))?;
    let log = requests.clone();
    let app = router(state.clone()).layer(middleware::from_fn(move |req, next| {
        record(log.clone(), req, next)
    }));
    let bound = BoundServer::bind(config, app)
        .await
        .map_err(|e| format!("bind: {e}"))?;
    state.set_listening(bound.local_addr());
    let url = format!("{}://{}", bound.scheme(), bound.local_addr());
    let (tx, rx) = oneshot::channel::<()>();
    let task = tokio::spawn(bound.run(async {
        let _ = rx.await;
    }));
    Ok((state, url, tx, task))
}

async fn launch(config: &ServerConfig, clock: &ManualClock, requests: &RequestLog) -> Launched {
    must(
        try_launch(config, clock, requests).await,
        "start the server",
    )
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.background.stop.send(true);
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

fn unique() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// A directory to run the `lode` binary in, with its own home and config directories.
#[derive(Debug)]
pub struct TestWorkspace {
    home: tempfile::TempDir,
    root: PathBuf,
    server_url: Option<String>,
    token_file: Option<PathBuf>,
    bin: PathBuf,
}

impl TestWorkspace {
    /// A workspace directory for `server` (sets `LODE_SERVER`). The `lode` binary is the one
    /// Cargo built for this workspace.
    pub fn new(server: &TestServer) -> Self {
        let mut ws = Self::detached();
        ws.server_url = Some(server.url().to_owned());
        ws
    }

    /// A workspace directory with no server configured.
    pub fn detached() -> Self {
        let home = must(tempfile::tempdir(), "temp dir");
        let root = home.path().join("ws");
        must(std::fs::create_dir_all(&root), "workspace dir");
        Self {
            home,
            root,
            server_url: None,
            token_file: None,
            bin: must(
                lode_binary(),
                "find the lode binary (build it with `cargo build -p lode-cli`)",
            ),
        }
    }

    /// Uses another `lode` binary.
    pub fn with_binary(mut self, bin: impl Into<PathBuf>) -> Self {
        self.bin = bin.into();
        self
    }

    /// Signs commands in with `token` through `LODE_TOKEN_FILE`.
    pub fn with_token(mut self, token: &str) -> Self {
        let file = self.home.path().join("token");
        must(
            std::fs::write(&file, format!("{token}\n")),
            "write token file",
        );
        self.token_file = Some(file);
        self
    }

    /// The directory commands run in.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The isolated home directory.
    pub fn home(&self) -> &Path {
        self.home.path()
    }

    /// The isolated config directory (`LODE_CONFIG_DIR`).
    pub fn config_dir(&self) -> PathBuf {
        self.home.path().join("config")
    }

    /// A `lode` command with the isolated environment, run in [`TestWorkspace::root`].
    pub fn cmd(&self) -> assert_cmd::Command {
        assert_cmd::Command::from_std(self.std_cmd())
    }

    /// [`TestWorkspace::cmd`] as a plain process command, for running `lode` as a child
    /// with pipes (such as `lode mcp`).
    pub fn std_cmd(&self) -> std::process::Command {
        let mut c = std::process::Command::new(&self.bin);
        let home = self.home.path();
        c.current_dir(&self.root)
            .env_remove("LODE_SERVER")
            .env_remove("LODE_TOKEN_FILE")
            .env_remove("LODE_CACHE_DIR")
            .env_remove("LODE_PROXY")
            .env("LODE_CONFIG_DIR", self.config_dir())
            .env("HOME", home)
            .env("USERPROFILE", home)
            .env("XDG_CONFIG_HOME", home.join("xdg-config"))
            .env("XDG_CACHE_HOME", home.join("xdg-cache"))
            .env("APPDATA", home.join("appdata"))
            .env("LOCALAPPDATA", home.join("localappdata"))
            .env("NO_COLOR", "1");
        if let Some(url) = &self.server_url {
            c.env("LODE_SERVER", url);
        }
        if let Some(t) = &self.token_file {
            c.env("LODE_TOKEN_FILE", t);
        }
        c
    }

    /// Runs `lode args…` and returns the assertion handle.
    pub fn lode(&self, args: &[&str]) -> assert_cmd::assert::Assert {
        self.cmd().args(args).assert()
    }

    /// Runs `lode --json args…`, which must succeed, and parses stdout as one JSON document.
    pub fn json(&self, args: &[&str]) -> serde_json::Value {
        let (code, v) = self.json_status(args);
        assert_eq!(code, 0, "lode {args:?} failed: {v}");
        v
    }

    /// Runs `lode --json args…` and returns the exit code and stdout parsed as one JSON
    /// document (failing the test if it is not exactly one).
    #[allow(clippy::panic)]
    pub fn json_status(&self, args: &[&str]) -> (i32, serde_json::Value) {
        let out = must(self.cmd().arg("--json").args(args).output(), "run lode");
        let text = String::from_utf8_lossy(&out.stdout);
        let v: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => panic!(
                "lode {args:?}: stdout is not one JSON document ({e}):\n{text}\nstderr:\n{}",
                String::from_utf8_lossy(&out.stderr)
            ),
        };
        (out.status.code().unwrap_or(-1), v)
    }

    /// Writes a file under the root, creating directories.
    pub fn write(&self, path: &str, content: impl AsRef<[u8]>) {
        let p = self.root.join(path);
        if let Some(parent) = p.parent() {
            must(std::fs::create_dir_all(parent), "create dirs");
        }
        must(std::fs::write(&p, content), "write file");
    }

    /// Reads a file under the root.
    pub fn read(&self, path: &str) -> Vec<u8> {
        must(std::fs::read(self.root.join(path)), "read file")
    }

    /// Whether a file exists under the root.
    pub fn exists(&self, path: &str) -> bool {
        self.root.join(path).exists()
    }

    /// Removes a file under the root.
    pub fn remove(&self, path: &str) {
        must(std::fs::remove_file(self.root.join(path)), "remove file");
    }
}
