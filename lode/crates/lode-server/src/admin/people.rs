//! People (T142): users, agents and groups; one principal's state, password, single
//! sign-on identity and tokens.

use axum::extract::{Path, State};
use axum::response::Response;
use lode_proto::admin::{
    CreateGroupRequest, CreatePrincipalRequest, DeletePrincipalResponse, GroupList,
    PatchPrincipalRequest, PrincipalList,
};
use lode_proto::auth::{CreateTokenRequest, CreateTokenResponse, PrincipalView, TokenList};
use lode_proto::consts::paths;
use lode_proto::PrincipalKind;

use super::api::path;
use super::format::at;
use super::html::{encode, Html};
use super::layout::{error_text, sentence};
use super::session::{redirect_with_note, Post};
use super::{html, Admin, Ctx, Nav, Page};

/// The page of principal `name`.
pub(crate) fn person_href(name: &str) -> String {
    format!("/admin/people/{}", encode(name))
}

fn state_badge(p: &PrincipalView) -> Html {
    if p.disabled {
        html!("<span class=\"badge bad\">disabled</span>")
    } else {
        html!("<span class=\"badge ok\">active</span>")
    }
}

fn link(name: &str) -> Html {
    html!("<a href=\"{}\">{}</a>", person_href(name), name)
}

