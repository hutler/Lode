//! Signing in and out of the web pages: a name and password, or single sign-on. Any user
//! signs in (T173); server administrators land on the admin pages, everyone else on the
//! depot browser under `/web/`.

use axum::extract::rejection::{FormRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use lode_meta::{PrincipalRow, Reads};
use serde::Deserialize;

use super::html;
use super::session::{cookie, set_cookie, SESSION_COOKIE, SESSION_HOURS, SIGN_IN_COOKIE};
use super::session::{ViewerPost};
use super::{html_response, Ctx, Nav, Page, Viewer};
use crate::auth::{
    change_own_password, check_password_sign_in, ct_eq, finish_admin, issue_token, random_verifier, start_admin,
    would_be_server_admin, ClientAddr,
};

/// The sign-in page.
pub(crate) const SIGN_IN: &str = "/admin/sign-in";
/// The "choose a new password" form for a password that must be changed.
pub(crate) const SIGN_IN_PASSWORD: &str = "/admin/sign-in/password";
/// Starts a single sign-on.
pub(crate) const SSO_START: &str = "/admin/sign-in/sso";
/// Signs out.
pub(crate) const SIGN_OUT: &str = "/admin/sign-out";
/// The name of the token behind a server administrator's session.
pub(crate) const TOKEN_NAME: &str = "web admin";
/// The name of the token behind any other user's session.
pub(crate) const VIEWER_TOKEN_NAME: &str = "web";
/// The depot browser, where users who are not server administrators land.
pub(crate) const WEB_HOME: &str = "/web/";

/// How long the sign-in cookie lives: long enough to fill in the form or finish a single
/// sign-on.
const SIGN_IN_COOKIE_SECONDS: i64 = 30 * 60;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct SignInQuery {
    #[serde(default)]
    next: Option<String>,
    #[serde(default)]
    signed_out: Option<String>,
}

/// Where to go after signing in: a page of the admin UI or the depot browser, or the
/// overview.
fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(n)
            if (n.starts_with("/admin/") || n.starts_with(WEB_HOME))
                && !n.contains("//")
                && !n.contains('\\')
                && !n.starts_with("/admin/sign-")
                && n.len() < 500 =>
        {
            n.to_owned()
        }
        _ => "/admin/".to_owned(),
    }
}

/// The sign-in cookie of this browser, and whether it is new.
fn sign_in_cookie(headers: &HeaderMap) -> (String, bool) {
    match cookie(headers, SIGN_IN_COOKIE).filter(|v| (43..=128).contains(&v.len())) {
        Some(v) => (v, false),
        None => (random_verifier(), true),
    }
}

fn with_sign_in_cookie(ctx: &Ctx, mut r: Response, value: &str, new: bool) -> Response {
    if new {
        // Lax, not Strict: it must come back when the identity provider sends the browser
        // back here.
        r.headers_mut().append(
            header::SET_COOKIE,
            set_cookie(
                SIGN_IN_COOKIE,
                value,
                SIGN_IN_COOKIE_SECONDS,
                ctx.secure(),
                false,
            ),
        );
    }
    r
}

/// The sign-in page, with an error or note above the form.
fn sign_in_page(
    ctx: &Ctx,
    status: StatusCode,
    csrf: &str,
    next: &str,
    name: &str,
    message: Option<html::Html>,
) -> Response {
    let state = ctx.state();
    let sso = match state.sso() {
        Some(s) => html!(
            "<p><a class=\"button primary\" href=\"{}?next={}\">Sign in with {}</a></p>\
             <p class=\"muted\">Or with a name and password:</p>",
            SSO_START,
            html::encode(next),
            s.config().label
        ),
        None => html::Html::new(),
    };
    let body = html!(
        "<div class=\"signin card\"><h1>Sign in</h1>\
         <p class=\"muted\">Browse depots and review shelves. Server administrators also \
         get the admin pages.</p>{}{}\
         <form class=\"stack\" method=\"post\" action=\"{}\">\
         <input type=\"hidden\" name=\"csrf\" value=\"{}\"/>\
         <input type=\"hidden\" name=\"next\" value=\"{}\"/>\
         <label>Name<input type=\"text\" name=\"name\" value=\"{}\" autocomplete=\"username\" \
         required=\"required\" autofocus=\"autofocus\"/></label>\
         <label>Password<input type=\"password\" name=\"password\" \
         autocomplete=\"current-password\" required=\"required\"/></label>\
         <button type=\"submit\" class=\"primary\">Sign in</button></form></div>",
        message,
        sso,
        SIGN_IN,
        csrf,
        next,
        name
    );
    let mut r = Page::new("Sign in", Nav::None).render(ctx, None, body);
    *r.status_mut() = status;
    r
}

