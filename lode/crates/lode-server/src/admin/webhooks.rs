//! Webhooks (T143): every depot's hooks, adding one (its secret shown once), a test delivery,
//! removing, and a hook's recent deliveries. The pages exist only while the webhooks module
//! is on.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use lode_proto::consts::paths;
use lode_proto::depot::DepotList;
use lode_proto::webhooks::{
    CreateWebhookRequest, CreatedWebhook, DeliveryList, DeliveryState, DeliveryView, WebhookFormat,
    WebhookList, WebhookView, EVENTS,
};

use super::api::path;
use super::format::at;
use super::html::{encode, Html};
use super::layout::error_text;
use super::session::{redirect_with_note, Post};
use super::{html, Admin, Ctx, Nav, Page};

/// The webhooks list.
pub(crate) const LIST: &str = "/admin/webhooks";

fn module_on(ctx: &Ctx) -> bool {
    ctx.state().module_names().iter().any(|m| m == "webhooks")
}

/// The page of one hook and its deliveries.
fn hook_href(depot: &str, id: i64) -> String {
    format!("{LIST}/{}/{id}", encode(depot))
}

fn state_text(s: DeliveryState) -> Html {
    match s {
        DeliveryState::Ok => html!("<span class=\"ok\">delivered</span>"),
        DeliveryState::Pending => html!("<span class=\"warn\">pending</span>"),
        DeliveryState::Failed => html!("<span class=\"bad\">failed</span>"),
    }
}

async fn list_page(ctx: &Ctx, admin: &Admin, top: Html) -> Response {
    if !module_on(ctx) {
        return super::not_found_page(ctx, Some(admin));
    }
    let api = ctx.api(admin);
    let depots = match api.get::<DepotList>(paths::DEPOTS).await {
        Ok(d) => d.depots,
        Err(e) => {
            let body = html!("<h1>Webhooks</h1>{}{}", top, error_text(&e));
            return Page::new("Webhooks", Nav::Webhooks).render(ctx, Some(admin), body);
        }
    };
    let mut hooks: Vec<WebhookView> = Vec::new();
    let mut problems = Html::new();
    for d in &depots {
        match api
            .get::<WebhookList>(&path(paths::WEBHOOKS, &[&d.name]))
            .await
        {
            Ok(l) => hooks.extend(l.webhooks),
            Err(e) => problems.push(html!("<p>{}: {}</p>", d.name, error_text(&e))),
        }
    }

    let csrf = admin.csrf_field();
    let rows: Html = hooks
        .iter()
        .map(|h| {
            let href = hook_href(&h.depot, h.id);
            let active = if h.active {
                html!("<span class=\"ok\">active</span>")
            } else {
                html!("<span class=\"muted\">inactive</span>")
            };
            html!(
                "<tr id=\"hook-{}\"><td class=\"num\" data-label=\"Hook\"><a href=\"{}\">#{}</a></td>\
                 <td data-label=\"Depot\">{}</td><td data-label=\"Sends to\"><code>{}</code></td>\
                 <td data-label=\"Events\">{}</td><td data-label=\"Branches\">{}</td>\
                 <td data-label=\"Format\">{}</td><td data-label=\"State\">{}</td><td data-label=\"Added\">{} by {}</td>\
                 <td><div class=\"actions\"><form method=\"post\" action=\"{}/ping\">{}\
                 <button type=\"submit\">Send a test</button></form>\
                 <form method=\"post\" action=\"{}/remove\" data-confirm=\"Remove hook #{}? It stops sending at once.\">{}\
                 <button type=\"submit\" class=\"danger\">Remove</button></form></div></td></tr>",
                h.id,
                href.as_str(),
                h.id,
                h.depot,
                h.url,
                h.events.join(", "),
                h.branch.as_deref().unwrap_or("all"),
                h.format.as_str(),
                active,
                at(h.created_at),
                h.created_by.name,
                href.as_str(),
                csrf.clone(),
                href.as_str(),
                h.id,
                csrf.clone()
            )
        })
        .collect();
    let table = if hooks.is_empty() {
        html!("<p class=\"muted\">No webhooks yet.</p>")
    } else {
        html!(
            "<table class=\"wide\"><thead><tr><th class=\"num\">Hook</th><th>Depot</th>\
             <th>Sends to</th><th>Events</th><th>Branches</th><th>Format</th><th>State</th><th>Added</th><th></th>\
             </tr></thead><tbody>{}</tbody></table>",
            rows
        )
    };
    let depot_options: Html = depots
        .iter()
        .map(|d| html!("<option value=\"{}\">{}</option>", d.name, d.name))
        .collect();
    let events: Html = EVENTS
        .iter()
        .map(|e| {
            html!(
                "<label class=\"check\"><input type=\"checkbox\" name=\"events\" value=\"{}\"/>{}</label>",
                *e,
                *e
            )
        })
        .collect();
    let http_note = if ctx.state().config().webhooks.allow_http {
        html!("an <code>https://</code> or <code>http://</code> address")
    } else {
        html!("an <code>https://</code> address")
    };
    let add = if depots.is_empty() {
        html!("<p class=\"muted\">Create a depot first.</p>")
    } else {
        html!(
            "<form method=\"post\" action=\"{}\">{}\
             <div class=\"row\"><label>Depot<select name=\"depot\">{}</select></label>\
             <label>Sends to<input type=\"url\" name=\"url\" required=\"required\" \
             placeholder=\"https://ci.example.com/lode\"/></label>\
             <label>Branches<input type=\"text\" name=\"branch\" placeholder=\"all, or a glob like release/*\"/></label>\
             <label>Format<select name=\"format\">\
             <option value=\"lode\">Lode (signed JSON)</option>\
             <option value=\"slack\">Slack</option><option value=\"discord\">Discord</option>\
             <option value=\"telegram\">Telegram</option></select></label></div>\
             <fieldset><legend>Events</legend>{}</fieldset>\
             <p class=\"muted\">The receiver gets a POST at {} for each event, signed with the \
             hook's secret in <code>X-Lode-Signature</code>. The chat formats send one line of \
             text to a Slack or Discord webhook URL, or to a Telegram bot's \
             <code>sendMessage</code> URL with <code>?chat_id=</code>.</p>\
             <button type=\"submit\" class=\"primary\">Add webhook</button></form>",
            LIST,
            csrf,
            depot_options,
            events,
            http_note
        )
    };
    let body = html!(
        "<h1>Webhooks</h1>{}{}<section><h2 id=\"hooks\">Hooks</h2>{}</section>\
         <section><h2 id=\"add\">Add a webhook</h2>{}</section>",
        top,
        problems,
        table,
        add
    );
    Page::new("Webhooks", Nav::Webhooks).render(ctx, Some(admin), body)
}

