//! T174 acceptance: an agent shelves through MCP, a person reviews the shelf on the web and
//! submits it there; the checks gate and the API's own rules apply to the web's buttons.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_proto::admin::{AccessLevel, AclEffect, PatchPrincipalRequest};
use lode_proto::checks::{CheckState, CheckTarget, CheckTargetKind, ReportCheckRequest};
use lode_testkit::web::{Browser, Page};
use lode_testkit::{TestServer, TestServerOptions};
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use serde_json::{json, Value};

const PASSWORD: &str = "correct horse battery";
/// A PNG header, base64: enough to be served as an image.
const PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUg==";

type McpClient = RunningService<RoleClient, ()>;

async fn connect(server: &TestServer, token: &str) -> McpClient {
    let config = StreamableHttpClientTransportConfig::with_uri(format!("{}/mcp", server.url()))
        .auth_header(token);
    ().serve(StreamableHttpClientTransport::from_config(config))
        .await
        .expect("connect")
}

async fn call(client: &McpClient, name: &str, args: Value) -> Value {
    let Value::Object(args) = args else {
        panic!("arguments must be an object")
    };
    let r = client
        .call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(args))
        .await
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_ne!(r.is_error, Some(true), "{name}: {:?}", r.structured_content);
    r.structured_content.expect("structured result")
}

