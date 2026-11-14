//! Single sign-on through an OpenID Connect provider (T122).
//!
//! The server is the OIDC client (authorization code flow with PKCE). A Lode client starts a
//! sign-in by opening `GET /auth/sso/start` in a browser with a loopback address of its own,
//! a state and a PKCE challenge. The server sends the browser to the provider with its own
//! state, nonce and PKCE pair; the provider sends it back to `GET /auth/sso/callback`, where
//! the server trades the code for an ID token, checks it, finds the Lode user and sends the
//! browser to the client's loopback address with a one-time code. The client trades that code
//! and its PKCE verifier for an ordinary Lode token at `POST /auth/sso/token`.
//!
//! The admin web UI (T140) signs in the same way, with itself in the client's place: its
//! sign-in cookie is the PKCE verifier, and the provider's answer leads back to
//! `/admin/sign-in/sso-done` instead of a loopback address.

mod jwt;
mod provider;

use std::collections::HashMap;
use std::sync::Mutex;

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use lode_core::ErrorCode;
use lode_meta::{MetaError, NewPrincipal, PrincipalId, PrincipalKind, PrincipalRow, Reads};
use lode_proto::auth::{LoginResponse, SsoTokenRequest};
use lode_proto::consts::paths;
use rand::RngCore;
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use self::jwt::{Expect, TokenError};
use self::provider::{Discovery, Exchange, Provider, ProviderError};
use super::handlers::{issue_token, principal_view, MAX_LABEL_LEN};
use crate::config::{is_loopback_host, OidcConfig};
use crate::error::{ApiError, ApiResult};
use crate::extract::ApiJson;
use crate::state::AppState;

/// A started sign-in waits this long for the provider to send the browser back.
const PENDING_MS: i64 = 10 * 60 * 1000;
/// A one-time code is valid this long.
const CODE_MS: i64 = 2 * 60 * 1000;
/// Most sign-ins in progress at once; the oldest is dropped beyond it.
const MAX_PENDING: usize = 10_000;

/// Single sign-on state: the provider and the sign-ins in progress.
pub struct Sso {
    cfg: OidcConfig,
    provider: Provider,
    pending: Mutex<HashMap<String, Pending>>,
    codes: Mutex<HashMap<String, Issued>>,
}

impl std::fmt::Debug for Sso {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sso")
            .field("issuer", &self.cfg.issuer)
            .finish_non_exhaustive()
    }
}

/// Where a sign-in goes back to once the provider answers.
enum Back {
    /// A Lode program's loopback address, with the program's state.
    Client {
        redirect: reqwest::Url,
        state: String,
    },
    /// The admin web UI.
    Admin,
}

/// Where the admin web UI finishes a single sign-on.
pub(crate) const ADMIN_SSO_DONE: &str = "/admin/sign-in/sso-done";

/// A sign-in sent to the provider, keyed by the server's state.
struct Pending {
    back: Back,
    client_challenge: String,
    token_name: Option<String>,
    nonce: String,
    verifier: String,
    callback: String,
    expires: i64,
}

/// A one-time code handed to a client, keyed by the code.
struct Issued {
    principal: PrincipalId,
    admin: bool,
    client_challenge: String,
    token_name: Option<String>,
    expires: i64,
}

impl Sso {
    /// Sets up single sign-on for `cfg`. Nothing is fetched until the first sign-in.
    pub fn new(cfg: OidcConfig) -> Self {
        Self {
            provider: Provider::new(&cfg.issuer),
            cfg,
            pending: Mutex::new(HashMap::new()),
            codes: Mutex::new(HashMap::new()),
        }
    }

    /// The configuration.
    pub fn config(&self) -> &OidcConfig {
        &self.cfg
    }

    fn add_pending(&self, key: String, p: Pending, now: i64) {
        let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, v| v.expires > now);
        if map.len() >= MAX_PENDING {
            if let Some(oldest) = map
                .iter()
                .min_by_key(|(_, v)| v.expires)
                .map(|(k, _)| k.clone())
            {
                map.remove(&oldest);
            }
        }
        map.insert(key, p);
    }

    fn take_pending(&self, key: &str, now: i64) -> Option<Pending> {
        let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(key).filter(|p| p.expires > now)
    }

    fn add_code(&self, code: String, issued: Issued, now: i64) {
        let mut map = self.codes.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, v| v.expires > now);
        map.insert(code, issued);
    }

    fn take_code(&self, code: &str, now: i64) -> Option<Issued> {
        let mut map = self.codes.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(code).filter(|c| c.expires > now)
    }
}

