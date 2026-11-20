//! The depot browser (T173): read-only pages for every signed-in user under `/web/`.
//!
//! A user sees what their token lets the API show: the depots they can read, the branches
//! and paths their access rules allow. Every page reads through the API router like the
//! admin pages do, so hidden paths stay hidden and obliterated content is reported as such.
//! Each page that shows a revision, a file or a depot offers the `lode://` link that opens
//! the same thing in Lode Desk or `lode`, with a copy button.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use lode_core::{ChangeAction, EntryKind};
use lode_proto::auth::TokenList;
use lode_proto::consts::{paths, HEADER_KIND};
use lode_proto::depot::{BranchList, BranchView, DepotList};
use lode_proto::error::ErrorBody;
use lode_proto::links::{self, Link};
use lode_proto::read::{ChangeView, RevisionList, RevisionView, TreeListing};
use serde::Deserialize;

use super::api::path as api_path;
use super::format::{at, bytes, count};
use super::html::{encode, Html};
use super::layout::error_text;
use super::session::{redirect_with_note, ViewerPost};
use super::signin::WEB_HOME;
use super::{html, Ctx, Nav, Page, Viewer};
use crate::auth::{change_own_password, ClientAddr};

/// The account page: own tokens and password.
pub(crate) const ACCOUNT: &str = "/web/account";
/// Largest file shown as text; larger files are offered as a download.
const MAX_TEXT: usize = 2 * 1024 * 1024;
/// Largest file content read for one side of a diff.
const MAX_DIFF_FILE: usize = 1024 * 1024;
/// Most files diffed on one revision page; the rest are listed without a diff.
const MAX_DIFFS: usize = 50;
/// Revisions per page of history.
const HISTORY_PAGE: u32 = 50;

/// The browser's routes, merged into the admin UI's router.
pub(crate) fn routes(r: axum::Router<Ctx>) -> axum::Router<Ctx> {
    r.route(
        "/web",
        axum::routing::get(|| async { Redirect::permanent(WEB_HOME) }),
    )
    .route(WEB_HOME, axum::routing::get(depots))
    .route(ACCOUNT, axum::routing::get(account))
    .route("/web/account/password", axum::routing::post(password))
    .route(
        "/web/account/tokens/{id}/revoke",
        axum::routing::post(revoke_token),
    )
    .route("/web/depots/{d}", axum::routing::get(tree))
    .route("/web/depots/{d}/file", axum::routing::get(file))
    .route("/web/depots/{d}/raw", axum::routing::get(raw))
    .route("/web/depots/{d}/history", axum::routing::get(history))
    .route("/web/depots/{d}/r/{n}", axum::routing::get(revision))
    .merge(super::web_shelves::routes())
    .route(
        "/web/{*rest}",
        axum::routing::get(not_found).post(not_found),
    )
}

/// The page of depot `d`.
pub(crate) fn depot_href(d: &str) -> String {
    format!("/web/depots/{}", encode(d))
}

pub(crate) fn revision_href(d: &str, n: u64) -> String {
    format!("{}/r/{n}", depot_href(d))
}

/// Where a page is: a branch, and a fixed revision or the branch's head.
#[derive(Debug, Default, Clone, Deserialize)]
pub(crate) struct At {
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    rev: Option<u64>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    before: Option<u64>,
    #[serde(default)]
    download: Option<String>,
}

impl At {
    fn from(q: Result<Query<At>, QueryRejection>) -> Self {
        let mut at = q.map(|Query(a)| a).unwrap_or_default();
        at.branch = at.branch.filter(|b| !b.is_empty());
        at.path = at
            .path
            .map(|p| p.trim_matches('/').to_owned())
            .filter(|p| !p.is_empty());
        at
    }

    /// A query string for `path` at this place, keeping a fixed revision.
    fn query(&self, branch: &str, path: Option<&str>) -> String {
        let mut q = format!("?branch={}", encode(branch));
        if let Some(r) = self.rev {
            q.push_str(&format!("&rev={r}"));
        }
        if let Some(p) = path {
            q.push_str(&format!("&path={}", encode(p)));
        }
        q
    }
}

/// The address this server is reached at, for `lode://` links: the request's host, over
/// https when the server speaks TLS or a proxy in front says so.
pub(crate) fn origin(ctx: &Ctx, headers: &HeaderMap) -> Option<String> {
    let host = headers.get(header::HOST)?.to_str().ok()?;
    let forwarded_https = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("https"));
    let scheme = if ctx.secure() || forwarded_https {
        "https"
    } else {
        "http"
    };
    Some(format!("{scheme}://{host}"))
}

