//! Shelf review on the web (T174): the open shelves of a depot, one shelf with its diff and
//! checks, and the two actions the API already offers, Submit and Delete.
//!
//! There is no approval state: submitting is the acceptance, and the checks module's submit
//! gate decides whether a submit may go in. Both actions are API requests with the session's
//! token, so `write` (submit) and ownership or depot admin (delete) are checked by the API,
//! and its refusals are shown as it words them, with each unmet check listed.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use lode_core::ChangeAction;
use lode_proto::checks::{CheckList, CheckState};
use lode_proto::commit::CommitResponse;
use lode_proto::consts::paths;
use lode_proto::error::ErrorBody;
use lode_proto::links::Link;
use lode_proto::shelves::{ShelfList, ShelfState, ShelfView, SubmitShelfRequest};
use lode_proto::PrincipalKind;

use super::api::path as api_path;
use super::format::at;
use super::html::{encode, Html};
use super::layout::{error_text, sentence};
use super::session::{redirect_with_note, ViewerPost};
use super::web::{depot_href, diff_block, failure, image_type, link_box, origin, revision_href};
use super::{html, Ctx, Nav, Page, Viewer};

/// How many earlier versions of a shelf get their checks listed.
const CHECK_VERSIONS: u64 = 5;

/// Whether the shelves module is on (the pages need its API).
pub(crate) fn enabled(ctx: &Ctx) -> bool {
    ctx.state().module_names().iter().any(|m| m == "shelves")
}

fn checks_enabled(ctx: &Ctx) -> bool {
    ctx.state().module_names().iter().any(|m| m == "checks")
}

pub(crate) fn routes() -> Router<Ctx> {
    use axum::routing::{get, post};
    Router::new()
        .route("/web/depots/{d}/shelves", get(list))
        .route("/web/depots/{d}/shelves/{id}", get(show))
        .route("/web/depots/{d}/shelves/{id}/raw", get(raw))
        .route("/web/depots/{d}/shelves/{id}/submit", post(submit))
        .route("/web/depots/{d}/shelves/{id}/delete", post(delete))
}

fn shelves_href(d: &str) -> String {
    format!("{}/shelves", depot_href(d))
}

fn shelf_href(d: &str, id: u64) -> String {
    format!("{}/shelves/{id}", depot_href(d))
}

fn off(ctx: &Ctx, viewer: &Viewer, d: &str) -> Response {
    let e = ErrorBody {
        code: "not_found".to_owned(),
        message: "the shelves module is not enabled on this server".to_owned(),
        details: serde_json::Value::Null,
    };
    failure(ctx, viewer, d, &e)
}

/// Who owns a shelf: a user, or an agent and the user it belongs to.
fn owner(s: &ShelfView) -> Html {
    match (&s.owner.kind, &s.agent_owner) {
        (PrincipalKind::Agent, Some(o)) => html!(
            "{} <span class=\"muted\">(agent of {})</span>",
            s.owner.name,
            o
        ),
        (PrincipalKind::Agent, None) => {
            html!("{} <span class=\"muted\">(agent)</span>", s.owner.name)
        }
        _ => html!("{}", s.owner.name),
    }
}

fn state_badge(state: CheckState) -> Html {
    match state {
        CheckState::Success => html!("<span class=\"badge ok\">success</span>"),
        CheckState::Pending => html!("<span class=\"badge\">pending</span>"),
        s => html!("<span class=\"badge bad\">{}</span>", s.as_str()),
    }
}

async fn checks_of(
    ctx: &Ctx,
    viewer: &Viewer,
    d: &str,
    id: u64,
    version: u64,
) -> Result<CheckList, ErrorBody> {
    let q = format!(
        "{}?target_kind=shelf&target_id={id}&target_version={version}",
        api_path(paths::CHECKS, &[d])
    );
    ctx.api(&viewer.0).get::<CheckList>(&q).await
}

/// A short summary of a shelf version's checks: "2 success, 1 failure".
fn checks_summary(list: &CheckList) -> Html {
    if list.checks.is_empty() {
        return html!("<span class=\"muted\">none reported</span>");
    }
    let mut counts: Vec<(CheckState, usize)> = Vec::new();
    for c in &list.checks {
        match counts.iter_mut().find(|(s, _)| *s == c.state) {
            Some((_, n)) => *n += 1,
            None => counts.push((c.state, 1)),
        }
    }
    counts
        .iter()
        .enumerate()
        .map(|(i, (s, n))| {
            let sep = if i == 0 { "" } else { " " };
            html!("{}{} {}", sep, *n as u64, state_badge(*s))
        })
        .collect()
}

