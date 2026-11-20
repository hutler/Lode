//! Depots and their maintenance (T141): the list, one depot's branches, revisions, locks and
//! obliterated content, garbage collection, checks and obliterate.

use axum::extract::{Path, State};
use axum::response::Response;
use lode_meta::Reads;
use lode_proto::admin::{
    DeleteDepotRequest, DeleteDepotResponse, MaintenanceReport, ObliterateReport,
    ObliterateRequest, ObliterateTarget, ObliterationList, TrashList,
};
use lode_proto::consts::paths;
use lode_proto::depot::{BranchList, BranchState, CreateDepotRequest, DepotList};
use lode_proto::error::ErrorBody;
use lode_proto::locks::{LockList, UnlockRequest, UnlockResponse};
use lode_proto::read::RevisionList;

use super::api::path;
use super::format::{at, bytes, count};
use super::html::{encode, Html};
use super::layout::{error_text, sentence};
use super::session::{redirect_with_note, Post};
use super::{html, Admin, Ctx, Nav, Page};

/// The page of depot `d`.
pub(crate) fn depot_href(d: &str) -> String {
    format!("/admin/depots/{}", encode(d))
}

fn has_module(ctx: &Ctx, name: &str) -> bool {
    ctx.state().module_names().iter().any(|m| m == name)
}

/// The size of a depot's stored content, when it is kept in files.
async fn depot_size(ctx: &Ctx, name: &str) -> Option<u64> {
    let state = ctx.state();
    if state.config().storage.s3_in_use().is_some() {
        return None;
    }
    let lookup = name.to_owned();
    let id = state
        .meta()
        .read(move |tx| tx.depot_by_name(&lookup))
        .await
        .ok()
        .flatten()?
        .id;
    crate::sizes::depot_size(state, id).await
}

fn size_text(size: Option<u64>) -> Html {
    match size {
        Some(n) => html!("{}", bytes(n)),
        None => html!("<span class=\"muted\">—</span>"),
    }
}

fn create_form(admin: &Admin, name: &str) -> Html {
    html!(
        "<form class=\"row\" method=\"post\" action=\"/admin/depots\">{}\
         <label>New depot<input type=\"text\" name=\"name\" value=\"{}\" required=\"required\" \
         placeholder=\"game\"/></label><button type=\"submit\" class=\"primary\">Create</button></form>",
        admin.csrf_field(),
        name
    )
}

async fn list_page(ctx: &Ctx, admin: &Admin, error: Option<&ErrorBody>, name: &str) -> Response {
    let api = ctx.api(admin);
    let depots = match api.get::<DepotList>(paths::DEPOTS).await {
        Ok(d) => d.depots,
        Err(e) => {
            let body = html!("<h1>Depots</h1>{}", error_text(&e));
            return Page::new("Depots", Nav::Depots).render(ctx, Some(admin), body);
        }
    };
    let mut rows = Html::new();
    for d in &depots {
        let branches = api
            .get::<BranchList>(&format!("{}?all=true", path(paths::BRANCHES, &[&d.name])))
            .await
            .map(|b| b.branches)
            .unwrap_or_default();
        let last = api
            .get::<RevisionList>(&format!("{}?limit=1", path(paths::REVISIONS, &[&d.name])))
            .await
            .ok()
            .and_then(|r| r.revisions.into_iter().next());
        let (revs, last) = match last {
            Some(r) => (
                html!("{}", r.rev),
                html!("{} by {}", at(r.created_at), r.author.name),
            ),
            None => (html!("0"), html!("<span class=\"muted\">No commits</span>")),
        };
        let active = branches
            .iter()
            .filter(|b| b.state == BranchState::Active)
            .count();
        rows.push(html!(
            "<tr><td data-label=\"Depot\"><a href=\"{}\">{}</a></td>\
             <td class=\"num\" data-label=\"Revisions\">{}</td>\
             <td class=\"num\" data-label=\"Branches\">{}</td>\
             <td class=\"num\" data-label=\"Size\">{}</td><td data-label=\"Last commit\">{}</td></tr>",
            depot_href(&d.name),
            d.name,
            revs,
            active,
            size_text(depot_size(ctx, &d.name).await),
            last
        ));
    }
    let table = if depots.is_empty() {
        html!("<p class=\"muted\">No depots yet. Create the first one below.</p>")
    } else {
        html!(
            "<table class=\"wide\"><thead><tr><th>Depot</th><th class=\"num\">Revisions</th>\
             <th class=\"num\">Branches</th><th class=\"num\">Size</th><th>Last commit</th></tr>\
             </thead><tbody>{}</tbody></table>",
            rows
        )
    };
    let trash = trash_section(ctx, admin).await;
    let body = html!(
        "<h1>Depots</h1><section>{}</section><section><h2>Create a depot</h2>{}{}</section>{}",
        table,
        error.map(error_text),
        create_form(admin, name),
        trash
    );
    Page::new("Depots", Nav::Depots).render(ctx, Some(admin), body)
}

