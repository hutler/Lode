//! A test OpenID Connect provider for single sign-on tests (T122).
//!
//! It serves discovery, its key set (one RSA key and one P-256 key), an authorization
//! endpoint that approves at once and a token endpoint that checks the client secret and the
//! PKCE verifier. [`TestProvider::set`] bends the next ID tokens to test each refusal:
//! another audience, an expired token, a broken signature, a stale nonce, other claims.
//! [`browse`] plays the browser: it follows redirects the way one would.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Form, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{
    EcdsaKeyPair, KeyPair, RsaKeyPair, RsaPublicKeyComponents, ECDSA_P256_SHA256_FIXED_SIGNING,
    RSA_PKCS1_SHA256,
};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use crate::must;

/// The test RSA key (2048 bits, PKCS#8). Made for these tests only; it protects nothing.
const RSA_KEY: &[u8] = include_bytes!("../fixtures/oidc-test-rsa.pk8");

/// The signing algorithm of the next ID tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alg {
    /// RSA with SHA-256.
    Rs256,
    /// ECDSA P-256 with SHA-256.
    Es256,
}

/// How the provider answers. Change it with [`TestProvider::set`].
#[derive(Debug, Clone)]
pub struct Behaviour {
    /// Claims added to every ID token, such as `email`.
    pub claims: Map<String, Value>,
    /// Signing algorithm.
    pub alg: Alg,
    /// An audience other than the client id.
    pub audience: Option<String>,
    /// Seconds from now until the token expires; negative for an expired token.
    pub expires_in: i64,
    /// Breaks the signature.
    pub tamper: bool,
    /// A nonce other than the one the sign-in asked for.
    pub nonce: Option<String>,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            claims: Map::new(),
            alg: Alg::Rs256,
            audience: None,
            expires_in: 300,
            tamper: false,
            nonce: None,
        }
    }
}

struct Grant {
    client_id: String,
    redirect_uri: String,
    nonce: String,
    challenge: String,
}

struct Inner {
    issuer: String,
    client_id: String,
    client_secret: String,
    rsa: RsaKeyPair,
    ec: EcdsaKeyPair,
    behaviour: Mutex<Behaviour>,
    grants: Mutex<HashMap<String, Grant>>,
    nonces: Mutex<Vec<String>>,
}

/// A provider listening on a loopback port. Stops on drop.
pub struct TestProvider {
    inner: Arc<Inner>,
    _stop: oneshot::Sender<()>,
}

impl std::fmt::Debug for TestProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestProvider")
            .field("issuer", &self.inner.issuer)
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn random() -> String {
    let mut b = [0u8; 16];
    must(
        ring::rand::SecureRandom::fill(&SystemRandom::new(), &mut b).map_err(|_| "rng"),
        "random bytes",
    );
    URL_SAFE_NO_PAD.encode(b)
}

impl TestProvider {
    /// Starts a provider that knows one client. Needs a Tokio runtime.
    pub async fn start(client_id: &str, client_secret: &str) -> Self {
        let rng = SystemRandom::new();
        let rsa = must(RsaKeyPair::from_pkcs8(RSA_KEY), "test RSA key");
        let pk8 = must(
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng),
            "P-256 key",
        );
        let ec = must(
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pk8.as_ref(), &rng),
            "P-256 key",
        );
        let listener = must(tokio::net::TcpListener::bind("127.0.0.1:0").await, "bind");
        let addr = must(listener.local_addr(), "address");
        let inner = Arc::new(Inner {
            issuer: format!("http://{addr}"),
            client_id: client_id.to_owned(),
            client_secret: client_secret.to_owned(),
            rsa,
            ec,
            behaviour: Mutex::new(Behaviour::default()),
            grants: Mutex::new(HashMap::new()),
            nonces: Mutex::new(Vec::new()),
        });
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/authorize", get(authorize))
            .route("/token", post(token))
            .with_state(inner.clone());
        let (tx, rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await;
        });
        Self { inner, _stop: tx }
    }

    /// The issuer URL, such as `http://127.0.0.1:41234`.
    pub fn issuer(&self) -> &str {
        &self.inner.issuer
    }

    /// Changes how the next ID tokens are made.
    pub fn set(&self, f: impl FnOnce(&mut Behaviour)) {
        f(&mut lock(&self.inner.behaviour));
    }

    /// Back to the default behaviour.
    pub fn reset(&self) {
        *lock(&self.inner.behaviour) = Behaviour::default();
    }

    /// Nonces of the sign-ins seen so far, oldest first.
    pub fn nonces(&self) -> Vec<String> {
        lock(&self.inner.nonces).clone()
    }
}