/// The single sign-on routes. They answer `not_found` when `[oidc]` is not configured.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(paths::SSO_START, get(start))
        .route(paths::SSO_CALLBACK, get(callback))
        .route(paths::SSO_TOKEN, post(token))
}

fn sso(state: &AppState) -> ApiResult<&Sso> {
    state
        .sso()
        .ok_or_else(|| ApiError::not_found("single sign-on is not set up on this server"))
}

fn random() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// RFC 7636 characters, with a length range.
fn pkce_text(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A small page for the browser when there is no client to send it back to.
fn page(status: StatusCode, text: &str) -> Response {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Lode sign-in</title></head>\
         <body style=\"font-family:system-ui,sans-serif;max-width:36em;margin:4em auto\">\
         <h1>Lode sign-in</h1><p>{}</p></body></html>",
        escape_html(text)
    );
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

/// Sends the browser back to the client's loopback address with these parameters.
fn back_to_client(url: &reqwest::Url, pairs: &[(&str, &str)]) -> Response {
    let mut u = url.clone();
    {
        let mut q = u.query_pairs_mut();
        for (k, v) in pairs {
            q.append_pair(k, v);
        }
    }
    Redirect::to(u.as_str()).into_response()
}

fn refuse(p: &Pending, message: &str) -> Response {
    match &p.back {
        Back::Client { redirect, state } => back_to_client(
            redirect,
            &[
                ("error", "access_denied"),
                ("error_description", message),
                ("state", state),
            ],
        ),
        Back::Admin => page(
            StatusCode::FORBIDDEN,
            &format!("The sign-in was refused: {message}. Start again from the admin page."),
        ),
    }
}

/// The client's redirect address: `http` to a loopback host with a port.
fn client_redirect(raw: &str) -> Option<reqwest::Url> {
    let u = reqwest::Url::parse(raw).ok()?;
    let ok = u.scheme() == "http"
        && u.host_str().is_some_and(is_loopback_host)
        && u.port().is_some()
        && u.username().is_empty()
        && u.password().is_none()
        && u.fragment().is_none()
        && u.query().is_none();
    ok.then_some(u)
}

/// Where the provider sends the browser back: `public_url`, or the address this request
/// came to.
fn callback_url(state: &AppState, cfg: &OidcConfig, headers: &HeaderMap) -> Option<String> {
    let base = match &cfg.public_url {
        Some(u) => u.clone(),
        None => {
            let host = headers.get(header::HOST)?.to_str().ok()?;
            if host.is_empty() || host.contains(['/', '\\', '@', ' ']) {
                return None;
            }
            let scheme = if state.config().tls.is_some() {
                "https"
            } else {
                "http"
            };
            format!("{scheme}://{host}")
        }
    };
    Some(format!("{base}{}", paths::SSO_CALLBACK))
}

#[derive(Debug, Deserialize)]
struct StartQuery {
    redirect_uri: String,
    state: String,
    code_challenge: String,
    #[serde(default)]
    code_challenge_method: Option<String>,
    #[serde(default)]
    token_name: Option<String>,
}

async fn start(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Result<Query<StartQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(sso) = sso(&state) else {
        return page(
            StatusCode::NOT_FOUND,
            "Single sign-on is not set up on this server.",
        );
    };
    let Ok(Query(q)) = query else {
        return page(
            StatusCode::BAD_REQUEST,
            "This sign-in link is incomplete. Start again from Lode.",
        );
    };
    let Some(redirect) = client_redirect(&q.redirect_uri) else {
        return page(
            StatusCode::BAD_REQUEST,
            "This sign-in link does not lead back to a Lode program on this computer.",
        );
    };
    let bad = |message: &str| {
        back_to_client(
            &redirect,
            &[
                ("error", "invalid_request"),
                ("error_description", message),
                ("state", &q.state),
            ],
        )
    };
    if !pkce_text(&q.state, 8, 128) {
        return page(
            StatusCode::BAD_REQUEST,
            "This sign-in link has no valid state.",
        );
    }
    if !pkce_text(&q.code_challenge, 43, 43)
        || q.code_challenge_method
            .as_deref()
            .is_some_and(|m| m != "S256")
    {
        return bad("the sign-in needs a PKCE S256 challenge");
    }
    let token_name = q.token_name.filter(|n| !n.trim().is_empty());
    if token_name.as_ref().is_some_and(|n| n.len() > MAX_LABEL_LEN) {
        return bad("token name is too long");
    }
    let back = Back::Client {
        redirect: redirect.clone(),
        state: q.state.clone(),
    };
    match to_provider(&state, sso, &headers, back, q.code_challenge, token_name).await {
        Ok(r) => r,
        Err(message) => bad(&message),
    }
}

/// Starts a sign-in of the admin web UI: `verifier` is its sign-in cookie, which must come
/// back with the one-time code.
pub(crate) async fn start_admin(state: &AppState, headers: &HeaderMap, verifier: &str) -> Response {
    let Ok(sso) = sso(state) else {
        return page(
            StatusCode::NOT_FOUND,
            "Single sign-on is not set up on this server.",
        );
    };
    let challenge = s256(verifier);
    match to_provider(state, sso, headers, Back::Admin, challenge, None).await {
        Ok(r) => r,
        Err(message) => page(
            StatusCode::BAD_GATEWAY,
            &format!("Single sign-on cannot start: {message}."),
        ),
    }
}

/// The one-time code of an admin web UI sign-in, checked against its sign-in cookie: the
/// user it signed in.
pub(crate) fn finish_admin(state: &AppState, code: &str, verifier: &str) -> Option<PrincipalId> {
    let sso = state.sso()?;
    let issued = sso.take_code(code, state.clock().now_ms())?;
    (issued.admin && pkce_text(verifier, 43, 128) && s256(verifier) == issued.client_challenge)
        .then_some(issued.principal)
}

/// A fresh random text: a sign-in cookie for the admin web UI.
pub(crate) fn random_verifier() -> String {
    random()
}

/// Records a pending sign-in and sends the browser to the provider.
async fn to_provider(
    state: &AppState,
    sso: &Sso,
    headers: &HeaderMap,
    back: Back,
    client_challenge: String,
    token_name: Option<String>,
) -> Result<Response, String> {
    let Some(callback) = callback_url(state, &sso.cfg, headers) else {
        return Err("the server cannot tell its own address; set [oidc] public_url".into());
    };
    let now = state.clock().now_ms();
    let disc = match sso.provider.discovery(now).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "single sign-on: discovery failed");
            return Err(e.0);
        }
    };
    let key = random();
    let nonce = random();
    let verifier = random();
    let Ok(mut authorize) = reqwest::Url::parse(&disc.authorization_endpoint) else {
        return Err("the identity provider's authorization endpoint is not a URL".into());
    };
    authorize
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &sso.cfg.client_id)
        .append_pair("redirect_uri", &callback)
        .append_pair("scope", &sso.cfg.scopes.join(" "))
        .append_pair("state", &key)
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge", &s256(&verifier))
        .append_pair("code_challenge_method", "S256");
    sso.add_pending(
        key,
        Pending {
            back,
            client_challenge,
            token_name,
            nonce,
            verifier,
            callback,
            expires: now + PENDING_MS,
        },
        now,
    );
    Ok(Redirect::to(authorize.as_str()).into_response())
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