/// The trash: deleted depots, with restore and erase-now forms. Empty when nothing is there.
async fn trash_section(ctx: &Ctx, admin: &Admin) -> Html {
    let list = match ctx.api(admin).get::<TrashList>(paths::ADMIN_TRASH).await {
        Ok(l) => l.depots,
        Err(e) => {
            return html!(
                "<section><h2 id=\"trash\">Trash</h2>{}</section>",
                error_text(&e)
            )
        }
    };
    if list.is_empty() {
        return Html::new();
    }
    let csrf = admin.csrf_field();
    let rows: Html = list
        .iter()
        .map(|d| {
            let enc = encode(&d.name);
            html!(
                "<tr><td data-label=\"Depot\">{}</td><td data-label=\"Deleted\">{}</td>\
                 <td data-label=\"Erased after\">{}</td><td><div class=\"actions\">\
                 <form method=\"post\" action=\"/admin/trash/{}/restore\">{}\
                 <button type=\"submit\">Restore</button></form>\
                 <form class=\"row\" method=\"post\" action=\"/admin/trash/{}/erase\">{}\
                 <label>Type <code>{}</code> to erase now<input type=\"text\" name=\"confirm_name\" \
                 required=\"required\" autocomplete=\"off\"/></label>\
                 <button type=\"submit\" class=\"danger\">Erase now</button></form></div></td></tr>",
                d.name,
                at(d.deleted_at),
                at(d.purge_after),
                enc.clone(),
                csrf.clone(),
                enc,
                csrf.clone(),
                d.name
            )
        })
        .collect();
    html!(
        "<section><h2 id=\"trash\">Trash</h2><p class=\"muted\">Deleted depots are kept here \
         for {} days and can be restored; then they are erased with their history and content.</p>\
         <table class=\"wide\"><thead><tr><th>Depot</th><th>Deleted</th><th>Erased after</th>\
         <th></th></tr></thead><tbody>{}</tbody></table></section>",
        crate::trash::TRASH_DAYS,
        rows
    )
}

pub(crate) async fn list(State(ctx): State<Ctx>, admin: Admin) -> Response {
    list_page(&ctx, &admin, None, "").await
}

pub(crate) async fn create(State(ctx): State<Ctx>, post: Post) -> Response {
    let name = post.fields.text("name");
    let req = CreateDepotRequest { name: name.clone() };
    match ctx.api(&post.admin).post_ok(paths::DEPOTS, &req).await {
        Ok(()) => redirect_with_note(&ctx, &depot_href(&name), &format!("Depot {name} created.")),
        Err(e) => list_page(&ctx, &post.admin, Some(&e), &name).await,
    }
}