/// The open shelves of a depot.
async fn list(viewer: Viewer, State(ctx): State<Ctx>, Path(d): Path<String>) -> Response {
    if !enabled(&ctx) {
        return off(&ctx, &viewer, &d);
    }
    let q = format!("{}?state=open", api_path(paths::SHELVES, &[&d]));
    let shelves = match ctx.api(&viewer.0).get::<ShelfList>(&q).await {
        Ok(l) => l.shelves,
        Err(e) => return failure(&ctx, &viewer, &d, &e),
    };
    let checks = checks_enabled(&ctx);
    let mut rows = Html::new();
    for s in &shelves {
        let summary = if checks {
            match checks_of(&ctx, &viewer, &d, s.id, s.version).await {
                Ok(l) => checks_summary(&l),
                Err(e) => error_text(&e),
            }
        } else {
            html!("<span class=\"muted\">—</span>")
        };
        rows.push(html!(
            "<tr><td class=\"num\" data-label=\"Shelf\"><a href=\"{}\">{}</a></td>\
             <td data-label=\"Description\">{}</td><td data-label=\"Owner\">{}</td>\
             <td data-label=\"Branch\">{}</td><td class=\"num\" data-label=\"Version\">{}</td>\
             <td class=\"num\" data-label=\"Base\">r{}</td><td data-label=\"Checks\">{}</td>\
             <td data-label=\"Updated\">{}</td></tr>",
            shelf_href(&d, s.id),
            s.id,
            s.description.lines().next().unwrap_or(""),
            owner(s),
            s.branch,
            s.version,
            s.base_rev,
            summary,
            at(s.updated_at)
        ));
    }
    let content = if shelves.is_empty() {
        html!("<p class=\"muted\">No shelves are waiting.</p>")
    } else {
        html!(
            "<table class=\"wide\"><thead><tr><th class=\"num\">Shelf</th><th>Description</th>\
             <th>Owner</th><th>Branch</th><th class=\"num\">Version</th><th class=\"num\">Base</th>\
             <th>Checks</th><th>Updated</th></tr></thead><tbody>{}</tbody></table>",
            rows
        )
    };
    let body = html!(
        "<h1>Shelves in {}</h1><p class=\"crumbs\"><a href=\"{}\">{}</a> / shelves</p>\
         <section>{}</section>",
        d,
        depot_href(&d),
        d,
        content
    );
    Page::new("Shelves", Nav::Depots).render_web(&ctx, &viewer, body)
}

/// Why a submit was refused, with each unmet check when the checks gate refused it.
fn refusal(e: &ErrorBody) -> Html {
    let mut out = error_text(e);
    if e.code == "checks_required" {
        if let Some(missing) = e.details["missing"].as_array() {
            let items: Html = missing
                .iter()
                .map(|m| {
                    let name = m["name"].as_str().unwrap_or("?");
                    let reporter = m["reporter"].as_str().unwrap_or("?");
                    let why = match m["reason"].as_str() {
                        Some("absent") => format!("not reported yet (by {reporter})"),
                        Some("not_success") => format!(
                            "{} (must be success, reported by {reporter})",
                            m["state"].as_str().unwrap_or("?")
                        ),
                        Some("wrong_reporter") => format!(
                            "reported by {} instead of {reporter}",
                            m["reported_by"].as_str().unwrap_or("?")
                        ),
                        Some(other) => other.to_owned(),
                        None => String::new(),
                    };
                    html!("<li><code>{}</code>: {}</li>", name, why)
                })
                .collect();
            out.push(html!(
                "<p>Checks that must pass first:</p><ul class=\"unmet\">{}</ul>",
                items
            ));
        }
    }
    out
}