async fn list_page(ctx: &Ctx, admin: &Admin, top: Html) -> Response {
    let api = ctx.api(admin);
    let (principals, groups) = match (
        api.get::<PrincipalList>(paths::ADMIN_PRINCIPALS).await,
        api.get::<GroupList>(paths::ADMIN_GROUPS).await,
    ) {
        (Ok(p), Ok(g)) => (p.principals, g.groups),
        (Err(e), _) | (_, Err(e)) => {
            let body = html!("<h1>People</h1>{}", error_text(&e));
            return Page::new("People", Nav::People).render(ctx, Some(admin), body);
        }
    };
    let csrf = admin.csrf_field();
    let users: Html = principals
        .iter()
        .filter(|p| p.kind == PrincipalKind::User)
        .map(|p| {
            html!(
                "<tr><td data-label=\"Name\">{}</td><td data-label=\"Display name\">{}</td>\
                 <td data-label=\"Single sign-on\">{}</td><td data-label=\"State\">{}</td></tr>",
                link(&p.name),
                p.display_name.as_deref().unwrap_or(""),
                p.sso.as_deref().unwrap_or(""),
                state_badge(p)
            )
        })
        .collect();
    let agents: Html = principals
        .iter()
        .filter(|p| p.kind == PrincipalKind::Agent)
        .map(|p| {
            html!(
                "<tr><td data-label=\"Name\">{}</td><td data-label=\"Owner\">{}</td>\
                 <td data-label=\"State\">{}</td></tr>",
                link(&p.name),
                p.owner.as_deref().unwrap_or(""),
                state_badge(p)
            )
        })
        .collect();
    let agents = if agents.is_empty() {
        html!("<p class=\"muted\">No agents yet.</p>")
    } else {
        html!(
            "<table class=\"wide\"><thead><tr><th>Name</th><th>Owner</th><th>State</th></tr>\
             </thead><tbody>{}</tbody></table>",
            agents
        )
    };
    let owners: Html = principals
        .iter()
        .filter(|p| p.kind == PrincipalKind::User && !p.disabled)
        .map(|p| html!("<option value=\"{}\">{}</option>", p.name, p.name))
        .collect();
    let group_rows: Html = groups
        .iter()
        .map(|g| {
            let members: Html = g
                .members
                .iter()
                .map(|m| {
                    html!(
                        "<li>{} <form class=\"inline\" method=\"post\" action=\"/admin/people/groups/{}/remove\">{}\
                         <input type=\"hidden\" name=\"name\" value=\"{}\"/>\
                         <button type=\"submit\" class=\"link\" aria-label=\"Remove {} from {}\">remove</button></form></li>",
                        link(m),
                        encode(&g.name),
                        csrf.clone(),
                        m,
                        m,
                        g.name
                    )
                })
                .collect();
            html!(
                "<tr><td data-label=\"Group\">{}</td><td data-label=\"Members\"><ul class=\"members\">{}</ul>\
                 <form class=\"row\" method=\"post\" action=\"/admin/people/groups/{}/add\">{}\
                 <input type=\"text\" name=\"name\" placeholder=\"name\" aria-label=\"Member to add to {}\" required=\"required\"/>\
                 <button type=\"submit\">Add</button></form></td></tr>",
                g.name,
                members,
                encode(&g.name),
                csrf.clone(),
                g.name
            )
        })
        .collect();
    let groups = if groups.is_empty() {
        html!("<p class=\"muted\">No groups yet.</p>")
    } else {
        html!(
            "<table class=\"wide\"><thead><tr><th>Group</th><th>Members</th></tr></thead>\
             <tbody>{}</tbody></table>",
            group_rows
        )
    };
    let body = html!(
        "<h1>People</h1>{}\
         <section><h2 id=\"users\">Users</h2><table class=\"wide\"><thead><tr><th>Name</th>\
         <th>Display name</th><th>Single sign-on</th><th>State</th></tr></thead><tbody>{}</tbody></table>\
         <h2 class=\"spaced\">Add a user</h2>\
         <form class=\"stack\" method=\"post\" action=\"/admin/people/users\">{}\
         <label>Name<input type=\"text\" name=\"name\" required=\"required\" placeholder=\"ana\"/></label>\
         <label>Display name<input type=\"text\" name=\"display_name\" placeholder=\"Ana Lima\"/></label>\
         <label>Password (or leave empty)<input type=\"password\" name=\"password\" autocomplete=\"new-password\"/></label>\
         <label>Single sign-on identity (or leave empty)<input type=\"text\" name=\"sso\" placeholder=\"ana@example.com\"/></label>\
         <button type=\"submit\" class=\"primary\">Add user</button></form></section>\
         <section><h2 id=\"agents\">Agents</h2>{}<h2 class=\"spaced\">Add an agent</h2>\
         <form class=\"row\" method=\"post\" action=\"/admin/people/agents\">{}\
         <label>Name<input type=\"text\" name=\"name\" required=\"required\" placeholder=\"build-bot\"/></label>\
         <label>Owner<select name=\"owner\">{}</select></label>\
         <button type=\"submit\" class=\"primary\">Add agent</button></form></section>\
         <section><h2 id=\"groups\">Groups</h2>{}<h2 class=\"spaced\">Add a group</h2>\
         <form class=\"row\" method=\"post\" action=\"/admin/people/groups\">{}\
         <label>Name<input type=\"text\" name=\"name\" required=\"required\" placeholder=\"artists\"/></label>\
         <button type=\"submit\" class=\"primary\">Add group</button></form></section>",
        top,
        users,
        csrf.clone(),
        agents,
        csrf.clone(),
        owners,
        groups,
        csrf
    );
    Page::new("People", Nav::People).render(ctx, Some(admin), body)
}

pub(crate) async fn list(State(ctx): State<Ctx>, admin: Admin) -> Response {
    list_page(&ctx, &admin, Html::new()).await
}