/// The `lode://` link of `link` with a copy button and an "open" link, or nothing when the
/// address cannot be put in a link.
pub(crate) fn link_box(link: Option<Link>) -> Html {
    let Some(text) = link.and_then(|l| links::build(&l).ok()) else {
        return Html::new();
    };
    html!(
        "<div class=\"secret lode-link\"><code id=\"lode-link\">{}</code>\
         <button type=\"button\" data-copy=\"lode-link\">Copy link</button>\
         <a class=\"button\" href=\"{}\">Open in Lode Desk</a></div>",
        text,
        text
    )
}

/// An error page in the browser's frame: 404 for what does not exist or is hidden.
pub(crate) fn failure(ctx: &Ctx, viewer: &Viewer, title: &str, e: &ErrorBody) -> Response {
    let body = html!(
        "<h1>{}</h1>{}<p><a href=\"{}\">Back to the depots</a></p>",
        title,
        error_text(e),
        WEB_HOME
    );
    let mut r = Page::new(title, Nav::Depots).render_web(ctx, viewer, body);
    *r.status_mut() = match e.code.as_str() {
        "not_found" => StatusCode::NOT_FOUND,
        "permission_denied" | "unauthenticated" => StatusCode::FORBIDDEN,
        "invalid_request" | "invalid_path" => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    r
}

fn invalid(message: &str) -> ErrorBody {
    ErrorBody {
        code: "invalid_request".to_owned(),
        message: message.to_owned(),
        details: serde_json::Value::Null,
    }
}

async fn not_found(viewer: Viewer, State(ctx): State<Ctx>) -> Response {
    let e = ErrorBody {
        code: "not_found".to_owned(),
        message: "there is no such page".to_owned(),
        details: serde_json::Value::Null,
    };
    failure(&ctx, &viewer, "Not found", &e)
}

/// The depots this user can read.
async fn depots(viewer: Viewer, State(ctx): State<Ctx>) -> Response {
    let list = match ctx.api(&viewer.0).get::<DepotList>(paths::DEPOTS).await {
        Ok(l) => l.depots,
        Err(e) => return failure(&ctx, &viewer, "Depots", &e),
    };
    let content = if list.is_empty() {
        html!(
            "<p class=\"muted\">There are no depots you can read. Ask a server administrator \
             for access.</p>"
        )
    } else {
        let rows: Html = list
            .iter()
            .map(|d| {
                let last = match d.last_commit_at {
                    Some(t) => at(t),
                    None => html!("<span class=\"muted\">—</span>"),
                };
                html!(
                    "<tr><td data-label=\"Depot\"><a href=\"{}\">{}</a></td>\
                     <td data-label=\"Created\">{}</td><td data-label=\"Last commit\">{}</td></tr>",
                    depot_href(&d.name),
                    d.name,
                    at(d.created_at),
                    last
                )
            })
            .collect();
        html!(
            "<table class=\"wide\"><thead><tr><th>Depot</th><th>Created</th><th>Last commit</th>\
             </tr></thead><tbody>{}</tbody></table>",
            rows
        )
    };
    let body = html!("<h1>Depots</h1><section>{}</section>", content);
    Page::new("Depots", Nav::Depots).render_web(&ctx, &viewer, body)
}

/// The depot's branches, and the one this page is on with its revision.
struct Place {
    branches: Vec<BranchView>,
    branch: String,
    rev: u64,
}

async fn place(ctx: &Ctx, viewer: &Viewer, d: &str, at: &At) -> Result<Place, ErrorBody> {
    let api = ctx.api(&viewer.0);
    let branches = api
        .get::<BranchList>(&api_path(paths::BRANCHES, &[d]))
        .await?
        .branches;
    let current = match &at.branch {
        Some(b) => branches.iter().find(|x| &x.name == b),
        None => branches
            .iter()
            .find(|x| x.name == crate::depots::MAIN_BRANCH)
            .or_else(|| branches.first()),
    };
    let Some(current) = current else {
        return Err(ErrorBody {
            code: "not_found".to_owned(),
            message: match &at.branch {
                Some(b) => format!("no branch {b} in depot {d}"),
                None => format!("depot {d} has no branch you can read"),
            },
            details: serde_json::Value::Null,
        });
    };
    let rev = at.rev.unwrap_or(current.head_rev);
    Ok(Place {
        branch: current.name.clone(),
        rev,
        branches,
    })
}

/// The branch picker: a form that reloads the page on another branch (works without
/// JavaScript).
fn branch_picker(action: &str, place: &Place, path: Option<&str>) -> Html {
    let options: Html = place
        .branches
        .iter()
        .map(|b| {
            if b.name == place.branch {
                html!("<option selected=\"selected\">{}</option>", b.name)
            } else {
                html!("<option>{}</option>", b.name)
            }
        })
        .collect();
    let path = match path {
        Some(p) => html!("<input type=\"hidden\" name=\"path\" value=\"{}\"/>", p),
        None => Html::new(),
    };
    html!(
        "<form class=\"row\" method=\"get\" action=\"{}\"><label>Branch\
         <select name=\"branch\">{}</select></label>{}\
         <button type=\"submit\">Show</button></form>",
        action,
        options,
        path
    )
}

/// The path as links to each directory above it.
fn breadcrumbs(d: &str, at: &At, branch: &str, path: Option<&str>) -> Html {
    let mut out = html!(
        "<a href=\"{}{}\">{}</a>",
        depot_href(d),
        at.query(branch, None),
        d
    );
    if let Some(p) = path {
        let parts: Vec<&str> = p.split('/').collect();
        for i in 0..parts.len() {
            let sub = parts[..=i].join("/");
            if i + 1 == parts.len() {
                out.push(html!(" / <strong>{}</strong>", parts[i]));
            } else {
                out.push(html!(
                    " / <a href=\"{}{}\">{}</a>",
                    depot_href(d),
                    at.query(branch, Some(&sub)),
                    parts[i]
                ));
            }
        }
    }
    html!("<p class=\"crumbs\">{}</p>", out)
}

fn revision_rows(d: &str, list: &[RevisionView]) -> Html {
    list.iter()
        .map(|r| {
            html!(
                "<tr><td class=\"num\" data-label=\"Revision\"><a href=\"{}\">r{}</a></td>\
                 <td data-label=\"Branch\">{}</td><td data-label=\"Author\">{}</td>\
                 <td data-label=\"When\">{}</td><td data-label=\"Message\">{}</td></tr>",
                revision_href(d, r.rev),
                r.rev,
                r.branch,
                r.author.name,
                at(r.created_at),
                r.message.lines().next().unwrap_or("")
            )
        })
        .collect()
}

fn revision_table(d: &str, list: &[RevisionView]) -> Html {
    if list.is_empty() {
        return html!("<p class=\"muted\">No revisions.</p>");
    }
    html!(
        "<table class=\"wide\"><thead><tr><th class=\"num\">Revision</th><th>Branch</th>\
         <th>Author</th><th>When</th><th>Message</th></tr></thead><tbody>{}</tbody></table>",
        revision_rows(d, list)
    )
}

/// A directory of a depot at a branch's head or a revision, with recent revisions.
async fn tree(
    viewer: Viewer,
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    Path(d): Path<String>,
    q: Result<Query<At>, QueryRejection>,
) -> Response {
    let at = At::from(q);
    let place = match place(&ctx, &viewer, &d, &at).await {
        Ok(p) => p,
        Err(e) => return failure(&ctx, &viewer, &d, &e),
    };
    let api = ctx.api(&viewer.0);
    let dir = at.path.as_deref();
    let mut tree_q = format!("{}?rev={}", api_path(paths::TREE, &[&d]), place.rev);
    if let Some(p) = dir {
        tree_q.push_str(&format!("&path={}", encode(p)));
    }
    let listing = match api.get::<TreeListing>(&tree_q).await {
        Ok(l) => l,
        Err(e) => return failure(&ctx, &viewer, &d, &e),
    };
    let mut entries = listing.entries;
    entries.sort_by(|a, b| {
        (a.kind != EntryKind::Dir, a.name.to_lowercase())
            .cmp(&(b.kind != EntryKind::Dir, b.name.to_lowercase()))
    });
    let child = |name: &str| match dir {
        Some(p) => format!("{p}/{name}"),
        None => name.to_owned(),
    };
    let base = depot_href(&d);
    let rows: Html = entries
        .iter()
        .map(|e| {
            let sub = child(&e.name);
            match e.kind {
                EntryKind::Dir => html!(
                    "<tr><td data-label=\"Name\"><a href=\"{}{}\">{}/</a></td>\
                     <td class=\"num\" data-label=\"Size\"></td></tr>",
                    base,
                    at.query(&place.branch, Some(&sub)),
                    e.name
                ),
                _ => html!(
                    "<tr><td data-label=\"Name\"><a href=\"{}/file{}\">{}</a>{}</td>\
                     <td class=\"num\" data-label=\"Size\">{}</td></tr>",
                    base,
                    at.query(&place.branch, Some(&sub)),
                    e.name,
                    if e.kind == EntryKind::Symlink {
                        html!(" <span class=\"badge\">link</span>")
                    } else {
                        Html::new()
                    },
                    bytes(e.size)
                ),
            }
        })
        .collect();
    let listing = if entries.is_empty() {
        html!("<p class=\"muted\">This directory is empty.</p>")
    } else {
        html!(
            "<table class=\"wide files\"><thead><tr><th>Name</th><th class=\"num\">Size</th>\
             </tr></thead><tbody>{}</tbody></table>",
            rows
        )
    };
    let mut hist_q = format!(
        "{}?limit=10&branch={}&before={}",
        api_path(paths::REVISIONS, &[&d]),
        encode(&place.branch),
        place.rev + 1
    );
    if let Some(p) = dir {
        hist_q.push_str(&format!("&path={}", encode(p)));
    }
    let recent = match api.get::<RevisionList>(&hist_q).await {
        Ok(l) => revision_table(&d, &l.revisions),
        Err(e) => error_text(&e),
    };
    let shown_at = if at.rev.is_some() {
        html!(
            "at r{} · <a href=\"{}{}\">go to the head</a>",
            place.rev,
            base,
            At::default().query(&place.branch, dir)
        )
    } else {
        html!("head r{}", place.rev)
    };
    let link = origin(&ctx, &headers).map(|server| Link::Revision {
        server,
        depot: d.clone(),
        rev: place.rev,
    });
    let shelves_link = if super::web_shelves::enabled(&ctx) {
        html!(
            "<a href=\"{}/shelves\">Shelves waiting for review</a>",
            base
        )
    } else {
        Html::new()
    };
    let body = html!(
        "<h1>{}</h1>{}<div class=\"row spread\">{}<p class=\"muted\">{} on {}</p></div>{}\
         <section>{}</section>\
         <section><h2>Recent revisions</h2>{}<p class=\"actions\">\
         <a href=\"{}/history?branch={}\">All history</a>{}</p></section>",
        d,
        breadcrumbs(&d, &at, &place.branch, dir),
        branch_picker(&base, &place, dir),
        shown_at,
        place.branch,
        link_box(link),
        listing,
        recent,
        base,
        encode(&place.branch),
        shelves_link
    );
    Page::new(&d, Nav::Depots).render_web(&ctx, &viewer, body)
}

/// Image types shown inline; anything else is text or a download.
pub(crate) fn image_type(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => return None,
    })
}