/// A shelf's page, with `problem` (a refused action) above the forms.
async fn shelf_page(
    ctx: &Ctx,
    viewer: &Viewer,
    headers: &HeaderMap,
    d: &str,
    id: u64,
    problem: Option<(StatusCode, Html, Option<String>)>,
) -> Response {
    let q = format!(
        "{}?diff=true",
        api_path(paths::SHELF, &[d, &id.to_string()])
    );
    let s = match ctx.api(&viewer.0).get::<ShelfView>(&q).await {
        Ok(s) => s,
        Err(e) => return failure(ctx, viewer, d, &e),
    };
    let changes = s.changes.clone().unwrap_or_default();
    let diffs = s.diff.clone().unwrap_or_default();
    let base = depot_href(d);
    let href = shelf_href(d, id);
    let mut files = Html::new();
    for c in &changes {
        let path = c.path.as_str();
        let action = match c.action {
            ChangeAction::Add if c.copy_from.is_some() => "copied",
            ChangeAction::Add => "added",
            ChangeAction::Modify => "modified",
            ChangeAction::Delete => "deleted",
        };
        let content = if image_type(path).is_some() {
            let old = match c.action {
                ChangeAction::Add => html!("<p class=\"muted\">New file.</p>"),
                _ => html!(
                    "<img src=\"{}/raw?rev={}&amp;path={}\" alt=\"Before\"/>",
                    base,
                    s.base_rev,
                    encode(path)
                ),
            };
            let new = match c.action {
                ChangeAction::Delete => html!("<p class=\"muted\">Deleted.</p>"),
                _ => html!(
                    "<img src=\"{}/raw?path={}\" alt=\"After\"/>",
                    href,
                    encode(path)
                ),
            };
            html!(
                "<div class=\"side-by-side\"><figure>{}<figcaption>Base r{}</figcaption></figure>\
                 <figure>{}<figcaption>Shelf version {}</figcaption></figure></div>",
                old,
                s.base_rev,
                new,
                s.version
            )
        } else {
            match diffs.iter().find(|f| f.path == c.path) {
                Some(f) => match (&f.patch, &f.skipped) {
                    (Some(p), _) => diff_block(p),
                    (None, Some(why)) => {
                        let why = match why.as_str() {
                            "binary" => "Not text.",
                            "too_large" => "Too large to compare here.",
                            "symlink" => "A symbolic link.",
                            "missing" => "The content is not on the server yet.",
                            _ => "Not compared.",
                        };
                        html!("<p class=\"muted\">{}</p>", why)
                    }
                    (None, None) => Html::new(),
                },
                None => Html::new(),
            }
        };
        files.push(html!(
            "<section class=\"change\"><h2><code>{}</code> <span class=\"badge\">{}</span></h2>{}\
             </section>",
            path,
            action,
            content
        ));
    }
    if changes.is_empty() {
        files.push(html!(
            "<section><p class=\"muted\">This shelf changes no files.</p></section>"
        ));
    }

    let checks = if checks_enabled(ctx) {
        let first = s.version.saturating_sub(CHECK_VERSIONS - 1).max(1);
        let mut rows = Html::new();
        for v in (first..=s.version).rev() {
            match checks_of(ctx, viewer, d, id, v).await {
                Ok(l) => {
                    for c in &l.checks {
                        let name = match &c.url {
                            Some(u) if u.starts_with("https://") || u.starts_with("http://") => {
                                html!("<a href=\"{}\" rel=\"noreferrer\">{}</a>", u, c.name)
                            }
                            _ => html!("{}", c.name),
                        };
                        rows.push(html!(
                            "<tr><td class=\"num\" data-label=\"Version\">{}</td>\
                             <td data-label=\"Check\">{}</td><td data-label=\"State\">{}</td>\
                             <td data-label=\"Summary\">{}</td><td data-label=\"By\">{}</td>\
                             <td data-label=\"When\">{}</td></tr>",
                            v,
                            name,
                            state_badge(c.state),
                            c.summary.as_deref().unwrap_or(""),
                            c.reporter.name,
                            at(c.updated_at)
                        ));
                    }
                }
                Err(e) => rows.push(html!("<tr><td colspan=\"6\">{}</td></tr>", error_text(&e))),
            }
        }
        if rows.is_empty() {
            html!("<section><h2>Checks</h2><p class=\"muted\">No checks reported.</p></section>")
        } else {
            html!(
                "<section><h2>Checks</h2><table class=\"wide\"><thead><tr>\
                 <th class=\"num\">Version</th><th>Check</th><th>State</th><th>Summary</th>\
                 <th>By</th><th>When</th></tr></thead><tbody>{}</tbody></table></section>",
                rows
            )
        }
    } else {
        Html::new()
    };

    let (status, problem, message) = match problem {
        Some((st, p, m)) => (st, p, m),
        None => (StatusCode::OK, Html::new(), None),
    };
    let actions = match s.state {
        ShelfState::Open => html!(
            "<section id=\"submit\"><h2>Submit</h2>{}\
             <form class=\"stack wide-form\" method=\"post\" action=\"{}/submit\">{}\
             <input type=\"hidden\" name=\"expected_version\" value=\"{}\"/>\
             <label>Commit message<textarea name=\"message\" required=\"required\">{}</textarea></label>\
             <div class=\"actions\"><button type=\"submit\" class=\"primary\">Submit version {}</button>\
             </div></form>\
             <form class=\"spaced\" method=\"post\" action=\"{}/delete\" \
             data-confirm=\"Delete shelf {}? Its content stays on the server only until garbage \
             collection.\">{}<button type=\"submit\" class=\"danger\">Delete shelf</button></form>\
             </section>",
            problem,
            href,
            viewer.0.csrf_field(),
            s.version,
            message.unwrap_or_else(|| s.description.clone()),
            s.version,
            href,
            id,
            viewer.0.csrf_field()
        ),
        ShelfState::Submitted => match s.submitted_rev {
            Some(r) => html!(
                "<p class=\"note\">Submitted as <a href=\"{}\">r{}</a>.</p>",
                revision_href(d, r),
                r
            ),
            None => html!("<p class=\"note\">Submitted.</p>"),
        },
        ShelfState::Deleted => html!("<p class=\"note\">This shelf was deleted.</p>"),
    };
    let state = match s.state {
        ShelfState::Open => "open",
        ShelfState::Submitted => "submitted",
        ShelfState::Deleted => "deleted",
    };
    let merged: Html = s
        .merged_from
        .iter()
        .map(|m| {
            html!(
                "<dt>Merges</dt><dd>{} r{}–r{}</dd>",
                m.branch,
                m.from_rev,
                m.to_rev
            )
        })
        .collect();
    let link = origin(ctx, headers).map(|server| Link::Shelf {
        server,
        depot: d.to_owned(),
        id,
    });
    let body = html!(
        "<h1>Shelf {} <span class=\"muted\">in {}</span></h1>\
         <p class=\"crumbs\"><a href=\"{}\">{}</a> / <a href=\"{}\">shelves</a> / {}</p>{}\
         <dl class=\"facts card\"><dt>State</dt><dd>{}</dd><dt>Owner</dt><dd>{}</dd>\
         <dt>Branch</dt><dd>{}</dd><dt>Base</dt><dd><a href=\"{}\">r{}</a></dd>\
         <dt>Version</dt><dd>{}</dd>{}<dt>Updated</dt><dd>{}</dd></dl>\
         <section><h2>Description</h2><pre class=\"message\">{}</pre></section>{}{}{}",
        id,
        d,
        base,
        d,
        shelves_href(d),
        id,
        link_box(link),
        state,
        owner(&s),
        s.branch,
        revision_href(d, s.base_rev),
        s.base_rev,
        s.version,
        merged,
        at(s.updated_at),
        s.description,
        files,
        checks,
        actions
    );
    let mut r = Page::new(&format!("Shelf {id}"), Nav::Depots).render_web(ctx, viewer, body);
    *r.status_mut() = status;
    r
}

