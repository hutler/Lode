//! Typed async client for the Lode HTTP API (ARCHITECTURE §6).
//!
//! One method per core endpoint, using the `lode-proto` types. Server rejections arrive as
//! [`Error::Api`] with the stable `code`; everything else (connection, TLS, a broken body) is
//! a different variant, so callers can tell "the server said no" from "the server was not
//! reached".
//!
//! Requests that are safe to repeat (reads, `PUT`, `DELETE`, the missing-objects query and
//! commits, which carry a `request_id`) are retried with backoff on transport failures and on
//! 502/503/504. A 4xx is never retried.

#![forbid(unsafe_code)]

mod blobs;
mod credentials;
mod error;
mod io;
mod manifest;
mod retry;

use std::sync::Arc;
use std::time::Duration;

use lode_core::{DepotPath, ObjectId};
use lode_proto::admin::{
    AclList, AclRule, CreateGroupRequest, CreatePrincipalRequest, GroupList, GroupView,
    MaintenanceReport, PatchPrincipalRequest, PrincipalList,
};
use lode_proto::auth::{
    CreateTokenRequest, CreateTokenResponse, Info, LoginRequest, LoginResponse, PrincipalView,
    TokenList, WhoAmI,
};
use lode_proto::commit::{CommitRequest, CommitResponse};
use lode_proto::consts::{HEADER_ACCESS_EPOCH, MAX_MISSING_QUERY};
use lode_proto::depot::{
    BranchList, BranchView, CreateBranchRequest, CreateDepotRequest, DepotList, DepotView,
    MissingObjectsRequest, MissingObjectsResponse, PatchBranchRequest,
};
use lode_proto::locks::{LockList, LockRequest, LocksQuery, UnlockRequest, UnlockResponse};
use lode_proto::read::{
    ManifestDelta, ManifestEntry, RevisionList, RevisionListQuery, RevisionView, TreeListing,
};
use lode_proto::shelves::{
    CreateShelfRequest, ShelfList, ShelfState, ShelfView, ShelvesQuery, SubmitShelfRequest,
    UpdateShelfRequest,
};
use reqwest::header::RETRY_AFTER;
use reqwest::{Method, RequestBuilder, Response, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::Serialize;

pub use blobs::FileContent;
pub use credentials::{read_token_file, CredentialStore};
pub use error::{ApiError, Error, Result};
pub use manifest::ManifestStream;
pub use reqwest::Url as ServerUrl;
pub use retry::RetryPolicy;

/// `User-Agent` of every request: `lode-client/<version>`, after the product a caller names.
pub const USER_AGENT: &str = concat!("lode-client/", env!("CARGO_PKG_VERSION"));

/// zstd level used for uploads unless configured otherwise.
pub const DEFAULT_UPLOAD_LEVEL: i32 = 3;

/// Configures a [`Client`].
#[derive(Debug, Clone)]
pub struct ClientBuilder {
    url: String,
    token: Option<String>,
    allow_insecure: bool,
    retry: RetryPolicy,
    product: Option<String>,
    upload_level: Option<i32>,
    root_certs_pem: Vec<Vec<u8>>,
    connect_timeout: Duration,
}

impl ClientBuilder {
    /// A builder for the server at `url`, such as `https://lode.example.com` (a path prefix
    /// is kept, for servers behind a reverse proxy).
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            token: None,
            allow_insecure: false,
            retry: RetryPolicy::default(),
            product: None,
            upload_level: Some(DEFAULT_UPLOAD_LEVEL),
            root_certs_pem: Vec::new(),
            connect_timeout: Duration::from_secs(15),
        }
    }

    /// The bearer token to send.
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Allows `http://` to hosts other than this machine (`--insecure`).
    pub fn allow_insecure_http(mut self, allow: bool) -> Self {
        self.allow_insecure = allow;
        self
    }

    /// Retry schedule.
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    /// A product put before `lode-client/<v>` in `User-Agent`, such as `lode/0.1.0`. The
    /// server records it as `provenance.client`.
    pub fn product(mut self, product: impl Into<String>) -> Self {
        self.product = Some(product.into());
        self
    }

    /// zstd level for uploads; `None` sends content uncompressed.
    pub fn upload_compression(mut self, level: Option<i32>) -> Self {
        self.upload_level = level;
        self
    }

    /// Trusts a CA certificate (PEM) in addition to the system roots, for servers with a
    /// private certificate authority.
    pub fn add_root_certificate_pem(mut self, pem: impl Into<Vec<u8>>) -> Self {
        self.root_certs_pem.push(pem.into());
        self
    }

    /// Time allowed to establish a connection.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Checks the URL and builds the client.
    pub fn build(self) -> Result<Client> {
        let base = parse_server_url(&self.url, self.allow_insecure)?;
        let user_agent = match &self.product {
            Some(p) => format!("{p} {USER_AGENT}"),
            None => USER_AGENT.to_owned(),
        };
        let mut http = reqwest::Client::builder()
            .user_agent(user_agent)
            .connect_timeout(self.connect_timeout)
            // Redirects could carry the token elsewhere or downgrade to http; the API never
            // redirects.
            .redirect(reqwest::redirect::Policy::none());
        if !self.allow_insecure {
            // Plain http is only allowed for loopback, which `parse_server_url` checked.
            if base.scheme() == "https" {
                http = http.https_only(true);
            }
        }
        for pem in &self.root_certs_pem {
            let cert = reqwest::Certificate::from_pem(pem).map_err(|e| Error::InvalidUrl {
                url: self.url.clone(),
                reason: format!("bad CA certificate: {e}"),
            })?;
            http = http.add_root_certificate(cert);
        }
        let http = http.build().map_err(Error::Transport)?;
        Ok(Client {
            http,
            base,
            token: self.token.map(Arc::from),
            retry: self.retry,
            upload_level: self.upload_level,
        })
    }
}