async fn callback(
    State(state): State<AppState>,
    query: Result<Query<CallbackQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(sso) = sso(&state) else {
        return page(
            StatusCode::NOT_FOUND,
            "Single sign-on is not set up on this server.",
        );
    };
    let now = state.clock().now_ms();
    let q = query.map(|Query(q)| q).ok();
    let Some(pending) = q
        .as_ref()
        .and_then(|q| q.state.as_deref())
        .and_then(|s| sso.take_pending(s, now))
    else {
        return page(
            StatusCode::BAD_REQUEST,
            "This sign-in has expired or was already used. Start again from Lode.",
        );
    };
    let Some(q) = q else {
        return refuse(&pending, "the identity provider's answer is incomplete");
    };
    if let Some(err) = q.error {
        let desc = q
            .error_description
            .map(|d| format!(": {}", d.chars().take(200).collect::<String>()))
            .unwrap_or_default();
        return refuse(
            &pending,
            &format!("the identity provider refused the sign-in ({err}){desc}"),
        );
    }
    let Some(code) = q.code else {
        return refuse(&pending, "the identity provider's answer has no code");
    };
    match sign_in(&state, sso, &pending, &code, now).await {
        Ok(principal) => {
            let one_time = random();
            let admin = matches!(pending.back, Back::Admin);
            sso.add_code(
                one_time.clone(),
                Issued {
                    principal,
                    admin,
                    client_challenge: pending.client_challenge.clone(),
                    token_name: pending.token_name.clone(),
                    expires: now + CODE_MS,
                },
                now,
            );
            match &pending.back {
                Back::Client { redirect, state } => {
                    back_to_client(redirect, &[("code", &one_time), ("state", state)])
                }
                Back::Admin => Redirect::to(&format!(
                    "{ADMIN_SSO_DONE}?code={}",
                    crate::admin::html::encode(&one_time)
                ))
                .into_response(),
            }
        }
        Err(message) => {
            tracing::info!(reason = %message, "single sign-on refused");
            refuse(&pending, &message)
        }
    }
}