/// The depot page, with an optional report or error at the top.
async fn depot_page(ctx: &Ctx, admin: &Admin, d: &str, top: Html) -> Response {
    let api = ctx.api(admin);
    let branches = match api
        .get::<BranchList>(&format!("{}?all=true", path(paths::BRANCHES, &[d])))
        .await
    {
        Ok(b) => b.branches,
        Err(e) => {
            let body = html!("<h1>{}</h1>{}", d, error_text(&e));
            let mut r = Page::new(d, Nav::Depots).render(ctx, Some(admin), body);
            if e.code == "not_found" {
                *r.status_mut() = axum::http::StatusCode::NOT_FOUND;
            }
            return r;
        }
    };
    let revisions = api
        .get::<RevisionList>(&format!("{}?limit=15", path(paths::REVISIONS, &[d])))
        .await;
    let href = depot_href(d);

    let branch_rows: Html = branches
        .iter()
        .map(|b| {
            let from = match &b.parent {
                Some(p) => html!("{} at r{}", p.branch, p.rev),
                None => html!("<span class=\"muted\">—</span>"),
            };
            let state = match b.state {
                BranchState::Active => html!("<span class=\"badge ok\">active</span>"),
                BranchState::Archived => html!("<span class=\"badge\">archived</span>"),
            };
            html!(
                "<tr><td data-label=\"Branch\">{}</td><td class=\"num\" data-label=\"Head\">r{}</td>\
                 <td data-label=\"Created from\">{}</td><td data-label=\"State\">{}</td></tr>",
                b.name,
                b.head_rev,
                from,
                state
            )
        })
        .collect();
    let rev_section = match revisions {
        Ok(list) if list.revisions.is_empty() => html!("<p class=\"muted\">No commits yet.</p>"),
        Ok(list) => {
            let rows: Html = list
                .revisions
                .iter()
                .map(|r| {
                    html!(
                        "<tr><td class=\"num\" data-label=\"Revision\">r{}</td>\
                         <td data-label=\"Branch\">{}</td><td data-label=\"Author\">{}</td>\
                         <td data-label=\"When\">{}</td><td data-label=\"Message\">{}</td></tr>",
                        r.rev,
                        r.branch,
                        r.author.name,
                        at(r.created_at),
                        r.message.lines().next().unwrap_or("")
                    )
                })
                .collect();
            html!(
                "<table class=\"wide\"><thead><tr><th class=\"num\">Revision</th><th>Branch</th>\
                 <th>Author</th><th>When</th><th>Message</th></tr></thead><tbody>{}</tbody></table>",
                rows
            )
        }
        Err(e) => error_text(&e),
    };

    let locks = if has_module(ctx, "locks") {
        let content = match api.get::<LockList>(&path(paths::LOCKS, &[d])).await {
            Ok(l) if l.locks.is_empty() => html!("<p class=\"muted\">No files are locked.</p>"),
            Ok(l) => {
                let rows: Html = l
                    .locks
                    .iter()
                    .map(|k| {
                        html!(
                            "<tr><td data-label=\"Path\"><code>{}</code></td>\
                             <td data-label=\"Branch\">{}</td><td data-label=\"Owner\">{}</td>\
                             <td data-label=\"Workspace\">{}</td><td data-label=\"Since\">{}</td>\
                             <td data-label=\"Comment\">{}</td><td>\
                             <form method=\"post\" action=\"{}/locks/break\" data-confirm=\"Break {}'s lock on {}?\">{}\
                             <input type=\"hidden\" name=\"branch\" value=\"{}\"/>\
                             <input type=\"hidden\" name=\"path\" value=\"{}\"/>\
                             <button type=\"submit\" class=\"danger\">Break</button></form></td></tr>",
                            k.path.as_str(),
                            k.branch,
                            k.owner.name,
                            k.workspace,
                            at(k.created_at),
                            k.comment.as_deref().unwrap_or(""),
                            href,
                            k.owner.name,
                            k.path.as_str(),
                            admin.csrf_field(),
                            k.branch,
                            k.path.as_str()
                        )
                    })
                    .collect();
                html!(
                    "<table class=\"wide\"><thead><tr><th>Path</th><th>Branch</th><th>Owner</th>\
                     <th>Workspace</th><th>Since</th><th>Comment</th><th></th></tr></thead>\
                     <tbody>{}</tbody></table>",
                    rows
                )
            }
            Err(e) => error_text(&e),
        };
        html!("<section><h2 id=\"locks\">Locks</h2>{}</section>", content)
    } else {
        Html::new()
    };

    let obliterated = match api
        .get::<ObliterationList>(&path(paths::ADMIN_OBLITERATED, &[d]))
        .await
    {
        Ok(l) if l.obliterated.is_empty() => {
            html!("<p class=\"muted\">Nothing was obliterated.</p>")
        }
        Ok(l) => {
            let rows: Html = l
                .obliterated
                .iter()
                .map(|o| {
                    html!(
                        "<tr><td data-label=\"Path\"><code>{}</code></td>\
                         <td data-label=\"Content\"><code>{}</code></td>\
                         <td class=\"num\" data-label=\"Size\">{}</td><td data-label=\"Reason\">{}</td>\
                         <td data-label=\"By\">{}</td><td data-label=\"When\">{}</td></tr>",
                        o.path,
                        short(&o.id.to_string()),
                        bytes(o.size),
                        o.reason,
                        o.by,
                        at(o.at)
                    )
                })
                .collect();
            html!(
                "<table class=\"wide\"><thead><tr><th>Path</th><th>Content</th>\
                 <th class=\"num\">Size</th><th>Reason</th><th>By</th><th>When</th></tr></thead>\
                 <tbody>{}</tbody></table>",
                rows
            )
        }
        Err(e) => error_text(&e),
    };

    let csrf = admin.csrf_field();
    let body = html!(
        "<p class=\"muted\"><a href=\"/admin/depots\">Depots</a> /</p><h1>{}</h1>{}\
         <dl class=\"facts card\"><dt>Revisions</dt><dd>{}</dd><dt>Stored content</dt><dd>{}</dd></dl>\
         <section><h2 id=\"branches\">Branches</h2><table class=\"wide\"><thead><tr><th>Branch</th>\
         <th class=\"num\">Head</th><th>Created from</th><th>State</th></tr></thead><tbody>{}</tbody>\
         </table></section>\
         <section><h2 id=\"revisions\">Recent revisions</h2>{}</section>{}\
         <section><h2 id=\"maintenance\">Maintenance</h2>\
         <div class=\"actions\">\
         <form method=\"post\" action=\"{}/gc\">{}<input type=\"hidden\" name=\"dry_run\" value=\"1\"/>\
         <button type=\"submit\">Garbage collection: dry run</button></form>\
         <form class=\"row\" method=\"post\" action=\"{}/fsck\">{}\
         <label class=\"check\"><input type=\"checkbox\" name=\"full\" value=\"1\"/>Read and re-hash every object</label>\
         <button type=\"submit\">Check the depot</button></form></div>\
         <p class=\"muted\">A dry run shows what garbage collection would remove; nothing changes until \
         you run it from the report.</p></section>\
         <section><h2 id=\"obliterate\">Obliterate</h2>\
         <p class=\"muted\">Removes content for good: every version of a file or folder, or the version \
         one revision had (<code>path@rev</code>). Start with a dry run, which lists every use.</p>\
         <form class=\"stack\" method=\"post\" action=\"{}/obliterate\">{}\
         <label>Paths, one per line<textarea name=\"targets\" required=\"required\" \
         placeholder=\"Content/secret.txt&#10;Builds/old@42\"></textarea></label>\
         <label>Reason<input type=\"text\" name=\"reason\" placeholder=\"Leaked credentials\"/></label>\
         <button type=\"submit\">Dry run</button></form></section>\
         <section><h2 id=\"obliterated\">Obliterated content</h2>{}</section>\
         <section><h2 id=\"delete\">Delete this depot</h2>\
         <p class=\"muted\">The depot goes to the trash: it disappears for everyone and is erased \
         after {} days, until then it can be restored from the Depots page. Erasing now removes \
         its history and content at once and cannot be undone.</p>\
         <form class=\"stack\" method=\"post\" action=\"{}/delete\">{}\
         <label>Type <code>{}</code> to confirm<input type=\"text\" name=\"confirm_name\" \
         required=\"required\" autocomplete=\"off\"/></label>\
         <label class=\"check\"><input type=\"checkbox\" name=\"force\" value=\"1\"/>\
         Erase now instead of moving to the trash</label>\
         <button type=\"submit\" class=\"danger\">Delete depot</button></form></section>",
        d,
        top,
        revisions_count(&branches),
        size_text(depot_size(ctx, d).await),
        branch_rows,
        rev_section,
        locks,
        href,
        csrf.clone(),
        href,
        csrf.clone(),
        href,
        csrf.clone(),
        obliterated,
        crate::trash::TRASH_DAYS,
        href,
        csrf,
        d
    );
    Page::new(d, Nav::Depots).render(ctx, Some(admin), body)
}

