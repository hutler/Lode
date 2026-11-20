//! Access rules (T142): the rules in evaluation order, adding and removing them, and checking
//! what a principal gets on a path, with the rule that decided it.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use lode_proto::admin::{AccessCheck, AccessLevel, AclEffect, AclList, AclRule, AclSubject};
use lode_proto::consts::paths;
use lode_proto::depot::DepotList;
use serde::Deserialize;

use super::api::path;
use super::html::{encode, Html};
use super::layout::{error_text, sentence};
use super::session::{redirect_with_note, Post};
use super::{html, Admin, Ctx, Nav, Page};

/// What the check form asks.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct CheckQuery {
    #[serde(default)]
    principal: String,
    #[serde(default)]
    depot: String,
    #[serde(default)]
    branch: String,
    #[serde(default)]
    path: String,
    /// Rules list filter: only rules that can apply in this depot.
    #[serde(default)]
    in_depot: String,
    /// Rules list filter: a user or group name, or `all` for rules for everyone.
    #[serde(default)]
    who: String,
}

impl CheckQuery {
    /// Whether the rules list shows `r` (the same rules `GET /admin/acl?depot=&subject=` gives).
    fn shows(&self, r: &AclRule) -> bool {
        let depot_ok = self.in_depot.is_empty()
            || r.depot.is_none()
            || r.depot.as_deref() == Some(self.in_depot.as_str());
        let who_ok = match (self.who.as_str(), &r.subject) {
            ("", _) => true,
            ("all", AclSubject::All) => true,
            (w, AclSubject::User(n) | AclSubject::Group(n)) => n == w,
            _ => false,
        };
        depot_ok && who_ok
    }
}

fn level_text(l: Option<AccessLevel>) -> &'static str {
    match l {
        None => "no access",
        Some(AccessLevel::Read) => "read",
        Some(AccessLevel::Write) => "write",
        Some(AccessLevel::Admin) => "admin",
    }
}

fn subject_text(s: &AclSubject) -> Html {
    match s {
        AclSubject::All => html!("everyone"),
        AclSubject::Agents => html!("every agent"),
        AclSubject::User(n) => html!("user <a href=\"/admin/people/{}\">{}</a>", encode(n), n),
        AclSubject::Group(g) => html!("group {}", g),
    }
}

/// A rule in words: `allow write to group artists on main, Content/**, in game`.
fn rule_text(r: &AclRule) -> Html {
    let effect = match r.effect {
        AclEffect::Allow => "allow",
        AclEffect::Deny => "deny",
    };
    html!(
        "{} {} to {} on branches <code>{}</code>, paths <code>{}</code>, in {}",
        effect,
        level_text(Some(r.level)),
        subject_text(&r.subject),
        r.branch,
        r.path,
        r.depot.as_deref().unwrap_or("every depot")
    )
}