/// What a file holds, as far as a page shows it.
enum Content {
    Text(String),
    Binary,
    TooLarge,
}

fn classify(buf: Vec<u8>) -> Content {
    if buf[..buf.len().min(8192)].contains(&0) {
        return Content::Binary;
    }
    match String::from_utf8(buf) {
        Ok(s) => Content::Text(s),
        Err(_) => Content::Binary,
    }
}

fn files_query(d: &str, rev: u64, path: &str) -> String {
    format!(
        "{}?rev={rev}&path={}",
        api_path(paths::FILES, &[d]),
        encode(path)
    )
}

/// Reads a file through the API, up to `max` bytes.
async fn read_file(
    ctx: &Ctx,
    viewer: &Viewer,
    d: &str,
    rev: u64,
    path: &str,
    max: usize,
) -> Result<(Content, bool), ErrorBody> {
    let resp = ctx.api(&viewer.0).raw(&files_query(d, rev, path)).await?;
    let symlink = resp
        .headers()
        .get(HEADER_KIND)
        .is_some_and(|v| v.as_bytes() == b"symlink");
    let len = resp
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if len.is_some_and(|n| n > max) {
        return Ok((Content::TooLarge, symlink));
    }
    match axum::body::to_bytes(resp.into_body(), max).await {
        Ok(b) => Ok((classify(b.to_vec()), symlink)),
        Err(_) => Ok((Content::TooLarge, symlink)),
    }
}