fn revisions_count(branches: &[lode_proto::depot::BranchView]) -> u64 {
    branches.iter().map(|b| b.head_rev).max().unwrap_or(0)
}

/// The first 12 characters of an object id.
fn short(id: &str) -> String {
    id.chars().take(12).collect()
}

pub(crate) async fn show(State(ctx): State<Ctx>, admin: Admin, Path(d): Path<String>) -> Response {
    depot_page(&ctx, &admin, &d, Html::new()).await
}

fn maintenance_report(title: &str, r: &MaintenanceReport, removed: &str) -> Html {
    let problems: Html = r.problems.iter().map(|p| html!("<li>{}</li>", p)).collect();
    let problems = if r.problems.is_empty() {
        html!("<p class=\"ok\">No problems found.</p>")
    } else {
        html!(
            "<p class=\"bad\">{}:</p><ul>{}</ul>",
            count(r.problems.len() as u64, "problem"),
            problems
        )
    };
    html!(
        "<section class=\"report\" id=\"report\"><h2>{}</h2><dl class=\"facts\">\
         <dt>Objects checked</dt><dd>{}</dd><dt>{}</dt><dd>{} ({})</dd></dl>{}</section>",
        title,
        r.objects_checked,
        removed,
        r.objects_affected,
        bytes(r.bytes_affected),
        problems
    )
}

