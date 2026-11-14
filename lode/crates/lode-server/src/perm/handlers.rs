//! `/admin/acl`: list, add and remove access rules.

use std::collections::HashMap;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::{Json, Router};
use lode_core::PathGlobSet;
use lode_meta::{AclId, AclRow, MetaError, Reads};
use lode_proto::admin::{AccessCheck, AccessLevel, AclEffect, AclList, AclRule, AclSubject};
use lode_proto::consts::paths;

use crate::auth::Caller;
use crate::error::{ApiError, ApiResult};
use crate::extract::{ApiJson, ApiPath, ApiQuery};
use crate::perm::Level;
use crate::state::AppState;

/// The access rule administration routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(paths::ADMIN_ACL, get(list).post(add))
        .route(paths::ADMIN_ACL_CHECK, get(check))
        .route(paths::ADMIN_ACL_RULE, delete(remove))
}

struct Names {
    depots: HashMap<i64, String>,
    principals: HashMap<i64, String>,
    groups: HashMap<i64, String>,
}

fn load_names(tx: &impl Reads) -> lode_meta::Result<Names> {
    Ok(Names {
        depots: tx.depots()?.into_iter().map(|d| (d.id.0, d.name)).collect(),
        principals: tx
            .principals()?
            .into_iter()
            .map(|p| (p.id.0, p.name))
            .collect(),
        groups: tx.groups()?.into_iter().map(|g| (g.id.0, g.name)).collect(),
    })
}

fn to_view(r: &AclRow, n: &Names) -> AclRule {
    let missing = || "?".to_owned();
    AclRule {
        id: Some(r.id.0),
        depot: r
            .depot_id
            .map(|d| n.depots.get(&d.0).cloned().unwrap_or_else(missing)),
        subject: match r.subject {
            lode_meta::AclSubject::All => AclSubject::All,
            lode_meta::AclSubject::User(p) => {
                AclSubject::User(n.principals.get(&p.0).cloned().unwrap_or_else(missing))
            }
            lode_meta::AclSubject::Group(g) => {
                AclSubject::Group(n.groups.get(&g.0).cloned().unwrap_or_else(missing))
            }
        },
        branch: r.branch_glob.clone(),
        path: r.path_glob.clone(),
        effect: match r.effect {
            lode_meta::AclEffect::Allow => AclEffect::Allow,
            lode_meta::AclEffect::Deny => AclEffect::Deny,
        },
        level: match r.level {
            lode_meta::AccessLevel::Read => AccessLevel::Read,
            lode_meta::AccessLevel::Write => AccessLevel::Write,
            lode_meta::AccessLevel::Admin => AccessLevel::Admin,
        },
    }
}