/// The page that replaces a must-change password before the first sign-in.
fn change_page(
    ctx: &Ctx,
    status: StatusCode,
    csrf: &str,
    next: &str,
    name: &str,
    message: Option<html::Html>,
) -> Response {
    let body = html!(
        "<div class=\"signin card\"><h1>Choose a new password</h1>\
         <p class=\"muted\">The password for {} must be changed before it can be used.</p>{}\
         <form class=\"stack\" method=\"post\" action=\"{}\">\
         <input type=\"hidden\" name=\"csrf\" value=\"{}\"/>\
         <input type=\"hidden\" name=\"next\" value=\"{}\"/>\
         <input type=\"hidden\" name=\"name\" value=\"{}\"/>\
         <label>Current password<input type=\"password\" name=\"password\" \
         autocomplete=\"current-password\" required=\"required\" autofocus=\"autofocus\"/></label>\
         <label>New password<input type=\"password\" name=\"new_password\" \
         autocomplete=\"new-password\" required=\"required\" minlength=\"8\"/></label>\
         <label>New password again<input type=\"password\" name=\"new_password_again\" \
         autocomplete=\"new-password\" required=\"required\" minlength=\"8\"/></label>\
         <button type=\"submit\" class=\"primary\">Change and sign in</button></form></div>",
        name,
        message,
        SIGN_IN_PASSWORD,
        csrf,
        next,
        name
    );
    let mut r = Page::new("Choose a new password", Nav::None).render(ctx, None, body);
    *r.status_mut() = status;
    r
}

pub(crate) async fn form(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    viewer: Option<Viewer>,
    query: Result<Query<SignInQuery>, QueryRejection>,
) -> Response {
    let q = query.map(|Query(q)| q).unwrap_or_default();
    let next = safe_next(q.next.as_deref());
    if let Some(v) = viewer {
        let next = landing(&next, v.is_admin(&ctx));
        return Redirect::to(&next).into_response();
    }
    let (csrf, new) = sign_in_cookie(&headers);
    let message = q
        .signed_out
        .map(|_| html!("<p class=\"note\" role=\"status\">You are signed out.</p>"));
    let r = sign_in_page(&ctx, StatusCode::OK, &csrf, &next, "", message);
    with_sign_in_cookie(&ctx, r, &csrf, new)
}

/// Issues the session token for `principal` and sets the session cookie on `r`.
async fn start_session(
    ctx: &Ctx,
    principal: &PrincipalRow,
    admin: bool,
    mut r: Response,
) -> Result<Response, String> {
    let state = ctx.state();
    let expires = state.clock().now_ms() + SESSION_HOURS * 3_600_000;
    let name = if admin { TOKEN_NAME } else { VIEWER_TOKEN_NAME };
    let (token, _) = issue_token(
        state,
        principal.id,
        name.to_owned(),
        state.scopes().all(),
        Some(expires),
    )
    .await
    .map_err(|e| e.body().message.clone())?;
    let h = r.headers_mut();
    h.append(
        header::SET_COOKIE,
        set_cookie(
            SESSION_COOKIE,
            &token,
            SESSION_HOURS * 3600,
            ctx.secure(),
            true,
        ),
    );
    h.append(
        header::SET_COOKIE,
        set_cookie(SIGN_IN_COOKIE, "", 0, ctx.secure(), false),
    );
    h.append(
        header::SET_COOKIE,
        super::session::clear_legacy_session(ctx.secure()),
    );
    tracing::info!(principal = %principal.name, admin, "web UI: signed in");
    Ok(r)
}

/// Where a user lands: `next`, except that a user who is not a server administrator goes
/// to the depot browser instead of an admin page.
fn landing(next: &str, admin: bool) -> String {
    if admin || next.starts_with(WEB_HOME) {
        next.to_owned()
    } else {
        WEB_HOME.to_owned()
    }
}

