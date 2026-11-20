//! `/info`, login, identity and tokens.

use serde::{Deserialize, Serialize};

use crate::{rfc3339, PrincipalKind, Timestamp};

/// `GET /info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    /// Server build version, such as `0.2.0` on a release or `0.2.0+5.g4a33c89` on a commit
    /// after it. For people and logs; compare `api_version`, not this.
    pub server_version: String,
    /// API version; see [`crate::consts::API_VERSION`].
    pub api_version: String,
    /// Names of the enabled modules.
    #[serde(default)]
    pub modules: Vec<String>,
    /// Single sign-on, when the server offers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sso: Option<SsoInfo>,
    /// How large files are cut into chunks, when the server stores chunked files
    /// (ARCHITECTURE §3.5). Absent on servers before 0.3: send files whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunking: Option<ChunkingInfo>,
    /// Set when the answer comes from a caching proxy (T154) in front of the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyInfo>,
}

/// What a caching proxy adds to the upstream server's `GET /info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyInfo {
    /// The server the proxy fetches from.
    pub upstream: String,
    /// The proxy's version.
    pub version: String,
}

/// The chunking parameters `GET /info` reports. A client that cuts files the same way
/// shares chunks with other clients' uploads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkingInfo {
    /// Name of the scheme, `fastcdc-2020-lode-v1`.
    pub algorithm: String,
    /// Smallest chunk but the last, in bytes.
    pub min: u32,
    /// Average chunk size.
    pub avg: u32,
    /// Largest chunk.
    pub max: u32,
    /// Files smaller than this are sent whole.
    pub threshold: u64,
}

impl ChunkingInfo {
    /// The parameters of this build.
    pub fn current() -> Self {
        Self {
            algorithm: lode_core::CHUNK_ALGORITHM.to_owned(),
            min: lode_core::CHUNK_MIN,
            avg: lode_core::CHUNK_AVG,
            max: lode_core::CHUNK_MAX,
            threshold: lode_core::CHUNK_THRESHOLD,
        }
    }

    /// Whether this build cuts files the way the server describes.
    pub fn is_current(&self) -> bool {
        *self == Self::current()
    }
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

/// `POST /auth/password`: a user changes their own password. Also the only use of a password
/// that must be changed (`details.reason` `password_change_required` on login).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangePasswordRequest {
    /// User name.
    pub name: String,
    /// The current password.
    pub password: String,
    /// The new password.
    pub new_password: String,
}

/// Answer to a password change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangePasswordResponse {
    /// Whose password changed.
    pub name: String,
}

/// `details.reason` of an `unauthenticated` login whose password must be changed first.
pub const REASON_PASSWORD_CHANGE_REQUIRED: &str = "password_change_required";
/// `details.reason` of an `unauthenticated` login whose must-change password ran out.
pub const REASON_PASSWORD_EXPIRED: &str = "password_expired";

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