/// Trades the code, checks the ID token and finds (or creates) the Lode user.
async fn sign_in(
    state: &AppState,
    sso: &Sso,
    pending: &Pending,
    code: &str,
    now: i64,
) -> Result<PrincipalId, String> {
    let disc = sso.provider.discovery(now).await.map_err(|e| e.0)?;
    let secret = sso.cfg.client_secret().map_err(|e| {
        tracing::error!(error = %e, "single sign-on: cannot read the client secret");
        "the server cannot read its single sign-on client secret".to_owned()
    })?;
    let id_token = sso
        .provider
        .exchange(
            &disc,
            Exchange {
                code,
                redirect_uri: &pending.callback,
                verifier: &pending.verifier,
                client_id: &sso.cfg.client_id,
                client_secret: secret.as_deref(),
            },
        )
        .await
        .map_err(|ProviderError(m)| m)?;
    let claims = verify(sso, &disc, &id_token, &pending.nonce, now)
        .await
        .map_err(|e| e.to_string())?;
    let principal = find_user(state, &sso.cfg, &claims).await?;
    Ok(principal.id)
}

#[derive(Debug, thiserror::Error)]
enum VerifyError {
    #[error(transparent)]
    Token(#[from] TokenError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

async fn verify(
    sso: &Sso,
    disc: &Discovery,
    id_token: &str,
    nonce: &str,
    now: i64,
) -> Result<Map<String, Value>, VerifyError> {
    let jwt = jwt::parse(id_token)?;
    let mut keys = sso.provider.keys(disc, now, false).await?;
    if !jwt.has_key(&keys) {
        keys = sso.provider.keys(disc, now, true).await?;
    }
    jwt.verify(&keys)?;
    jwt::check_claims(
        &jwt.claims,
        Expect {
            issuer: &disc.issuer,
            client_id: &sso.cfg.client_id,
            nonce,
            now: now.div_euclid(1000),
        },
    )?;
    Ok(jwt.claims)
}

/// A login name from an identity such as `Ana.Lima@example.com`: `ana.lima`.
fn name_from(identity: &str) -> String {
    let local = identity.split('@').next().unwrap_or("");
    let cleaned: String = local
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect();
    let trimmed = cleaned.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    let mut name: String = trimmed.chars().take(56).collect();
    if name.is_empty() {
        name = "user".into();
    }
    name
}

/// The Lode user for the checked claims.
async fn find_user(
    state: &AppState,
    cfg: &OidcConfig,
    claims: &Map<String, Value>,
) -> Result<PrincipalRow, String> {
    let claim = &cfg.user_claim;
    let identity = claims
        .get(claim)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("the sign-in token has no `{claim}` claim"))?
        .to_lowercase();
    if identity.len() > 254 {
        return Err(format!("the `{claim}` claim is too long"));
    }
    if claim == "email" && claims.get("email_verified") == Some(&Value::Bool(false)) {
        return Err(format!(
            "the identity provider has not verified the e-mail address {identity}"
        ));
    }
    if !cfg.allowed_domains.is_empty() {
        let domain = identity.rsplit_once('@').map(|(_, d)| d).unwrap_or("");
        if !cfg.allowed_domains.iter().any(|d| d == domain) {
            return Err(format!(
                "{identity} is not in a domain allowed on this server"
            ));
        }
    }
    let display = claims
        .get("name")
        .and_then(Value::as_str)
        .map(|s| s.chars().take(100).collect::<String>());
    let auto_create = cfg.auto_create;
    let now = state.clock().now_ms();
    let who = identity.clone();
    let found = state
        .meta()
        .write(move |tx| {
            if let Some(p) = tx.principal_by_sso(&who)? {
                return Ok(Some(p));
            }
            if !auto_create {
                return Ok(None);
            }
            let base = name_from(&who);
            let mut name = base.clone();
            let mut n = 1;
            while tx.principal_by_name(&name)?.is_some() {
                n += 1;
                if n > 1000 {
                    return Err(MetaError::AlreadyExists("principal name".into()));
                }
                name = format!("{base}-{n}");
            }
            let id = tx.create_principal(
                &NewPrincipal {
                    kind: PrincipalKind::User,
                    name,
                    display_name: display,
                    owner_id: None,
                    password_hash: None,
                },
                now,
            )?;
            tx.set_principal_sso(id, Some(&who))?;
            tx.principal(id)
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "single sign-on: user lookup failed");
            "the server could not look up the user".to_owned()
        })?;
    let Some(p) = found else {
        return Err(format!(
            "no Lode user is linked to {identity}; ask a server administrator to add you"
        ));
    };
    if p.kind != PrincipalKind::User {
        return Err("agents cannot sign in".into());
    }
    if p.disabled {
        return Err(format!("the Lode user {} is disabled", p.name));
    }
    if auto_create {
        tracing::info!(principal = %p.name, "single sign-on: signed in");
    }
    Ok(p)
}

