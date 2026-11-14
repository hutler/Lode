//! `/auth`, `/tokens` and the principal and group administration endpoints.

use std::net::IpAddr;

use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use lode_core::ErrorCode;
use lode_meta::{
    MetaError, NewPrincipal, PrincipalId, PrincipalKind, PrincipalRow, Reads, TokenRow,
};
use lode_proto::admin::{
    CreateGroupRequest, CreatePrincipalRequest, GroupList, GroupView, PatchPrincipalRequest,
    PrincipalList,
};
use lode_proto::auth::{
    CreateTokenRequest, CreateTokenResponse, LoginRequest, LoginResponse, PrincipalView, TokenList,
    TokenView, WhoAmI,
};
use lode_proto::consts::paths;

use super::caller::Caller;
use super::password::{hash_password, verify_password, MIN_PASSWORD_LEN};
use super::token::{generate_token, hash_token};
use super::{ms_to_time, time_to_ms};
use crate::config::PasswordLogin;
use crate::error::{ApiError, ApiResult};
use crate::extract::{ApiJson, ApiPath as Path};
use crate::server::ClientInfo;
use crate::state::AppState;

/// The authentication, token and principal/group administration routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(paths::LOGIN, post(login))
        .route(paths::WHOAMI, get(whoami))
        .route(paths::TOKENS, get(list_tokens).post(create_token))
        .route(paths::TOKEN, axum::routing::delete(delete_token))
        .route(
            paths::ADMIN_PRINCIPALS,
            get(list_principals).post(create_principal),
        )
        .route(paths::ADMIN_PRINCIPAL, patch(patch_principal))
        .route(
            paths::ADMIN_PRINCIPAL_TOKENS,
            get(list_tokens_for).post(create_token_for),
        )
        .route(
            paths::ADMIN_PRINCIPAL_TOKEN,
            axum::routing::delete(delete_token_for),
        )
        .route(paths::ADMIN_GROUPS, get(list_groups).post(create_group))
        .route(
            paths::ADMIN_GROUP_MEMBER,
            put(add_member).delete(remove_member),
        )
        .merge(super::sso::routes())
}

const MAX_NAME_LEN: usize = 64;
pub(super) const MAX_LABEL_LEN: usize = 100;
/// Longest single sign-on identity, such as an e-mail address.
const MAX_SSO_LEN: usize = 254;

/// Principal and group names: 1 to 64 characters of ASCII letters, digits, `.`, `_`, `-`,
/// starting with a letter or digit.
fn check_name(what: &str, name: &str) -> ApiResult<()> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(ApiError::invalid_request(format!(
            "invalid {what} name: use 1 to {MAX_NAME_LEN} letters, digits, '.', '_' or '-', \
             starting with a letter or digit"
        )))
    }
}

fn check_password(password: &str) -> ApiResult<()> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(ApiError::invalid_request(format!(
            "password must have at least {MIN_PASSWORD_LEN} characters"
        )));
    }
    Ok(())
}

async fn hash_password_blocking(password: String) -> ApiResult<String> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(|_| ApiError::internal())
}

/// A single sign-on identity as an administrator gives it: trimmed and in lower case; `None`
/// for an empty one (which removes a link).
fn check_sso(sso: &str) -> ApiResult<Option<String>> {
    let s = sso.trim();
    if s.is_empty() {
        return Ok(None);
    }
    if s.len() > MAX_SSO_LEN || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ApiError::invalid_request(format!(
            "invalid single sign-on identity: use at most {MAX_SSO_LEN} characters without spaces"
        )));
    }
    Ok(Some(s.to_lowercase()))
}

pub(super) fn principal_view(p: &PrincipalRow, owner: Option<String>) -> PrincipalView {
    PrincipalView {
        name: p.name.clone(),
        kind: match p.kind {
            PrincipalKind::User => lode_proto::PrincipalKind::User,
            PrincipalKind::Agent => lode_proto::PrincipalKind::Agent,
        },
        display_name: p.display_name.clone(),
        owner,
        disabled: p.disabled,
        sso: p.sso_id.clone(),
    }
}

fn owner_name(tx: &impl Reads, p: &PrincipalRow) -> lode_meta::Result<Option<String>> {
    match p.owner_id {
        Some(id) => Ok(tx.principal(id)?.map(|o| o.name)),
        None => Ok(None),
    }
}