/// Parses and checks a server URL: `http` or `https`, and `http` only to loopback unless
/// `allow_insecure`. Query and fragment are dropped.
pub fn parse_server_url(url: &str, allow_insecure: bool) -> Result<Url> {
    let bad = |reason: &str| Error::InvalidUrl {
        url: url.to_owned(),
        reason: reason.to_owned(),
    };
    let mut parsed = Url::parse(url.trim()).map_err(|e| bad(&e.to_string()))?;
    match parsed.scheme() {
        "https" => {}
        "http" => {
            if !allow_insecure && !is_loopback(&parsed) {
                return Err(Error::InsecureUrl(
                    parsed.host_str().unwrap_or_default().to_owned(),
                ));
            }
        }
        _ => return Err(bad("the scheme must be http or https")),
    }
    if parsed.host().is_none() || parsed.cannot_be_a_base() {
        return Err(bad("no host"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(bad("credentials do not belong in the URL"));
    }
    parsed.set_query(None);
    parsed.set_fragment(None);
    Ok(parsed)
}

fn is_loopback(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let ip = host.trim_start_matches('[').trim_end_matches(']');
    match ip.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => host.eq_ignore_ascii_case("localhost"),
    }
}

/// Whether a request may be sent again after a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Retry {
    No,
    Yes,
}