pub(crate) async fn gc(State(ctx): State<Ctx>, Path(d): Path<String>, post: Post) -> Response {
    let dry_run = post.fields.flag("dry_run");
    let url = path(paths::ADMIN_GC, &[&d]);
    let url = if dry_run {
        format!("{url}?dry_run=true")
    } else {
        url
    };
    let top = match ctx
        .api(&post.admin)
        .post_empty::<MaintenanceReport>(&url)
        .await
    {
        Ok(r) if dry_run => {
            let run = if r.objects_affected == 0 {
                html!("<p>Nothing to remove.</p>")
            } else {
                html!(
                    "<form method=\"post\" action=\"{}/gc\" data-confirm=\"Remove {} from {}?\">{}\
                     <button type=\"submit\" class=\"danger\">Run garbage collection</button></form>",
                    depot_href(&d),
                    count(r.objects_affected, "unreferenced object"),
                    d,
                    post.admin.csrf_field()
                )
            };
            let mut h = maintenance_report("Garbage collection: dry run", &r, "Would remove");
            h.push(html!("<div class=\"card\">{}</div>", run));
            h
        }
        Ok(r) => maintenance_report("Garbage collection", &r, "Removed"),
        Err(e) => error_text(&e),
    };
    depot_page(&ctx, &post.admin, &d, top).await
}

pub(crate) async fn fsck(State(ctx): State<Ctx>, Path(d): Path<String>, post: Post) -> Response {
    let full = post.fields.flag("full");
    let url = path(paths::ADMIN_FSCK, &[&d]);
    let url = if full {
        format!("{url}?full=true")
    } else {
        url
    };
    let top = match ctx
        .api(&post.admin)
        .post_empty::<MaintenanceReport>(&url)
        .await
    {
        Ok(r) => maintenance_report(
            if full { "Full check" } else { "Check" },
            &r,
            "Damaged or missing",
        ),
        Err(e) => error_text(&e),
    };
    depot_page(&ctx, &post.admin, &d, top).await
}

/// Targets from the form's lines: `path` or `path@rev`.
fn targets(text: &str) -> Result<Vec<ObliterateTarget>, String> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (p, rev) = match line.rsplit_once('@') {
            Some((p, r)) if r.trim_start_matches('r').parse::<u64>().is_ok() => {
                (p, r.trim_start_matches('r').parse::<u64>().ok())
            }
            _ => (line, None),
        };
        let path = lode_core::DepotPath::parse(p.trim_start_matches('/'))
            .map_err(|e| format!("{line}: {e}"))?;
        out.push(ObliterateTarget { path, rev });
    }
    if out.is_empty() {
        return Err("name at least one path".into());
    }
    Ok(out)
}