async fn page(ctx: &Ctx, admin: &Admin, q: &CheckQuery, top: Html) -> Response {
    let api = ctx.api(admin);
    let rules = match api.get::<AclList>(paths::ADMIN_ACL).await {
        Ok(l) => l.rules,
        Err(e) => {
            let body = html!("<h1>Access</h1>{}", error_text(&e));
            return Page::new("Access", Nav::Access).render(ctx, Some(admin), body);
        }
    };
    let depots = api
        .get::<DepotList>(paths::DEPOTS)
        .await
        .map(|d| d.depots)
        .unwrap_or_default();

    // The check, when the form was filled in.
    let check = if q.principal.is_empty() && q.depot.is_empty() && q.path.is_empty() {
        None
    } else {
        let mut url = format!(
            "{}?principal={}&depot={}&path={}",
            paths::ADMIN_ACL_CHECK,
            encode(&q.principal),
            encode(&q.depot),
            encode(&q.path)
        );
        if !q.branch.is_empty() {
            url.push_str(&format!("&branch={}", encode(&q.branch)));
        }
        Some(api.get::<AccessCheck>(&url).await)
    };
    let decided = check
        .as_ref()
        .and_then(|c| c.as_ref().ok())
        .and_then(|c| c.decided_by);
    let matched: Vec<i64> = check
        .as_ref()
        .and_then(|c| c.as_ref().ok())
        .map(|c| c.matched.clone())
        .unwrap_or_default();

    let csrf = admin.csrf_field();
    let shown = rules.iter().enumerate().filter(|(_, r)| q.shows(r)).count();
    let rows: Html = rules
        .iter()
        .enumerate()
        .filter(|(_, r)| q.shows(r))
        .map(|(i, r)| {
            let id = r.id.unwrap_or_default();
            let mark = if Some(id) == decided {
                html!(" class=\"decided\"")
            } else if matched.contains(&id) {
                html!(" class=\"matched\"")
            } else {
                Html::new()
            };
            let effect = match r.effect {
                AclEffect::Allow => html!("<span class=\"ok\">allow</span>"),
                AclEffect::Deny => html!("<span class=\"bad\">deny</span>"),
            };
            html!(
                "<tr id=\"rule-{}\"{}><td class=\"num\" data-label=\"Order\">{}</td>\
                 <td class=\"num\" data-label=\"Rule\">#{}</td><td data-label=\"Depot\">{}</td>\
                 <td data-label=\"Who\">{}</td><td data-label=\"Branches\"><code>{}</code></td>\
                 <td data-label=\"Paths\"><code>{}</code></td><td data-label=\"Effect\">{}</td>\
                 <td data-label=\"Level\">{}</td><td>\
                 <form method=\"post\" action=\"/admin/access/rules/{}/remove\" data-confirm=\"Remove rule #{}?\">{}\
                 <button type=\"submit\" class=\"danger\">Remove</button></form></td></tr>",
                id,
                mark,
                i + 1,
                id,
                r.depot.as_deref().unwrap_or("every depot"),
                subject_text(&r.subject),
                r.branch,
                r.path,
                effect,
                level_text(Some(r.level)),
                id,
                id,
                csrf.clone()
            )
        })
        .collect();
    let table = if rules.is_empty() {
        html!("<p class=\"muted\">No rules: only administrators see anything.</p>")
    } else if shown == 0 {
        html!("<p class=\"muted\">No rule matches the filter.</p>")
    } else {
        html!(
            "<table class=\"wide\"><thead><tr><th class=\"num\">Order</th><th class=\"num\">Rule</th>\
             <th>Depot</th><th>Who</th><th>Branches</th><th>Paths</th><th>Effect</th><th>Level</th>\
             <th></th></tr></thead><tbody>{}</tbody></table>",
            rows
        )
    };
    let depot_options = |selected: &str, every: bool| -> Html {
        let mut out = if every {
            html!("<option value=\"\">Every depot</option>")
        } else {
            Html::new()
        };
        for d in &depots {
            out.push(if d.name == selected {
                html!(
                    "<option value=\"{}\" selected=\"selected\">{}</option>",
                    d.name,
                    d.name
                )
            } else {
                html!("<option value=\"{}\">{}</option>", d.name, d.name)
            });
        }
        out
    };

    let result = match &check {
        None => Html::new(),
        Some(Err(e)) => error_text(e),
        Some(Ok(c)) => {
            let why = if c.depot_admin {
                match c
                    .decided_by
                    .and_then(|id| rules.iter().find(|r| r.id == Some(id)))
                {
                    Some(r) => html!(
                        "{} administers {}, which passes over path rules. Rule #{}: {}.",
                        c.principal,
                        c.depot,
                        r.id.unwrap_or_default(),
                        rule_text(r)
                    ),
                    None => html!("{} is the built-in administrator.", c.principal),
                }
            } else {
                match c
                    .decided_by
                    .and_then(|id| rules.iter().find(|r| r.id == Some(id)))
                {
                    Some(r) => html!(
                        "Decided by rule #{}: {}.",
                        r.id.unwrap_or_default(),
                        rule_text(r)
                    ),
                    None if c.matched.is_empty() => html!("No rule applies to this path."),
                    None => html!("No rule that applies allows anything here."),
                }
            };
            html!(
                "<div class=\"card\" id=\"check-result\" role=\"status\"><p><strong>{}</strong> gets \
                 <strong>{}</strong> on <code>{}</code> of branch {} in {}.</p><p>{}</p></div>",
                c.principal,
                level_text(c.level),
                c.path.as_str(),
                c.branch,
                c.depot,
                why
            )
        }
    };
    let branch = if q.branch.is_empty() {
        "main"
    } else {
        q.branch.as_str()
    };
    let body = html!(
        "<h1>Access</h1>{}\
         <section><h2 id=\"rules\">Rules</h2><p class=\"muted\">Rules are read in this order. \
         Without an allow, a path is invisible; a deny at a level takes away that level and every \
         higher one; otherwise the highest allow counts. An allow admin on every depot makes a \
         server administrator.</p>\
         <form class=\"row\" method=\"get\" action=\"/admin/access#rules\">\
         <label>In depot<select name=\"in_depot\">{}</select></label>\
         <label>For<input type=\"text\" name=\"who\" value=\"{}\" placeholder=\"ana, artists or all\"/></label>\
         <button type=\"submit\">Filter</button></form>{}</section>\
         <section><h2 id=\"check\">Check access</h2>\
         <form class=\"row\" method=\"get\" action=\"/admin/access#check\">\
         <label>Who<input type=\"text\" name=\"principal\" value=\"{}\" required=\"required\" placeholder=\"ana\"/></label>\
         <label>Depot<select name=\"depot\">{}</select></label>\
         <label>Branch<input type=\"text\" name=\"branch\" value=\"{}\"/></label>\
         <label>Path<input type=\"text\" name=\"path\" value=\"{}\" required=\"required\" placeholder=\"Content/hero.psd\"/></label>\
         <button type=\"submit\">Check</button></form>{}</section>\
         <section><h2 id=\"add\">Add a rule</h2>\
         <form class=\"row\" method=\"post\" action=\"/admin/access/rules\">{}\
         <label>Depot<select name=\"depot\">{}</select></label>\
         <label>Who<select name=\"subject\"><option value=\"user\">User</option>\
         <option value=\"group\">Group</option><option value=\"all\">Everyone</option><option value=\"agents\">Every agent</option></select></label>\
         <label>Name<input type=\"text\" name=\"name\" placeholder=\"ana or artists\"/></label>\
         <label>Branches<input type=\"text\" name=\"branch\" value=\"**\" required=\"required\"/></label>\
         <label>Paths<input type=\"text\" name=\"path\" value=\"**\" required=\"required\"/></label>\
         <label>Effect<select name=\"effect\"><option value=\"allow\">Allow</option>\
         <option value=\"deny\">Deny</option></select></label>\
         <label>Level<select name=\"level\"><option value=\"read\">Read</option>\
         <option value=\"write\">Write</option><option value=\"admin\">Admin</option></select></label>\
         <button type=\"submit\" class=\"primary\">Add rule</button></form></section>",
        top,
        depot_options(&q.in_depot, true),
        q.who,
        table,
        q.principal,
        depot_options(&q.depot, false),
        branch,
        q.path,
        result,
        csrf,
        depot_options("", true)
    );
    Page::new("Access", Nav::Access).render(ctx, Some(admin), body)
}