/// A connection to one Lode server. Cheap to clone; clones share the connection pool.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    token: Option<Arc<str>>,
    retry: RetryPolicy,
    upload_level: Option<i32>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base.as_str())
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Shorthand for [`ClientBuilder::new`].
    pub fn builder(url: impl Into<String>) -> ClientBuilder {
        ClientBuilder::new(url)
    }

    /// The server URL.
    pub fn server_url(&self) -> &Url {
        &self.base
    }

    /// The same client with another token, such as one just obtained from [`Client::login`].
    pub fn with_token(&self, token: impl Into<String>) -> Self {
        Self {
            token: Some(Arc::from(token.into())),
            ..self.clone()
        }
    }

    // ---- plumbing ----

    /// `base` + `/api/v1/` + `segments`, each percent-encoded as one segment (so a branch
    /// name's `/` becomes `%2F`).
    pub(crate) fn url(&self, segments: &[&str]) -> Result<Url> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|()| Error::Protocol("server URL cannot take a path".into()))?
            .pop_if_empty()
            .extend(["api", "v1"])
            .extend(segments);
        Ok(url)
    }

    fn request(&self, method: Method, url: Url) -> RequestBuilder {
        let b = self.http.request(method, url);
        match &self.token {
            Some(t) => b.bearer_auth(t),
            None => b,
        }
    }

    /// Sends the request `make` builds, retrying per policy, and returns a successful
    /// response or the decoded error.
    pub(crate) async fn send(
        &self,
        retry: Retry,
        mut make: impl FnMut() -> Result<RequestBuilder>,
    ) -> Result<Response> {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let may_retry = retry == Retry::Yes && attempt < self.retry.max_attempts;
            match make()?.send().await {
                Err(e) => {
                    if may_retry && error::transient_transport(&e) {
                        tracing::debug!(attempt, error = %e, "request failed; retrying");
                        tokio::time::sleep(self.retry.delay(attempt, None)).await;
                        continue;
                    }
                    return Err(Error::Transport(e));
                }
                Ok(resp) if resp.status().is_success() => return Ok(resp),
                Ok(resp) => {
                    let status = resp.status();
                    if may_retry && error::retryable_status(status) {
                        let after = retry_after(&resp);
                        drop(resp);
                        tracing::debug!(attempt, %status, "server unavailable; retrying");
                        tokio::time::sleep(self.retry.delay(attempt, after)).await;
                        continue;
                    }
                    return Err(api_error(resp).await);
                }
            }
        }
    }

    async fn json<T: DeserializeOwned>(&self, retry: Retry, method: Method, url: Url) -> Result<T> {
        let resp = self
            .send(retry, || Ok(self.request(method.clone(), url.clone())))
            .await?;
        decode(resp).await
    }

    async fn json_body<B: Serialize, T: DeserializeOwned>(
        &self,
        retry: Retry,
        method: Method,
        url: Url,
        body: &B,
    ) -> Result<T> {
        let resp = self
            .send(retry, || {
                Ok(self.request(method.clone(), url.clone()).json(body))
            })
            .await?;
        decode(resp).await
    }

    async fn no_content(&self, method: Method, url: Url) -> Result<()> {
        self.send(Retry::Yes, || Ok(self.request(method.clone(), url.clone())))
            .await?;
        Ok(())
    }

    // ---- server and sign-in ----

    /// `GET /info`: versions and enabled modules. Needs no token.
    pub async fn info(&self) -> Result<Info> {
        self.json(Retry::Yes, Method::GET, self.url(&["info"])?)
            .await
    }

    /// `POST /auth/login`: exchanges a password for a new token. Not retried, since each
    /// success creates a token.
    pub async fn login(&self, req: &LoginRequest) -> Result<LoginResponse> {
        self.json_body(Retry::No, Method::POST, self.url(&["auth", "login"])?, req)
            .await
    }

    /// `GET /auth/whoami`.
    pub async fn whoami(&self) -> Result<WhoAmI> {
        self.json(Retry::Yes, Method::GET, self.url(&["auth", "whoami"])?)
            .await
    }

    /// `GET /tokens`: the caller's own tokens.
    pub async fn tokens(&self) -> Result<TokenList> {
        self.json(Retry::Yes, Method::GET, self.url(&["tokens"])?)
            .await
    }

    /// `POST /tokens`.
    pub async fn create_token(&self, req: &CreateTokenRequest) -> Result<CreateTokenResponse> {
        self.json_body(Retry::No, Method::POST, self.url(&["tokens"])?, req)
            .await
    }

    /// `DELETE /tokens/{id}`.
    pub async fn delete_token(&self, id: i64) -> Result<()> {
        self.no_content(Method::DELETE, self.url(&["tokens", &id.to_string()])?)
            .await
    }

    // ---- depots and branches ----

    /// `GET /depots`.
    pub async fn depots(&self) -> Result<DepotList> {
        self.json(Retry::Yes, Method::GET, self.url(&["depots"])?)
            .await
    }

    /// `POST /depots`.
    pub async fn create_depot(&self, name: &str) -> Result<DepotView> {
        let req = CreateDepotRequest {
            name: name.to_owned(),
        };
        self.json_body(Retry::No, Method::POST, self.url(&["depots"])?, &req)
            .await
    }

    /// `GET /depots/{d}/branches`; `all` includes archived branches.
    pub async fn branches(&self, depot: &str, all: bool) -> Result<BranchList> {
        let mut url = self.url(&["depots", depot, "branches"])?;
        if all {
            url.query_pairs_mut().append_pair("all", "true");
        }
        self.json(Retry::Yes, Method::GET, url).await
    }

    /// `GET /depots/{d}/branches/{b}`.
    pub async fn branch(&self, depot: &str, branch: &str) -> Result<BranchView> {
        self.json(
            Retry::Yes,
            Method::GET,
            self.url(&["depots", depot, "branches", branch])?,
        )
        .await
    }

    /// `POST /depots/{d}/branches`.
    pub async fn create_branch(
        &self,
        depot: &str,
        req: &CreateBranchRequest,
    ) -> Result<BranchView> {
        self.json_body(
            Retry::No,
            Method::POST,
            self.url(&["depots", depot, "branches"])?,
            req,
        )
        .await
    }

    /// `PATCH /depots/{d}/branches/{b}`, such as archiving it.
    pub async fn patch_branch(
        &self,
        depot: &str,
        branch: &str,
        req: &PatchBranchRequest,
    ) -> Result<BranchView> {
        self.json_body(
            Retry::Yes,
            Method::PATCH,
            self.url(&["depots", depot, "branches", branch])?,
            req,
        )
        .await
    }

    // ---- content ----

    /// `POST /depots/{d}/objects/missing`: which of `ids` the server lacks. Any number of
    /// ids; they are sent in batches of the server's limit.
    pub async fn missing_objects(&self, depot: &str, ids: &[ObjectId]) -> Result<Vec<ObjectId>> {
        let url = self.url(&["depots", depot, "objects", "missing"])?;
        let mut missing = Vec::new();
        for chunk in ids.chunks(MAX_MISSING_QUERY) {
            let req = MissingObjectsRequest {
                ids: chunk.to_vec(),
            };
            let resp: MissingObjectsResponse = self
                .json_body(Retry::Yes, Method::POST, url.clone(), &req)
                .await?;
            missing.extend(resp.missing);
        }
        Ok(missing)
    }

    /// `POST /depots/{d}/branches/{b}/commits`. Retried like a read: the `request_id` makes
    /// a repeat return the revision the first attempt created (`replayed`).
    pub async fn commit(
        &self,
        depot: &str,
        branch: &str,
        req: &CommitRequest,
    ) -> Result<CommitResponse> {
        self.json_body(
            Retry::Yes,
            Method::POST,
            self.url(&["depots", depot, "branches", branch, "commits"])?,
            req,
        )
        .await
    }

    // ---- locks module ----

    /// `POST /depots/{d}/locks`: takes every lock or none. Taking a lock the same workspace
    /// already holds changes nothing, so a retry is safe.
    pub async fn lock(&self, depot: &str, req: &LockRequest) -> Result<LockList> {
        self.json_body(
            Retry::Yes,
            Method::POST,
            self.url(&["depots", depot, "locks"])?,
            req,
        )
        .await
    }

    /// `DELETE /depots/{d}/locks`.
    pub async fn unlock(&self, depot: &str, req: &UnlockRequest) -> Result<UnlockResponse> {
        self.json_body(
            Retry::Yes,
            Method::DELETE,
            self.url(&["depots", depot, "locks"])?,
            req,
        )
        .await
    }

    /// `GET /depots/{d}/locks`.
    pub async fn locks(&self, depot: &str, q: &LocksQuery) -> Result<LockList> {
        let mut url = self.url(&["depots", depot, "locks"])?;
        {
            let mut p = url.query_pairs_mut();
            if let Some(b) = &q.branch {
                p.append_pair("branch", b);
            }
            if let Some(x) = &q.prefix {
                p.append_pair("prefix", x);
            }
            if let Some(o) = &q.owner {
                p.append_pair("owner", o);
            }
        }
        self.json(Retry::Yes, Method::GET, url).await
    }

    // ---- shelves module ----

    /// `GET /depots/{d}/shelves`: open shelves unless `q.state` says otherwise.
    pub async fn shelves(&self, depot: &str, q: &ShelvesQuery) -> Result<ShelfList> {
        let mut url = self.url(&["depots", depot, "shelves"])?;
        {
            let mut p = url.query_pairs_mut();
            if let Some(o) = &q.owner {
                p.append_pair("owner", o);
            }
            if let Some(b) = &q.branch {
                p.append_pair("branch", b);
            }
            if let Some(s) = q.state {
                p.append_pair(
                    "state",
                    match s {
                        ShelfState::Open => "open",
                        ShelfState::Submitted => "submitted",
                        ShelfState::Deleted => "deleted",
                    },
                );
            }
        }
        self.json(Retry::Yes, Method::GET, url).await
    }

    /// `GET /depots/{d}/shelves/{id}`, with a diff per file when `diff` is set.
    pub async fn shelf(&self, depot: &str, id: u64, diff: bool) -> Result<ShelfView> {
        let mut url = self.url(&["depots", depot, "shelves", &id.to_string()])?;
        if diff {
            url.query_pairs_mut().append_pair("diff", "true");
        }
        self.json(Retry::Yes, Method::GET, url).await
    }

    /// `POST /depots/{d}/shelves`. Not retried: a repeat would make a second shelf.
    pub async fn create_shelf(&self, depot: &str, req: &CreateShelfRequest) -> Result<ShelfView> {
        self.json_body(
            Retry::No,
            Method::POST,
            self.url(&["depots", depot, "shelves"])?,
            req,
        )
        .await
    }

    /// `PUT /depots/{d}/shelves/{id}`: replaces the shelf's content and raises its version.
    pub async fn update_shelf(
        &self,
        depot: &str,
        id: u64,
        req: &UpdateShelfRequest,
    ) -> Result<ShelfView> {
        self.json_body(
            Retry::No,
            Method::PUT,
            self.url(&["depots", depot, "shelves", &id.to_string()])?,
            req,
        )
        .await
    }

    /// `DELETE /depots/{d}/shelves/{id}`.
    pub async fn delete_shelf(&self, depot: &str, id: u64) -> Result<ShelfView> {
        self.json(
            Retry::Yes,
            Method::DELETE,
            self.url(&["depots", depot, "shelves", &id.to_string()])?,
        )
        .await
    }

    /// `POST /depots/{d}/shelves/{id}/submit`. Retried: a submitted shelf answers with its
    /// revision.
    pub async fn submit_shelf(
        &self,
        depot: &str,
        id: u64,
        req: &SubmitShelfRequest,
    ) -> Result<CommitResponse> {
        self.json_body(
            Retry::Yes,
            Method::POST,
            self.url(&["depots", depot, "shelves", &id.to_string(), "submit"])?,
            req,
        )
        .await
    }

    // ---- history and trees ----

    /// `GET /depots/{d}/revisions`.
    pub async fn revisions(&self, depot: &str, q: &RevisionListQuery) -> Result<RevisionList> {
        let mut url = self.url(&["depots", depot, "revisions"])?;
        {
            let mut p = url.query_pairs_mut();
            if let Some(b) = &q.branch {
                p.append_pair("branch", b);
            }
            if let Some(path) = &q.path {
                p.append_pair("path", path.as_str());
            }
            if let Some(n) = q.before {
                p.append_pair("before", &n.to_string());
            }
            if let Some(n) = q.after {
                p.append_pair("after", &n.to_string());
            }
            if let Some(a) = &q.author {
                p.append_pair("author", a);
            }
            if let Some(k) = q.author_kind {
                p.append_pair(
                    "author_kind",
                    match k {
                        lode_proto::PrincipalKind::User => "user",
                        lode_proto::PrincipalKind::Agent => "agent",
                    },
                );
            }
            for (key, on) in [("own", q.own), ("follow", q.follow), ("changes", q.changes)] {
                if on {
                    p.append_pair(key, "true");
                }
            }
            if let Some(n) = q.limit {
                p.append_pair("limit", &n.to_string());
            }
        }
        self.json(Retry::Yes, Method::GET, url).await
    }

    /// `GET /depots/{d}/revisions/{n}`.
    pub async fn revision(&self, depot: &str, rev: u64) -> Result<RevisionView> {
        self.json(
            Retry::Yes,
            Method::GET,
            self.url(&["depots", depot, "revisions", &rev.to_string()])?,
        )
        .await
    }

    /// `GET /depots/{d}/manifest?rev=N`: every visible file at `rev`, filtered by the view.
    pub async fn manifest(
        &self,
        depot: &str,
        rev: u64,
        include: &[String],
        exclude: &[String],
    ) -> Result<ManifestStream<ManifestEntry>> {
        self.manifest_request(depot, rev, None, include, exclude)
            .await
    }

    /// `GET /depots/{d}/manifest?rev=N&base=M`: the differences from `base` to `rev`.
    pub async fn manifest_delta(
        &self,
        depot: &str,
        base: u64,
        rev: u64,
        include: &[String],
        exclude: &[String],
    ) -> Result<ManifestStream<ManifestDelta>> {
        self.manifest_request(depot, rev, Some(base), include, exclude)
            .await
    }

    async fn manifest_request<T: DeserializeOwned + Send + 'static>(
        &self,
        depot: &str,
        rev: u64,
        base: Option<u64>,
        include: &[String],
        exclude: &[String],
    ) -> Result<ManifestStream<T>> {
        let mut url = self.url(&["depots", depot, "manifest"])?;
        {
            let mut p = url.query_pairs_mut();
            p.append_pair("rev", &rev.to_string());
            if let Some(b) = base {
                p.append_pair("base", &b.to_string());
            }
            for g in include {
                p.append_pair("include", g);
            }
            for g in exclude {
                p.append_pair("exclude", g);
            }
        }
        let resp = self
            .send(Retry::Yes, || Ok(self.request(Method::GET, url.clone())))
            .await?;
        let epoch = resp
            .headers()
            .get(HEADER_ACCESS_EPOCH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok());
        Ok(ManifestStream::new(resp, epoch))
    }

    /// `GET /depots/{d}/tree`: one directory at `rev`; `None` is the root.
    pub async fn tree(
        &self,
        depot: &str,
        rev: u64,
        path: Option<&DepotPath>,
    ) -> Result<TreeListing> {
        let mut url = self.url(&["depots", depot, "tree"])?;
        {
            let mut p = url.query_pairs_mut();
            p.append_pair("rev", &rev.to_string());
            if let Some(path) = path {
                p.append_pair("path", path.as_str());
            }
        }
        self.json(Retry::Yes, Method::GET, url).await
    }

    // ---- administration ----

    /// `GET /admin/principals`.
    pub async fn principals(&self) -> Result<PrincipalList> {
        self.json(Retry::Yes, Method::GET, self.url(&["admin", "principals"])?)
            .await
    }

    /// `POST /admin/principals`.
    pub async fn create_principal(&self, req: &CreatePrincipalRequest) -> Result<PrincipalView> {
        self.json_body(
            Retry::No,
            Method::POST,
            self.url(&["admin", "principals"])?,
            req,
        )
        .await
    }

    /// `PATCH /admin/principals/{name}`.
    pub async fn patch_principal(
        &self,
        name: &str,
        req: &PatchPrincipalRequest,
    ) -> Result<PrincipalView> {
        self.json_body(
            Retry::Yes,
            Method::PATCH,
            self.url(&["admin", "principals", name])?,
            req,
        )
        .await
    }

    /// `POST /admin/principals/{name}/tokens`: a token for another principal, such as an
    /// agent.
    pub async fn create_token_for(
        &self,
        name: &str,
        req: &CreateTokenRequest,
    ) -> Result<CreateTokenResponse> {
        self.json_body(
            Retry::No,
            Method::POST,
            self.url(&["admin", "principals", name, "tokens"])?,
            req,
        )
        .await
    }

    /// `GET /admin/groups`.
    pub async fn groups(&self) -> Result<GroupList> {
        self.json(Retry::Yes, Method::GET, self.url(&["admin", "groups"])?)
            .await
    }

    /// `POST /admin/groups`.
    pub async fn create_group(&self, name: &str) -> Result<GroupView> {
        let req = CreateGroupRequest {
            name: name.to_owned(),
        };
        self.json_body(
            Retry::No,
            Method::POST,
            self.url(&["admin", "groups"])?,
            &req,
        )
        .await
    }

    /// `PUT /admin/groups/{g}/members/{name}`.
    pub async fn add_group_member(&self, group: &str, name: &str) -> Result<()> {
        self.no_content(
            Method::PUT,
            self.url(&["admin", "groups", group, "members", name])?,
        )
        .await
    }

    /// `DELETE /admin/groups/{g}/members/{name}`.
    pub async fn remove_group_member(&self, group: &str, name: &str) -> Result<()> {
        self.no_content(
            Method::DELETE,
            self.url(&["admin", "groups", group, "members", name])?,
        )
        .await
    }

    /// `GET /admin/acl`.
    pub async fn acl(&self) -> Result<AclList> {
        self.json(Retry::Yes, Method::GET, self.url(&["admin", "acl"])?)
            .await
    }

    /// `POST /admin/acl`: adds a rule and returns it with its id.
    pub async fn add_acl_rule(&self, rule: &AclRule) -> Result<AclRule> {
        self.json_body(Retry::No, Method::POST, self.url(&["admin", "acl"])?, rule)
            .await
    }

    /// `DELETE /admin/acl/{id}`.
    pub async fn delete_acl_rule(&self, id: i64) -> Result<()> {
        self.no_content(
            Method::DELETE,
            self.url(&["admin", "acl", &id.to_string()])?,
        )
        .await
    }

    /// `POST /admin/depots/{d}/gc`.
    pub async fn gc(&self, depot: &str, dry_run: bool) -> Result<MaintenanceReport> {
        let mut url = self.url(&["admin", "depots", depot, "gc"])?;
        if dry_run {
            url.query_pairs_mut().append_pair("dry_run", "true");
        }
        self.json(Retry::No, Method::POST, url).await
    }

    /// `POST /admin/depots/{d}/fsck`.
    pub async fn fsck(&self, depot: &str, full: bool) -> Result<MaintenanceReport> {
        let mut url = self.url(&["admin", "depots", depot, "fsck"])?;
        if full {
            url.query_pairs_mut().append_pair("full", "true");
        }
        self.json(Retry::Yes, Method::POST, url).await
    }
}