/// Text with line numbers.
fn code_block(text: &str) -> Html {
    let rows: Html = text
        .lines()
        .enumerate()
        .map(|(i, line)| {
            html!(
                "<tr><td class=\"ln\">{}</td><td class=\"src\">{}</td></tr>",
                i + 1,
                line
            )
        })
        .collect();
    html!("<div class=\"code\"><table>{}</table></div>", rows)
}

/// One file at a revision or a branch's head: its content and its history.
async fn file(
    viewer: Viewer,
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    Path(d): Path<String>,
    q: Result<Query<At>, QueryRejection>,
) -> Response {
    let at = At::from(q);
    let Some(path) = at.path.clone() else {
        return failure(&ctx, &viewer, &d, &invalid("no file was named"));
    };
    let place = match place(&ctx, &viewer, &d, &at).await {
        Ok(p) => p,
        Err(e) => return failure(&ctx, &viewer, &d, &e),
    };
    let base = depot_href(&d);
    let raw_href = format!("{base}/raw?rev={}&path={}", place.rev, encode(&path));
    let content = if let Some(_mime) = image_type(&path) {
        html!(
            "<p class=\"image\"><img src=\"{}\" alt=\"{}\"/></p>",
            raw_href,
            path
        )
    } else {
        match read_file(&ctx, &viewer, &d, place.rev, &path, MAX_TEXT).await {
            Ok((Content::Text(t), true)) => html!("<p>A symbolic link to <code>{}</code>.</p>", t),
            Ok((Content::Text(t), false)) if t.is_empty() => {
                html!("<p class=\"muted\">This file is empty.</p>")
            }
            Ok((Content::Text(t), false)) => code_block(&t),
            Ok((Content::Binary, _)) => {
                html!("<p class=\"muted\">This file is not text. Download it to open it.</p>")
            }
            Ok((Content::TooLarge, _)) => html!(
                "<p class=\"muted\">This file is too large to show here ({} at most). \
                 Download it to open it.</p>",
                bytes(MAX_TEXT as u64)
            ),
            Err(e) if e.code == "obliterated" => html!(
                "<p class=\"error\" role=\"alert\">This version of the file was obliterated: \
                 its content was removed from the server for good.</p>"
            ),
            Err(e) => return failure(&ctx, &viewer, &d, &e),
        }
    };
    let history_q = format!(
        "{}?limit=20&branch={}&path={}&before={}",
        api_path(paths::REVISIONS, &[&d]),
        encode(&place.branch),
        encode(&path),
        place.rev + 1
    );
    let history = match ctx.api(&viewer.0).get::<RevisionList>(&history_q).await {
        Ok(l) => revision_table(&d, &l.revisions),
        Err(e) => error_text(&e),
    };
    let link = origin(&ctx, &headers).map(|server| Link::File {
        server,
        depot: d.clone(),
        path: path.clone(),
        rev: at.rev,
        branch: Some(place.branch.clone()),
    });
    let shown_at = if at.rev.is_some() {
        html!("at r{}", place.rev)
    } else {
        html!("head r{}", place.rev)
    };
    let body = html!(
        "<h1>{}</h1>{}<div class=\"row spread\">{}<p class=\"muted\">{} on {}</p></div>{}\
         <section><div class=\"actions\"><a class=\"button\" href=\"{}&amp;download=1\">Download</a>\
         </div>{}</section><section><h2>History</h2>{}</section>",
        path.rsplit('/').next().unwrap_or(&path),
        breadcrumbs(&d, &at, &place.branch, Some(&path)),
        branch_picker(&format!("{base}/file"), &place, Some(&path)),
        shown_at,
        place.branch,
        link_box(link),
        raw_href,
        content,
        history
    );
    Page::new(&path, Nav::Depots).render_web(&ctx, &viewer, body)
}

