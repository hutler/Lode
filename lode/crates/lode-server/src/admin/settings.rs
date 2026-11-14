//! Settings (T143): the configuration `loded` runs with, read-only, with secrets hidden.

use axum::extract::State;
use axum::response::Response;

use super::{html, Admin, Ctx, Nav, Page};

pub(crate) async fn show(State(ctx): State<Ctx>, admin: Admin) -> Response {
    let text = ctx.state().config().effective_toml();
    let body = html!(
        "<h1>Settings</h1><p class=\"muted\">The settings this server runs with, as \
         <code>loded.toml</code> would set them, defaults included. Secrets show as \
         <code>***</code>. To change a setting, edit the configuration file and restart \
         <code>loded</code>.</p><pre class=\"settings\" id=\"settings\">{}</pre>",
        text
    );
    Page::new("Settings", Nav::Settings).render(&ctx, Some(&admin), body)
}