fn retry_after(resp: &Response) -> Option<Duration> {
    resp.headers()
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Most bytes of an error body read; error bodies are small.
const MAX_ERROR_BODY: usize = 64 * 1024;

async fn api_error(resp: Response) -> Error {
    let status = resp.status();
    let mut body = Vec::new();
    let mut stream = resp;
    loop {
        match stream.chunk().await {
            Ok(Some(c)) => {
                body.extend_from_slice(&c);
                if body.len() > MAX_ERROR_BODY {
                    break;
                }
            }
            Ok(None) => break,
            // The status already says what happened; a broken error body adds nothing.
            Err(_) => break,
        }
    }
    match serde_json::from_slice::<lode_proto::error::ErrorBody>(&body) {
        Ok(b) => Error::Api(ApiError {
            status: status.as_u16(),
            code: b.code,
            message: b.message,
            details: b.details,
        }),
        Err(_) => Error::Api(ApiError::from_status(status, &body)),
    }
}

async fn decode<T: DeserializeOwned>(resp: Response) -> Result<T> {
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(Error::Transport)?;
    // A 204 for an endpoint that returns JSON would be a server bug; say so plainly.
    if status == StatusCode::NO_CONTENT {
        return Err(Error::Protocol("expected a JSON body, got none".into()));
    }
    serde_json::from_slice(&bytes).map_err(|e| Error::Protocol(format!("bad JSON response: {e}")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn plain_http_only_to_loopback() {
        for ok in [
            "http://localhost:8080",
            "http://LOCALHOST",
            "http://127.0.0.1:1",
            "http://127.3.4.5",
            "http://[::1]:9000",
            "https://lode.example.com",
        ] {
            assert!(parse_server_url(ok, false).is_ok(), "{ok}");
        }
        for bad in ["http://lode.example.com", "http://10.0.0.5", "http://[::2]"] {
            assert!(
                matches!(parse_server_url(bad, false), Err(Error::InsecureUrl(_))),
                "{bad}"
            );
            assert!(parse_server_url(bad, true).is_ok(), "{bad}");
        }
        for bad in ["ftp://h", "lode.example.com", "https://u:p@h", "file:///x"] {
            assert!(
                matches!(parse_server_url(bad, true), Err(Error::InvalidUrl { .. })),
                "{bad}"
            );
        }
    }

    #[test]
    fn urls_keep_a_prefix_and_encode_segments() {
        let c = Client::builder("https://h/lode/?x=1").build().unwrap();
        assert_eq!(
            c.url(&["depots", "game", "branches", "release/1.0", "commits"])
                .unwrap()
                .as_str(),
            "https://h/lode/api/v1/depots/game/branches/release%2F1.0/commits"
        );
        let c = Client::builder("https://h").build().unwrap();
        assert_eq!(c.url(&["info"]).unwrap().as_str(), "https://h/api/v1/info");
    }

    #[test]
    fn debug_hides_the_token() {
        let c = Client::builder("https://h")
            .token("s3cret")
            .build()
            .unwrap();
        assert!(!format!("{c:?}").contains("s3cret"));
    }
}