async fn show(
    viewer: Viewer,
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    Path((d, id)): Path<(String, u64)>,
) -> Response {
    if !enabled(&ctx) {
        return off(&ctx, &viewer, &d);
    }
    shelf_page(&ctx, &viewer, &headers, &d, id, None).await
}

/// One file of a shelf, for the side-by-side images: the shelf's new content, found through
/// the shelf so that only files of a shelf this user may see are served.
async fn raw(
    viewer: Viewer,
    State(ctx): State<Ctx>,
    Path((d, id)): Path<(String, u64)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if !enabled(&ctx) {
        return off(&ctx, &viewer, &d);
    }
    let api = ctx.api(&viewer.0);
    let s = match api
        .get::<ShelfView>(&api_path(paths::SHELF, &[&d, &id.to_string()]))
        .await
    {
        Ok(s) => s,
        Err(e) => return failure(&ctx, &viewer, &d, &e),
    };
    let path = q.get("path").map(String::as_str).unwrap_or("");
    let change = s
        .changes
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|c| c.path.as_str() == path);
    let Some(new_id) = change.and_then(|c| c.new_id) else {
        let e = ErrorBody {
            code: "not_found".to_owned(),
            message: format!("shelf {id} has no new content for {path}"),
            details: serde_json::Value::Null,
        };
        return failure(&ctx, &viewer, &d, &e);
    };
    let resp = match api
        .raw(&api_path(paths::BLOB, &[&d, &new_id.to_string()]))
        .await
    {
        Ok(r) => r,
        Err(e) => return failure(&ctx, &viewer, &d, &e),
    };
    let (parts, body) = resp.into_parts();
    let mut r = Response::new(body);
    let h = r.headers_mut();
    if let Some(len) = parts.headers.get(header::CONTENT_LENGTH) {
        h.insert(header::CONTENT_LENGTH, len.clone());
    }
    match image_type(path) {
        Some(mime) => {
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
            h.insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("inline"),
            );
        }
        None => {
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            h.insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment"),
            );
        }
    }
    r
}

