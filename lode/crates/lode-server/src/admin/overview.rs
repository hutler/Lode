//! The overview: how the server is doing.

use std::collections::HashMap;

use axum::extract::State;
use axum::response::Response;
use lode_meta::{PrincipalKind, Reads};

use super::format::{bytes, count, span, when};
use super::html::Html;
use super::{Admin, Ctx, Nav, Page};
use crate::config::{StorageKind, TlsConfig};
use crate::html;

/// Recent commits shown, across depots.
const RECENT: usize = 10;

/// What the overview counts and lists from the database.
struct Facts {
    depots: Vec<lode_meta::DepotRow>,
    users: usize,
    disabled_users: usize,
    agents: usize,
    tokens: usize,
    runs: Vec<lode_meta::MaintenanceRunRow>,
    recent: Vec<Recent>,
}

struct Recent {
    depot: String,
    rev: u64,
    branch: String,
    author: String,
    at: i64,
    message: String,
}

async fn facts(ctx: &Ctx) -> Result<Facts, lode_meta::MetaError> {
    let now = ctx.state().clock().now_ms();
    ctx.state()
        .meta()
        .read(move |tx| {
            let depots = tx.depots()?;
            let principals = tx.principals()?;
            let names: HashMap<_, _> = principals.iter().map(|p| (p.id, p.name.clone())).collect();
            let mut tokens = 0;
            for p in &principals {
                tokens += tx
                    .tokens_of(p.id)?
                    .iter()
                    .filter(|t| t.expires_at.is_none_or(|e| e > now))
                    .count();
            }
            let mut recent = Vec::new();
            for d in &depots {
                let branches: HashMap<_, _> = tx
                    .branches(d.id)?
                    .into_iter()
                    .map(|b| (b.id, b.name))
                    .collect();
                let from = d.next_rev.saturating_sub(RECENT as u64 + 1);
                for r in tx.revisions_after(d.id, from, RECENT as u32)? {
                    recent.push(Recent {
                        depot: d.name.clone(),
                        rev: r.num,
                        branch: branches.get(&r.branch_id).cloned().unwrap_or_default(),
                        author: names.get(&r.author_id).cloned().unwrap_or_default(),
                        at: r.created_at,
                        message: r.message,
                    });
                }
            }
            recent.sort_by_key(|r| std::cmp::Reverse((r.at, r.rev)));
            recent.truncate(RECENT);
            let users = principals.iter().filter(|p| p.kind == PrincipalKind::User);
            Ok(Facts {
                users: users.clone().count(),
                disabled_users: users.filter(|p| p.disabled).count(),
                agents: principals
                    .iter()
                    .filter(|p| p.kind == PrincipalKind::Agent)
                    .count(),
                tokens,
                runs: tx.maintenance_runs()?,
                recent,
                depots,
            })
        })
        .await
}

fn fact(term: &str, value: Html) -> Html {
    html!("<dt>{}</dt><dd>{}</dd>", term, value)
}

fn text(value: impl Into<String>) -> Html {
    html!("{}", value.into())
}

/// The TLS mode and when its certificate expires.
async fn tls(ctx: &Ctx, now: i64) -> (Html, Html) {
    let cfg = ctx.state().config();
    let (mode, cert) = match &cfg.tls {
        None => {
            return (
                html!("Off: plain HTTP. Put a TLS proxy in front, or set <code>[tls]</code>."),
                text("None"),
            )
        }
        Some(TlsConfig::Files { cert, .. }) => (text("Certificate files"), cert.clone()),
        Some(TlsConfig::Acme(a)) => (
            html!("Automatic certificate for <code>{}</code>", a.domain),
            crate::acme::cached_cert_path(a, &cfg.data_dir),
        ),
    };
    let expiry = match tokio::fs::read(&cert)
        .await
        .ok()
        .and_then(|pem| crate::acme::pem_expiry(&pem))
    {
        Some(secs) => {
            let at = secs.saturating_mul(1000);
            let left = at - now;
            let class = if left < 0 {
                "bad"
            } else if left < 14 * 86_400_000 {
                "warn"
            } else {
                "ok"
            };
            let rel = if left < 0 {
                "expired".to_owned()
            } else {
                format!("in {}", span(left))
            };
            html!("{} <span class=\"{}\">({})</span>", when(at), class, rel)
        }
        None => html!("<span class=\"muted\">Not known yet</span>"),
    };
    (mode, expiry)
}

async fn storage(ctx: &Ctx, now: i64) -> (Html, Html) {
    let cfg = ctx.state().config();
    match (&cfg.storage.kind, cfg.storage.s3_in_use()) {
        (StorageKind::S3, Some(s3)) => {
            let place = if s3.prefix.is_empty() {
                html!("S3 bucket <code>{}</code>", s3.bucket)
            } else {
                html!(
                    "S3 bucket <code>{}</code>, prefix <code>{}</code>",
                    s3.bucket,
                    s3.prefix
                )
            };
            (
                place,
                html!("<span class=\"muted\">Not measured for S3</span>"),
            )
        }
        _ => {
            let dir = cfg.data_dir.join(crate::server::DEPOTS_DIR);
            let size = match ctx.sizes.size(&dir, now).await {
                Some(n) => text(bytes(n)),
                None => html!("<span class=\"muted\">Cannot be read</span>"),
            };
            (
                html!("Files in <code>{}</code>", dir.display().to_string()),
                size,
            )
        }
    }
}