pub(crate) async fn list(State(ctx): State<Ctx>, admin: Admin) -> Response {
    list_page(&ctx, &admin, Html::new()).await
}

pub(crate) async fn create(State(ctx): State<Ctx>, post: Post) -> Response {
    if !module_on(&ctx) {
        return super::not_found_page(&ctx, Some(&post.admin));
    }
    let f = &post.fields;
    let depot = f.text("depot");
    let events = f.all("events");
    if events.is_empty() {
        let top = html!("<p class=\"error\" role=\"alert\">Pick at least one event to send.</p>");
        return list_page(&ctx, &post.admin, top).await;
    }
    let branch = f.text("branch");
    let req = CreateWebhookRequest {
        url: f.text("url"),
        events,
        branch: (!branch.is_empty()).then_some(branch),
        format: WebhookFormat::parse(&f.text("format")).unwrap_or_default(),
    };
    let top = match ctx
        .api(&post.admin)
        .post::<_, CreatedWebhook>(&path(paths::WEBHOOKS, &[&depot]), &req)
        .await
    {
        // The secret is on this answer only: the server keeps it, but no page shows it again.
        Ok(c) => html!(
            "<section class=\"card\" id=\"new-webhook\"><h2>Webhook #{} added to {}</h2>\
             <p>Copy the secret now and give it to the receiver, which uses it to check each \
             delivery's signature. It is not shown again.</p>\
             <div class=\"secret\"><code id=\"webhook-secret\">{}</code>\
             <button type=\"button\" data-copy=\"webhook-secret\">Copy</button></div></section>",
            c.hook.id,
            c.hook.depot,
            c.secret
        ),
        Err(e) => error_text(&e),
    };
    list_page(&ctx, &post.admin, top).await
}

pub(crate) async fn ping(
    State(ctx): State<Ctx>,
    Path((d, id)): Path<(String, i64)>,
    post: Post,
) -> Response {
    if !module_on(&ctx) {
        return super::not_found_page(&ctx, Some(&post.admin));
    }
    let url = path(paths::WEBHOOK_PING, &[&d, &id.to_string()]);
    match ctx.api(&post.admin).post_empty::<DeliveryView>(&url).await {
        // The API queues the delivery; the sender sends it at once, and its result shows in
        // the list below as the page refreshes.
        Ok(v) => redirect_with_note(
            &ctx,
            &hook_href(&d, id),
            &format!("Test delivery #{} queued. Its result shows below.", v.id),
        ),
        Err(e) => hook_page(&ctx, &post.admin, &d, id, error_text(&e)).await,
    }
}

pub(crate) async fn remove(
    State(ctx): State<Ctx>,
    Path((d, id)): Path<(String, i64)>,
    post: Post,
) -> Response {
    if !module_on(&ctx) {
        return super::not_found_page(&ctx, Some(&post.admin));
    }
    let url = path(paths::WEBHOOK, &[&d, &id.to_string()]);
    match ctx.api(&post.admin).delete(&url).await {
        Ok(()) => redirect_with_note(&ctx, LIST, &format!("Webhook #{id} removed.")),
        Err(e) => list_page(&ctx, &post.admin, error_text(&e)).await,
    }
}