fn token_view(t: &TokenRow) -> TokenView {
    TokenView {
        id: t.id.0,
        name: t.name.clone(),
        scopes: t.scopes.clone(),
        created_at: ms_to_time(t.created_at),
        expires_at: t.expires_at.map(ms_to_time),
        last_used_at: t.last_used_at.map(ms_to_time),
    }
}

fn client_ip(info: Option<&ConnectInfo<ClientInfo>>) -> Option<IpAddr> {
    info.map(|c| c.0 .0.ip())
}

/// Optional client address: present when the server runs with connection info.
struct ClientAddr(Option<IpAddr>);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for ClientAddr {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(client_ip(
            parts.extensions.get::<ConnectInfo<ClientInfo>>(),
        )))
    }
}

/// Creates and stores a token, returning its secret once.
pub(super) async fn issue_token(
    state: &AppState,
    principal: PrincipalId,
    name: String,
    scopes: Vec<String>,
    expires_at: Option<i64>,
) -> ApiResult<(String, TokenRow)> {
    let token = generate_token(&mut rand::rngs::OsRng);
    let hash = hash_token(&token);
    let now = state.clock().now_ms();
    let row = state
        .meta()
        .write(move |tx| {
            let id = tx.create_token(principal, &name, &hash, &scopes, expires_at, now)?;
            Ok(TokenRow {
                id,
                principal_id: principal,
                name,
                hash: hash.to_vec(),
                scopes,
                expires_at,
                last_used_at: None,
                created_at: now,
            })
        })
        .await?;
    Ok((token, row))
}

async fn login(
    State(state): State<AppState>,
    ClientAddr(addr): ClientAddr,
    ApiJson(req): ApiJson<LoginRequest>,
) -> ApiResult<Json<LoginResponse>> {
    let now = state.clock().now_ms();
    let limiter = state.login_limiter();
    if let Some(ms) = limiter.locked_for(&req.name, addr, now) {
        return Err(ApiError::new(
            ErrorCode::Unauthenticated,
            "too many failed sign-in attempts; try again later",
        )
        .with_details(serde_json::json!({ "retry_after_seconds": (ms + 999) / 1000 })));
    }
    let name = req.name.clone();
    let principal = state
        .meta()
        .read(move |tx| tx.principal_by_name(&name))
        .await?;
    // Agents and disabled accounts cannot sign in with a password; they still pay for one
    // hash so the answer does not reveal which case applied.
    let stored = principal
        .as_ref()
        .filter(|p| p.kind == PrincipalKind::User && !p.disabled)
        .and_then(|p| p.password_hash.clone());
    let password = req.password;
    let ok = tokio::task::spawn_blocking(move || verify_password(&password, stored.as_deref()))
        .await
        .map_err(|_| ApiError::internal())?;
    let principal = match (ok, principal) {
        (true, Some(p)) => p,
        _ => {
            limiter.record_failure(&req.name, addr, now);
            return Err(ApiError::new(
                ErrorCode::Unauthenticated,
                "wrong name or password",
            ));
        }
    };
    limiter.record_success(&req.name);
    if state
        .config()
        .oidc
        .as_ref()
        .is_some_and(|o| o.password_login == PasswordLogin::Admins)
        && !would_be_server_admin(&state, &principal).await?
    {
        return Err(ApiError::new(
            ErrorCode::Unauthenticated,
            "password sign-in is turned off on this server except for server administrators; \
             sign in with single sign-on (lode login URL --sso)",
        ));
    }
    let label = req
        .token_name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "login".to_owned());
    if label.len() > MAX_LABEL_LEN {
        return Err(ApiError::invalid_request("token name is too long"));
    }
    let scopes = state.scopes().all();
    let (token, _) = issue_token(&state, principal.id, label, scopes, None).await?;
    Ok(Json(LoginResponse {
        token,
        principal: principal_view(&principal, None),
    }))
}

/// Whether `principal`, signed in with every scope, would be a server administrator.
async fn would_be_server_admin(state: &AppState, principal: &PrincipalRow) -> ApiResult<bool> {
    let id = principal.id;
    let groups = state.meta().read(move |tx| tx.groups_of(id)).await?;
    let caller = Caller {
        principal: principal.clone(),
        scopes: state.scopes().all(),
        token_id: lode_meta::TokenId(0),
        groups,
        via: lode_proto::commit::Via::Api,
    };
    Ok(state.permissions().is_server_admin(&caller))
}