pub(crate) async fn obliterate(
    State(ctx): State<Ctx>,
    Path(d): Path<String>,
    post: Post,
) -> Response {
    let text = post.fields.get("targets").unwrap_or("").to_owned();
    let reason = post.fields.text("reason");
    let confirmed = post.fields.flag("confirm");
    let targets = match targets(&text) {
        Ok(t) => t,
        Err(e) => {
            let top = html!("<p class=\"error\" role=\"alert\">{}</p>", sentence(&e));
            return depot_page(&ctx, &post.admin, &d, top).await;
        }
    };
    let req = ObliterateRequest {
        targets,
        reason: reason.clone(),
        dry_run: !confirmed,
    };
    let url = path(paths::ADMIN_OBLITERATE, &[&d]);
    let top = match ctx
        .api(&post.admin)
        .post::<_, ObliterateReport>(&url, &req)
        .await
    {
        Ok(r) => obliterate_report(&d, &post.admin, &r, &text, &reason),
        Err(e) => {
            let mut h = error_text(&e);
            if let Some(in_use) = e.details.get("in_use").and_then(|v| v.as_array()) {
                let items: Html = in_use
                    .iter()
                    .map(|u| {
                        html!(
                            "<li>{} holds <code>{}</code></li>",
                            u.get("branch").and_then(|v| v.as_str()).unwrap_or(""),
                            u.get("path").and_then(|v| v.as_str()).unwrap_or("")
                        )
                    })
                    .collect();
                h.push(html!("<ul>{}</ul>", items));
            }
            h
        }
    };
    depot_page(&ctx, &post.admin, &d, top).await
}

fn obliterate_report(
    d: &str,
    admin: &Admin,
    r: &ObliterateReport,
    text: &str,
    reason: &str,
) -> Html {
    let rows: Html = r
        .objects
        .iter()
        .map(|o| {
            let uses: Html = o
                .uses
                .iter()
                .map(|u| html!("<li>r{} on {}: <code>{}</code></li>", u.rev, u.branch, u.path.as_str()))
                .collect();
            html!(
                "<tr><td data-label=\"Content\"><code>{}</code></td>\
                 <td class=\"num\" data-label=\"Size\">{}</td><td data-label=\"Used by\"><ul>{}</ul></td></tr>",
                short(&o.id.to_string()),
                bytes(o.size),
                uses
            )
        })
        .collect();
    let table = if r.objects.is_empty() {
        html!("<p class=\"muted\">No stored content matches.</p>")
    } else {
        html!(
            "<table class=\"wide\"><thead><tr><th>Content</th><th class=\"num\">Size</th>\
             <th>Every use</th></tr></thead><tbody>{}</tbody></table>",
            rows
        )
    };
    if !r.dry_run {
        return html!(
            "<section class=\"report\" id=\"report\"><h2>Obliterated</h2><p class=\"ok\">{} \
             ({}) removed for good.</p>{}</section>",
            count(r.objects.len() as u64, "object"),
            bytes(r.bytes),
            table
        );
    }
    let next = if !r.in_use.is_empty() {
        let items: Html = r
            .in_use
            .iter()
            .map(|u| {
                html!(
                    "<li>{} holds <code>{}</code></li>",
                    u.branch,
                    u.path.as_str()
                )
            })
            .collect();
        html!(
            "<p class=\"error\">Branch heads still hold this content, so it cannot be obliterated. \
             Delete or replace the files there first, or archive the branches.</p><ul>{}</ul>",
            items
        )
    } else if r.objects.is_empty() {
        Html::new()
    } else {
        html!(
            "<form class=\"stack\" method=\"post\" action=\"{}/obliterate\" \
             data-confirm=\"Obliterate {} from {} for good? This cannot be undone.\">{}\
             <input type=\"hidden\" name=\"targets\" value=\"{}\"/>\
             <input type=\"hidden\" name=\"confirm\" value=\"1\"/>\
             <label>Reason (recorded with each object)<input type=\"text\" name=\"reason\" value=\"{}\" \
             required=\"required\"/></label>\
             <button type=\"submit\" class=\"danger\">Obliterate</button></form>",
            depot_href(d),
            count(r.objects.len() as u64, "object"),
            d,
            admin.csrf_field(),
            text,
            reason
        )
    };
    html!(
        "<section class=\"report\" id=\"report\"><h2>Obliterate: dry run</h2>\
         <p>{} ({}) would be removed.</p>{}{}</section>",
        count(r.objects.len() as u64, "object"),
        bytes(r.bytes),
        table,
        next
    )
}

