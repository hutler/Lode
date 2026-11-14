//! The identity provider as seen from the server: its discovery document, its signing keys
//! and its token endpoint. Documents and keys are cached; keys are fetched again when a token
//! names one that is not known yet (the provider rotated them).

use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::de::DeserializeOwned;
use serde::Deserialize;

use super::jwt::Jwks;
use crate::config::provider_url_ok;

/// How long a discovery document or key set is used before it is fetched again.
const CACHE_MS: i64 = 60 * 60 * 1000;
/// Keys are fetched again for an unknown key id at most this often.
const REFETCH_MS: i64 = 30 * 1000;
/// Largest answer read from the provider.
const MAX_BODY: usize = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);

/// The parts of `/.well-known/openid-configuration` Lode uses.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Discovery {
    pub(crate) issuer: String,
    pub(crate) authorization_endpoint: String,
    pub(crate) token_endpoint: String,
    pub(crate) jwks_uri: String,
    #[serde(default)]
    pub(crate) token_endpoint_auth_methods_supported: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TokenAnswer {
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// A failure talking to the provider. The text is shown to the person signing in and
/// logged; it never holds a secret, a code or a token.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub(crate) struct ProviderError(pub(crate) String);

struct Cached<T> {
    value: T,
    at: i64,
}

/// One configured provider.
pub(crate) struct Provider {
    issuer: String,
    http: Result<reqwest::Client, String>,
    discovery: tokio::sync::Mutex<Option<Cached<Discovery>>>,
    keys: tokio::sync::Mutex<Option<Cached<Jwks>>>,
}

fn checked_url(what: &str, url: &str) -> Result<reqwest::Url, ProviderError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| ProviderError(format!("the identity provider's {what} is not a URL")))?;
    if !provider_url_ok(&parsed) {
        return Err(ProviderError(format!(
            "the identity provider's {what} is not an https:// address"
        )));
    }
    Ok(parsed)
}

/// Encodes for `application/x-www-form-urlencoded` (also what client_secret_basic applies
/// before the Base64 step).
pub(crate) fn form_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(char::from(b))
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A form body from name and value pairs.
pub(crate) fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", form_escape(k), form_escape(v)))
        .collect::<Vec<_>>()
        .join("&")
}