async fn whoami(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<WhoAmI>> {
    let p = caller.principal.clone();
    let owner = state.meta().read(move |tx| owner_name(tx, &p)).await?;
    Ok(Json(WhoAmI {
        principal: principal_view(&caller.principal, owner),
        scopes: caller.scopes.clone(),
        token_id: caller.token_id.0,
    }))
}

async fn list_tokens(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<TokenList>> {
    let id = caller.principal.id;
    let rows = state.meta().read(move |tx| tx.tokens_of(id)).await?;
    Ok(Json(TokenList {
        tokens: rows.iter().map(token_view).collect(),
    }))
}

/// Validates a token request and returns its expiry and scopes. `agent` is the agent the
/// token is for, when it is minted for an agent by an administrator or the agent's owner.
///
/// A server administrator may grant any registered scope. The token `loded init` prints
/// carries only the core scopes, because modules are not known yet at that point; without this
/// nobody could ever hold a module scope such as `lock` except through `login`. An agent's
/// owner may grant `read` and the agent-default scopes only (SPEC §7.1); anyone else only
/// scopes the token making the request has. An agent token asked for without scopes gets
/// the agent defaults.
fn check_token_request(
    state: &AppState,
    caller: &Caller,
    req: &CreateTokenRequest,
    agent: Option<&PrincipalRow>,
) -> ApiResult<(Option<i64>, Vec<String>)> {
    if req.name.trim().is_empty() || req.name.len() > MAX_LABEL_LEN {
        return Err(ApiError::invalid_request(format!(
            "token name must have 1 to {MAX_LABEL_LEN} characters"
        )));
    }
    let scopes = match (req.scopes.is_empty(), agent) {
        (true, Some(_)) => state.scopes().agent_token_defaults(),
        (true, None) => {
            return Err(ApiError::invalid_request(
                "a token needs at least one scope",
            ))
        }
        (false, _) => req.scopes.clone(),
    };
    for s in &scopes {
        if !state.scopes().contains(s) {
            return Err(ApiError::invalid_request(format!("unknown scope `{s}`")));
        }
    }
    if !state.permissions().is_server_admin(caller) {
        let missing = match agent {
            Some(_) => {
                let allowed = state.scopes().agent_token_defaults();
                scopes
                    .iter()
                    .find(|s| !allowed.contains(s))
                    .map(|s| format!("only an administrator can grant `{s}` to an agent"))
            }
            None => scopes
                .iter()
                .find(|s| !caller.has_scope(s))
                .map(|s| format!("cannot grant `{s}`: the token making this request lacks it")),
        };
        if let Some(msg) = missing {
            return Err(ApiError::new(ErrorCode::PermissionDenied, msg));
        }
    }
    let expires = req.expires_at.map(time_to_ms);
    if expires.is_some_and(|e| e <= state.clock().now_ms()) {
        return Err(ApiError::invalid_request("expiry must be in the future"));
    }
    Ok((expires, scopes))
}

fn dedup(mut scopes: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    scopes.retain(|s| seen.insert(s.clone()));
    scopes
}

async fn create_token(
    State(state): State<AppState>,
    caller: Caller,
    ApiJson(req): ApiJson<CreateTokenRequest>,
) -> ApiResult<(StatusCode, Json<CreateTokenResponse>)> {
    let (expires, scopes) = check_token_request(&state, &caller, &req, None)?;
    let (token, row) = issue_token(
        &state,
        caller.principal.id,
        req.name,
        dedup(scopes),
        expires,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(CreateTokenResponse {
            token,
            info: token_view(&row),
        }),
    ))
}

async fn delete_token(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let owner = caller.principal.id;
    let deleted = state
        .meta()
        .write(move |tx| {
            if tx.tokens_of(owner)?.iter().any(|t| t.id.0 == id) {
                tx.delete_token(lode_meta::TokenId(id))?;
                Ok(true)
            } else {
                Ok(false)
            }
        })
        .await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("no such token"))
    }
}

async fn list_principals(
    State(state): State<AppState>,
    caller: Caller,
) -> ApiResult<Json<PrincipalList>> {
    state.permissions().require_server_admin(&caller)?;
    let list = state
        .meta()
        .read(|tx| {
            let rows = tx.principals()?;
            let by_id: std::collections::HashMap<_, _> =
                rows.iter().map(|p| (p.id, p.name.clone())).collect();
            Ok(rows
                .iter()
                .map(|p| principal_view(p, p.owner_id.and_then(|o| by_id.get(&o).cloned())))
                .collect())
        })
        .await?;
    Ok(Json(PrincipalList { principals: list }))
}

fn map_conflict(e: MetaError) -> ApiError {
    match e {
        MetaError::AlreadyExists(what) => {
            ApiError::new(ErrorCode::Conflict, format!("{what} already exists"))
        }
        MetaError::NotFound(what) => ApiError::not_found(format!("no such {what}")),
        other => other.into(),
    }
}