pub(crate) async fn submit(
    State(ctx): State<Ctx>,
    ClientAddr(addr): ClientAddr,
    headers: HeaderMap,
    form: Result<Form<Vec<(String, String)>>, FormRejection>,
) -> Response {
    let fields = super::Fields::from(form.map(|Form(f)| f).unwrap_or_default());
    let next = safe_next(fields.get("next"));
    let (expected, new) = sign_in_cookie(&headers);
    let sent = fields.get("csrf").unwrap_or("");
    if new || !ct_eq(sent.as_bytes(), expected.as_bytes()) {
        let message =
            html!("<p class=\"error\" role=\"alert\">The sign-in form expired. Try again.</p>");
        let r = sign_in_page(
            &ctx,
            StatusCode::FORBIDDEN,
            &expected,
            &next,
            "",
            Some(message),
        );
        return with_sign_in_cookie(&ctx, r, &expected, new);
    }
    let name = fields.text("name");
    let password = fields.get("password").unwrap_or("").to_owned();
    let refuse = |status, text: &str| {
        let message = html!(
            "<p class=\"error\" role=\"alert\">{}</p>",
            super::layout::sentence(text)
        );
        sign_in_page(&ctx, status, &expected, &next, &name, Some(message))
    };
    let principal = match check_password_sign_in(ctx.state(), addr, &name, password).await {
        Ok(p) => p,
        Err(e)
            if e.body().details["reason"]
                == lode_proto::auth::REASON_PASSWORD_CHANGE_REQUIRED =>
        {
            return change_page(&ctx, StatusCode::OK, &expected, &next, &name, None);
        }
        Err(e) => return refuse(StatusCode::UNAUTHORIZED, &e.body().message),
    };
    finish_sign_in(&ctx, &principal, &next, refuse).await
}

/// Lets a user in: the session cookie and a redirect to where they land.
async fn finish_sign_in(
    ctx: &Ctx,
    principal: &PrincipalRow,
    next: &str,
    refuse: impl Fn(StatusCode, &str) -> Response,
) -> Response {
    let admin = match would_be_server_admin(ctx.state(), principal).await {
        Ok(a) => a,
        Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, &e.body().message),
    };
    let r = Redirect::to(&landing(next, admin)).into_response();
    match start_session(ctx, principal, admin, r).await {
        Ok(r) => r,
        Err(message) => refuse(StatusCode::INTERNAL_SERVER_ERROR, &message),
    }
}

/// The "choose a new password" form: changes the password, then signs in with the new one.
pub(crate) async fn change_password(
    State(ctx): State<Ctx>,
    ClientAddr(addr): ClientAddr,
    headers: HeaderMap,
    form: Result<Form<Vec<(String, String)>>, FormRejection>,
) -> Response {
    let fields = super::Fields::from(form.map(|Form(f)| f).unwrap_or_default());
    let next = safe_next(fields.get("next"));
    let (expected, new) = sign_in_cookie(&headers);
    let sent = fields.get("csrf").unwrap_or("");
    if new || !ct_eq(sent.as_bytes(), expected.as_bytes()) {
        let message =
            html!("<p class=\"error\" role=\"alert\">The sign-in form expired. Try again.</p>");
        let r = sign_in_page(&ctx, StatusCode::FORBIDDEN, &expected, &next, "", Some(message));
        return with_sign_in_cookie(&ctx, r, &expected, new);
    }
    let name = fields.text("name");
    let again = |status, text: &str| {
        let message = html!(
            "<p class=\"error\" role=\"alert\">{}</p>",
            super::layout::sentence(text)
        );
        change_page(&ctx, status, &expected, &next, &name, Some(message))
    };
    let new_password = fields.get("new_password").unwrap_or("").to_owned();
    if fields.get("new_password_again").unwrap_or("") != new_password {
        return again(StatusCode::BAD_REQUEST, "The new passwords differ.");
    }
    let current = fields.get("password").unwrap_or("").to_owned();
    let principal =
        match change_own_password(ctx.state(), addr, &name, current, new_password).await {
            Ok(p) => p,
            Err(e) if e.body().code == "invalid_request" => {
                return again(StatusCode::BAD_REQUEST, &e.body().message)
            }
            Err(e) => return again(StatusCode::UNAUTHORIZED, &e.body().message),
        };
    let refuse = |status, text: &str| {
        let message = html!(
            "<p class=\"error\" role=\"alert\">{}</p>",
            super::layout::sentence(text)
        );
        sign_in_page(&ctx, status, &expected, &next, &name, Some(message))
    };
    finish_sign_in(&ctx, &principal, &next, refuse).await
}