fn database_size(ctx: &Ctx) -> Html {
    let db = ctx
        .state()
        .config()
        .data_dir
        .join(crate::server::META_DB_FILE);
    let len = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.len()).ok();
    let mut wal = db.clone().into_os_string();
    wal.push("-wal");
    match len(&db) {
        Some(n) => text(bytes(n + len(std::path::Path::new(&wal)).unwrap_or(0))),
        None => html!("<span class=\"muted\">Cannot be read</span>"),
    }
}

async fn backups(ctx: &Ctx, now: i64) -> Html {
    let state = ctx.state();
    let Some(schedule) = state.config().backup.schedule() else {
        return html!(
            "<p class=\"muted\">Scheduled backups are off. Set <code>every</code> under \
             <code>[backup]</code> in <code>loded.toml</code> to turn them on, or run \
             <code>loded backup</code>.</p>"
        );
    };
    let dir = schedule.dir.clone();
    let newest = tokio::task::spawn_blocking(move || crate::maintenance::newest_backup(&dir))
        .await
        .ok()
        .flatten();
    let every_ms = i64::try_from(schedule.every.as_millis()).unwrap_or(i64::MAX);
    let mut out = Html::new();
    out.push(fact(
        "Schedule",
        html!(
            "Every {}, keeping {} in <code>{}</code>",
            span(every_ms),
            schedule.keep,
            schedule.dir.display().to_string()
        ),
    ));
    let newest_ms = newest.as_ref().map(|(t, _)| crate::auth::time_to_ms(*t));
    match state.last_backup() {
        Some(run) => {
            let result = match &run.outcome {
                Ok(n) => html!(
                    "<span class=\"ok\">Done</span>, {} of content copied",
                    bytes(*n)
                ),
                Err(e) => html!("<span class=\"bad\">Failed:</span> {}", e),
            };
            out.push(fact("Last backup", html!("{}: {}", when(run.at), result)));
        }
        None => match &newest {
            Some((_, path)) => {
                let size = match ctx.sizes.size(path, now).await {
                    Some(n) => html!(", {}", bytes(n)),
                    None => Html::new(),
                };
                out.push(fact(
                    "Last backup",
                    html!(
                        "{}: <span class=\"ok\">complete</span>{}",
                        when(newest_ms.unwrap_or(0)),
                        size
                    ),
                ));
            }
            None => out.push(fact("Last backup", text("None yet"))),
        },
    }
    let next = newest_ms
        .map_or(now, |t| t.saturating_add(every_ms))
        .max(now);
    let next = if next <= now {
        html!("Due now")
    } else {
        html!("{} (in {})", when(next), span(next - now))
    };
    out.push(fact("Next backup", next));
    let copy = match (&schedule.copy_to, state.last_backup_copy()) {
        (None, _) => html!(
            "<span class=\"muted\">Off. Add <code>[backup.copy_to]</code> to send each backup \
             to an S3-compatible bucket on another machine.</span>"
        ),
        (Some(c), None) => html!(
            "To <code>{}</code>, keeping {}. <span class=\"muted\">None since the server \
             started.</span>",
            c.s3.bucket,
            c.keep
        ),
        (Some(c), Some(run)) => {
            let result = match &run.outcome {
                Ok(n) => html!("<span class=\"ok\">Done</span>, {} sent", bytes(*n)),
                Err(e) => html!("<span class=\"bad\">Failed:</span> {}", e),
            };
            html!(
                "To <code>{}</code>, keeping {}. {}: {}",
                c.s3.bucket,
                c.keep,
                when(run.at),
                result
            )
        }
    };
    out.push(fact("Copy elsewhere", copy));
    html!("<dl class=\"facts\">{}</dl>", out)
}

fn maintenance(f: &Facts, now: i64) -> Html {
    if f.depots.is_empty() {
        return html!("<p class=\"muted\">No depots yet.</p>");
    }
    let run = |depot, kinds: &[&str]| {
        f.runs
            .iter()
            .filter(|r| r.depot_id == depot && kinds.contains(&r.kind.as_str()))
            .max_by_key(|r| r.at)
    };
    let ago = |at: i64| {
        html!(
            "{} <span class=\"muted\">({} ago)</span>",
            when(at),
            span(now - at)
        )
    };
    let rows: Html = f
        .depots
        .iter()
        .map(|d| {
            let gc = match run(d.id, &["gc"]) {
                Some(r) => html!(
                    "{}<br/>{}, {} removed",
                    ago(r.at),
                    count(r.objects_affected, "object"),
                    bytes(r.bytes_affected)
                ),
                None => html!("<span class=\"muted\">Never</span>"),
            };
            let check = match run(d.id, &["fsck", "fsck-full"]) {
                Some(r) => {
                    let full = if r.kind == "fsck-full" { " (full)" } else { "" };
                    let result = if r.problems == 0 {
                        html!("<span class=\"ok\">No problems</span>")
                    } else {
                        html!("<span class=\"bad\">{}</span>", count(r.problems, "problem"))
                    };
                    html!("{}{}<br/>{}", ago(r.at), full, result)
                }
                None => html!("<span class=\"muted\">Never</span>"),
            };
            html!(
                "<tr><td data-label=\"Depot\"><a href=\"/admin/depots/{}\">{}</a></td>\
                 <td data-label=\"Garbage collection\">{}</td><td data-label=\"Check\">{}</td></tr>",
                super::html::encode(&d.name),
                d.name,
                gc,
                check
            )
        })
        .collect();
    html!(
        "<table class=\"wide\"><thead><tr><th>Depot</th><th>Last garbage collection</th>\
         <th>Last check</th></tr></thead><tbody>{}</tbody></table>",
        rows
    )
}