async fn create_principal(
    State(state): State<AppState>,
    caller: Caller,
    ApiJson(req): ApiJson<CreatePrincipalRequest>,
) -> ApiResult<(StatusCode, Json<PrincipalView>)> {
    state.permissions().require_server_admin(&caller)?;
    check_name("principal", &req.name)?;
    let kind = match req.kind {
        lode_proto::PrincipalKind::User => PrincipalKind::User,
        lode_proto::PrincipalKind::Agent => PrincipalKind::Agent,
    };
    match kind {
        PrincipalKind::User if req.owner.is_some() => {
            return Err(ApiError::invalid_request("a user has no owner"));
        }
        PrincipalKind::Agent if req.password.is_some() => {
            return Err(ApiError::invalid_request(
                "agents sign in with tokens, not passwords",
            ));
        }
        PrincipalKind::Agent if req.owner.is_none() => {
            return Err(ApiError::invalid_request("an agent needs an owner"));
        }
        PrincipalKind::Agent if req.sso.is_some() => {
            return Err(ApiError::invalid_request(
                "agents sign in with tokens, not single sign-on",
            ));
        }
        _ => {}
    }
    let sso = match &req.sso {
        Some(s) => check_sso(s)?,
        None => None,
    };
    let password_hash = match req.password {
        Some(pw) => {
            check_password(&pw)?;
            Some(hash_password_blocking(pw).await?)
        }
        None => None,
    };
    let now = state.clock().now_ms();
    let name = req.name;
    let display_name = req.display_name;
    let owner = req.owner;
    let owner_for_tx = owner.clone();
    let created = state
        .meta()
        .write(move |tx| {
            let owner_id = match &owner_for_tx {
                Some(o) => match tx.principal_by_name(o)? {
                    Some(p) if p.kind == PrincipalKind::User => Some(p.id),
                    _ => {
                        return Ok(Err(ApiError::invalid_request(format!(
                            "owner `{o}` is not an existing user"
                        ))))
                    }
                },
                None => None,
            };
            let id = tx.create_principal(
                &NewPrincipal {
                    kind,
                    name,
                    display_name,
                    owner_id,
                    password_hash,
                },
                now,
            )?;
            if let Some(s) = &sso {
                tx.set_principal_sso(id, Some(s))?;
            }
            let row = tx
                .principal(id)?
                .ok_or_else(|| MetaError::NotFound("principal".into()))?;
            Ok(Ok(row))
        })
        .await
        .map_err(map_conflict)??;
    Ok((StatusCode::CREATED, Json(principal_view(&created, owner))))
}

async fn patch_principal(
    State(state): State<AppState>,
    caller: Caller,
    Path(name): Path<String>,
    ApiJson(req): ApiJson<PatchPrincipalRequest>,
) -> ApiResult<Json<PrincipalView>> {
    state.permissions().require_server_admin(&caller)?;
    let password_hash = match req.password {
        Some(pw) => {
            check_password(&pw)?;
            Some(hash_password_blocking(pw).await?)
        }
        None => None,
    };
    let sso = match &req.sso {
        Some(s) => Some(check_sso(s)?),
        None => None,
    };
    let disabled = req.disabled;
    let result = state
        .meta()
        .write(move |tx| {
            let Some(p) = tx.principal_by_name(&name)? else {
                return Ok(Err(ApiError::not_found("no such principal")));
            };
            if password_hash.is_some() && p.kind == PrincipalKind::Agent {
                return Ok(Err(ApiError::invalid_request(
                    "agents sign in with tokens, not passwords",
                )));
            }
            if let Some(h) = &password_hash {
                tx.set_password_hash(p.id, Some(h))?;
            }
            if sso.as_ref().is_some_and(Option::is_some) && p.kind == PrincipalKind::Agent {
                return Ok(Err(ApiError::invalid_request(
                    "agents sign in with tokens, not single sign-on",
                )));
            }
            if let Some(s) = &sso {
                tx.set_principal_sso(p.id, s.as_deref())?;
            }
            if let Some(d) = disabled {
                tx.set_principal_disabled(p.id, d)?;
            }
            let row = tx
                .principal(p.id)?
                .ok_or_else(|| MetaError::NotFound("principal".into()))?;
            let owner = owner_name(tx, &row)?;
            Ok(Ok(principal_view(&row, owner)))
        })
        .await
        .map_err(map_conflict)??;
    Ok(Json(result))
}