pub(crate) async fn sso_start(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    query: Result<Query<SignInQuery>, QueryRejection>,
) -> Response {
    let (verifier, new) = sign_in_cookie(&headers);
    let r = start_admin(ctx.state(), &headers, &verifier).await;
    let mut r = with_sign_in_cookie(&ctx, r, &verifier, new);
    // Where to go afterwards rides in a cookie of its own; the provider's round trip cannot
    // carry it.
    if let Some(next) = query.ok().and_then(|Query(q)| q.next) {
        r.headers_mut().append(
            header::SET_COOKIE,
            set_cookie(
                NEXT_COOKIE,
                &html::encode(&safe_next(Some(&next))),
                SIGN_IN_COOKIE_SECONDS,
                ctx.secure(),
                false,
            ),
        );
    }
    r
}

/// Where to go after a single sign-on.
const NEXT_COOKIE: &str = "lode_admin_next";

#[derive(Debug, Deserialize)]
pub(crate) struct DoneQuery {
    #[serde(default)]
    code: String,
}

pub(crate) async fn sso_done(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    query: Result<Query<DoneQuery>, QueryRejection>,
) -> Response {
    let (expected, new) = sign_in_cookie(&headers);
    let next = cookie(&headers, NEXT_COOKIE)
        .map(|n| super::session::decode(&n))
        .map(|n| safe_next(Some(&n)))
        .unwrap_or_else(|| "/admin/".to_owned());
    let refuse = |status, text: &str| {
        let message = html!("<p class=\"error\" role=\"alert\">{}</p>", text);
        let r = sign_in_page(&ctx, status, &expected, &next, "", Some(message));
        with_sign_in_cookie(&ctx, r, &expected, new)
    };
    let code = query.map(|Query(q)| q.code).unwrap_or_default();
    let Some(id) = (!new)
        .then(|| finish_admin(ctx.state(), &code, &expected))
        .flatten()
    else {
        return refuse(
            StatusCode::FORBIDDEN,
            "This sign-in has expired or was started in another browser. Try again.",
        );
    };
    let principal = match ctx.state().meta().read(move |tx| tx.principal(id)).await {
        Ok(Some(p)) if !p.disabled => p,
        _ => return refuse(StatusCode::FORBIDDEN, "This account is disabled."),
    };
    let admin = match would_be_server_admin(ctx.state(), &principal).await {
        Ok(a) => a,
        Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, &e.body().message),
    };
    let next = landing(&next, admin);
    // The browser arrives from the identity provider, so it would not send the new
    // SameSite=Strict cookie on a redirect; a page of this site that moves on sends it.
    let page = html!(
        "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"/>\
         <meta http-equiv=\"refresh\" content=\"0; url={}\"/><title>Signed in · Lode admin</title>\
         <link rel=\"stylesheet\" href=\"{}\"/></head><body><main id=\"main\">\
         <p>Signed in. <a href=\"{}\">Continue</a>.</p></main></body></html>\n",
        next,
        super::assets::CSS_PATH,
        next
    );
    let mut r = html_response(StatusCode::OK, page);
    r.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(NEXT_COOKIE, "", 0, ctx.secure(), false),
    );
    match start_session(&ctx, &principal, admin, r).await {
        Ok(r) => r,
        Err(message) => refuse(StatusCode::INTERNAL_SERVER_ERROR, &message),
    }
}

pub(crate) async fn sign_out(State(ctx): State<Ctx>, post: ViewerPost) -> Response {
    let session = &post.viewer.0;
    let id = session.caller.token_id.0;
    let path = super::api::path(lode_proto::consts::paths::TOKEN, &[&id.to_string()]);
    if let Err(e) = ctx.api(session).delete(&path).await {
        tracing::warn!(error = %e.message, "admin web UI: cannot delete the session token");
    }
    let mut r = Redirect::to(&format!("{SIGN_IN}?signed_out=1")).into_response();
    r.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(SESSION_COOKIE, "", 0, ctx.secure(), true),
    );
    r.headers_mut().append(
        header::SET_COOKIE,
        super::session::clear_legacy_session(ctx.secure()),
    );
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_admin_pages_are_next() {
        assert_eq!(safe_next(Some("/admin/depots/game")), "/admin/depots/game");
        assert_eq!(safe_next(Some("https://evil.example/")), "/admin/");
        assert_eq!(safe_next(Some("/admin//evil.example")), "/admin/");
        assert_eq!(safe_next(Some("/admin/sign-out")), "/admin/");
        assert_eq!(safe_next(None), "/admin/");
        assert_eq!(safe_next(Some("/web/game")), "/web/game");
        assert_eq!(landing("/admin/people", false), "/web/");
        assert_eq!(landing("/web/game", false), "/web/game");
        assert_eq!(landing("/admin/people", true), "/admin/people");
    }
}
