//! The authenticated caller of a request.

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use lode_core::ErrorCode;
use lode_meta::{GroupId, PrincipalKind, PrincipalRow, Reads, TokenId};
use lode_proto::commit::Via;

use super::token::{ct_eq, hash_token, TOKEN_PREFIX};
use crate::error::ApiError;
use crate::state::AppState;

/// Name of the built-in administrator created by `loded init`.
pub const ADMIN_USER: &str = "admin";

/// `last_used_at` is written at most this often per token.
const TOUCH_INTERVAL_MS: i64 = 60_000;

/// Who is calling, established from the bearer token.
#[derive(Debug, Clone)]
pub struct Caller {
    /// The principal.
    pub principal: PrincipalRow,
    /// Scopes of the token that are registered on this server.
    pub scopes: Vec<String>,
    /// The token used.
    pub token_id: TokenId,
    /// Groups the principal belongs to, as of this request.
    pub groups: Vec<GroupId>,
    /// The endpoint the request came through; the MCP endpoint sets `Mcp`. Recorded in
    /// provenance.
    pub via: Via,
}

impl Caller {
    /// Whether the token carries `scope`.
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }

    /// Fails with `permission_denied` unless the token carries `scope`.
    pub fn require_scope(&self, scope: &str) -> Result<(), ApiError> {
        if self.has_scope(scope) {
            Ok(())
        } else {
            Err(ApiError::new(
                ErrorCode::PermissionDenied,
                format!("this token lacks the `{scope}` scope"),
            ))
        }
    }

    /// Fails with `permission_denied` unless the token carries at least one of `scopes`.
    pub fn require_any_scope(&self, scopes: &[&str]) -> Result<(), ApiError> {
        if scopes.iter().any(|s| self.has_scope(s)) {
            Ok(())
        } else {
            let list = scopes
                .iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(" or ");
            Err(ApiError::new(
                ErrorCode::PermissionDenied,
                format!("this token needs the {list} scope"),
            ))
        }
    }

    /// Whether this is the built-in administrator.
    pub fn is_builtin_admin(&self) -> bool {
        self.principal.kind == PrincipalKind::User && self.principal.name == ADMIN_USER
    }
}

fn unauthenticated(message: &str) -> ApiError {
    ApiError::new(ErrorCode::Unauthenticated, message)
}

impl FromRequestParts<AppState> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        if let Some(c) = parts.extensions.get::<Caller>() {
            return Ok(c.clone());
        }
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .ok_or_else(|| unauthenticated("missing bearer token"))?;
        let token = header
            .to_str()
            .ok()
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|t| t.starts_with(TOKEN_PREFIX))
            .ok_or_else(|| unauthenticated("malformed bearer token"))?;
        let caller = authenticate(state, token).await?;
        parts.extensions.insert(caller.clone());
        Ok(caller)
    }
}

/// Resolves a bearer token to a caller.
pub(crate) async fn authenticate(state: &AppState, token: &str) -> Result<Caller, ApiError> {
    let hash = hash_token(token);
    let now = state.clock().now_ms();
    let found = state
        .meta()
        .read(move |tx| {
            let Some(row) = tx.token_by_hash(&hash)? else {
                return Ok(None);
            };
            let Some(principal) = tx.principal(row.principal_id)? else {
                return Ok(None);
            };
            // An agent is disabled with its owner (SPEC §7.1).
            let owner_disabled = match principal.owner_id {
                Some(o) if principal.kind == lode_meta::PrincipalKind::Agent => {
                    tx.principal(o)?.map_or(true, |o| o.disabled)
                }
                _ => false,
            };
            let groups = tx.groups_of(principal.id)?;
            Ok(Some((row, principal, groups, owner_disabled)))
        })
        .await?;
    let Some((row, principal, groups, owner_disabled)) = found else {
        return Err(unauthenticated("invalid token"));
    };
    if !ct_eq(&row.hash, &hash) {
        return Err(unauthenticated("invalid token"));
    }
    if row.expires_at.is_some_and(|e| e <= now) {
        return Err(unauthenticated("token expired"));
    }
    if principal.disabled {
        return Err(unauthenticated("account disabled"));
    }
    if owner_disabled {
        return Err(unauthenticated("the agent's owner is disabled"));
    }
    let stale = match row.last_used_at {
        Some(last) => now - last >= TOUCH_INTERVAL_MS,
        None => true,
    };
    if stale {
        let id = row.id;
        state
            .meta()
            .write(move |tx| tx.touch_token(id, now))
            .await?;
    }
    Ok(Caller {
        scopes: state.scopes().effective(&row.scopes),
        principal,
        token_id: row.id,
        groups,
        via: Via::Api,
    })
}