async fn hook_page(ctx: &Ctx, admin: &Admin, d: &str, id: i64, top: Html) -> Response {
    if !module_on(ctx) {
        return super::not_found_page(ctx, Some(admin));
    }
    let api = ctx.api(admin);
    let hook = api
        .get::<WebhookList>(&path(paths::WEBHOOKS, &[d]))
        .await
        .map(|l| l.webhooks.into_iter().find(|h| h.id == id));
    let hook = match hook {
        Ok(Some(h)) => h,
        Ok(None) => return super::not_found_page(ctx, Some(admin)),
        Err(e) => {
            let body = html!("<h1>Webhook #{}</h1>{}{}", id, top, error_text(&e));
            let mut r = Page::new("Webhook", Nav::Webhooks).render(ctx, Some(admin), body);
            if e.code == "not_found" {
                *r.status_mut() = StatusCode::NOT_FOUND;
            }
            return r;
        }
    };
    let deliveries = api
        .get::<DeliveryList>(&path(paths::WEBHOOK_DELIVERIES, &[d, &id.to_string()]))
        .await;
    let deliveries = match deliveries {
        Err(e) => error_text(&e),
        Ok(l) if l.deliveries.is_empty() => html!("<p class=\"muted\">Nothing sent yet.</p>"),
        Ok(l) => {
            let rows: Html = l
                .deliveries
                .iter()
                .map(|v| {
                    let next = v.next_attempt_at.map(at).unwrap_or_default();
                    let answer = match (v.last_status, &v.last_error) {
                        (Some(s), _) => html!("HTTP {}", s),
                        (None, Some(e)) => html!("{}", e.as_str()),
                        (None, None) => Html::new(),
                    };
                    html!(
                        "<tr id=\"delivery-{}\"><td class=\"num\" data-label=\"Delivery\">#{}</td>\
                         <td data-label=\"Event\">{}</td><td data-label=\"State\">{}</td>\
                         <td class=\"num\" data-label=\"Attempts\">{}</td>\
                         <td data-label=\"Last answer\">{}</td><td data-label=\"Next attempt\">{}</td>\
                         <td data-label=\"Queued\">{}</td></tr>",
                        v.id,
                        v.id,
                        v.event,
                        state_text(v.state),
                        v.attempts,
                        answer,
                        next,
                        at(v.created_at)
                    )
                })
                .collect();
            html!(
                "<table class=\"wide\"><thead><tr><th class=\"num\">Delivery</th><th>Event</th>\
                 <th>State</th><th class=\"num\">Attempts</th><th>Last answer</th>\
                 <th>Next attempt</th><th>Queued</th></tr></thead><tbody>{}</tbody></table>",
                rows
            )
        }
    };
    let csrf = admin.csrf_field();
    let href = hook_href(d, id);
    let body = html!(
        "<p class=\"muted\"><a href=\"{}\">Webhooks</a> /</p><h1>Webhook #{}</h1>{}\
         <section><dl class=\"facts\"><dt>Depot</dt><dd>{}</dd><dt>Sends to</dt><dd><code>{}</code></dd>\
         <dt>Events</dt><dd>{}</dd><dt>Branches</dt><dd>{}</dd><dt>Added</dt><dd>{} by {}</dd></dl>\
         <div class=\"actions spaced\"><form method=\"post\" action=\"{}/ping\">{}\
         <button type=\"submit\">Send a test</button></form>\
         <form method=\"post\" action=\"{}/remove\" data-confirm=\"Remove hook #{}? It stops sending at once.\">{}\
         <button type=\"submit\" class=\"danger\">Remove</button></form></div></section>\
         <section><h2 id=\"deliveries\">Recent deliveries</h2>\
         <p class=\"muted\">A delivery is tried again with growing pauses until the receiver \
         answers 2xx, for up to 24 hours.</p>{}</section>",
        LIST,
        id,
        top,
        hook.depot,
        hook.url,
        hook.events.join(", "),
        hook.branch.as_deref().unwrap_or("all"),
        at(hook.created_at),
        hook.created_by.name,
        href.as_str(),
        csrf.clone(),
        href.as_str(),
        id,
        csrf,
        deliveries
    );
    Page::new(&format!("Webhook #{id}"), Nav::Webhooks)
        .refresh(30)
        .render(ctx, Some(admin), body)
}

pub(crate) async fn show(
    State(ctx): State<Ctx>,
    admin: Admin,
    Path((d, id)): Path<(String, i64)>,
) -> Response {
    hook_page(&ctx, &admin, &d, id, Html::new()).await
}