/// The principal `name` when the caller may manage its tokens: a server administrator for
/// anyone, a user for the agents they own. Others get `not_found` or `permission_denied`.
async fn token_target(state: &AppState, caller: &Caller, name: String) -> ApiResult<PrincipalRow> {
    let target = state
        .meta()
        .read(move |tx| tx.principal_by_name(&name))
        .await?
        .ok_or_else(|| ApiError::not_found("no such principal"))?;
    let owns = target.kind == PrincipalKind::Agent && target.owner_id == Some(caller.principal.id);
    if !owns {
        state.permissions().require_server_admin(caller)?;
    }
    Ok(target)
}

async fn create_token_for(
    State(state): State<AppState>,
    caller: Caller,
    Path(name): Path<String>,
    ApiJson(req): ApiJson<CreateTokenRequest>,
) -> ApiResult<(StatusCode, Json<CreateTokenResponse>)> {
    let target = token_target(&state, &caller, name).await?;
    let agent = (target.kind == PrincipalKind::Agent).then_some(&target);
    let (expires, scopes) = check_token_request(&state, &caller, &req, agent)?;
    let (token, row) = issue_token(&state, target.id, req.name, dedup(scopes), expires).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreateTokenResponse {
            token,
            info: token_view(&row),
        }),
    ))
}

async fn list_tokens_for(
    State(state): State<AppState>,
    caller: Caller,
    Path(name): Path<String>,
) -> ApiResult<Json<TokenList>> {
    let target = token_target(&state, &caller, name).await?;
    let id = target.id;
    let rows = state.meta().read(move |tx| tx.tokens_of(id)).await?;
    Ok(Json(TokenList {
        tokens: rows.iter().map(token_view).collect(),
    }))
}

async fn delete_token_for(
    State(state): State<AppState>,
    caller: Caller,
    Path((name, id)): Path<(String, i64)>,
) -> ApiResult<StatusCode> {
    let target = token_target(&state, &caller, name).await?;
    let owner = target.id;
    let deleted = state
        .meta()
        .write(move |tx| {
            if tx.tokens_of(owner)?.iter().any(|t| t.id.0 == id) {
                tx.delete_token(lode_meta::TokenId(id))?;
                Ok(true)
            } else {
                Ok(false)
            }
        })
        .await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("no such token"))
    }
}

async fn list_groups(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<GroupList>> {
    state.permissions().require_server_admin(&caller)?;
    let groups = state
        .meta()
        .read(|tx| {
            let mut out = Vec::new();
            for g in tx.groups()? {
                let members = tx
                    .group_members(g.id)?
                    .into_iter()
                    .map(|p| p.name)
                    .collect();
                out.push(GroupView {
                    name: g.name,
                    members,
                });
            }
            Ok(out)
        })
        .await?;
    Ok(Json(GroupList { groups }))
}

async fn create_group(
    State(state): State<AppState>,
    caller: Caller,
    ApiJson(req): ApiJson<CreateGroupRequest>,
) -> ApiResult<(StatusCode, Json<GroupView>)> {
    state.permissions().require_server_admin(&caller)?;
    check_name("group", &req.name)?;
    let name = req.name.clone();
    state
        .meta()
        .write(move |tx| tx.create_group(&name).map(|_| ()))
        .await
        .map_err(map_conflict)?;
    Ok((
        StatusCode::CREATED,
        Json(GroupView {
            name: req.name,
            members: Vec::new(),
        }),
    ))
}

async fn change_member(
    state: AppState,
    caller: Caller,
    group: String,
    name: String,
    add: bool,
) -> ApiResult<StatusCode> {
    state.permissions().require_server_admin(&caller)?;
    state
        .meta()
        .write(move |tx| {
            let Some(g) = tx.group_by_name(&group)? else {
                return Ok(Err(ApiError::not_found("no such group")));
            };
            let Some(p) = tx.principal_by_name(&name)? else {
                return Ok(Err(ApiError::not_found("no such principal")));
            };
            if add {
                tx.add_group_member(g.id, p.id)?;
            } else {
                tx.remove_group_member(g.id, p.id)?;
            }
            Ok(Ok(()))
        })
        .await??;
    state.permissions().reload(state.meta()).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn add_member(
    State(state): State<AppState>,
    caller: Caller,
    Path((group, name)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    change_member(state, caller, group, name, true).await
}

async fn remove_member(
    State(state): State<AppState>,
    caller: Caller,
    Path((group, name)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    change_member(state, caller, group, name, false).await
}
