//! Web sessions: a cookie holding an ordinary token, and the CSRF token of its forms. The
//! admin pages take an [`Admin`] (a session of a server administrator); the pages for
//! everyone under `/web/` take a [`Viewer`] (any signed-in user, T173).
//!
//! Signing in issues a token named "web admin" that expires after [`SESSION_HOURS`] hours
//! and puts it in an `HttpOnly`, `SameSite=Strict` cookie (`Secure` when the server speaks
//! TLS). Every request authenticates that token like an API request, so a revoked token, an
//! expired one or a disabled user ends the session. Signing out deletes the token.
//!
//! Each form carries a CSRF token derived from the session token, which a page of another
//! site cannot read; a POST without the right one is refused.

use axum::extract::{FromRequest, FromRequestParts, OptionalFromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use sha2::{Digest, Sha256};

use super::html;
use super::{Ctx, Nav, Page};
use crate::auth::{authenticate, ct_eq, Caller};

/// The session cookie.
pub(crate) const SESSION_COOKIE: &str = "lode_admin";
/// A random value set before signing in: the sign-in form's CSRF token, and the PKCE
/// verifier of a single sign-on.
pub(crate) const SIGN_IN_COOKIE: &str = "lode_admin_signin";
/// A one-line note shown on the next page, such as "User ana disabled.".
pub(crate) const NOTE_COOKIE: &str = "lode_admin_note";
/// How long a session lasts.
pub(crate) const SESSION_HOURS: i64 = 8;

/// A signed-in server administrator.
#[derive(Debug, Clone)]
pub(crate) struct Admin {
    pub(crate) caller: Caller,
    token: String,
    /// The CSRF token for this session's forms.
    pub(crate) csrf: String,
    /// The note to show once, if any.
    pub(crate) note: Option<String>,
}

impl Admin {
    /// The session's token, for requests to the API.
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    /// The signed-in user's name.
    pub(crate) fn name(&self) -> &str {
        &self.caller.principal.name
    }

    /// A hidden form field with the CSRF token.
    pub(crate) fn csrf_field(&self) -> html::Html {
        html!(
            "<input type=\"hidden\" name=\"csrf\" value=\"{}\"/>",
            self.csrf
        )
    }
}

/// The CSRF token of a session.
pub(crate) fn csrf_for(token: &str) -> String {
    let digest = Sha256::digest(format!("lode admin csrf\n{token}").as_bytes());
    digest.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// The value of cookie `name`.
pub(crate) fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    cookies(headers, name).into_iter().next()
}

/// Every value of cookie `name`: a browser that kept a session cookie of an older release
/// (path `/admin`) sends it beside the current one (path `/`).
fn cookies(headers: &HeaderMap, name: &str) -> Vec<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .filter(|(k, _)| *k == name)
        .map(|(_, v)| v.trim().to_owned())
        .collect()
}