/// A file's bytes, streamed from the API: images inline, everything else as a download.
async fn raw(
    viewer: Viewer,
    State(ctx): State<Ctx>,
    Path(d): Path<String>,
    q: Result<Query<At>, QueryRejection>,
) -> Response {
    let at = At::from(q);
    let (Some(path), Some(rev)) = (at.path.clone(), at.rev) else {
        return failure(
            &ctx,
            &viewer,
            &d,
            &invalid("a file and a revision must be named"),
        );
    };
    let resp = match ctx.api(&viewer.0).raw(&files_query(&d, rev, &path)).await {
        Ok(r) => r,
        Err(e) => return failure(&ctx, &viewer, &d, &e),
    };
    let (parts, body) = resp.into_parts();
    let mut r = Response::new(body);
    let h = r.headers_mut();
    if let Some(len) = parts.headers.get(header::CONTENT_LENGTH) {
        h.insert(header::CONTENT_LENGTH, len.clone());
    }
    let name = path.rsplit('/').next().unwrap_or("file");
    let inline = at.download.is_none() && image_type(&path).is_some();
    let mime = image_type(&path)
        .filter(|_| inline)
        .unwrap_or("application/octet-stream");
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    let disposition = if inline {
        "inline".to_owned()
    } else {
        // The name in the plain form only when it is plain ASCII; the encoded form always.
        let plain: String = name
            .chars()
            .filter(|c| c.is_ascii_graphic() && *c != '"' && *c != '\\' || *c == ' ')
            .collect();
        format!(
            "attachment; filename=\"{plain}\"; filename*=UTF-8''{}",
            encode(name)
        )
    };
    if let Ok(v) = HeaderValue::from_str(&disposition) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    r
}