async fn with_password(s: &TestServer, name: &str) {
    s.admin()
        .patch_principal(
            name,
            &PatchPrincipalRequest {
                password: Some(PASSWORD.into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

async fn sign_in(s: &TestServer, name: &str) -> Browser {
    let mut b = Browser::new(s.url());
    let form = b.get("/admin/sign-in").await;
    let r = b
        .submit(
            &form,
            "/admin/sign-in",
            &[("name", name), ("password", PASSWORD)],
        )
        .await;
    assert_eq!(r.status, 303, "{}", r.body);
    b
}

fn by_class(page: &Page, class: &str) -> Vec<String> {
    page.doc()
        .find(&|n| n.attr("class") == Some(class))
        .iter()
        .map(|n| n.text())
        .collect()
}

fn error_text(page: &Page) -> String {
    by_class(page, "error").join(" ")
}

struct World {
    s: TestServer,
    bot: String,
    ci: String,
}

/// Depot `game` with a text file; `ana` writes, `rita` only reads, agent `bot` (ana's)
/// shelves, `ci` reports checks.
async fn world() -> World {
    let s = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| c.modules.checks = true),
    })
    .await;
    s.create_depot("game").await;
    s.commit_files("game", "main", &[("src/a.txt", &b"one\n"[..])])
        .await;
    s.create_user("ana", &["read", "write", "shelve"]).await;
    s.create_user("rita", &["read"]).await;
    with_password(&s, "ana").await;
    with_password(&s, "rita").await;
    with_password(&s, "admin").await;
    s.acl(
        "game",
        "ana",
        "*",
        "**",
        AclEffect::Allow,
        AccessLevel::Write,
    )
    .await;
    s.acl(
        "game",
        "rita",
        "*",
        "**",
        AclEffect::Allow,
        AccessLevel::Read,
    )
    .await;
    let bot = s.create_agent("bot", "ana", &["read", "shelve"]).await;
    s.acl(
        "game",
        "bot",
        "*",
        "**",
        AclEffect::Allow,
        AccessLevel::Read,
    )
    .await;
    let ci = s.create_user("ci", &["read", "checks"]).await;
    s.acl("game", "ci", "*", "**", AclEffect::Allow, AccessLevel::Read)
        .await;
    World { s, bot, ci }
}

impl World {
    async fn agent_shelves(&self, description: &str) -> u64 {
        let agent = connect(&self.s, &self.bot).await;
        let shelf = call(
            &agent,
            "create_shelf",
            json!({
                "depot": "game", "branch": "main", "description": description,
                "edits": [
                    { "path": "src/a.txt", "action": "modify", "text": "two\n" },
                    { "path": "art/logo.png", "action": "add", "base64": PNG_B64 }
                ]
            }),
        )
        .await;
        shelf["id"].as_u64().unwrap()
    }

    async fn report(&self, shelf: u64, version: u64, name: &str, state: CheckState) {
        self.s
            .client(&self.ci)
            .report_check(
                "game",
                &ReportCheckRequest {
                    target: CheckTarget {
                        kind: CheckTargetKind::Shelf,
                        id: shelf,
                        version: Some(version),
                    },
                    name: name.into(),
                    state,
                    summary: Some(format!("{name} said so")),
                    url: None,
                },
            )
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_shelves_through_mcp_and_a_person_submits_on_the_web() {
    let w = world().await;
    let id = w
        .agent_shelves("Count higher\n\nThe agent's reasoning.")
        .await;
    let mut ana = sign_in(&w.s, "ana").await;

    // The list: who, what, which version.
    let list = ana.get("/web/depots/game/shelves").await;
    assert_eq!(list.status, 200, "{}", list.body);
    let row = list.doc().all("tr")[1].text();
    assert!(row.contains("Count higher"), "{row}");
    assert!(row.contains("bot (agent of ana)"), "{row}");
    // The depot page links to it.
    let depot = ana.get("/web/depots/game").await;
    assert!(
        depot
            .doc()
            .containing("a", "Shelves waiting for review")
            .len()
            == 1
    );

    // The shelf: diff, images side by side, its link, the submit form.
    let href = format!("/web/depots/game/shelves/{id}");
    let page = ana.get(&href).await;
    assert_eq!(page.status, 200, "{}", page.body);
    assert_eq!(by_class(&page, "del"), ["-one"]);
    assert_eq!(by_class(&page, "add"), ["+two"]);
    let link = page.doc().by_id("lode-link").unwrap().text();
    assert!(
        link.ends_with(&format!("/game/shelf/{id}?http=1")),
        "{link}"
    );
    let imgs: Vec<String> = page
        .doc()
        .all("img")
        .iter()
        .filter_map(|i| i.attr("src").map(str::to_owned))
        .collect();
    assert_eq!(imgs, [format!("{href}/raw?path=art%2Flogo.png")]);
    let raw = ana.get(&imgs[0]).await;
    assert_eq!(raw.status, 200);
    assert_eq!(raw.headers["content-type"], "image/png");
    let message = page.doc().all("textarea")[0].text();
    assert!(message.starts_with("Count higher"), "{message}");

    // Submit with an edited message: the revision records the shelf.
    let r = ana
        .submit(
            &page,
            &format!("{href}/submit"),
            &[("message", "Count to two")],
        )
        .await;
    assert_eq!(r.status, 303, "{}", r.body);
    let to = r.location().unwrap().to_owned();
    let rev: u64 = to.rsplit('/').next().unwrap().parse().unwrap();
    assert_eq!(to, format!("/web/depots/game/r/{rev}"));
    let done = ana.follow(&to).await;
    assert!(
        by_class(&done, "note")
            .join(" ")
            .contains(&format!("Shelf {id} submitted as r{rev}.")),
        "{}",
        done.body
    );
    let view = w.s.admin().revision("game", rev).await.unwrap();
    assert_eq!(view.message, "Count to two");
    assert_eq!(view.author.name, "bot");
    assert_eq!(view.submitter.name, "ana");
    assert_eq!(view.provenance.unwrap().submitted_shelf, Some(id));
    // The shelf now says where it went and offers no buttons.
    let after = ana.get(&href).await;
    assert!(after.body.contains(&format!(
        "Submitted as <a href=\"/web/depots/game/r/{rev}\">"
    )));
    assert!(after.doc().all("textarea").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_apis_rules_apply_to_the_buttons() {
    let w = world().await;
    let id = w.agent_shelves("Count higher").await;
    let href = format!("/web/depots/game/shelves/{id}");

    // Without write access a submit is refused, as the API words it.
    let mut rita = sign_in(&w.s, "rita").await;
    let page = rita.get(&href).await;
    assert_eq!(page.status, 200);
    let r = rita.submit(&page, &format!("{href}/submit"), &[]).await;
    assert_eq!(r.status, 403, "{}", r.body);
    assert!(!error_text(&r).is_empty());
    // Nor may she delete someone else's shelf.
    let r = rita.submit(&page, &format!("{href}/delete"), &[]).await;
    assert_eq!(r.status, 403, "{}", r.body);
    assert!(
        error_text(&r).starts_with("Not deleted:"),
        "{}",
        error_text(&r)
    );

    // A page from before the agent updated the shelf: the submit is stale.
    let mut ana = sign_in(&w.s, "ana").await;
    let old = ana.get(&href).await;
    let agent = connect(&w.s, &w.bot).await;
    let v2 = call(
        &agent,
        "update_shelf",
        json!({ "depot": "game", "id": id,
                "edits": [{ "path": "src/a.txt", "action": "modify", "text": "three\n" }] }),
    )
    .await;
    assert_eq!(v2["version"], 2);
    let r = ana.submit(&old, &format!("{href}/submit"), &[]).await;
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(!error_text(&r).is_empty(), "{}", r.body);

    // An administrator deletes it.
    let mut admin = sign_in(&w.s, "admin").await;
    let page = admin.get(&href).await;
    let r = admin.submit(&page, &format!("{href}/delete"), &[]).await;
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(r.location(), Some("/web/depots/game/shelves"));
    let list = admin.follow("/web/depots/game/shelves").await;
    assert!(by_class(&list, "note")
        .join(" ")
        .contains(&format!("Shelf {id} deleted.")));
    assert!(list.body.contains("No shelves are waiting."));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unmet_checks_are_listed_until_they_pass() {
    let w = world().await;
    w.s.commit_files(
        "game",
        "main",
        &[(
            ".lodeconfig",
            &br#"[checks]
branches = ["main"]
required = [{ name = "ci/build", reporter = "ci" }]
"#[..],
        )],
    )
    .await;
    let id = w.agent_shelves("Gated").await;
    let href = format!("/web/depots/game/shelves/{id}");
    let mut ana = sign_in(&w.s, "ana").await;

    let page = ana.get(&href).await;
    let r = ana.submit(&page, &format!("{href}/submit"), &[]).await;
    assert_eq!(r.status, 409, "{}", r.body);
    let unmet: Vec<String> = r
        .doc()
        .all("li")
        .iter()
        .map(|n| n.text().replace(" :", ":"))
        .collect();
    assert!(
        unmet.contains(&"ci/build: not reported yet (by ci)".to_owned()),
        "{unmet:?}"
    );

    w.report(id, 1, "ci/build", CheckState::Failure).await;
    let page = ana.get(&href).await;
    assert!(
        page.body.contains("ci/build said so"),
        "the checks are listed"
    );
    let r = ana.submit(&page, &format!("{href}/submit"), &[]).await;
    let unmet: Vec<String> = r
        .doc()
        .all("li")
        .iter()
        .map(|n| n.text().replace(" :", ":"))
        .collect();
    assert!(
        unmet.contains(&"ci/build: failure (must be success, reported by ci)".to_owned()),
        "{unmet:?}"
    );
    // The list shows the state too.
    let list = ana.get("/web/depots/game/shelves").await;
    assert!(list.doc().all("tr")[1].text().contains("failure"));

    w.report(id, 1, "ci/build", CheckState::Success).await;
    let page = ana.get(&href).await;
    let r = ana.submit(&page, &format!("{href}/submit"), &[]).await;
    assert_eq!(r.status, 303, "{}", r.body);
}