async fn list(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<AclList>> {
    state.permissions().require_server_admin(&caller)?;
    let rules = state
        .meta()
        .read(|tx| {
            let names = load_names(tx)?;
            Ok(tx.acl_all()?.iter().map(|r| to_view(r, &names)).collect())
        })
        .await?;
    Ok(Json(AclList { rules }))
}

fn check_glob(what: &str, pattern: &str) -> ApiResult<()> {
    if pattern.is_empty() {
        return Err(ApiError::invalid_request(format!(
            "{what} pattern is empty"
        )));
    }
    PathGlobSet::new([pattern])
        .map(|_| ())
        .map_err(|e| ApiError::invalid_request(format!("{what} pattern: {e}")))
}

async fn add(
    State(state): State<AppState>,
    caller: Caller,
    ApiJson(rule): ApiJson<AclRule>,
) -> ApiResult<(StatusCode, Json<AclRule>)> {
    state.permissions().require_server_admin(&caller)?;
    check_glob("branch", &rule.branch)?;
    check_glob("path", &rule.path)?;
    let view = state
        .meta()
        .write(move |tx| {
            let depot_id = match &rule.depot {
                Some(name) => match tx.depot_by_name(name)? {
                    Some(d) => Some(d.id),
                    None => return Ok(Err(ApiError::not_found(format!("no depot `{name}`")))),
                },
                None => None,
            };
            let subject = match &rule.subject {
                AclSubject::All => lode_meta::AclSubject::All,
                AclSubject::User(name) => match tx.principal_by_name(name)? {
                    Some(p) => lode_meta::AclSubject::User(p.id),
                    None => return Ok(Err(ApiError::not_found(format!("no principal `{name}`")))),
                },
                AclSubject::Group(name) => match tx.group_by_name(name)? {
                    Some(g) => lode_meta::AclSubject::Group(g.id),
                    None => return Ok(Err(ApiError::not_found(format!("no group `{name}`")))),
                },
            };
            let row = AclRow {
                id: AclId(0),
                depot_id,
                subject,
                branch_glob: rule.branch.clone(),
                path_glob: rule.path.clone(),
                effect: match rule.effect {
                    AclEffect::Allow => lode_meta::AclEffect::Allow,
                    AclEffect::Deny => lode_meta::AclEffect::Deny,
                },
                level: match rule.level {
                    AccessLevel::Read => lode_meta::AccessLevel::Read,
                    AccessLevel::Write => lode_meta::AccessLevel::Write,
                    AccessLevel::Admin => lode_meta::AccessLevel::Admin,
                },
            };
            let id = tx.insert_acl(&row)?;
            Ok(Ok(AclRule {
                id: Some(id.0),
                ..rule
            }))
        })
        .await??;
    state.permissions().reload(state.meta()).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

async fn remove(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    state.permissions().require_server_admin(&caller)?;
    state
        .meta()
        .write(move |tx| tx.delete_acl(AclId(id)))
        .await
        .map_err(|e| match e {
            MetaError::NotFound(_) => ApiError::not_found("no such rule"),
            other => other.into(),
        })?;
    state.permissions().reload(state.meta()).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn check(
    State(state): State<AppState>,
    caller: Caller,
    q: ApiQuery,
) -> ApiResult<Json<AccessCheck>> {
    state.permissions().require_server_admin(&caller)?;
    let need = |k: &str| {
        q.get(k)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| ApiError::invalid_request(format!("`{k}` is required")))
    };
    let (name, depot) = (need("principal")?, need("depot")?);
    let branch = q
        .get("branch")
        .filter(|b| !b.is_empty())
        .unwrap_or("main")
        .to_owned();
    let path = q
        .path("path")?
        .ok_or_else(|| ApiError::invalid_request("`path` is required"))?;
    let (n, d) = (name.clone(), depot.clone());
    let found = state
        .meta()
        .read(move |tx| {
            let Some(p) = tx.principal_by_name(&n)? else {
                return Ok(Err(ApiError::not_found(format!("no principal `{n}`"))));
            };
            let Some(depot) = tx.depot_by_name(&d)? else {
                return Ok(Err(ApiError::not_found(format!("no depot `{d}`"))));
            };
            let groups = tx.groups_of(p.id)?;
            Ok(Ok((p, depot, groups)))
        })
        .await??;
    let (principal, depot_row, groups) = found;
    let every: Vec<String> = ["read", "write", "admin"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let subject = Caller {
        scopes: state.scopes().effective(&every),
        principal,
        token_id: lode_meta::TokenId(0),
        groups,
        via: lode_proto::commit::Via::Api,
    };
    let access = state.permissions().depot(&subject, depot_row.id);
    let explained = access.explain(&branch, &path);
    let level = access.level(&branch, &path);
    debug_assert_eq!(level, explained.level);
    Ok(Json(AccessCheck {
        principal: name,
        depot,
        branch,
        path,
        depot_admin: explained.depot_admin,
        matched: explained.matched,
        decided_by: explained.decided_by,
        level: match level {
            Level::None => None,
            Level::Read => Some(AccessLevel::Read),
            Level::Write => Some(AccessLevel::Write),
            Level::Admin => Some(AccessLevel::Admin),
        },
    }))
}