pub(crate) async fn show(
    State(ctx): State<Ctx>,
    admin: Admin,
    q: Result<Query<CheckQuery>, QueryRejection>,
) -> Response {
    let q = q.map(|Query(q)| q).unwrap_or_default();
    page(&ctx, &admin, &q, Html::new()).await
}

fn rule_from(post: &Post) -> Result<AclRule, String> {
    let f = &post.fields;
    let name = f.text("name");
    let subject = match f.text("subject").as_str() {
        "all" => AclSubject::All,
        "agents" => AclSubject::Agents,
        "group" if !name.is_empty() => AclSubject::Group(name),
        "user" if !name.is_empty() => AclSubject::User(name),
        "group" | "user" => return Err("name the user or group the rule is for".into()),
        other => return Err(format!("unknown subject `{other}`")),
    };
    let effect = match f.text("effect").as_str() {
        "allow" => AclEffect::Allow,
        "deny" => AclEffect::Deny,
        other => return Err(format!("unknown effect `{other}`")),
    };
    let level = match f.text("level").as_str() {
        "read" => AccessLevel::Read,
        "write" => AccessLevel::Write,
        "admin" => AccessLevel::Admin,
        other => return Err(format!("unknown level `{other}`")),
    };
    let depot = f.text("depot");
    Ok(AclRule {
        id: None,
        depot: (!depot.is_empty()).then_some(depot),
        subject,
        branch: f.text("branch"),
        path: f.text("path"),
        effect,
        level,
    })
}

pub(crate) async fn add(State(ctx): State<Ctx>, post: Post) -> Response {
    let rule = match rule_from(&post) {
        Ok(r) => r,
        Err(e) => {
            let top = html!("<p class=\"error\" role=\"alert\">{}</p>", sentence(&e));
            return page(&ctx, &post.admin, &CheckQuery::default(), top).await;
        }
    };
    match ctx
        .api(&post.admin)
        .post::<_, AclRule>(paths::ADMIN_ACL, &rule)
        .await
    {
        Ok(r) => redirect_with_note(
            &ctx,
            "/admin/access#rules",
            &format!("Rule #{} added.", r.id.unwrap_or_default()),
        ),
        Err(e) => page(&ctx, &post.admin, &CheckQuery::default(), error_text(&e)).await,
    }
}

pub(crate) async fn remove(State(ctx): State<Ctx>, Path(id): Path<i64>, post: Post) -> Response {
    let url = path(paths::ADMIN_ACL_RULE, &[&id.to_string()]);
    match ctx.api(&post.admin).delete(&url).await {
        Ok(()) => redirect_with_note(&ctx, "/admin/access#rules", &format!("Rule #{id} removed.")),
        Err(e) => page(&ctx, &post.admin, &CheckQuery::default(), error_text(&e)).await,
    }
}
