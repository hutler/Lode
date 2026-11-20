//! M14 acceptance: the admin web UI, driven like a browser and checked by parsing its pages.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_proto::admin::{
    AccessLevel, AclEffect, AclRule, AclSubject, CreatePrincipalRequest, PatchPrincipalRequest,
};
use lode_proto::PrincipalKind;
use lode_testkit::web::{Browser, Page};
use lode_testkit::{TestServer, TestServerOptions};

const PASSWORD: &str = "correct horse battery";

/// A server whose `admin` has a password.
async fn server() -> TestServer {
    server_with(|_| {}).await
}

async fn server_with(
    f: impl FnOnce(&mut lode_server::ServerConfig) + Send + 'static,
) -> TestServer {
    let s = TestServer::start_with(TestServerOptions {
        configure: Box::new(f),
    })
    .await;
    set_password(&s, "admin").await;
    s
}

async fn set_password(s: &TestServer, name: &str) {
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

async fn add_user(s: &TestServer, name: &str) {
    s.admin()
        .create_principal(&CreatePrincipalRequest {
            kind: PrincipalKind::User,
            name: name.into(),
            display_name: None,
            owner: None,
            password: Some(PASSWORD.into()),
            sso: None,
        })
        .await
        .unwrap();
}

/// Makes `name` a server administrator with an access rule; returns the rule's id.
async fn make_admin(s: &TestServer, name: &str) -> i64 {
    s.admin()
        .add_acl_rule(&AclRule {
            id: None,
            depot: None,
            subject: AclSubject::User(name.into()),
            branch: "**".into(),
            path: "**".into(),
            effect: AclEffect::Allow,
            level: AccessLevel::Admin,
        })
        .await
        .unwrap()
        .id
        .unwrap()
}

/// Signs in through the form and answers the form's answer.
async fn sign_in_as(b: &mut Browser, name: &str, password: &str) -> Page {
    let form = b.get("/admin/sign-in").await;
    assert_eq!(form.status, 200, "{}", form.body);
    b.submit(
        &form,
        "/admin/sign-in",
        &[("name", name), ("password", password)],
    )
    .await
}

async fn signed_in(s: &TestServer) -> Browser {
    let mut b = Browser::new(s.url());
    let r = sign_in_as(&mut b, "admin", PASSWORD).await;
    assert_eq!(r.status, 303, "{}", r.body);
    b
}

fn error_text(page: &Page) -> String {
    page.doc()
        .find(&|n| n.attr("class") == Some("error"))
        .iter()
        .map(|n| n.text())
        .collect::<Vec<_>>()
        .join(" ")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sign_in_and_out() {
    let s = server().await;
    let mut b = Browser::new(s.url());

    let r = b.get("/admin/").await;
    assert_eq!(r.status, 303);
    assert_eq!(r.location(), Some("/admin/sign-in?next=%2Fadmin%2F"));
    let r = b.get("/admin").await;
    assert_eq!(r.location(), Some("/admin/"));

    let form = b.follow("/admin/").await;
    assert_eq!(form.status, 200);
    assert!(form
        .cookie("lode_admin_signin")
        .unwrap()
        .contains("SameSite=Lax"));
    let wrong = b
        .submit(
            &form,
            "/admin/sign-in",
            &[("name", "admin"), ("password", "nope nope nope")],
        )
        .await;
    assert_eq!(wrong.status, 401);
    assert_eq!(error_text(&wrong), "Wrong name or password.");
    assert!(wrong.cookie("lode_admin").is_none());

    let ok = b
        .submit(
            &form,
            "/admin/sign-in",
            &[("name", "admin"), ("password", PASSWORD)],
        )
        .await;
    assert_eq!(ok.status, 303, "{}", ok.body);
    assert_eq!(ok.location(), Some("/admin/"));
    let cookie = ok.cookie("lode_admin").unwrap();
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Strict"), "{cookie}");
    assert!(cookie.contains("Max-Age=28800"), "{cookie}");
    assert!(cookie.contains("Path=/admin"), "{cookie}");
    assert!(!cookie.contains("Secure"), "no TLS, no Secure: {cookie}");

    // The session is an ordinary token named "web admin" that expires in eight hours.
    let tokens = s.admin().tokens_for("admin").await.unwrap().tokens;
    let web = tokens.iter().find(|t| t.name == "web admin").unwrap();
    let left = web.expires_at.unwrap() - lode_server::Clock::now(s.clock());
    assert!(
        left.whole_minutes() > 470 && left.whole_minutes() <= 480,
        "{left}"
    );

    let home = b.get("/admin/").await;
    assert_eq!(home.status, 200, "{}", home.body);
    let doc = home.doc();
    assert_eq!(doc.all("h1")[0].text(), "Overview");
    assert!(doc
        .find(&|n| n.attr("class") == Some("who"))
        .iter()
        .any(|n| n.text().contains("admin")));
    let csp = &home.headers["content-security-policy"];
    assert!(
        csp.contains("script-src 'self'") && !csp.contains("unsafe-inline"),
        "{csp}"
    );
    assert_eq!(home.headers["x-frame-options"], "DENY");
    // The sign-in page sends a signed-in administrator on.
    assert_eq!(b.get("/admin/sign-in").await.location(), Some("/admin/"));

    let out = b.submit(&home, "/admin/sign-out", &[]).await;
    assert_eq!(out.status, 303);
    assert_eq!(out.location(), Some("/admin/sign-in?signed_out=1"));
    assert!(out.cookie("lode_admin").unwrap().contains("Max-Age=0"));
    let tokens = s.admin().tokens_for("admin").await.unwrap().tokens;
    assert!(
        tokens.iter().all(|t| t.name != "web admin"),
        "sign-out deletes the token"
    );
    let again = b.follow("/admin/sign-in?signed_out=1").await;
    assert!(again.doc().containing("p", "You are signed out.").len() == 1);
    assert_eq!(b.get("/admin/").await.status, 303);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_server_administrators_get_in() {
    let s = server().await;
    add_user(&s, "ana").await;

    let mut b = Browser::new(s.url());
    let r = sign_in_as(&mut b, "ana", PASSWORD).await;
    assert_eq!(r.status, 403);
    assert_eq!(
        error_text(&r),
        "This account is not a server administrator."
    );
    assert!(
        r.cookie("lode_admin").is_none(),
        "no session for a non-administrator"
    );
    let tokens = s.admin().tokens_for("ana").await.unwrap().tokens;
    assert!(tokens.is_empty(), "no token was issued: {tokens:?}");

    // An administrator by an access rule gets in, and is out again once the rule goes.
    let rule = make_admin(&s, "ana").await;
    let r = sign_in_as(&mut b, "ana", PASSWORD).await;
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(b.get("/admin/").await.status, 200);
    s.admin().delete_acl_rule(rule).await.unwrap();
    let r = b.get("/admin/").await;
    assert_eq!(r.status, 403);
    assert_eq!(
        error_text(&r),
        "This account is not a server administrator."
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_ends_when_its_user_is_disabled_or_it_expires() {
    let s = server().await;
    add_user(&s, "ana").await;
    make_admin(&s, "ana").await;
    let mut b = Browser::new(s.url());
    assert_eq!(sign_in_as(&mut b, "ana", PASSWORD).await.status, 303);
    assert_eq!(b.get("/admin/").await.status, 200);

    s.admin()
        .patch_principal(
            "ana",
            &PatchPrincipalRequest {
                disabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let r = b.get("/admin/").await;
    assert_eq!(r.status, 303);
    assert_eq!(r.location(), Some("/admin/sign-in?next=%2Fadmin%2F"));
    assert!(r.cookie("lode_admin").unwrap().contains("Max-Age=0"));
    assert!(!b.cookies.contains_key("lode_admin"));

    // Eight hours later a session has expired.
    let mut b = signed_in(&s).await;
    assert_eq!(b.get("/admin/").await.status, 200);
    s.clock().advance_ms(8 * 3_600_000 + 1000);
    assert_eq!(b.get("/admin/").await.status, 303);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_post_needs_the_form_token() {
    let s = server().await;
    let mut b = Browser::new(s.url());

    // Signing in without the sign-in cookie's token.
    let r = b
        .post(
            "/admin/sign-in",
            &[("name", "admin"), ("password", PASSWORD)],
        )
        .await;
    assert_eq!(r.status, 403);
    assert_eq!(error_text(&r), "The sign-in form expired. Try again.");
    let r = b
        .post(
            "/admin/sign-in",
            &[("name", "admin"), ("password", PASSWORD), ("csrf", "x")],
        )
        .await;
    assert_eq!(r.status, 403);

    let mut b = signed_in(&s).await;
    for csrf in [None, Some("0123456789abcdef0123456789abcdef")] {
        let fields: Vec<(&str, &str)> = csrf.map(|c| ("csrf", c)).into_iter().collect();
        let r = b.post("/admin/sign-out", &fields).await;
        assert_eq!(r.status, 403, "{}", r.body);
        assert!(
            error_text(&r).contains("This form has expired"),
            "{}",
            r.body
        );
    }
    assert_eq!(b.get("/admin/").await.status, 200, "still signed in");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_session_cookie_is_secure_with_tls() {
    let dir = tempfile::tempdir().unwrap();
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let (cert_file, key_file) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
    std::fs::write(&cert_file, cert.cert.pem()).unwrap();
    std::fs::write(&key_file, cert.signing_key.serialize_pem()).unwrap();
    let s = TestServer::start_with(TestServerOptions {
        configure: Box::new(move |c| {
            c.tls = Some(lode_server::TlsConfig::Files {
                cert: cert_file,
                key: key_file,
            });
        }),
    })
    .await;
    let cfg = lode_server::ServerConfig::with_data_dir(s.data_dir());
    lode_server::set_admin_password(&cfg, PASSWORD)
        .await
        .unwrap();
    assert!(s.url().starts_with("https://"), "{}", s.url());
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(cert.cert.pem().as_bytes()).unwrap())
        .build()
        .unwrap();
    let url = s.url().replace("127.0.0.1", "localhost");
    let mut b = Browser::with_client(&url, http);
    let r = sign_in_as(&mut b, "admin", PASSWORD).await;
    assert_eq!(r.status, 303, "{}", r.body);
    let cookie = r.cookie("lode_admin").unwrap();
    assert!(cookie.contains("; Secure"), "{cookie}");
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    let home = b.get("/admin/").await;
    assert_eq!(home.status, 200);
    let tls = home.doc();
    let facts = tls.text();
    assert!(facts.contains("Certificate files"), "{facts}");
    assert!(
        !facts.contains("Not known yet"),
        "the expiry is read: {facts}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn values_are_escaped() {
    let s = server().await;
    // Depot names cannot hold markup, but commit messages and user display names can.
    s.create_depot("game").await;
    s.commit_files("game", "main", &[("a.txt", b"a")]).await;
    s.admin()
        .create_principal(&CreatePrincipalRequest {
            kind: PrincipalKind::User,
            name: "eve".into(),
            display_name: Some("<script>alert(1)</script>".into()),
            owner: None,
            password: None,
            sso: None,
        })
        .await
        .unwrap();
    let mut b = signed_in(&s).await;
    let home = b.get("/admin/").await;
    assert_eq!(home.status, 200);
    // Parsing fails on markup that is not well formed, and an unescaped value would add an
    // element.
    let doc = home.doc();
    assert_eq!(doc.all("script").len(), 1, "only the page's own script");
    assert_eq!(
        doc.all("script")[0].attr("src"),
        Some("/admin/static/admin.js")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turned_off_it_is_not_there() {
    let s = server_with(|c| c.admin_ui.enabled = false).await;
    let mut b = Browser::new(s.url());
    for path in ["/admin/", "/admin/sign-in", "/admin/static/admin.css"] {
        let r = b.get(path).await;
        assert_eq!(r.status, 404, "{path}");
        assert!(r.body.contains("not_found"), "{}", r.body);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_overview_shows_how_the_server_is_doing() {
    let s = server_with(|c| {
        c.backup.every = Some(std::time::Duration::from_secs(86_400));
    })
    .await;
    s.create_depot("game").await;
    let rev = s
        .commit_files("game", "main", &[("Content/a.txt", b"hello")])
        .await;
    s.admin().gc("game", false).await.unwrap();
    add_user(&s, "ana").await;
    s.create_agent("bot", "ana", &["read"]).await;

    let mut b = signed_in(&s).await;
    let home = b.get("/admin/").await;
    let doc = home.doc();
    let text = doc.text();
    for expected in [
        lode_build::VERSION,
        "Running for",
        "Off: plain HTTP",
        "Files in",
        "Database",
        "locks",
        "shelves",
        "Every 1 day, keeping 7",
        "None yet",
        "Due now",
    ] {
        assert!(text.contains(expected), "{expected} in {text}");
    }
    let stats: Vec<String> = doc
        .find(&|n| n.attr("class") == Some("stat"))
        .iter()
        .map(|n| n.text())
        .collect();
    assert_eq!(stats[0], "1 depots");
    assert_eq!(stats[1], "2 users");
    assert_eq!(stats[2], "1 agents");
    assert!(stats[3].ends_with("active tokens"), "{stats:?}");
    let maint = doc.by_id("maint").unwrap();
    let _ = maint;
    let rows = doc.find(&|n| n.name == "tr" && n.text().starts_with("game"));
    assert!(
        rows.iter()
            .any(|r| r.text().contains("objects") && r.text().contains("Never")),
        "the collection is shown and no check yet: {:?}",
        rows.iter().map(|r| r.text()).collect::<Vec<_>>()
    );
    assert!(
        rows.iter().any(|r| r.text().contains(&format!("r{rev}"))),
        "the commit is listed"
    );
    assert_eq!(
        doc.find(&|n| n.name == "body")
            .first()
            .and_then(|b| b.attr("data-refresh")),
        Some("30")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_sign_on_signs_administrators_in() {
    use lode_testkit::oidc::TestProvider;
    let provider = TestProvider::start("lode-test", "s3cret").await;
    let mut oidc = lode_server::OidcConfig::new(provider.issuer(), "lode-test")
        .unwrap()
        .with_client_secret("s3cret");
    oidc.label = "Example Corp".into();
    let s = server_with(move |c| c.oidc = Some(oidc)).await;
    s.admin()
        .patch_principal(
            "admin",
            &PatchPrincipalRequest {
                sso: Some("boss@example.com".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    s.admin()
        .create_principal(&CreatePrincipalRequest {
            kind: PrincipalKind::User,
            name: "ana".into(),
            display_name: None,
            owner: None,
            password: None,
            sso: Some("ana@example.com".into()),
        })
        .await
        .unwrap();
    let email = |address: &str| {
        let address = address.to_owned();
        provider.set(move |b| {
            b.claims.insert("email".into(), serde_json::json!(address));
            b.claims
                .insert("email_verified".into(), serde_json::json!(true));
        });
    };

    let mut b = Browser::new(s.url());
    let form = b.get("/admin/sign-in?next=%2Fadmin%2F").await;
    let doc = form.doc();
    let link = doc.containing("a", "Sign in with Example Corp")[0]
        .attr("href")
        .unwrap()
        .to_owned();
    assert_eq!(link, "/admin/sign-in/sso?next=%2Fadmin%2F");

    email("boss@example.com");
    let done = b.follow(&link).await;
    assert_eq!(done.status, 200, "{}", done.body);
    assert!(done.body.contains("url=/admin/"), "{}", done.body);
    let cookie = done.cookie("lode_admin").unwrap();
    assert!(cookie.contains("SameSite=Strict") && cookie.contains("HttpOnly"));
    assert_eq!(b.get("/admin/").await.status, 200);

    // A user who is not an administrator gets no session.
    let mut b = Browser::new(s.url());
    email("ana@example.com");
    let r = b.follow("/admin/sign-in/sso").await;
    assert_eq!(r.status, 403, "{}", r.body);
    assert_eq!(
        error_text(&r),
        "This account is not a server administrator."
    );
    assert!(!b.cookies.contains_key("lode_admin"));

    // The one-time code only works in the browser that started the sign-in.
    let mut first = Browser::new(s.url());
    email("boss@example.com");
    let start = first.get("/admin/sign-in/sso").await;
    let to_provider = start.location().unwrap().to_owned();
    let back = first.get(&to_provider).await;
    let callback = back.location().unwrap().to_owned();
    let done = first.get(&callback).await;
    let code_page = done.location().unwrap().to_owned();
    assert!(
        code_page.starts_with("/admin/sign-in/sso-done?code="),
        "{code_page}"
    );
    let mut other = Browser::new(s.url());
    let r = other.follow(&code_page).await;
    assert_eq!(r.status, 403);
    assert!(
        error_text(&r).contains("started in another browser"),
        "{}",
        r.body
    );
}

fn report(page: &Page) -> String {
    page.doc()
        .by_id("report")
        .map(|n| n.text())
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn depots_are_listed_and_created() {
    let s = server().await;
    s.create_depot("game").await;
    let rev = s.commit_files("game", "main", &[("a.txt", b"a")]).await;
    let mut b = signed_in(&s).await;

    let list = b.get("/admin/depots").await;
    assert_eq!(list.status, 200, "{}", list.body);
    let row = list
        .doc()
        .find(&|n| n.name == "tr" && n.text().starts_with("game"))[0]
        .text();
    assert!(
        row.starts_with(&format!("game {rev} 1 ")),
        "revisions and branches: {row}"
    );
    assert!(row.contains("by admin"), "{row}");

    let created = b.submit(&list, "/admin/depots", &[("name", "art")]).await;
    assert_eq!(created.status, 303, "{}", created.body);
    assert_eq!(created.location(), Some("/admin/depots/art"));
    let page = b.get("/admin/depots/art").await;
    assert_eq!(page.status, 200);
    assert_eq!(
        page.doc().find(&|n| n.attr("class") == Some("note"))[0].text(),
        "Depot art created."
    );
    // The note shows once.
    assert!(b
        .get("/admin/depots/art")
        .await
        .doc()
        .find(&|n| n.attr("class") == Some("note"))
        .is_empty());
    assert!(s
        .admin()
        .depots()
        .await
        .unwrap()
        .depots
        .iter()
        .any(|d| d.name == "art"));

    // The API's validation and errors come back on the page.
    let list = b.get("/admin/depots").await;
    let again = b.submit(&list, "/admin/depots", &[("name", "art")]).await;
    assert_eq!(again.status, 200);
    assert!(
        error_text(&again).contains("already exists"),
        "{}",
        error_text(&again)
    );
    let bad = b
        .submit(&list, "/admin/depots", &[("name", "no spaces")])
        .await;
    assert!(
        error_text(&bad).to_lowercase().contains("depot name"),
        "{}",
        error_text(&bad)
    );

    let missing = b.get("/admin/depots/nope").await;
    assert_eq!(missing.status, 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_depot_shows_branches_revisions_and_locks() {
    let s = server().await;
    s.create_depot("game").await;
    let rev = s
        .commit_files("game", "main", &[("Content/hero.psd", b"v1")])
        .await;
    s.admin()
        .create_branch(
            "game",
            &lode_proto::depot::CreateBranchRequest {
                name: "old".into(),
                from_branch: "main".into(),
                from_rev: None,
                stream: None,
            },
        )
        .await
        .unwrap();
    s.admin()
        .patch_branch(
            "game",
            "old",
            &lode_proto::depot::PatchBranchRequest {
                state: Some(lode_proto::depot::BranchState::Archived),
                stream: None,
            },
        )
        .await
        .unwrap();
    let locker = s
        .token_for("admin", &["read", "write", "admin", "lock"])
        .await;
    s.client(&locker)
        .lock(
            "game",
            &lode_proto::locks::LockRequest {
                branch: "main".into(),
                workspace: "ws1".into(),
                comment: Some("painting".into()),
                paths: vec![lode_proto::locks::LockPath {
                    path: lode_core::DepotPath::parse("Content/hero.psd").unwrap(),
                    base_id: lode_core::blob_id(&b"v1"[..]).unwrap(),
                }],
            },
        )
        .await
        .unwrap();

    let mut b = signed_in(&s).await;
    let page = b.get("/admin/depots/game").await;
    assert_eq!(page.status, 200, "{}", page.body);
    let doc = page.doc();
    let branches: Vec<String> = doc
        .find(&|n| n.name == "tr" && n.find(&|c| c.name == "td").len() == 4)
        .iter()
        .map(|n| n.text())
        .collect();
    assert!(
        branches
            .iter()
            .any(|b| b.starts_with("main r") && b.ends_with("active")),
        "{branches:?}"
    );
    assert!(
        branches
            .iter()
            .any(|b| b.starts_with("old") && b.contains("main at r") && b.ends_with("archived")),
        "{branches:?}"
    );
    assert!(
        doc.text().contains(&format!("r{rev} main admin")),
        "the revision is listed"
    );
    let lock = doc.find(&|n| n.name == "tr" && n.text().starts_with("Content/hero.psd"))[0].text();
    assert!(
        lock.contains("main admin ws1") && lock.contains("painting"),
        "{lock}"
    );

    let broken = b.submit(&page, "/admin/depots/game/locks/break", &[]).await;
    assert_eq!(broken.status, 303, "{}", broken.body);
    let locks = s
        .admin()
        .locks("game", &lode_proto::locks::LocksQuery::default())
        .await
        .unwrap();
    assert!(locks.locks.is_empty(), "the lock is gone");
    let after = b.follow("/admin/depots/game#locks").await;
    assert!(after
        .doc()
        .text()
        .contains("Lock on Content/hero.psd broken."));
    assert!(after.doc().text().contains("No files are locked."));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn garbage_collection_dry_run_changes_nothing_then_runs() {
    let s = server().await;
    s.create_depot("game").await;
    s.commit_files("game", "main", &[("a.txt", b"kept")]).await;
    // Content uploaded but never committed is garbage once the grace period has passed.
    let junk = b"never committed";
    let id = lode_core::blob_id(&junk[..]).unwrap();
    s.admin()
        .put_blob("game", &id, std::io::Cursor::new(junk.to_vec()))
        .await
        .unwrap();
    s.clock().advance_ms(25 * 3_600_000);
    let mut b = signed_in(&s).await;
    let page = b.get("/admin/depots/game").await;

    let dry = b
        .submit(&page, "/admin/depots/game/gc", &[("dry_run", "1")])
        .await;
    assert_eq!(dry.status, 200, "{}", dry.body);
    let text = report(&dry);
    assert!(text.starts_with("Garbage collection: dry run"), "{text}");
    assert!(text.contains("Would remove 1 ("), "{text}");
    assert!(
        s.admin()
            .missing_objects("game", &[id])
            .await
            .unwrap()
            .is_empty(),
        "still stored"
    );

    let run = b.submit(&dry, "/admin/depots/game/gc", &[]).await;
    let text = report(&run);
    assert!(
        text.starts_with("Garbage collection Objects checked"),
        "{text}"
    );
    assert!(text.contains("Removed 1 ("), "{text}");
    assert_eq!(
        s.admin().missing_objects("game", &[id]).await.unwrap(),
        vec![id]
    );

    // The overview shows the run.
    let home = b.get("/admin/").await;
    let row = home
        .doc()
        .find(&|n| n.name == "tr" && n.text().starts_with("game"))[0]
        .text();
    assert!(row.contains("1 object,"), "{row}");

    let checked = b
        .submit(&page, "/admin/depots/game/fsck", &[("full", "1")])
        .await;
    let text = report(&checked);
    assert!(text.starts_with("Full check"), "{text}");
    assert!(text.contains("No problems found."), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn obliterate_lists_every_use_and_refuses_content_a_head_holds() {
    let s = server().await;
    s.create_depot("game").await;
    let r1 = s
        .commit_files("game", "main", &[("secret.txt", b"password=1")])
        .await;
    let r2 = s
        .commit_files("game", "main", &[("secret.txt", b"nothing here")])
        .await;
    let mut b = signed_in(&s).await;
    let page = b.get("/admin/depots/game").await;

    // Every version: the head still holds the newest, so it cannot go.
    let dry = b
        .submit(
            &page,
            "/admin/depots/game/obliterate",
            &[("targets", "secret.txt"), ("reason", "leak")],
        )
        .await;
    let text = report(&dry);
    assert!(text.contains("2 objects"), "{text}");
    assert!(
        text.contains(&format!("r{r1} on main: secret.txt")),
        "{text}"
    );
    assert!(
        text.contains(&format!("r{r2} on main: secret.txt")),
        "{text}"
    );
    assert!(
        text.contains("Branch heads still hold this content"),
        "{text}"
    );
    assert!(text.contains("main holds secret.txt"), "{text}");
    assert!(dry
        .doc()
        .find(&|n| n.name == "input" && n.attr("name") == Some("confirm"))
        .is_empty());
    // Sent anyway, the API refuses it.
    let forced = b
        .submit(
            &page,
            "/admin/depots/game/obliterate",
            &[
                ("targets", "secret.txt"),
                ("reason", "leak"),
                ("confirm", "1"),
            ],
        )
        .await;
    assert!(
        error_text(&forced).contains("active branch"),
        "{}",
        error_text(&forced)
    );
    assert!(forced.doc().text().contains("main holds secret.txt"));

    // The old version alone can go: a dry run, then the confirmed form from its report.
    let target = format!("secret.txt@{r1}");
    let dry = b
        .submit(
            &page,
            "/admin/depots/game/obliterate",
            &[("targets", target.as_str()), ("reason", "leaked password")],
        )
        .await;
    let text = report(&dry);
    assert!(text.contains("1 object ("), "{text}");
    assert!(
        s.admin()
            .obliterated("game")
            .await
            .unwrap()
            .obliterated
            .is_empty(),
        "a dry run changes nothing"
    );
    let done = b.submit(&dry, "/admin/depots/game/obliterate", &[]).await;
    let text = report(&done);
    assert!(text.starts_with("Obliterated 1 object ("), "{text}");
    let gone = s.admin().obliterated("game").await.unwrap().obliterated;
    assert_eq!(gone.len(), 1);
    assert_eq!(gone[0].reason, "leaked password");
    let list = done.doc().by_id("obliterated").map(|_| ()).is_some();
    assert!(list);
    let row = done
        .doc()
        .find(&|n| n.name == "tr" && n.text().contains("leaked password"))[0]
        .text();
    assert!(
        row.starts_with("secret.txt") && row.contains("admin"),
        "{row}"
    );
}

fn note(page: &Page) -> String {
    page.doc()
        .find(&|n| n.attr("class") == Some("note"))
        .first()
        .map(|n| n.text())
        .unwrap_or_default()
}

async fn principal(s: &TestServer, name: &str) -> lode_proto::auth::PrincipalView {
    s.admin()
        .principals()
        .await
        .unwrap()
        .principals
        .into_iter()
        .find(|p| p.name == name)
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn users_agents_and_groups_are_managed() {
    let s = server().await;
    let mut b = signed_in(&s).await;
    let people = b.get("/admin/people").await;
    assert_eq!(people.status, 200, "{}", people.body);

    // A user with a password and a single sign-on identity.
    let r = b
        .submit(
            &people,
            "/admin/people/users",
            &[
                ("name", "ana"),
                ("display_name", "Ana Lima"),
                ("password", PASSWORD),
                ("sso", "Ana@Example.com"),
            ],
        )
        .await;
    assert_eq!(r.location(), Some("/admin/people/ana"), "{}", r.body);
    let page = b.get("/admin/people/ana").await;
    assert_eq!(note(&page), "User ana added.");
    let facts = page.doc().text();
    assert!(
        facts.contains("Ana Lima") && facts.contains("ana@example.com"),
        "{facts}"
    );
    let login = s
        .anonymous()
        .login(&lode_proto::auth::LoginRequest {
            name: "ana".into(),
            password: PASSWORD.into(),
            token_name: None,
        })
        .await;
    assert!(login.is_ok(), "the password works");

    // The API's validation shows on the page.
    let r = b
        .submit(
            &people,
            "/admin/people/users",
            &[("name", "bob"), ("password", "short")],
        )
        .await;
    assert!(error_text(&r).contains("at least"), "{}", error_text(&r));

    // Disable, enable, password, single sign-on.
    let r = b.submit(&page, "/admin/people/ana/state", &[]).await;
    assert_eq!(r.status, 303);
    assert!(principal(&s, "ana").await.disabled);
    let page = b.get("/admin/people/ana").await;
    assert_eq!(note(&page), "ana disabled.");
    assert!(page.doc().text().contains("disabled"));
    b.submit(&page, "/admin/people/ana/state", &[]).await;
    assert!(!principal(&s, "ana").await.disabled);
    let page = b.get("/admin/people/ana").await;
    let r = b
        .submit(
            &page,
            "/admin/people/ana/password",
            &[("password", "another long secret")],
        )
        .await;
    assert_eq!(r.status, 303);
    let page = b.get("/admin/people/ana").await;
    assert_eq!(note(&page), "Password of ana set.");
    let unlink = page
        .doc()
        .find(&|n| {
            n.name == "form"
                && n.attr("action") == Some("/admin/people/ana/sso")
                && n.text().contains("Unlink")
        })
        .len();
    assert_eq!(unlink, 1);
    let r = b
        .post(
            "/admin/people/ana/sso",
            &[("csrf", &csrf(&page)), ("sso", "")],
        )
        .await;
    assert_eq!(r.status, 303);
    assert_eq!(principal(&s, "ana").await.sso, None);

    // An agent owned by ana.
    let people = b.get("/admin/people").await;
    let r = b
        .submit(
            &people,
            "/admin/people/agents",
            &[("name", "build-bot"), ("owner", "ana")],
        )
        .await;
    assert_eq!(r.status, 303, "{}", r.body);
    let bot = principal(&s, "build-bot").await;
    assert_eq!(bot.owner.as_deref(), Some("ana"));

    // A group with a member, then without.
    let r = b
        .submit(&people, "/admin/people/groups", &[("name", "artists")])
        .await;
    assert_eq!(r.status, 303);
    let people = b.get("/admin/people").await;
    b.submit(
        &people,
        "/admin/people/groups/artists/add",
        &[("name", "ana")],
    )
    .await;
    let groups = s.admin().groups().await.unwrap().groups;
    assert_eq!(groups[0].members, vec!["ana".to_owned()]);
    let people = b.get("/admin/people").await;
    assert_eq!(note(&people), "ana added to artists.");
    let r = b
        .submit(&people, "/admin/people/groups/artists/remove", &[])
        .await;
    assert_eq!(r.status, 303);
    assert!(s.admin().groups().await.unwrap().groups[0]
        .members
        .is_empty());
}

fn csrf(page: &Page) -> String {
    page.doc()
        .find(&|n| n.name == "input" && n.attr("name") == Some("csrf"))
        .first()
        .and_then(|n| n.attr("value"))
        .unwrap()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_token_value_is_shown_once() {
    let s = server().await;
    add_user(&s, "ana").await;
    let mut b = signed_in(&s).await;
    let page = b.get("/admin/people/ana").await;
    let scopes: Vec<String> = page
        .doc()
        .find(&|n| n.name == "input" && n.attr("name") == Some("scope"))
        .iter()
        .map(|n| n.attr("value").unwrap().to_owned())
        .collect();
    assert!(
        scopes.contains(&"lock".to_owned()),
        "module scopes are offered: {scopes:?}"
    );

    let created = b
        .submit(
            &page,
            "/admin/people/ana/tokens",
            &[("name", "ci"), ("days", "30")],
        )
        .await;
    assert_eq!(created.status, 200, "{}", created.body);
    let doc = created.doc();
    let value = doc.by_id("token-value").unwrap().text();
    assert!(value.starts_with("lode_"), "{value}");
    assert_eq!(
        doc.find(&|n| n.attr("data-copy") == Some("token-value"))
            .len(),
        1
    );
    let me = s.client(&value).whoami().await.unwrap();
    assert_eq!(me.principal.name, "ana");
    assert_eq!(me.scopes, vec!["read".to_owned(), "write".to_owned()]);

    // Listed by name and scopes; the value is nowhere again.
    let again = b.get("/admin/people/ana").await;
    assert!(!again.body.contains(&value), "the value is shown once only");
    let row = again
        .doc()
        .find(&|n| n.name == "tr" && n.text().starts_with("ci "))[0]
        .text();
    assert!(row.contains("read write"), "{row}");
    let tokens = s.admin().tokens_for("ana").await.unwrap().tokens;
    let t = tokens.iter().find(|t| t.name == "ci").unwrap();
    assert!(t.expires_at.is_some());

    // Revoked, it stops working.
    let action = format!("/admin/people/ana/tokens/{}/revoke", t.id);
    let r = b.submit(&again, &action, &[]).await;
    assert_eq!(r.status, 303);
    assert!(s.client(&value).whoami().await.is_err());
    assert_eq!(note(&b.follow("/admin/people/ana").await), "Token revoked.");

    // A bad expiry is refused before anything is created.
    let page = b.get("/admin/people/ana").await;
    let r = b
        .submit(
            &page,
            "/admin/people/ana/tokens",
            &[("name", "x"), ("days", "soon")],
        )
        .await;
    assert!(
        error_text(&r).contains("number of days"),
        "{}",
        error_text(&r)
    );
}

async fn add(b: &mut Browser, page: Page, fields: Vec<(&'static str, &'static str)>) {
    let r = b.submit(&page, "/admin/access/rules", &fields).await;
    assert_eq!(r.status, 303, "{}", r.body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn access_rules_are_listed_added_checked_and_removed() {
    let s = server().await;
    s.create_depot("game").await;
    add_user(&s, "ana").await;
    s.admin().create_group("artists").await.unwrap();
    s.admin().add_group_member("artists", "ana").await.unwrap();
    let mut b = signed_in(&s).await;
    let page = b.get("/admin/access").await;
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.doc().text().contains("No rules"));

    add(
        &mut b,
        page,
        vec![
            ("depot", "game"),
            ("subject", "group"),
            ("name", "artists"),
            ("branch", "**"),
            ("path", "Content/**"),
            ("effect", "allow"),
            ("level", "write"),
        ],
    )
    .await;
    let page = b.get("/admin/access").await;
    assert!(note(&page).starts_with("Rule #"), "{}", note(&page));
    add(
        &mut b,
        page,
        vec![
            ("depot", "game"),
            ("subject", "user"),
            ("name", "ana"),
            ("branch", "**"),
            ("path", "Content/Secret/**"),
            ("effect", "deny"),
            ("level", "read"),
        ],
    )
    .await;
    let rules = s.admin().acl().await.unwrap().rules;
    assert_eq!(rules.len(), 2);
    let (allow, deny) = (rules[0].id.unwrap(), rules[1].id.unwrap());
    let page = b.get("/admin/access").await;
    let rows: Vec<String> = page
        .doc()
        .find(&|n| n.name == "tr" && n.attr("id").is_some_and(|i| i.starts_with("rule-")))
        .iter()
        .map(|n| n.text())
        .collect();
    assert!(
        rows[0].starts_with(&format!(
            "1 #{allow} game group artists ** Content/** allow write"
        )),
        "{rows:?}"
    );
    assert!(
        rows[1].starts_with(&format!("2 #{deny} game user ana")),
        "{rows:?}"
    );

    // The allow decides on art, the deny under Secret.
    let check = b
        .get("/admin/access?principal=ana&depot=game&branch=main&path=Content%2Fhero.psd")
        .await;
    let doc = check.doc();
    let result = doc.by_id("check-result").unwrap().text();
    assert!(
        result.starts_with("ana gets write on Content/hero.psd of branch main in game."),
        "{result}"
    );
    assert!(
        result.contains(&format!(
            "Decided by rule #{allow}: allow write to group artists"
        )),
        "{result}"
    );
    assert_eq!(
        doc.by_id(&format!("rule-{allow}")).unwrap().attr("class"),
        Some("decided")
    );
    let check = b
        .get("/admin/access?principal=ana&depot=game&path=Content%2FSecret%2Fplan.txt")
        .await;
    let result = check.doc().by_id("check-result").unwrap().text();
    assert!(
        result.starts_with("ana gets no access on Content/Secret/plan.txt"),
        "{result}"
    );
    assert!(
        result.contains(&format!("Decided by rule #{deny}: deny read to user ana")),
        "{result}"
    );
    let check = b
        .get("/admin/access?principal=ana&depot=game&path=Docs%2Fa.md")
        .await;
    let result = check.doc().by_id("check-result").unwrap().text();
    assert!(result.contains("No rule applies to this path."), "{result}");
    let check = b
        .get("/admin/access?principal=admin&depot=game&path=x")
        .await;
    let result = check.doc().by_id("check-result").unwrap().text();
    assert!(
        result.contains("admin gets admin") && result.contains("built-in administrator"),
        "{result}"
    );
    let check = b
        .get("/admin/access?principal=nobody&depot=game&path=x")
        .await;
    assert!(
        error_text(&check).contains("No principal"),
        "{}",
        error_text(&check)
    );

    // Removing a rule.
    let page = b.get("/admin/access").await;
    let r = b
        .submit(&page, &format!("/admin/access/rules/{deny}/remove"), &[])
        .await;
    assert_eq!(r.status, 303);
    assert_eq!(s.admin().acl().await.unwrap().rules.len(), 1);
    // A rule the API refuses: a user rule without a name.
    let page = b.get("/admin/access").await;
    let r = b
        .submit(
            &page,
            "/admin/access/rules",
            &[("subject", "user"), ("name", "")],
        )
        .await;
    assert!(
        error_text(&r).contains("Name the user or group"),
        "{}",
        error_text(&r)
    );
}

/// A receiver of webhook deliveries that answers 200 and counts what it got.
async fn receiver() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let got = std::sync::Arc::new(AtomicUsize::new(0));
    let count = got.clone();
    let app = axum::Router::new().fallback(move || {
        let count = count.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            "ok"
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, got)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhooks_are_added_tested_and_removed() {
    let s = server_with(|c| {
        c.modules.webhooks = true;
        c.webhooks.allow_http = true;
    })
    .await;
    s.create_depot("game").await;
    let (url, got) = receiver().await;
    let mut b = signed_in(&s).await;

    let list = b.get("/admin/webhooks").await;
    assert_eq!(list.status, 200, "{}", list.body);
    let nav = list.doc().find(&|n| n.name == "nav")[0].text();
    assert!(nav.contains("Webhooks"), "{nav}");
    let events: Vec<String> = list
        .doc()
        .find(&|n| n.name == "input" && n.attr("name") == Some("events"))
        .iter()
        .map(|n| n.attr("value").unwrap().to_owned())
        .collect();
    assert_eq!(events, lode_proto::webhooks::EVENTS.to_vec());

    // At least one event, checked before anything is sent.
    let none = b
        .submit(
            &list,
            "/admin/webhooks",
            &[("depot", "game"), ("url", &url)],
        )
        .await;
    assert_eq!(error_text(&none), "Pick at least one event to send.");
    // The API's own checks come back on the page.
    let bad = b
        .submit(
            &list,
            "/admin/webhooks",
            &[
                ("depot", "game"),
                ("url", "ftp://x"),
                ("events", "revision.created"),
            ],
        )
        .await;
    assert!(!error_text(&bad).is_empty(), "{}", bad.body);

    let created = b
        .post(
            "/admin/webhooks",
            &[
                ("csrf", &csrf(&list)),
                ("depot", "game"),
                ("url", &url),
                ("events", "revision.created"),
                ("events", "lock.acquired"),
                ("branch", "main"),
            ],
        )
        .await;
    assert_eq!(created.status, 200, "{}", created.body);
    let secret = created.doc().by_id("webhook-secret").unwrap().text();
    assert!(secret.len() >= 32, "{secret}");
    let hooks = s
        .client(s.admin_token())
        .webhooks("game")
        .await
        .unwrap()
        .webhooks;
    assert_eq!(hooks.len(), 1);
    let hook = &hooks[0];
    assert_eq!(
        hook.events,
        vec!["revision.created".to_owned(), "lock.acquired".to_owned()]
    );
    assert_eq!(hook.branch.as_deref(), Some("main"));

    // Listed, without the secret.
    let list = b.get("/admin/webhooks").await;
    assert!(
        !list.body.contains(&secret),
        "the secret is shown once only"
    );
    let row = list
        .doc()
        .by_id(&format!("hook-{}", hook.id))
        .unwrap()
        .text();
    assert!(row.contains("game") && row.contains(&url), "{row}");
    assert!(row.contains("revision.created, lock.acquired"), "{row}");

    // A test delivery reaches the receiver and shows on the hook's page.
    let base = format!("/admin/webhooks/game/{}", hook.id);
    let r = b.submit(&list, &format!("{base}/ping"), &[]).await;
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(r.location(), Some(base.as_str()));
    let page = b.follow(&base).await;
    assert!(
        note(&page).starts_with("Test delivery #"),
        "{}",
        note(&page)
    );
    assert!(
        note(&page).ends_with("queued. Its result shows below."),
        "{}",
        note(&page)
    );
    let delivered = |page: &Page| {
        let doc = page.doc();
        let rows: Vec<String> = doc
            .find(&|n| n.name == "tr" && n.attr("id").is_some_and(|i| i.starts_with("delivery-")))
            .iter()
            .map(|n| n.text())
            .collect();
        assert_eq!(rows.len(), 1, "{rows:?}");
        rows[0].contains("delivered").then(|| rows[0].clone())
    };
    let mut page = page;
    let mut tries = 0;
    let row = loop {
        if let Some(row) = delivered(&page) {
            break row;
        }
        tries += 1;
        assert!(tries < 100, "the test delivery was not sent: {}", page.body);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        page = b.get(&base).await;
    };
    assert!(row.contains("ping") && row.contains("HTTP 200"), "{row}");
    assert_eq!(got.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(!page.body.contains(&secret));

    // Removed, it is gone.
    let r = b.submit(&page, &format!("{base}/remove"), &[]).await;
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        note(&b.follow("/admin/webhooks").await),
        format!("Webhook #{} removed.", hook.id)
    );
    assert!(s
        .client(s.admin_token())
        .webhooks("game")
        .await
        .unwrap()
        .webhooks
        .is_empty());
    assert_eq!(b.get(&base).await.status, 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_webhooks_module_there_is_no_webhooks_page() {
    let s = server().await;
    let mut b = signed_in(&s).await;
    let page = b.get("/admin/").await;
    let nav = page.doc().find(&|n| n.name == "nav")[0].text();
    assert!(!nav.contains("Webhooks"), "{nav}");
    assert_eq!(b.get("/admin/webhooks").await.status, 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_settings_page_shows_the_configuration_without_secrets() {
    use lode_testkit::oidc::TestProvider;
    let provider = TestProvider::start("lode", "very-secret-value").await;
    let oidc = lode_server::OidcConfig::new(provider.issuer(), "lode")
        .unwrap()
        .with_client_secret("very-secret-value");
    let s = server_with(move |c| c.oidc = Some(oidc)).await;
    let mut b = signed_in(&s).await;
    let page = b.get("/admin/settings").await;
    assert_eq!(page.status, 200, "{}", page.body);
    let text = page.doc().by_id("settings").unwrap().text();
    assert!(text.contains("[admin_ui]"), "{text}");
    assert!(text.contains("client_id = \"lode\""), "{text}");
    assert!(text.contains("client_secret = \"***\""), "{text}");
    assert!(!page.body.contains("very-secret-value"));
    assert!(text.contains("[modules]"), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_is_deleted_after_typing_their_name() {
    let s = server().await;
    add_user(&s, "ana").await;
    s.create_agent("ana-bot", "ana", &["read"]).await;
    let mut b = signed_in(&s).await;
    let page = b.get("/admin/people/ana").await;
    assert!(
        page.doc()
            .text()
            .contains("Their agents are deleted with them: ana-bot."),
        "{}",
        page.body
    );

    let wrong = b
        .submit(
            &page,
            "/admin/people/ana/delete",
            &[("confirm_name", "bob")],
        )
        .await;
    assert_eq!(wrong.status, 200);
    assert!(
        error_text(&wrong).contains("exactly"),
        "{}",
        error_text(&wrong)
    );
    assert!(!principal(&s, "ana").await.disabled);

    let r = b
        .submit(
            &page,
            "/admin/people/ana/delete",
            &[("confirm_name", "ana")],
        )
        .await;
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(r.location(), Some("/admin/people"));
    let people = b.get("/admin/people").await;
    assert_eq!(note(&people), "ana deleted, with agents ana-bot.");
    assert!(people
        .doc()
        .find(&|n| n.name == "a" && n.attr("href") == Some("/admin/people/ana"))
        .is_empty());
    assert_eq!(b.get("/admin/people/ana").await.status, 404);

    // The built-in administrator has no delete form.
    let admin = b.get("/admin/people/admin").await;
    assert!(admin
        .doc()
        .find(&|n| n.attr("action") == Some("/admin/people/admin/delete"))
        .is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_depot_goes_to_the_trash_and_comes_back_or_is_erased() {
    let s = server().await;
    s.create_depot("game").await;
    s.create_depot("art").await;
    let mut b = signed_in(&s).await;
    let page = b.get("/admin/depots/game").await;

    let wrong = b
        .submit(
            &page,
            "/admin/depots/game/delete",
            &[("confirm_name", "gam")],
        )
        .await;
    assert_eq!(wrong.status, 200);
    assert!(
        error_text(&wrong).contains("exactly"),
        "{}",
        error_text(&wrong)
    );

    let r = b
        .submit(
            &page,
            "/admin/depots/game/delete",
            &[("confirm_name", "game")],
        )
        .await;
    assert_eq!(r.status, 303, "{}", r.body);
    let list = b.get("/admin/depots").await;
    assert!(
        note(&list).starts_with("Depot game moved to the trash"),
        "{}",
        note(&list)
    );
    let trash = list.doc();
    let trash = trash.by_id("trash").expect("a trash section");
    assert!(trash.text().contains("Trash"));
    assert_eq!(b.get("/admin/depots/game").await.status, 404);

    // Restore brings it back.
    let r = b.submit(&list, "/admin/trash/game/restore", &[]).await;
    assert_eq!(r.location(), Some("/admin/depots/game"), "{}", r.body);
    assert_eq!(b.get("/admin/depots/game").await.status, 200);
    assert!(s.admin().trash().await.unwrap().depots.is_empty());

    // Into the trash again, then erased from it.
    let page = b.get("/admin/depots/game").await;
    b.submit(
        &page,
        "/admin/depots/game/delete",
        &[("confirm_name", "game")],
    )
    .await;
    let list = b.get("/admin/depots").await;
    let r = b
        .submit(
            &list,
            "/admin/trash/game/erase",
            &[("confirm_name", "game")],
        )
        .await;
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(note(&b.get("/admin/depots").await), "Depot game erased.");
    assert!(s.admin().trash().await.unwrap().depots.is_empty());

    // Erase now, straight from the depot page.
    let page = b.get("/admin/depots/art").await;
    let r = b
        .submit(
            &page,
            "/admin/depots/art/delete",
            &[("confirm_name", "art"), ("force", "1")],
        )
        .await;
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(s.admin().trash().await.unwrap().depots.is_empty());
    assert!(s.admin().depots().await.unwrap().depots.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rule_for_every_agent_is_added_and_removed() {
    let s = server().await;
    s.create_depot("game").await;
    let mut b = signed_in(&s).await;
    let page = b.get("/admin/access").await;
    assert!(page
        .body
        .contains("<option value=\"agents\">Every agent</option>"));
    add(
        &mut b,
        page,
        vec![
            ("depot", "game"),
            ("subject", "agents"),
            ("name", ""),
            ("branch", "main"),
            ("path", "**"),
            ("effect", "deny"),
            ("level", "write"),
        ],
    )
    .await;
    let rules = s.admin().acl().await.unwrap().rules;
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].subject, AclSubject::Agents);
    let id = rules[0].id.unwrap();
    let page = b.get("/admin/access").await;
    let row = page.doc().by_id(&format!("rule-{id}")).unwrap().text();
    assert!(row.contains("game every agent main ** deny write"), "{row}");
    let r = b
        .submit(&page, &format!("/admin/access/rules/{id}/remove"), &[])
        .await;
    assert_eq!(r.status, 303);
    assert!(s.admin().acl().await.unwrap().rules.is_empty());
}