async fn discovery(State(p): State<Arc<Inner>>) -> Json<Value> {
    let i = &p.issuer;
    Json(json!({
        "issuer": i,
        "authorization_endpoint": format!("{i}/authorize"),
        "token_endpoint": format!("{i}/token"),
        "jwks_uri": format!("{i}/jwks"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256", "ES256"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
        "code_challenge_methods_supported": ["S256"],
    }))
}

async fn jwks(State(p): State<Arc<Inner>>) -> Json<Value> {
    let rsa: RsaPublicKeyComponents<Vec<u8>> = p.rsa.public().into();
    let ec = p.ec.public_key().as_ref();
    Json(json!({
        "keys": [
            {"kty": "RSA", "kid": "rsa-1", "use": "sig", "alg": "RS256",
             "n": URL_SAFE_NO_PAD.encode(&rsa.n), "e": URL_SAFE_NO_PAD.encode(&rsa.e)},
            {"kty": "EC", "kid": "ec-1", "use": "sig", "alg": "ES256", "crv": "P-256",
             "x": URL_SAFE_NO_PAD.encode(&ec[1..33]), "y": URL_SAFE_NO_PAD.encode(&ec[33..65])},
        ]
    }))
}

fn oauth_error(status: StatusCode, error: &str) -> Response {
    (status, Json(json!({ "error": error }))).into_response()
}

async fn authorize(
    State(p): State<Arc<Inner>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let get = |k: &str| q.get(k).cloned().unwrap_or_default();
    if get("client_id") != p.client_id
        || get("response_type") != "code"
        || get("code_challenge_method") != "S256"
        || !get("scope").split(' ').any(|s| s == "openid")
    {
        return (StatusCode::BAD_REQUEST, "bad authorization request").into_response();
    }
    let code = random();
    let redirect_uri = get("redirect_uri");
    lock(&p.nonces).push(get("nonce"));
    lock(&p.grants).insert(
        code.clone(),
        Grant {
            client_id: get("client_id"),
            redirect_uri: redirect_uri.clone(),
            nonce: get("nonce"),
            challenge: get("code_challenge"),
        },
    );
    let Ok(mut back) = reqwest::Url::parse(&redirect_uri) else {
        return (StatusCode::BAD_REQUEST, "bad redirect_uri").into_response();
    };
    back.query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &get("state"));
    Redirect::to(back.as_str()).into_response()
}

fn client_auth(headers: &HeaderMap, form: &HashMap<String, String>) -> Option<(String, String)> {
    if let Some(basic) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
    {
        let raw = String::from_utf8(STANDARD.decode(basic).ok()?).ok()?;
        let (id, secret) = raw.split_once(':')?;
        return Some((id.to_owned(), secret.to_owned()));
    }
    Some((
        form.get("client_id")?.clone(),
        form.get("client_secret")?.clone(),
    ))
}

async fn token(
    State(p): State<Arc<Inner>>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    match client_auth(&headers, &form) {
        Some((id, secret)) if id == p.client_id && secret == p.client_secret => {}
        _ => return oauth_error(StatusCode::UNAUTHORIZED, "invalid_client"),
    }
    let get = |k: &str| form.get(k).cloned().unwrap_or_default();
    if get("grant_type") != "authorization_code" {
        return oauth_error(StatusCode::BAD_REQUEST, "unsupported_grant_type");
    }
    let Some(grant) = lock(&p.grants).remove(&get("code")) else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant");
    };
    let verifier = get("code_verifier");
    if grant.client_id != p.client_id
        || grant.redirect_uri != get("redirect_uri")
        || URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) != grant.challenge
    {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant");
    }
    let b = lock(&p.behaviour).clone();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut claims = Map::new();
    claims.insert("iss".into(), json!(p.issuer));
    claims.insert("sub".into(), json!(format!("sub-{}", random())));
    claims.insert(
        "aud".into(),
        json!(b.audience.clone().unwrap_or_else(|| p.client_id.clone())),
    );
    claims.insert("iat".into(), json!(now));
    claims.insert("exp".into(), json!(now + b.expires_in));
    claims.insert(
        "nonce".into(),
        json!(b.nonce.clone().unwrap_or(grant.nonce)),
    );
    for (k, v) in &b.claims {
        claims.insert(k.clone(), v.clone());
    }
    let id_token = sign(&p, &b, &Value::Object(claims));
    Json(json!({
        "access_token": random(),
        "token_type": "Bearer",
        "expires_in": 300,
        "id_token": id_token,
    }))
    .into_response()
}

fn sign(p: &Inner, b: &Behaviour, claims: &Value) -> String {
    let header = match b.alg {
        Alg::Rs256 => json!({"alg": "RS256", "kid": "rsa-1", "typ": "JWT"}),
        Alg::Es256 => json!({"alg": "ES256", "kid": "ec-1", "typ": "JWT"}),
    };
    let enc = |v: &Value| URL_SAFE_NO_PAD.encode(must(serde_json::to_vec(v), "json"));
    let input = format!("{}.{}", enc(&header), enc(claims));
    let rng = SystemRandom::new();
    let mut sig = match b.alg {
        Alg::Rs256 => {
            let mut sig = vec![0u8; p.rsa.public().modulus_len()];
            must(
                p.rsa
                    .sign(&RSA_PKCS1_SHA256, &rng, input.as_bytes(), &mut sig)
                    .map_err(|_| "sign"),
                "RS256 signature",
            );
            sig
        }
        Alg::Es256 => must(
            p.ec.sign(&rng, input.as_bytes()).map_err(|_| "sign"),
            "ES256 signature",
        )
        .as_ref()
        .to_vec(),
    };
    if b.tamper {
        if let Some(last) = sig.last_mut() {
            *last ^= 0x01;
        }
    }
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig))
}

/// Where a browser ended up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Browsed {
    /// The last address fetched.
    pub url: String,
    /// Its status.
    pub status: u16,
    /// Its body.
    pub body: String,
}

/// Plays a browser: fetches `url` and follows redirects (at most 10) to the last page.
pub async fn browse(url: &str) -> Browsed {
    let http = must(
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build(),
        "HTTP client",
    );
    let mut current = must(reqwest::Url::parse(url), "browse URL");
    for _ in 0..10 {
        let resp = must(
            http.get(current.clone())
                .send()
                .await
                .map_err(|e| format!("{e:?}")),
            "browse",
        );
        let status = resp.status();
        if status.is_redirection() {
            let loc = resp
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            if let Some(loc) = loc {
                current = must(current.join(&loc), "redirect address");
                continue;
            }
        }
        let body = must(resp.text().await, "page body");
        return Browsed {
            url: current.to_string(),
            status: status.as_u16(),
            body,
        };
    }
    must(Err("more than 10 redirects"), "browse")
}