/// A branch's revisions, newest first, a page at a time.
async fn history(
    viewer: Viewer,
    State(ctx): State<Ctx>,
    Path(d): Path<String>,
    q: Result<Query<At>, QueryRejection>,
) -> Response {
    let at = At::from(q);
    let place = match place(&ctx, &viewer, &d, &at).await {
        Ok(p) => p,
        Err(e) => return failure(&ctx, &viewer, &d, &e),
    };
    let mut list_q = format!(
        "{}?limit={HISTORY_PAGE}&branch={}",
        api_path(paths::REVISIONS, &[&d]),
        encode(&place.branch)
    );
    if let Some(b) = at.before {
        list_q.push_str(&format!("&before={b}"));
    }
    let list = match ctx.api(&viewer.0).get::<RevisionList>(&list_q).await {
        Ok(l) => l,
        Err(e) => return failure(&ctx, &viewer, &d, &e),
    };
    let base = depot_href(&d);
    let older = match (list.more, list.revisions.last()) {
        (true, Some(last)) => html!(
            "<p><a href=\"{}/history?branch={}&amp;before={}\">Older revisions</a></p>",
            base,
            encode(&place.branch),
            last.rev
        ),
        _ => Html::new(),
    };
    let body = html!(
        "<h1>History of {}</h1>{}{}<section>{}{}</section>",
        d,
        breadcrumbs(&d, &At::default(), &place.branch, None),
        branch_picker(&format!("{base}/history"), &place, None),
        revision_table(&d, &list.revisions),
        older
    );
    Page::new(&d, Nav::Depots).render_web(&ctx, &viewer, body)
}

/// The unified diff of one change, as rows with added and removed lines marked.
pub(crate) fn diff_block(patch: &str) -> Html {
    let rows: Html = patch
        .lines()
        .map(|line| {
            let class = if line.starts_with("+++") || line.starts_with("---") {
                "head"
            } else if line.starts_with("@@") {
                "hunk"
            } else if line.starts_with('+') {
                "add"
            } else if line.starts_with('-') {
                "del"
            } else {
                "ctx"
            };
            html!("<div class=\"{}\">{}</div>", class, line)
        })
        .collect();
    html!("<pre class=\"diff\">{}</pre>", rows)
}

/// The diff of `c` in revision `rev` (whose parent is `parent`), or why there is none.
async fn change_diff(
    ctx: &Ctx,
    viewer: &Viewer,
    d: &str,
    rev: u64,
    parent: Option<u64>,
    c: &ChangeView,
) -> Html {
    if c.kind == Some(EntryKind::Symlink) {
        return html!("<p class=\"muted\">A symbolic link.</p>");
    }
    if c.kind == Some(EntryKind::Dir) {
        return Html::new();
    }
    let path = c.path.as_str();
    // The old side: the copy source for a copied file, the parent revision otherwise.
    let old_from = match (&c.copy_from, c.action, parent) {
        (Some(cf), _, _) => Some((cf.rev, cf.path.as_str().to_owned())),
        (None, ChangeAction::Add, _) | (None, _, None) => None,
        (None, _, Some(p)) => Some((p, path.to_owned())),
    };
    let side = |r: u64, p: String| async move {
        read_file(ctx, viewer, d, r, &p, MAX_DIFF_FILE)
            .await
            .map(|(c, _)| c)
    };
    let old = match &old_from {
        Some((r, p)) => Some(side(*r, p.clone()).await),
        None => None,
    };
    let new = match c.action {
        ChangeAction::Delete => None,
        _ => Some(side(rev, path.to_owned()).await),
    };
    let text = |side: Option<Result<Content, ErrorBody>>| -> Result<String, Html> {
        match side {
            None => Ok(String::new()),
            Some(Ok(Content::Text(t))) => Ok(t),
            Some(Ok(Content::Binary)) => Err(html!("<p class=\"muted\">Not text.</p>")),
            Some(Ok(Content::TooLarge)) => {
                Err(html!("<p class=\"muted\">Too large to compare here.</p>"))
            }
            Some(Err(e)) if e.code == "obliterated" => Err(html!(
                "<p class=\"muted\">A version of this file was obliterated.</p>"
            )),
            Some(Err(e)) => Err(error_text(&e)),
        }
    };
    let (old, new) = match (text(old), text(new)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(why), _) | (_, Err(why)) => return why,
    };
    if old == new {
        return html!("<p class=\"muted\">The content did not change.</p>");
    }
    let from = match old_from {
        Some((_, p)) => format!("a/{p}"),
        None => "/dev/null".to_owned(),
    };
    let to = match c.action {
        ChangeAction::Delete => "/dev/null".to_owned(),
        _ => format!("b/{path}"),
    };
    let mut opts = diffy::DiffOptions::new();
    opts.set_original_filename(from).set_modified_filename(to);
    diff_block(&opts.create_patch(&old, &new).to_string())
}

