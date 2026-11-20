//! The frame of every admin page: the top bar, the navigation and the note of the last
//! action.

use axum::http::{header, StatusCode};
use axum::response::Response;

use super::html::Html;
use super::session::{set_cookie, NOTE_COOKIE};
use super::{html_response, Admin, Ctx};
use crate::config::TlsConfig;
use crate::html;

/// The section of the navigation a page belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Nav {
    /// Pages outside the navigation (sign-in, errors).
    None,
    Overview,
    Depots,
    People,
    Access,
    Webhooks,
    Settings,
}

const SECTIONS: [(Nav, &str, &str); 6] = [
    (Nav::Overview, "/admin/", "Overview"),
    (Nav::Depots, "/admin/depots", "Depots"),
    (Nav::People, "/admin/people", "People"),
    (Nav::Access, "/admin/access", "Access"),
    (Nav::Webhooks, "/admin/webhooks", "Webhooks"),
    (Nav::Settings, "/admin/settings", "Settings"),
];

/// One page's frame.
pub(crate) struct Page<'a> {
    title: &'a str,
    nav: Nav,
    refresh: Option<u32>,
}

/// The name the top bar shows for this server: its public address when one is configured,
/// otherwise the address it listens on.
pub(crate) fn server_name(ctx: &Ctx) -> String {
    let cfg = ctx.state().config();
    if let Some(TlsConfig::Acme(a)) = &cfg.tls {
        return a.domain.clone();
    }
    if let Some(host) = cfg
        .oidc
        .as_ref()
        .and_then(|o| o.public_url.as_deref())
        .and_then(|u| reqwest::Url::parse(u).ok())
        .and_then(|u| u.host_str().map(str::to_owned))
    {
        return host;
    }
    ctx.state().listening().to_string()
}

impl<'a> Page<'a> {
    pub(crate) fn new(title: &'a str, nav: Nav) -> Self {
        Self {
            title,
            nav,
            refresh: None,
        }
    }

    /// Reloads the page's main part every `seconds` while it is open (with JavaScript).
    pub(crate) fn refresh(mut self, seconds: u32) -> Self {
        self.refresh = Some(seconds);
        self
    }

    /// The whole page around `body`, for `admin` (or nobody, before signing in).
    pub(crate) fn render(&self, ctx: &Ctx, admin: Option<&Admin>, body: Html) -> Response {
        let webhooks = ctx.state().module_names().iter().any(|m| m == "webhooks");
        let nav: Html = match admin {
            Some(_) => SECTIONS
                .iter()
                .filter(|(n, _, _)| *n != Nav::Webhooks || webhooks)
                .map(|(n, href, label)| {
                    if *n == self.nav {
                        html!("<a href=\"{}\" aria-current=\"page\">{}</a>", *href, *label)
                    } else {
                        html!("<a href=\"{}\">{}</a>", *href, *label)
                    }
                })
                .collect(),
            None => Html::new(),
        };
        let nav = if nav.is_empty() {
            nav
        } else {
            html!("<nav aria-label=\"Sections\">{}</nav>", nav)
        };
        let who = match admin {
            Some(a) => html!(
                "<div class=\"who\"><span>{}</span><form method=\"post\" action=\"{}\">{}\
                 <button type=\"submit\" class=\"link\">Sign out</button></form></div>",
                a.name(),
                super::signin::SIGN_OUT,
                a.csrf_field()
            ),
            None => Html::new(),
        };
        let note = match admin.and_then(|a| a.note.as_deref()) {
            Some(n) => html!("<p class=\"note\" role=\"status\">{}</p>", n),
            None => Html::new(),
        };
        let refresh = match self.refresh {
            Some(s) => html!(" data-refresh=\"{}\"", s),
            None => Html::new(),
        };
        let page = html!(
            "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"/>\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"/>\
             <meta name=\"color-scheme\" content=\"light dark\"/>\
             <title>{} · Lode admin</title>\
             <link rel=\"stylesheet\" href=\"{}\"/>\
             <script src=\"{}\" defer=\"defer\"></script></head>\
             <body{}><header class=\"top\"><div class=\"brand\"><a href=\"/admin/\">Lode admin</a>\
             <span class=\"server\">{} · {}</span></div>{}{}</header>\
             <main id=\"main\">{}{}</main></body></html>\n",
            self.title,
            super::assets::CSS_PATH,
            super::assets::JS_PATH,
            refresh,
            server_name(ctx),
            lode_build::VERSION,
            nav,
            who,
            note,
            body
        );
        let mut r = html_response(StatusCode::OK, page);
        if admin.is_some_and(|a| a.note.is_some()) {
            r.headers_mut().append(
                header::SET_COOKIE,
                set_cookie(NOTE_COOKIE, "", 0, ctx.secure(), true),
            );
        }
        r
    }
}

/// An API error as a paragraph.
pub(crate) fn error_text(e: &lode_proto::error::ErrorBody) -> Html {
    html!(
        "<p class=\"error\" role=\"alert\">{}</p>",
        sentence(&e.message)
    )
}

/// A message as a sentence: capital first letter, full stop at the end.
pub(crate) fn sentence(text: &str) -> String {
    let mut chars = text.trim().chars();
    let mut out: String = match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => return String::new(),
    };
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentences() {
        assert_eq!(sentence("no such depot"), "No such depot.");
        assert_eq!(sentence("Done."), "Done.");
        assert_eq!(sentence(""), "");
    }
}
