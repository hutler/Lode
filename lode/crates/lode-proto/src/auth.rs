//! `/info`, login, identity and tokens.

use serde::{Deserialize, Serialize};

use crate::{rfc3339, PrincipalKind, Timestamp};

/// `GET /info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    /// Server version, such as `0.1.0`.
    pub server_version: String,
    /// API version; see [`crate::consts::API_VERSION`].
    pub api_version: String,
    /// Names of the enabled modules.
    #[serde(default)]
    pub modules: Vec<String>,
    /// Single sign-on, when the server offers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sso: Option<SsoInfo>,
}

/// Single sign-on as `GET /info` describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SsoInfo {
    /// What sign-in buttons call the provider, such as `Example Corp`.
    pub label: String,
    /// Whether everyone may still sign in with a password; false when only server
    /// administrators may.
    pub password_login: bool,
}

/// `POST /auth/sso/token`: the one-time code the server handed the client's loopback
/// address, and the PKCE verifier whose challenge started the sign-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SsoTokenRequest {
    /// The one-time code.
    pub code: String,
    /// The PKCE code verifier.
    pub code_verifier: String,
}

/// `POST /auth/login`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginRequest {
    /// User name.
    pub name: String,
    /// Password.
    pub password: String,
    /// Label for the token created, such as the machine name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_name: Option<String>,
}

/// Answer to a login.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginResponse {
    /// The new bearer token. Shown only here.
    pub token: String,
    /// Who logged in.
    pub principal: PrincipalView,
}

/// A principal as shown to clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalView {
    /// Login name.
    pub name: String,
    /// User or agent.
    pub kind: PrincipalKind,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Owning user, for agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Disabled principals cannot authenticate.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub disabled: bool,
    /// The single sign-on identity linked to this user, such as an e-mail address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sso: Option<String>,
}

/// `GET /auth/whoami`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhoAmI {
    /// The caller.
    pub principal: PrincipalView,
    /// Scopes of the token used.
    pub scopes: Vec<String>,
    /// Id of the token used.
    pub token_id: i64,
}

/// A token as listed by its owner. The secret is never listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenView {
    /// Id.
    pub id: i64,
    /// Label.
    pub name: String,
    /// Scopes.
    pub scopes: Vec<String>,
    /// Creation time.
    #[serde(with = "rfc3339")]
    pub created_at: Timestamp,
    /// Expiry.
    #[serde(
        default,
        with = "rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub expires_at: Option<Timestamp>,
    /// Last use.
    #[serde(
        default,
        with = "rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_used_at: Option<Timestamp>,
}

/// `GET /tokens`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenList {
    /// Tokens of the caller.
    pub tokens: Vec<TokenView>,
}

/// `POST /tokens`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateTokenRequest {
    /// Label.
    pub name: String,
    /// Scopes; at most those of the token making the request. Empty for an agent token means
    /// the agent defaults (`read` plus the agent-default module scopes).
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Expiry.
    #[serde(
        default,
        with = "rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub expires_at: Option<Timestamp>,
}

/// Answer to token creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateTokenResponse {
    /// The bearer token. Shown only here.
    pub token: String,
    /// Its listing entry.
    pub info: TokenView,
}