/// Removes the session cookie of releases before T173, which lived under `/admin` only.
pub(crate) fn clear_legacy_session(secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    let text = format!(
        "{SESSION_COOKIE}=; Path=/admin; Max-Age=0; HttpOnly; SameSite=Strict{secure}"
    );
    HeaderValue::from_str(&text).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// A `Set-Cookie` value for the web pages (`/admin/` and `/web/`). `max_age` 0 removes the
/// cookie. The API never reads cookies, so the path `/` gives it no way in.
pub(crate) fn set_cookie(
    name: &str,
    value: &str,
    max_age: i64,
    secure: bool,
    strict: bool,
) -> HeaderValue {
    let same_site = if strict { "Strict" } else { "Lax" };
    let secure = if secure { "; Secure" } else { "" };
    let text = format!(
        "{name}={value}; Path=/; Max-Age={max_age}; HttpOnly; SameSite={same_site}{secure}"
    );
    // Names are constants and values are base64url, hex or percent-encoded text.
    HeaderValue::from_str(&text).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// Percent-decodes a cookie value written by [`html::encode`].
pub(crate) fn decode(text: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        char::from(b)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A redirect that also sets a note to show on the next page.
pub(crate) fn redirect_with_note(ctx: &Ctx, to: &str, note: &str) -> Response {
    let mut r = Redirect::to(to).into_response();
    let value = html::encode(&note.chars().take(300).collect::<String>());
    r.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(NOTE_COOKIE, &value, 60, ctx.secure(), true),
    );
    r
}

/// Where a visitor who is not signed in goes: the sign-in page, then back to `path`.
fn to_sign_in(ctx: &Ctx, path: &str, clear: bool) -> Response {
    let next = if (path.starts_with("/admin/") && !path.starts_with("/admin/sign-"))
        || path.starts_with("/web/")
    {
        format!("{}?next={}", super::signin::SIGN_IN, html::encode(path))
    } else {
        super::signin::SIGN_IN.to_owned()
    };
    let mut r = Redirect::to(&next).into_response();
    if clear {
        r.headers_mut().append(
            header::SET_COOKIE,
            set_cookie(SESSION_COOKIE, "", 0, ctx.secure(), true),
        );
        r.headers_mut()
            .append(header::SET_COOKIE, clear_legacy_session(ctx.secure()));
    }
    r
}

/// The page for a signed-in user who is not a server administrator.
fn not_admin(ctx: &Ctx, admin: &Admin) -> Response {
    let body = html!(
        "<h1>No access</h1><p class=\"error\">This account is not a server administrator.</p>\
         <p>Signed in as <strong>{}</strong>. <a href=\"/web/\">Browse your depots</a>, or sign out \
         and sign in as a server administrator.</p>\
         <form method=\"post\" action=\"{}\">{}<button type=\"submit\">Sign out</button></form>",
        admin.name(),
        super::signin::SIGN_OUT,
        admin.csrf_field()
    );
    let mut r = Page::new("No access", Nav::None).render(ctx, None, body);
    *r.status_mut() = StatusCode::FORBIDDEN;
    r
}

/// The session of a request, if its cookie names a valid token. `Err(true)` when there was
/// a cookie that no longer works.
async fn session(parts: &Parts, ctx: &Ctx) -> Result<Admin, bool> {
    let mut found = None;
    let mut had_cookie = false;
    for token in cookies(&parts.headers, SESSION_COOKIE) {
        if token.is_empty() {
            continue;
        }
        had_cookie = true;
        if let Ok(caller) = authenticate(&ctx.state, &token).await {
            found = Some((token, caller));
            break;
        }
    }
    let (token, caller) = found.ok_or(had_cookie)?;
    let note = cookie(&parts.headers, NOTE_COOKIE)
        .filter(|n| !n.is_empty())
        .map(|n| decode(&n));
    Ok(Admin {
        csrf: csrf_for(&token),
        caller,
        token,
        note,
    })
}

impl FromRequestParts<Ctx> for Admin {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, ctx: &Ctx) -> Result<Self, Response> {
        match session(parts, ctx).await {
            Ok(admin) if ctx.state.permissions().is_server_admin(&admin.caller) => Ok(admin),
            Ok(admin) => Err(not_admin(ctx, &admin)),
            Err(had_cookie) => Err(to_sign_in(ctx, parts.uri.path(), had_cookie)),
        }
    }
}

/// Any signed-in user, for the pages under `/web/`: what they see is what the API lets
/// their token see.
#[derive(Debug, Clone)]
pub(crate) struct Viewer(pub(crate) Admin);

impl Viewer {
    /// Whether this user is also a server administrator (for a link to the admin pages).
    pub(crate) fn is_admin(&self, ctx: &Ctx) -> bool {
        ctx.state.permissions().is_server_admin(&self.0.caller)
    }
}

impl FromRequestParts<Ctx> for Viewer {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, ctx: &Ctx) -> Result<Self, Response> {
        match session(parts, ctx).await {
            Ok(s) => Ok(Viewer(s)),
            Err(had_cookie) => Err(to_sign_in(ctx, parts.uri.path(), had_cookie)),
        }
    }
}

impl OptionalFromRequestParts<Ctx> for Viewer {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        ctx: &Ctx,
    ) -> Result<Option<Self>, Self::Rejection> {
        Ok(session(parts, ctx).await.ok().map(Viewer))
    }
}

/// A form a signed-in user submitted on a `/web/` page, with its CSRF token checked.
pub(crate) struct ViewerPost {
    pub(crate) viewer: Viewer,
    pub(crate) fields: Fields,
}

impl FromRequest<Ctx> for ViewerPost {
    type Rejection = Response;

    async fn from_request(req: Request, ctx: &Ctx) -> Result<Self, Response> {
        let (mut parts, body) = req.into_parts();
        let viewer = <Viewer as FromRequestParts<Ctx>>::from_request_parts(&mut parts, ctx).await?;
        let req = Request::from_parts(parts, body);
        let Form(pairs) = Form::<Vec<(String, String)>>::from_request(req, ctx)
            .await
            .map_err(|_| expired_form(ctx, None))?;
        let fields = Fields(pairs);
        let sent = fields.get("csrf").unwrap_or("");
        if !ct_eq(sent.as_bytes(), viewer.0.csrf.as_bytes()) {
            return Err(expired_form(ctx, None));
        }
        Ok(ViewerPost { viewer, fields })
    }
}

