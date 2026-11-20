//! The admin web UI (M14): `loded` serves pages for server administrators at `/admin/`,
//! and a read-only depot browser for every user at `/web/` (T173, see [`web`]).
//!
//! Pages are HTML rendered here, with every value escaped by [`html`]; they work without
//! JavaScript, and one small script (served as a file, under a content security policy that
//! allows no inline script) adds confirmations and live refresh. Only server administrators
//! get in, with a session cookie backed by an ordinary token (see [`session`]).
//!
//! Every action a page takes is a request to the API, sent to the same router that answers
//! clients (see [`api`]), so permissions, validation and errors are the API's. Pages read
//! what they show the same way, except the overview's server facts (uptime, sizes, the last
//! maintenance), which have no API.

pub mod html;

mod access;
mod api;
mod assets;
mod depots;
mod format;
mod layout;
mod overview;
mod people;
mod session;
mod settings;
mod signin;
mod web;
mod webhooks;

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;

use crate::html;
use crate::state::AppState;

pub(crate) use layout::{Nav, Page};
pub(crate) use session::{Admin, Fields, Viewer};

/// What every admin page handler gets.
#[derive(Clone)]
pub(crate) struct Ctx {
    state: AppState,
    /// The API router: core routes and the enabled modules' routes, without the admin UI.
    api: Router,
}

impl Ctx {
    pub(crate) fn state(&self) -> &AppState {
        &self.state
    }

    /// Whether the session cookie gets `Secure`: when the server itself speaks TLS.
    pub(crate) fn secure(&self) -> bool {
        self.state.config().tls.is_some()
    }
}

/// The admin web UI's routes, which send their actions to `api`. Empty when `[admin_ui]` is
/// off, so `/admin/` answers 404 like any unknown address.
pub fn routes(state: AppState, api: Router) -> Router {
    if !state.config().admin_ui.enabled {
        return Router::new();
    }
    let ctx = Ctx { state, api };
    web::routes(Router::new())
        .route("/admin", get(|| async { Redirect::permanent("/admin/") }))
        .route("/admin/", get(overview::page))
        .route(signin::SIGN_IN, get(signin::form).post(signin::submit))
        .route(signin::SIGN_IN_PASSWORD, post(signin::change_password))
        .route(signin::SSO_START, get(signin::sso_start))
        .route(crate::auth::ADMIN_SSO_DONE, get(signin::sso_done))
        .route(signin::SIGN_OUT, post(signin::sign_out))
        .route("/admin/depots", get(depots::list).post(depots::create))
        .route("/admin/depots/{d}", get(depots::show))
        .route("/admin/depots/{d}/gc", post(depots::gc))
        .route("/admin/depots/{d}/fsck", post(depots::fsck))
        .route("/admin/depots/{d}/obliterate", post(depots::obliterate))
        .route("/admin/depots/{d}/locks/break", post(depots::break_lock))
        .route("/admin/depots/{d}/delete", post(depots::delete))
        .route("/admin/trash/{d}/restore", post(depots::restore))
        .route("/admin/trash/{d}/erase", post(depots::erase))
        .route("/admin/people", get(people::list))
        .route("/admin/people/users", post(people::create_user))
        .route("/admin/people/agents", post(people::create_agent))
        .route("/admin/people/groups", post(people::create_group))
        .route("/admin/people/groups/{g}/add", post(people::add_member))
        .route(
            "/admin/people/groups/{g}/remove",
            post(people::remove_member),
        )
        .route("/admin/people/{name}", get(people::show))
        .route("/admin/people/{name}/state", post(people::set_state))
        .route("/admin/people/{name}/delete", post(people::delete))
        .route("/admin/people/{name}/password", post(people::set_password))
        .route("/admin/people/{name}/sso", post(people::set_sso))
        .route("/admin/people/{name}/tokens", post(people::create_token))
        .route(
            "/admin/people/{name}/tokens/{id}/revoke",
            post(people::revoke_token),
        )
        .route("/admin/access", get(access::show))
        .route("/admin/access/rules", post(access::add))
        .route("/admin/access/rules/{id}/remove", post(access::remove))
        .route(webhooks::LIST, get(webhooks::list).post(webhooks::create))
        .route("/admin/webhooks/{d}/{id}", get(webhooks::show))
        .route("/admin/webhooks/{d}/{id}/ping", post(webhooks::ping))
        .route("/admin/webhooks/{d}/{id}/remove", post(webhooks::remove))
        .route("/admin/settings", get(settings::show))
        .route(assets::CSS_PATH, get(assets::css))
        .route(assets::JS_PATH, get(assets::js))
        .route("/admin/{*rest}", get(not_found).post(not_found))
        .layer(axum::middleware::map_response(protect))
        .with_state(ctx)
}

async fn not_found(admin: Option<Admin>, ctx: axum::extract::State<Ctx>) -> Response {
    not_found_page(&ctx, admin.as_ref())
}

/// The 404 page.
pub(crate) fn not_found_page(ctx: &Ctx, admin: Option<&Admin>) -> Response {
    let body = html!(
        "<h1>Not found</h1><p>There is no such page. <a href=\"/admin/\">Go to the overview</a>.</p>"
    );
    let mut r = Page::new("Not found", Nav::None).render(ctx, admin, body);
    *r.status_mut() = StatusCode::NOT_FOUND;
    r
}

/// Headers on every admin response: no inline script or style, no framing, no caching of
/// pages, no guessing of content types.
async fn protect(mut r: Response) -> Response {
    let h = r.headers_mut();
    let set = |h: &mut axum::http::HeaderMap, name, value: &'static str| {
        h.insert(name, HeaderValue::from_static(value));
    };
    set(
        h,
        header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; style-src 'self'; script-src 'self'; img-src 'self' data:; \
         connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
    );
    set(h, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(h, header::X_FRAME_OPTIONS, "DENY");
    set(h, header::REFERRER_POLICY, "same-origin");
    if !h.contains_key(header::CACHE_CONTROL) {
        set(h, header::CACHE_CONTROL, "no-store");
    }
    r
}

/// An HTML response.
pub(crate) fn html_response(status: StatusCode, page: html::Html) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        page.into_string(),
    )
        .into_response()
}