async fn token(
    State(state): State<AppState>,
    ApiJson(req): ApiJson<SsoTokenRequest>,
) -> ApiResult<Json<LoginResponse>> {
    let sso = sso(&state)?;
    let now = state.clock().now_ms();
    let invalid = || {
        ApiError::new(
            ErrorCode::Unauthenticated,
            "this sign-in code is not valid or has expired; sign in again",
        )
    };
    let issued = sso
        .take_code(&req.code, now)
        .filter(|i| !i.admin)
        .ok_or_else(invalid)?;
    if !pkce_text(&req.code_verifier, 43, 128)
        || s256(&req.code_verifier) != issued.client_challenge
    {
        return Err(invalid());
    }
    let id = issued.principal;
    let principal = state
        .meta()
        .read(move |tx| tx.principal(id))
        .await?
        .filter(|p| !p.disabled)
        .ok_or_else(|| ApiError::new(ErrorCode::Unauthenticated, "account disabled"))?;
    let label = issued.token_name.unwrap_or_else(|| "sso".to_owned());
    let scopes = state.scopes().all();
    let (token, _) = issue_token(&state, principal.id, label, scopes, None).await?;
    Ok(Json(LoginResponse {
        token,
        principal: principal_view(&principal, None),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_identities() {
        assert_eq!(name_from("Ana.Lima@example.com"), "ana.lima");
        assert_eq!(name_from("_x+tag@example.com"), "xtag");
        assert_eq!(name_from("@example.com"), "user");
        assert_eq!(name_from("bob"), "bob");
    }

    #[test]
    fn client_redirects_must_be_loopback_http() {
        assert!(client_redirect("http://127.0.0.1:5000/cb").is_some());
        assert!(client_redirect("http://[::1]:5000/").is_some());
        assert!(client_redirect("http://localhost:5000/").is_some());
        assert!(client_redirect("http://127.0.0.1/cb").is_none(), "no port");
        assert!(client_redirect("https://127.0.0.1:5000/").is_none());
        assert!(client_redirect("http://evil.example:5000/").is_none());
        assert!(client_redirect("http://u@127.0.0.1:5000/").is_none());
        assert!(client_redirect("http://127.0.0.1:5000/?a=1").is_none());
    }

    #[test]
    fn pkce_values() {
        // RFC 7636 appendix B.
        assert_eq!(
            s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert!(pkce_text(&random(), 43, 43));
        assert!(!pkce_text("short", 43, 128));
        assert!(!pkce_text(&"a b".repeat(20), 8, 128));
    }
}