/// One revision: who, when, why, and the diff of each changed file.
async fn revision(
    viewer: Viewer,
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    Path((d, n)): Path<(String, u64)>,
) -> Response {
    let r = match ctx
        .api(&viewer.0)
        .get::<RevisionView>(&api_path(paths::REVISION, &[&d, &n.to_string()]))
        .await
    {
        Ok(r) => r,
        Err(e) => return failure(&ctx, &viewer, &d, &e),
    };
    let base = depot_href(&d);
    let changes = r.changes.clone().unwrap_or_default();
    let mut sections = Html::new();
    for (i, c) in changes.iter().enumerate() {
        let action = match c.action {
            ChangeAction::Add if c.copy_from.is_some() => "copied",
            ChangeAction::Add => "added",
            ChangeAction::Modify => "modified",
            ChangeAction::Delete => "deleted",
        };
        let from = match &c.copy_from {
            Some(cf) => html!(
                " <span class=\"muted\">from {} at r{}</span>",
                cf.path.as_str(),
                cf.rev
            ),
            None => Html::new(),
        };
        let name = if c.action == ChangeAction::Delete {
            html!("<code>{}</code>", c.path.as_str())
        } else {
            html!(
                "<a href=\"{}/file?branch={}&amp;rev={}&amp;path={}\"><code>{}</code></a>",
                base,
                encode(&r.branch),
                r.rev,
                encode(c.path.as_str()),
                c.path.as_str()
            )
        };
        let diff = if i < MAX_DIFFS {
            change_diff(&ctx, &viewer, &d, r.rev, r.parent, c).await
        } else {
            html!("<p class=\"muted\">Not compared: this revision changes many files.</p>")
        };
        sections.push(html!(
            "<section class=\"change\"><h2>{} <span class=\"badge\">{}</span>{}</h2>{}</section>",
            name,
            action,
            from,
            diff
        ));
    }
    if changes.is_empty() {
        sections.push(html!(
            "<section><p class=\"muted\">No changes you can see.</p></section>"
        ));
    }
    if r.changes_truncated {
        sections.push(html!(
            "<p class=\"muted\">Only the first changes are listed.</p>"
        ));
    }
    let parent = match r.parent {
        Some(p) => html!("<a href=\"{}\">r{}</a>", revision_href(&d, p), p),
        None => html!("<span class=\"muted\">none</span>"),
    };
    let merged: Html = r
        .merged_from
        .iter()
        .map(|m| {
            html!(
                "<dt>Merged</dt><dd>{} r{}–r{}</dd>",
                m.branch,
                m.from_rev,
                m.to_rev
            )
        })
        .collect();
    let submitter = if r.submitter.name != r.author.name {
        html!("<dt>Submitted by</dt><dd>{}</dd>", r.submitter.name)
    } else {
        Html::new()
    };
    let link = origin(&ctx, &headers).map(|server| Link::Revision {
        server,
        depot: d.clone(),
        rev: r.rev,
    });
    let body = html!(
        "<h1>r{} <span class=\"muted\">in {}</span></h1>{}\
         <dl class=\"facts card\"><dt>Branch</dt><dd><a href=\"{}?branch={}&amp;rev={}\">{}</a> \
         <span class=\"muted\">(browse files at this revision)</span></dd>\
         <dt>Author</dt><dd>{}</dd>{}<dt>When</dt><dd>{}</dd><dt>Parent</dt><dd>{}</dd>{}\
         <dt>Files</dt><dd>{}</dd></dl>\
         <section><h2>Message</h2><pre class=\"message\">{}</pre></section>{}",
        r.rev,
        d,
        link_box(link),
        base,
        encode(&r.branch),
        r.rev,
        r.branch,
        r.author.name,
        submitter,
        at(r.created_at),
        parent,
        merged,
        count(changes.len() as u64, "file"),
        r.message,
        sections
    );
    Page::new(&format!("r{n}"), Nav::Depots).render_web(&ctx, &viewer, body)
}