fn optional(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

async fn create(ctx: &Ctx, post: &Post, kind: PrincipalKind) -> Response {
    let f = &post.fields;
    let name = f.text("name");
    let req = CreatePrincipalRequest {
        kind,
        name: name.clone(),
        display_name: optional(f.text("display_name")),
        owner: optional(f.text("owner")),
        password: f
            .get("password")
            .filter(|p| !p.is_empty())
            .map(str::to_owned),
        sso: optional(f.text("sso")),
    };
    match ctx
        .api(&post.admin)
        .post_ok(paths::ADMIN_PRINCIPALS, &req)
        .await
    {
        Ok(()) => {
            let what = if kind == PrincipalKind::User {
                "User"
            } else {
                "Agent"
            };
            redirect_with_note(ctx, &person_href(&name), &format!("{what} {name} added."))
        }
        Err(e) => list_page(ctx, &post.admin, error_text(&e)).await,
    }
}

pub(crate) async fn create_user(State(ctx): State<Ctx>, post: Post) -> Response {
    create(&ctx, &post, PrincipalKind::User).await
}

pub(crate) async fn create_agent(State(ctx): State<Ctx>, post: Post) -> Response {
    create(&ctx, &post, PrincipalKind::Agent).await
}

pub(crate) async fn create_group(State(ctx): State<Ctx>, post: Post) -> Response {
    let name = post.fields.text("name");
    let req = CreateGroupRequest { name: name.clone() };
    match ctx
        .api(&post.admin)
        .post_ok(paths::ADMIN_GROUPS, &req)
        .await
    {
        Ok(()) => redirect_with_note(
            &ctx,
            "/admin/people#groups",
            &format!("Group {name} added."),
        ),
        Err(e) => list_page(&ctx, &post.admin, error_text(&e)).await,
    }
}

async fn member(ctx: &Ctx, post: &Post, group: &str, add: bool) -> Response {
    let name = post.fields.text("name");
    let url = path(paths::ADMIN_GROUP_MEMBER, &[group, &name]);
    let api = ctx.api(&post.admin);
    let r = if add {
        api.put(&url).await
    } else {
        api.delete(&url).await
    };
    match r {
        Ok(()) => {
            let note = if add {
                format!("{name} added to {group}.")
            } else {
                format!("{name} removed from {group}.")
            };
            redirect_with_note(ctx, "/admin/people#groups", &note)
        }
        Err(e) => list_page(ctx, &post.admin, error_text(&e)).await,
    }
}

pub(crate) async fn add_member(
    State(ctx): State<Ctx>,
    Path(group): Path<String>,
    post: Post,
) -> Response {
    member(&ctx, &post, &group, true).await
}

pub(crate) async fn remove_member(
    State(ctx): State<Ctx>,
    Path(group): Path<String>,
    post: Post,
) -> Response {
    member(&ctx, &post, &group, false).await
}

/// One principal's page, with an optional result or error at the top.
async fn person_page(ctx: &Ctx, admin: &Admin, name: &str, top: Html) -> Response {
    let api = ctx.api(admin);
    let (principals, groups) = match (
        api.get::<PrincipalList>(paths::ADMIN_PRINCIPALS).await,
        api.get::<GroupList>(paths::ADMIN_GROUPS).await,
    ) {
        (Ok(p), Ok(g)) => (p.principals, g.groups),
        (Err(e), _) | (_, Err(e)) => {
            let body = html!("<h1>{}</h1>{}", name, error_text(&e));
            return Page::new(name, Nav::People).render(ctx, Some(admin), body);
        }
    };
    let Some(p) = principals.iter().find(|p| p.name == name) else {
        let body = html!(
            "<h1>{}</h1><p class=\"error\">No such user or agent.</p>",
            name
        );
        let mut r = Page::new(name, Nav::People).render(ctx, Some(admin), body);
        *r.status_mut() = axum::http::StatusCode::NOT_FOUND;
        return r;
    };
    let href = person_href(name);
    let csrf = admin.csrf_field();
    let user = p.kind == PrincipalKind::User;
    let in_groups: Vec<&str> = groups
        .iter()
        .filter(|g| g.members.iter().any(|m| m == name))
        .map(|g| g.name.as_str())
        .collect();
    let mut facts = html!(
        "<dt>Kind</dt><dd>{}</dd><dt>State</dt><dd>{}</dd>",
        if user { "User" } else { "Agent" },
        state_badge(p)
    );
    if let Some(d) = &p.display_name {
        facts.push(html!("<dt>Display name</dt><dd>{}</dd>", d));
    }
    if let Some(o) = &p.owner {
        facts.push(html!("<dt>Owner</dt><dd>{}</dd>", link(o)));
    }
    if user {
        let sso = match &p.sso {
            Some(s) => html!("{}", s),
            None => html!("<span class=\"muted\">Not linked</span>"),
        };
        facts.push(html!("<dt>Single sign-on</dt><dd>{}</dd>", sso));
    }
    facts.push(html!(
        "<dt>Groups</dt><dd>{}</dd>",
        if in_groups.is_empty() {
            "None".to_owned()
        } else {
            in_groups.join(", ")
        }
    ));

    let toggle = if p.disabled {
        html!(
            "<form method=\"post\" action=\"{}/state\">{}<input type=\"hidden\" name=\"disabled\" value=\"false\"/>\
             <button type=\"submit\">Enable</button></form>",
            href,
            csrf.clone()
        )
    } else {
        html!(
            "<form method=\"post\" action=\"{}/state\" data-confirm=\"Disable {}? Their tokens stop working at once.\">{}\
             <input type=\"hidden\" name=\"disabled\" value=\"true\"/>\
             <button type=\"submit\" class=\"danger\">Disable</button></form>",
            href,
            name,
            csrf.clone()
        )
    };
    let user_forms = if user {
        html!(
            "<form class=\"row\" method=\"post\" action=\"{}/password\">{}\
             <label>New password<input type=\"password\" name=\"password\" autocomplete=\"new-password\" required=\"required\"/></label>\
             <button type=\"submit\">Set password</button></form>\
             <form class=\"row\" method=\"post\" action=\"{}/sso\">{}\
             <label>Single sign-on identity<input type=\"text\" name=\"sso\" value=\"{}\" placeholder=\"ana@example.com\"/></label>\
             <button type=\"submit\">Link</button></form>{}",
            href,
            csrf.clone(),
            href,
            csrf.clone(),
            p.sso.as_deref().unwrap_or(""),
            if p.sso.is_some() {
                html!(
                    "<form method=\"post\" action=\"{}/sso\">{}<input type=\"hidden\" name=\"sso\" value=\"\"/>\
                     <button type=\"submit\">Unlink single sign-on</button></form>",
                    href,
                    csrf.clone()
                )
            } else {
                Html::new()
            }
        )
    } else {
        Html::new()
    };

    let tokens = match api
        .get::<TokenList>(&path(paths::ADMIN_PRINCIPAL_TOKENS, &[name]))
        .await
    {
        Ok(t) if t.tokens.is_empty() => html!("<p class=\"muted\">No tokens.</p>"),
        Ok(t) => {
            let rows: Html = t
                .tokens
                .iter()
                .map(|t| {
                    let expires = match t.expires_at {
                        Some(e) => at(e),
                        None => html!("<span class=\"muted\">Never</span>"),
                    };
                    let used = match t.last_used_at {
                        Some(u) => at(u),
                        None => html!("<span class=\"muted\">Never</span>"),
                    };
                    html!(
                        "<tr><td data-label=\"Name\">{}</td><td data-label=\"Scopes\">{}</td>\
                         <td data-label=\"Created\">{}</td><td data-label=\"Expires\">{}</td>\
                         <td data-label=\"Last used\">{}</td><td>\
                         <form method=\"post\" action=\"{}/tokens/{}/revoke\" data-confirm=\"Revoke the token {}?\">{}\
                         <button type=\"submit\" class=\"danger\">Revoke</button></form></td></tr>",
                        t.name,
                        t.scopes.join(" "),
                        at(t.created_at),
                        expires,
                        used,
                        href,
                        t.id,
                        t.name,
                        csrf.clone()
                    )
                })
                .collect();
            html!(
                "<table class=\"wide\"><thead><tr><th>Name</th><th>Scopes</th><th>Created</th>\
                 <th>Expires</th><th>Last used</th><th></th></tr></thead><tbody>{}</tbody></table>",
                rows
            )
        }
        Err(e) => error_text(&e),
    };
    let delete = if name == crate::auth::ADMIN_USER {
        Html::new()
    } else {
        let owned: Vec<&str> = principals
            .iter()
            .filter(|a| a.kind == PrincipalKind::Agent && a.owner.as_deref() == Some(name))
            .map(|a| a.name.as_str())
            .collect();
        let agents_note = if owned.is_empty() {
            Html::new()
        } else {
            html!(" Their agents are deleted with them: {}.", owned.join(", "))
        };
        html!(
            "<section><h2 id=\"delete\">Delete</h2>\
             <p class=\"muted\">{} can no longer sign in and disappears from lists; tokens, group \
             memberships, access rules and locks go.{} The name stays in history and cannot be \
             given to anyone else. To only stop access for a while, disable instead.</p>\
             <form class=\"row\" method=\"post\" action=\"{}/delete\">{}\
             <label>Type <code>{}</code> to confirm<input type=\"text\" name=\"confirm_name\" \
             required=\"required\" autocomplete=\"off\"/></label>\
             <button type=\"submit\" class=\"danger\">Delete {}</button></form></section>",
            name,
            agents_note,
            href,
            csrf.clone(),
            name,
            name
        )
    };
    let defaults = if user {
        vec!["read".to_owned(), "write".to_owned()]
    } else {
        ctx.state().scopes().agent_token_defaults()
    };
    let scopes: Html = ctx
        .state()
        .scopes()
        .all()
        .iter()
        .map(|s| {
            let checked = if defaults.contains(s) {
                html!(" checked=\"checked\"")
            } else {
                Html::new()
            };
            html!(
                "<label class=\"check\"><input type=\"checkbox\" name=\"scope\" value=\"{}\"{}/>{}</label>",
                s,
                checked,
                s
            )
        })
        .collect();

    let body = html!(
        "<p class=\"muted\"><a href=\"/admin/people\">People</a> /</p><h1>{}</h1>{}\
         <div class=\"grid\"><section><h2>Account</h2><dl class=\"facts\">{}</dl></section>\
         <section><h2>Change</h2><div class=\"stack-forms\">{}{}</div></section></div>\
         <section><h2 id=\"tokens\">Tokens</h2>{}<h2 class=\"spaced\">New token</h2>\
         <form class=\"stack\" method=\"post\" action=\"{}/tokens\">{}\
         <label>Name<input type=\"text\" name=\"name\" required=\"required\" placeholder=\"ci\"/></label>\
         <fieldset><legend>Scopes</legend>{}</fieldset>\
         <label>Expires after (days; empty for never)<input type=\"text\" name=\"days\" inputmode=\"numeric\" placeholder=\"90\"/></label>\
         <button type=\"submit\" class=\"primary\">Create token</button></form></section>{}",
        name,
        top,
        facts,
        toggle,
        user_forms,
        tokens,
        href,
        csrf,
        scopes,
        delete
    );
    Page::new(name, Nav::People).render(ctx, Some(admin), body)
}

pub(crate) async fn show(
    State(ctx): State<Ctx>,
    admin: Admin,
    Path(name): Path<String>,
) -> Response {
    person_page(&ctx, &admin, &name, Html::new()).await
}

async fn patch(
    ctx: &Ctx,
    post: &Post,
    name: &str,
    req: PatchPrincipalRequest,
    note: String,
) -> Response {
    let url = path(paths::ADMIN_PRINCIPAL, &[name]);
    match ctx.api(&post.admin).patch(&url, &req).await {
        Ok(()) => redirect_with_note(ctx, &person_href(name), &note),
        Err(e) => person_page(ctx, &post.admin, name, error_text(&e)).await,
    }
}

pub(crate) async fn set_state(
    State(ctx): State<Ctx>,
    Path(name): Path<String>,
    post: Post,
) -> Response {
    let disabled = post.fields.text("disabled") == "true";
    let note = if disabled {
        format!("{name} disabled.")
    } else {
        format!("{name} enabled.")
    };
    let req = PatchPrincipalRequest {
        disabled: Some(disabled),
        ..Default::default()
    };
    patch(&ctx, &post, &name, req, note).await
}

pub(crate) async fn delete(
    State(ctx): State<Ctx>,
    Path(name): Path<String>,
    post: Post,
) -> Response {
    if post.fields.text("confirm_name") != name {
        let top = html!(
            "<p class=\"error\" role=\"alert\">Type the name {} exactly to delete it.</p>",
            name
        );
        return person_page(&ctx, &post.admin, &name, top).await;
    }
    let url = path(paths::ADMIN_PRINCIPAL, &[&name]);
    match ctx
        .api(&post.admin)
        .delete_json::<DeletePrincipalResponse>(&url)
        .await
    {
        Ok(r) => {
            let note = if r.agents.is_empty() {
                format!("{} deleted.", r.name)
            } else {
                format!("{} deleted, with agents {}.", r.name, r.agents.join(", "))
            };
            redirect_with_note(&ctx, "/admin/people", &note)
        }
        Err(e) => person_page(&ctx, &post.admin, &name, error_text(&e)).await,
    }
}

pub(crate) async fn set_password(
    State(ctx): State<Ctx>,
    Path(name): Path<String>,
    post: Post,
) -> Response {
    let req = PatchPrincipalRequest {
        password: Some(post.fields.get("password").unwrap_or("").to_owned()),
        ..Default::default()
    };
    patch(&ctx, &post, &name, req, format!("Password of {name} set.")).await
}

pub(crate) async fn set_sso(
    State(ctx): State<Ctx>,
    Path(name): Path<String>,
    post: Post,
) -> Response {
    let sso = post.fields.text("sso");
    let note = if sso.is_empty() {
        format!("Single sign-on of {name} unlinked.")
    } else {
        format!("{name} linked to {sso}.")
    };
    let req = PatchPrincipalRequest {
        sso: Some(sso),
        ..Default::default()
    };
    patch(&ctx, &post, &name, req, note).await
}

fn token_request(post: &Post, now: time::OffsetDateTime) -> Result<CreateTokenRequest, String> {
    let days = post.fields.text("days");
    let expires_at = if days.is_empty() {
        None
    } else {
        let n: i64 = days
            .parse()
            .ok()
            .filter(|n| (1..=3650).contains(n))
            .ok_or("the expiry must be a number of days from 1 to 3650")?;
        Some(now + time::Duration::days(n))
    };
    Ok(CreateTokenRequest {
        name: post.fields.text("name"),
        scopes: post.fields.all("scope"),
        expires_at,
    })
}

pub(crate) async fn create_token(
    State(ctx): State<Ctx>,
    Path(name): Path<String>,
    post: Post,
) -> Response {
    let req = match token_request(&post, ctx.state().clock().now()) {
        Ok(r) => r,
        Err(e) => {
            let top = html!("<p class=\"error\" role=\"alert\">{}</p>", sentence(&e));
            return person_page(&ctx, &post.admin, &name, top).await;
        }
    };
    let url = path(paths::ADMIN_PRINCIPAL_TOKENS, &[&name]);
    let top = match ctx
        .api(&post.admin)
        .post::<_, CreateTokenResponse>(&url, &req)
        .await
    {
        // The value is shown on this answer only: it is not stored, and the page that
        // follows a reload no longer has it.
        Ok(r) => html!(
            "<section class=\"card\" id=\"new-token\"><h2>Token {} created</h2>\
             <p>Copy the token now. It is not shown again.</p>\
             <div class=\"secret\"><code id=\"token-value\">{}</code>\
             <button type=\"button\" data-copy=\"token-value\">Copy</button></div></section>",
            r.info.name,
            r.token
        ),
        Err(e) => error_text(&e),
    };
    person_page(&ctx, &post.admin, &name, top).await
}

pub(crate) async fn revoke_token(
    State(ctx): State<Ctx>,
    Path((name, id)): Path<(String, i64)>,
    post: Post,
) -> Response {
    let url = path(paths::ADMIN_PRINCIPAL_TOKEN, &[&name, &id.to_string()]);
    match ctx.api(&post.admin).delete(&url).await {
        Ok(()) => redirect_with_note(
            &ctx,
            &format!("{}#tokens", person_href(&name)),
            "Token revoked.",
        ),
        Err(e) => person_page(&ctx, &post.admin, &name, error_text(&e)).await,
    }
}