fn recent(f: &Facts) -> Html {
    if f.recent.is_empty() {
        return html!("<p class=\"muted\">No commits yet.</p>");
    }
    let rows: Html = f
        .recent
        .iter()
        .map(|r| {
            let first = r.message.lines().next().unwrap_or("");
            html!(
                "<tr><td data-label=\"Depot\"><a href=\"/admin/depots/{}\">{}</a></td>\
                 <td class=\"num\" data-label=\"Revision\">r{}</td><td data-label=\"Branch\">{}</td>\
                 <td data-label=\"Author\">{}</td><td data-label=\"When\">{}</td>\
                 <td data-label=\"Message\">{}</td></tr>",
                super::html::encode(&r.depot),
                r.depot,
                r.rev,
                r.branch,
                r.author,
                when(r.at),
                first
            )
        })
        .collect();
    html!(
        "<table class=\"wide\"><thead><tr><th>Depot</th><th class=\"num\">Revision</th>\
         <th>Branch</th><th>Author</th><th>When</th><th>Message</th></tr></thead>\
         <tbody>{}</tbody></table>",
        rows
    )
}

pub(crate) async fn page(State(ctx): State<Ctx>, admin: Admin) -> Response {
    let state = ctx.state();
    let now = state.clock().now_ms();
    let f = match facts(&ctx).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, "admin overview: cannot read the database");
            let body = html!(
                "<h1>Overview</h1><p class=\"error\">The server cannot read its database. \
                 See the server log.</p>"
            );
            return Page::new("Overview", Nav::Overview).render(&ctx, Some(&admin), body);
        }
    };
    let (tls_mode, cert) = tls(&ctx, now).await;
    let (store, store_size) = storage(&ctx, now).await;
    let modules = if state.module_names().is_empty() {
        html!("<span class=\"muted\">None</span>")
    } else {
        state
            .module_names()
            .iter()
            .map(|m| html!("<span class=\"badge\">{}</span> ", m))
            .collect()
    };
    let mut server = Html::new();
    server.push(fact("Version", text(env!("CARGO_PKG_VERSION"))));
    server.push(fact(
        "Running for",
        html!(
            "{} <span class=\"muted\">(since {})</span>",
            span(now - state.started_at()),
            when(state.started_at())
        ),
    ));
    server.push(fact(
        "Listening on",
        html!("<code>{}</code>", state.listening()),
    ));
    server.push(fact("TLS", tls_mode));
    server.push(fact("Certificate expires", cert));
    server.push(fact("Storage", store));
    server.push(fact("Stored content", store_size));
    server.push(fact("Database", database_size(&ctx)));
    server.push(fact("Modules", modules));
    let disabled = if f.disabled_users > 0 {
        html!(
            " <span class=\"muted\">({} disabled)</span>",
            f.disabled_users
        )
    } else {
        Html::new()
    };
    let stat = |n: usize, label: &str, extra: Html| {
        html!(
            "<div class=\"stat\"><strong>{}</strong><span>{}{}</span></div>",
            n,
            label,
            extra
        )
    };
    let stats: Html = [
        stat(f.depots.len(), "depots", Html::new()),
        stat(f.users, "users", disabled),
        stat(f.agents, "agents", Html::new()),
        stat(f.tokens, "active tokens", Html::new()),
    ]
    .into_iter()
    .collect();
    let body = html!(
        "<h1>Overview</h1>\
         <div class=\"grid\"><section aria-labelledby=\"server\"><h2 id=\"server\">Server</h2>\
         <dl class=\"facts\">{}</dl></section>\
         <section aria-labelledby=\"counts\"><h2 id=\"counts\">In use</h2><div class=\"stats\">{}</div>\
         <h2 id=\"backups\" class=\"spaced\">Backups</h2>{}</section></div>\
         <section aria-labelledby=\"maint\"><h2 id=\"maint\">Maintenance</h2>{}</section>\
         <section aria-labelledby=\"recent\"><h2 id=\"recent\">Recent commits</h2>{}</section>",
        server,
        stats,
        backups(&ctx, now).await,
        maintenance(&f, now),
        recent(&f)
    );
    Page::new("Overview", Nav::Overview)
        .refresh(30)
        .render(&ctx, Some(&admin), body)
}