/// The account page: the user's tokens and a password change.
async fn account(viewer: Viewer, State(ctx): State<Ctx>) -> Response {
    let tokens = match ctx.api(&viewer.0).get::<TokenList>(paths::TOKENS).await {
        Ok(t) => t.tokens,
        Err(e) => return failure(&ctx, &viewer, "Account", &e),
    };
    let me = viewer.0.caller.token_id.0;
    let rows: Html = tokens
        .iter()
        .map(|t| {
            let expires = match t.expires_at {
                Some(x) => at(x),
                None => html!("<span class=\"muted\">never</span>"),
            };
            let used = match t.last_used_at {
                Some(x) => at(x),
                None => html!("<span class=\"muted\">never</span>"),
            };
            let action = if t.id == me {
                html!("<span class=\"badge ok\">this session</span>")
            } else {
                html!(
                    "<form method=\"post\" action=\"/web/account/tokens/{}/revoke\" \
                     data-confirm=\"Revoke token {}? Programs that use it stop working.\">{}\
                     <button type=\"submit\" class=\"danger\">Revoke</button></form>",
                    t.id,
                    t.name,
                    viewer.0.csrf_field()
                )
            };
            html!(
                "<tr><td data-label=\"Name\">{}</td><td data-label=\"Scopes\">{}</td>\
                 <td data-label=\"Created\">{}</td><td data-label=\"Expires\">{}</td>\
                 <td data-label=\"Last used\">{}</td><td>{}</td></tr>",
                t.name,
                t.scopes.join(", "),
                at(t.created_at),
                expires,
                used,
                action
            )
        })
        .collect();
    let body = html!(
        "<h1>Account</h1><dl class=\"facts card\"><dt>Name</dt><dd>{}</dd></dl>\
         <section><h2>Tokens</h2><p class=\"muted\">Tokens let programs such as <code>lode</code> \
         and Lode Desk act as you. Revoke one you no longer use.</p>\
         <table class=\"wide\"><thead><tr><th>Name</th><th>Scopes</th><th>Created</th>\
         <th>Expires</th><th>Last used</th><th></th></tr></thead><tbody>{}</tbody></table></section>\
         <section><h2>Change password</h2>\
         <form class=\"stack\" method=\"post\" action=\"/web/account/password\">{}\
         <label>Current password<input type=\"password\" name=\"password\" \
         autocomplete=\"current-password\" required=\"required\"/></label>\
         <label>New password<input type=\"password\" name=\"new_password\" \
         autocomplete=\"new-password\" required=\"required\" minlength=\"8\"/></label>\
         <label>New password again<input type=\"password\" name=\"new_password_again\" \
         autocomplete=\"new-password\" required=\"required\" minlength=\"8\"/></label>\
         <button type=\"submit\" class=\"primary\">Change password</button></form></section>",
        viewer.0.name(),
        rows,
        viewer.0.csrf_field()
    );
    Page::new("Account", Nav::Account).render_web(&ctx, &viewer, body)
}

async fn password(
    State(ctx): State<Ctx>,
    ClientAddr(addr): ClientAddr,
    post: ViewerPost,
) -> Response {
    let f = &post.fields;
    let new_password = f.get("new_password").unwrap_or("").to_owned();
    if f.get("new_password_again").unwrap_or("") != new_password {
        return redirect_with_note(&ctx, ACCOUNT, "The new passwords differ.");
    }
    let current = f.get("password").unwrap_or("").to_owned();
    let name = post.viewer.0.name().to_owned();
    match change_own_password(ctx.state(), addr, &name, current, new_password).await {
        Ok(_) => redirect_with_note(&ctx, ACCOUNT, "Password changed."),
        Err(e) => redirect_with_note(
            &ctx,
            ACCOUNT,
            &super::layout::sentence(&format!("password not changed: {}", e.body().message)),
        ),
    }
}

async fn revoke_token(State(ctx): State<Ctx>, Path(id): Path<i64>, post: ViewerPost) -> Response {
    let path = api_path(paths::TOKEN, &[&id.to_string()]);
    match ctx.api(&post.viewer.0).delete(&path).await {
        Ok(()) => redirect_with_note(&ctx, ACCOUNT, "Token revoked."),
        Err(e) => redirect_with_note(&ctx, ACCOUNT, &super::layout::sentence(&e.message)),
    }
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_keep_a_fixed_revision() {
        let at = At {
            rev: Some(7),
            ..At::default()
        };
        assert_eq!(
            at.query("main", Some("a b/c")),
            "?branch=main&rev=7&path=a%20b%2Fc"
        );
        assert_eq!(At::default().query("dev", None), "?branch=dev");
    }

    #[test]
    fn images_by_extension() {
        assert_eq!(image_type("art/logo.PNG"), Some("image/png"));
        assert_eq!(image_type("icon.svg"), None);
        assert_eq!(image_type("Makefile"), None);
    }

    #[test]
    fn content_kinds() {
        assert!(matches!(classify(b"hello\n".to_vec()), Content::Text(_)));
        assert!(matches!(classify(vec![0, 1, 2]), Content::Binary));
        assert!(matches!(classify(vec![0xff, 0xfe]), Content::Binary));
    }
}