impl OptionalFromRequestParts<Ctx> for Admin {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        ctx: &Ctx,
    ) -> Result<Option<Self>, Self::Rejection> {
        Ok(session(parts, ctx)
            .await
            .ok()
            .filter(|a| ctx.state.permissions().is_server_admin(&a.caller)))
    }
}

/// The fields of a submitted form.
#[derive(Debug, Clone, Default)]
pub(crate) struct Fields(Vec<(String, String)>);

impl Fields {
    /// A field's value, trimmed; empty when absent.
    pub(crate) fn text(&self, name: &str) -> String {
        self.get(name)
            .map(|v| v.trim().to_owned())
            .unwrap_or_default()
    }

    /// A field's value as sent.
    pub(crate) fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Whether a checkbox is ticked.
    pub(crate) fn flag(&self, name: &str) -> bool {
        self.get(name)
            .is_some_and(|v| !v.is_empty() && v != "false")
    }

    /// Every value of a field that can repeat, such as ticked boxes of one name.
    pub(crate) fn all(&self, name: &str) -> Vec<String> {
        self.0
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .collect()
    }
}

impl From<Vec<(String, String)>> for Fields {
    fn from(v: Vec<(String, String)>) -> Self {
        Self(v)
    }
}

/// A form an administrator submitted, with its CSRF token checked.
pub(crate) struct Post {
    pub(crate) admin: Admin,
    pub(crate) fields: Fields,
}

/// The page for a form whose CSRF token is missing or wrong.
pub(crate) fn expired_form(ctx: &Ctx, admin: Option<&Admin>) -> Response {
    let body = html!(
        "<h1>Form expired</h1><p class=\"error\">This form has expired or did not come from \
         this server. Go back, reload the page and try again.</p>"
    );
    let mut r = Page::new("Form expired", Nav::None).render(ctx, admin, body);
    *r.status_mut() = StatusCode::FORBIDDEN;
    r
}

impl FromRequest<Ctx> for Post {
    type Rejection = Response;

    async fn from_request(req: Request, ctx: &Ctx) -> Result<Self, Response> {
        let (mut parts, body) = req.into_parts();
        let admin = <Admin as FromRequestParts<Ctx>>::from_request_parts(&mut parts, ctx).await?;
        let req = Request::from_parts(parts, body);
        let Form(pairs) = Form::<Vec<(String, String)>>::from_request(req, ctx)
            .await
            .map_err(|_| expired_form(ctx, Some(&admin)))?;
        let fields = Fields(pairs);
        let sent = fields.get("csrf").unwrap_or("");
        if !ct_eq(sent.as_bytes(), admin.csrf.as_bytes()) {
            return Err(expired_form(ctx, Some(&admin)));
        }
        Ok(Post { admin, fields })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_are_read_and_written() {
        let mut h = HeaderMap::new();
        h.append(
            header::COOKIE,
            HeaderValue::from_static("a=1; lode_admin=lode_x"),
        );
        h.append(
            header::COOKIE,
            HeaderValue::from_static("lode_admin_note=User%20ana"),
        );
        assert_eq!(cookie(&h, SESSION_COOKIE).as_deref(), Some("lode_x"));
        assert_eq!(
            cookie(&h, NOTE_COOKIE).map(|n| decode(&n)).as_deref(),
            Some("User ana")
        );
        assert_eq!(cookie(&h, "b"), None);
        let v = set_cookie(SESSION_COOKIE, "t", 60, true, true);
        assert_eq!(
            v.to_str().unwrap_or(""),
            "lode_admin=t; Path=/; Max-Age=60; HttpOnly; SameSite=Strict; Secure"
        );
        let v = set_cookie(SIGN_IN_COOKIE, "t", 60, false, false);
        assert!(v.to_str().unwrap_or("").ends_with("SameSite=Lax"));
    }

    #[test]
    fn csrf_tokens_differ_per_session() {
        assert_eq!(csrf_for("a").len(), 32);
        assert_eq!(csrf_for("a"), csrf_for("a"));
        assert_ne!(csrf_for("a"), csrf_for("b"));
    }

    #[test]
    fn decoding_keeps_broken_escapes() {
        assert_eq!(decode("a%2Fb%"), "a/b%");
        assert_eq!(decode("%zz"), "%zz");
        assert_eq!(decode("%C3%A9"), "é");
    }
}