async fn submit(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    Path((d, id)): Path<(String, u64)>,
    post: ViewerPost,
) -> Response {
    let viewer = &post.viewer;
    if !enabled(&ctx) {
        return off(&ctx, viewer, &d);
    }
    let message = post.fields.get("message").unwrap_or("").to_owned();
    let Ok(expected_version) = post.fields.text("expected_version").parse::<u64>() else {
        let p = html!("<p class=\"error\" role=\"alert\">The form named no shelf version.</p>");
        return shelf_page(
            &ctx,
            viewer,
            &headers,
            &d,
            id,
            Some((StatusCode::BAD_REQUEST, p, Some(message))),
        )
        .await;
    };
    let req = SubmitShelfRequest {
        expected_version,
        message: Some(message.clone()).filter(|m| !m.trim().is_empty()),
        force_flow: false,
    };
    let path = api_path(paths::SHELF_SUBMIT, &[&d, &id.to_string()]);
    match ctx
        .api(&viewer.0)
        .post::<_, CommitResponse>(&path, &req)
        .await
    {
        Ok(done) => redirect_with_note(
            &ctx,
            &revision_href(&d, done.rev),
            &format!("Shelf {id} submitted as r{}.", done.rev),
        ),
        Err(e) => {
            let status = match e.code.as_str() {
                "permission_denied" | "unauthenticated" => StatusCode::FORBIDDEN,
                "not_found" => StatusCode::NOT_FOUND,
                "invalid_request" => StatusCode::BAD_REQUEST,
                _ => StatusCode::CONFLICT,
            };
            shelf_page(
                &ctx,
                viewer,
                &headers,
                &d,
                id,
                Some((status, refusal(&e), Some(message))),
            )
            .await
        }
    }
}

async fn delete(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    Path((d, id)): Path<(String, u64)>,
    post: ViewerPost,
) -> Response {
    let viewer = &post.viewer;
    if !enabled(&ctx) {
        return off(&ctx, viewer, &d);
    }
    let path = api_path(paths::SHELF, &[&d, &id.to_string()]);
    match ctx.api(&viewer.0).delete(&path).await {
        Ok(()) => redirect_with_note(&ctx, &shelves_href(&d), &format!("Shelf {id} deleted.")),
        Err(e) => {
            let p = html!(
                "<p class=\"error\" role=\"alert\">{}</p>",
                sentence(&format!("not deleted: {}", e.message))
            );
            let status = if e.code == "permission_denied" {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::CONFLICT
            };
            shelf_page(&ctx, viewer, &headers, &d, id, Some((status, p, None))).await
        }
    }
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmet_checks_are_listed_with_their_reasons() {
        let e = ErrorBody {
            code: "checks_required".to_owned(),
            message: "shelf 1 version 2 has not passed the checks".to_owned(),
            details: serde_json::json!({ "missing": [
                { "name": "build", "reason": "absent", "reporter": "ci" },
                { "name": "test", "reason": "not_success", "state": "failure", "reporter": "ci" },
                { "name": "lint", "reason": "wrong_reporter", "reported_by": "ana", "reporter": "ci" }
            ]}),
        };
        let h = refusal(&e).into_string();
        assert!(
            h.contains("<code>build</code>: not reported yet (by ci)"),
            "{h}"
        );
        assert!(
            h.contains("failure (must be success, reported by ci)"),
            "{h}"
        );
        assert!(h.contains("reported by ana instead of ci"), "{h}");
    }
}