impl Provider {
    pub(crate) fn new(issuer: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("loded/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| e.to_string());
        Self {
            issuer: issuer.to_owned(),
            http,
            discovery: tokio::sync::Mutex::new(None),
            keys: tokio::sync::Mutex::new(None),
        }
    }

    fn http(&self) -> Result<&reqwest::Client, ProviderError> {
        self.http
            .as_ref()
            .map_err(|e| ProviderError(format!("cannot set up an HTTP client: {e}")))
    }

    async fn read_body(mut resp: reqwest::Response, what: &str) -> Result<Vec<u8>, ProviderError> {
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| {
            ProviderError(format!(
                "reading the identity provider's {what} failed: {e}"
            ))
        })? {
            if body.len() + chunk.len() > MAX_BODY {
                return Err(ProviderError(format!(
                    "the identity provider's {what} is too large"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        url: reqwest::Url,
        what: &str,
    ) -> Result<T, ProviderError> {
        let resp = self.http()?.get(url).send().await.map_err(|e| {
            ProviderError(format!(
                "the identity provider could not be reached for its {what}: {}",
                e.without_url()
            ))
        })?;
        if !resp.status().is_success() {
            return Err(ProviderError(format!(
                "the identity provider answered {} for its {what}",
                resp.status()
            )));
        }
        let body = Self::read_body(resp, what).await?;
        serde_json::from_slice(&body)
            .map_err(|e| ProviderError(format!("the identity provider's {what} is invalid: {e}")))
    }

    /// The discovery document, from the cache when fresh.
    pub(crate) async fn discovery(&self, now: i64) -> Result<Discovery, ProviderError> {
        let mut slot = self.discovery.lock().await;
        if let Some(c) = slot.as_ref().filter(|c| now - c.at < CACHE_MS) {
            return Ok(c.value.clone());
        }
        let url = checked_url(
            "issuer",
            &format!(
                "{}/.well-known/openid-configuration",
                self.issuer.trim_end_matches('/')
            ),
        )?;
        let doc: Discovery = self.get_json(url, "discovery document").await?;
        if doc.issuer.trim_end_matches('/') != self.issuer.trim_end_matches('/') {
            return Err(ProviderError(format!(
                "the identity provider calls itself {}, not {} as configured",
                doc.issuer, self.issuer
            )));
        }
        for (what, u) in [
            ("authorization endpoint", &doc.authorization_endpoint),
            ("token endpoint", &doc.token_endpoint),
            ("key set address", &doc.jwks_uri),
        ] {
            checked_url(what, u)?;
        }
        *slot = Some(Cached {
            value: doc.clone(),
            at: now,
        });
        Ok(doc)
    }

    /// The signing keys. `refresh` asks for a fresh copy (a token named an unknown key); it
    /// is honoured at most every 30 seconds.
    pub(crate) async fn keys(
        &self,
        disc: &Discovery,
        now: i64,
        refresh: bool,
    ) -> Result<Jwks, ProviderError> {
        let mut slot = self.keys.lock().await;
        if let Some(c) = slot.as_ref() {
            let age = now - c.at;
            if age < CACHE_MS && !(refresh && age >= REFETCH_MS) {
                return Ok(c.value.clone());
            }
        }
        let keys: Jwks = self
            .get_json(checked_url("key set address", &disc.jwks_uri)?, "key set")
            .await?;
        *slot = Some(Cached {
            value: keys.clone(),
            at: now,
        });
        Ok(keys)
    }

    /// Trades an authorization code for the ID token.
    pub(crate) async fn exchange(
        &self,
        disc: &Discovery,
        exchange: Exchange<'_>,
    ) -> Result<String, ProviderError> {
        let url = checked_url("token endpoint", &disc.token_endpoint)?;
        let mut pairs = vec![
            ("grant_type", "authorization_code"),
            ("code", exchange.code),
            ("redirect_uri", exchange.redirect_uri),
            ("code_verifier", exchange.verifier),
            ("client_id", exchange.client_id),
        ];
        let methods = &disc.token_endpoint_auth_methods_supported;
        let use_basic = methods.is_empty() || methods.iter().any(|m| m == "client_secret_basic");
        let mut req = self.http()?.post(url);
        if let Some(secret) = exchange.client_secret {
            if use_basic {
                let pair = format!(
                    "{}:{}",
                    form_escape(exchange.client_id),
                    form_escape(secret)
                );
                req = req.header(
                    reqwest::header::AUTHORIZATION,
                    format!("Basic {}", STANDARD.encode(pair)),
                );
            } else {
                pairs.push(("client_secret", secret));
            }
        }
        let body = form(&pairs);
        let resp = req
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| {
                ProviderError(format!(
                    "the identity provider's token endpoint could not be reached: {}",
                    e.without_url()
                ))
            })?;
        let status = resp.status();
        let bytes = Self::read_body(resp, "token answer").await?;
        let answer: TokenAnswer = serde_json::from_slice(&bytes).map_err(|_| {
            ProviderError(format!(
                "the identity provider's token endpoint answered {status} without JSON"
            ))
        })?;
        if let Some(err) = answer.error {
            let desc = answer
                .error_description
                .map(|d| format!(": {}", d.chars().take(200).collect::<String>()))
                .unwrap_or_default();
            return Err(ProviderError(format!(
                "the identity provider refused the sign-in ({err}){desc}"
            )));
        }
        answer
            .id_token
            .filter(|_| status.is_success())
            .ok_or_else(|| {
                ProviderError(format!(
                    "the identity provider's token endpoint answered {status} without an ID token"
                ))
            })
    }
}

/// The parameters of a code exchange.
#[derive(Clone, Copy)]
pub(crate) struct Exchange<'a> {
    pub(crate) code: &'a str,
    pub(crate) redirect_uri: &'a str,
    pub(crate) verifier: &'a str,
    pub(crate) client_id: &'a str,
    pub(crate) client_secret: Option<&'a str>,
}

impl std::fmt::Debug for Exchange<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Exchange")
            .field("redirect_uri", &self.redirect_uri)
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_escaping_matches_the_form_encoding() {
        assert_eq!(form_escape("a b:c/+"), "a+b%3Ac%2F%2B");
        assert_eq!(form_escape("plain-id_1.2~"), "plain-id_1.2%7E");
        assert_eq!(form(&[("a", "1"), ("b c", "é")]), "a=1&b+c=%C3%A9");
    }
}
