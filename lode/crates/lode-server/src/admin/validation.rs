//! Read-only validation activity on the depot page.
use axum::extract::{Path, State};
use axum::response::Response;
use lode_proto::admin::{ValidationActivity, ValidationRun};
use lode_proto::consts::paths;
use serde::Deserialize;

use super::api::path;
use super::depots::depot_href;
use super::format::when;
use super::html::Html;
use super::layout::error_text;
use super::{html, Admin, Ctx, Nav, Page};

fn table(d: &str, runs: &[ValidationRun], empty: &str) -> Html {
    if runs.is_empty() {
        return html!("<p class=\"muted\">{}</p>", empty);
    }
    let rows: Html = runs
        .iter()
        .map(|r| {
            html!(
        "<tr><td data-label=\"Shelf\"><a href=\"{}/validation/{}/{}\">Shelf {} · v{}</a></td>\
         <td data-label=\"State\">{}</td><td data-label=\"Updated\">{}</td></tr>",
        depot_href(d), r.shelf, r.version, r.shelf, r.version, r.state, when(r.updated_at)
    )
        })
        .collect();
    html!("<table class=\"wide\"><thead><tr><th>Shelf version</th><th>State</th><th>Updated</th></tr></thead><tbody>{}</tbody></table>", rows)
}

pub(super) async fn section(ctx: &Ctx, admin: &Admin, d: &str) -> Html {
    let body = match ctx
        .api(admin)
        .get::<ValidationActivity>(&path(paths::ADMIN_VALIDATION, &[d]))
        .await
    {
        Ok(activity) => html!(
            "<h3>Queue</h3>{}<h3>Recent runs</h3>{}",
            table(d, &activity.active, "No queued or running checks."),
            table(d, &activity.recent, "No finished runs yet.")
        ),
        Err(e) => error_text(&e),
    };
    html!(
        "<section><h2 id=\"validation\">Validation</h2>{}</section>",
        body
    )
}

#[derive(Deserialize)]
struct ResultView {
    state: String,
    results: Vec<Script>,
}
#[derive(Deserialize)]
struct Script {
    name: String,
    outcome: String,
    reason: Option<String>,
    text: Option<String>,
    #[serde(default)]
    problems: Vec<Problem>,
}
#[derive(Deserialize)]
struct Problem {
    message: String,
    level: String,
    path: Option<String>,
    line: Option<u64>,
}

fn script(s: Script) -> Html {
    let diagnostics: Html = s
        .problems
        .iter()
        .map(|p| {
            let location = match (&p.path, p.line) {
                (Some(path), Some(line)) => format!("{path}:{line}"),
                (Some(path), None) => path.clone(),
                _ => String::new(),
            };
            html!(
                "<li><code>{}</code> {} {}</li>",
                location,
                p.level,
                p.message
            )
        })
        .collect();
    html!(
        "<section><h2>{}</h2><p>{}</p>{}{}{}</section>",
        s.name,
        s.outcome,
        s.reason.map(|r| html!("<p>{}</p>", r)),
        (!diagnostics.is_empty()).then(|| html!("<ul>{}</ul>", diagnostics)),
        s.text
            .filter(|t| !t.is_empty())
            .map(|t| html!("<pre>{}</pre>", t))
    )
}

pub(super) async fn show(
    State(ctx): State<Ctx>,
    admin: Admin,
    Path((d, shelf, version)): Path<(String, u64, u64)>,
) -> Response {
    if !super::depots::has_module(&ctx, "validate") {
        return super::not_found_page(&ctx, Some(&admin));
    }
    let uri = path(
        paths::VALIDATION_RESULT,
        &[&d, &shelf.to_string(), &version.to_string()],
    );
    let (status, content) = match ctx.api(&admin).get::<ResultView>(&uri).await {
        Ok(run) => (
            axum::http::StatusCode::OK,
            html!(
                "<p>State: {}</p>{}",
                run.state,
                run.results.into_iter().map(script).collect::<Html>()
            ),
        ),
        Err(e) => {
            let code = e
                .code
                .parse::<lode_core::ErrorCode>()
                .unwrap_or(lode_core::ErrorCode::Internal);
            let status = axum::http::StatusCode::from_u16(code.http_status())
                .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
            (status, error_text(&e))
        }
    };
    let title = format!("Validation · shelf {shelf} · v{version}");
    let body = html!(
        "<p><a href=\"{}#validation\">{} validation</a></p><h1>{}</h1>{}",
        depot_href(&d),
        d,
        title,
        content
    );
    let mut page = Page::new(&title, Nav::Depots).render(&ctx, Some(&admin), body);
    *page.status_mut() = status;
    page
}