async fn delete_request(
    ctx: &Ctx,
    post: &Post,
    d: &str,
    force: bool,
) -> Result<DeleteDepotResponse, Html> {
    let confirm = post.fields.text("confirm_name");
    if confirm != d {
        return Err(html!(
            "<p class=\"error\" role=\"alert\">Type the depot's name {} exactly to delete it.</p>",
            d
        ));
    }
    let req = DeleteDepotRequest { confirm, force };
    ctx.api(&post.admin)
        .delete_with::<_, DeleteDepotResponse>(&path(paths::ADMIN_DEPOT, &[d]), &req)
        .await
        .map_err(|e| error_text(&e))
}

fn deleted_note(r: &DeleteDepotResponse) -> String {
    if r.purged {
        format!("Depot {} erased.", r.name)
    } else {
        format!(
            "Depot {} moved to the trash. It can be restored for {} days.",
            r.name,
            crate::trash::TRASH_DAYS
        )
    }
}

pub(crate) async fn delete(State(ctx): State<Ctx>, Path(d): Path<String>, post: Post) -> Response {
    let force = post.fields.flag("force");
    match delete_request(&ctx, &post, &d, force).await {
        Ok(r) => redirect_with_note(&ctx, "/admin/depots", &deleted_note(&r)),
        Err(top) => depot_page(&ctx, &post.admin, &d, top).await,
    }
}

pub(crate) async fn erase(State(ctx): State<Ctx>, Path(d): Path<String>, post: Post) -> Response {
    match delete_request(&ctx, &post, &d, true).await {
        Ok(r) => redirect_with_note(&ctx, "/admin/depots#trash", &deleted_note(&r)),
        Err(top) => trash_error(&ctx, &post.admin, top).await,
    }
}

pub(crate) async fn restore(State(ctx): State<Ctx>, Path(d): Path<String>, post: Post) -> Response {
    let url = path(paths::ADMIN_DEPOT_RESTORE, &[&d]);
    match ctx.api(&post.admin).post_ok(&url, &()).await {
        Ok(()) => redirect_with_note(&ctx, &depot_href(&d), &format!("Depot {d} restored.")),
        Err(e) => trash_error(&ctx, &post.admin, error_text(&e)).await,
    }
}

/// The depots page with an error above the trash.
async fn trash_error(ctx: &Ctx, admin: &Admin, top: Html) -> Response {
    let body = html!(
        "<h1>Depots</h1>{}<p><a href=\"/admin/depots#trash\">Back to the trash</a></p>",
        top
    );
    Page::new("Depots", Nav::Depots).render(ctx, Some(admin), body)
}

pub(crate) async fn break_lock(
    State(ctx): State<Ctx>,
    Path(d): Path<String>,
    post: Post,
) -> Response {
    let branch = post.fields.text("branch");
    let p = post.fields.text("path");
    let parsed = match lode_core::DepotPath::parse(&p) {
        Ok(p) => p,
        Err(e) => {
            let top = html!(
                "<p class=\"error\" role=\"alert\">{}</p>",
                sentence(&e.to_string())
            );
            return depot_page(&ctx, &post.admin, &d, top).await;
        }
    };
    let req = UnlockRequest {
        branch,
        paths: vec![parsed],
        force: true,
    };
    let url = path(paths::LOCKS, &[&d]);
    match ctx
        .api(&post.admin)
        .delete_with::<_, UnlockResponse>(&url, &req)
        .await
    {
        Ok(_) => redirect_with_note(
            &ctx,
            &format!("{}#locks", depot_href(&d)),
            &format!("Lock on {p} broken."),
        ),
        Err(e) => depot_page(&ctx, &post.admin, &d, error_text(&e)).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_take_paths_and_revisions() {
        let t = targets("Content/a.txt\n\n  Builds/old@42 \nx@r7\nweird@name").unwrap_or_default();
        let shown: Vec<(String, Option<u64>)> = t
            .iter()
            .map(|t| (t.path.as_str().to_owned(), t.rev))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("Content/a.txt".into(), None),
                ("Builds/old".into(), Some(42)),
                ("x".into(), Some(7)),
                ("weird@name".into(), None),
            ]
        );
        assert!(targets(" \n").is_err());
    }
}
