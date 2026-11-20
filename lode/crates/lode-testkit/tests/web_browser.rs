//! T173 acceptance: the read-only depot browser under `/web/`, for any signed-in user, showing
//! only what their access rules allow.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_proto::admin::{AccessLevel, AclEffect, CreatePrincipalRequest};
use lode_proto::PrincipalKind;
use lode_testkit::web::{Browser, Page};
use lode_testkit::TestServer;

const PASSWORD: &str = "correct horse battery";
/// The smallest PNG header, enough for a content type check.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

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

async fn sign_in(s: &TestServer, name: &str, password: &str) -> Browser {
    let mut b = Browser::new(s.url());
    let form = b.get("/admin/sign-in").await;
    let r = b
        .submit(
            &form,
            "/admin/sign-in",
            &[("name", name), ("password", password)],
        )
        .await;
    assert_eq!(r.status, 303, "{}", r.body);
    b
}

fn texts(page: &Page, tag: &str) -> Vec<String> {
    page.doc().all(tag).iter().map(|n| n.text()).collect()
}

fn note(page: &Page) -> String {
    page.doc()
        .find(&|n| n.attr("class") == Some("note"))
        .iter()
        .map(|n| n.text())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A depot `game` that `ana` reads except `secret/`, and a depot `other` she cannot read.
async fn setup() -> (TestServer, u64) {
    let s = TestServer::start().await;
    add_user(&s, "ana").await;
    s.create_depot("game").await;
    s.create_depot("other").await;
    s.commit_files(
        "game",
        "main",
        &[
            ("README.md", b"Hello\nworld\n"),
            ("art/logo.png", PNG),
            ("bin/data.bin", b"\0\x01\x02binary"),
            ("secret/keys.txt", b"hunter2"),
        ],
    )
    .await;
    let r2 = s
        .commit_files("game", "main", &[("README.md", b"Hello\nthere\n")])
        .await;
    s.commit_files("other", "main", &[("private.txt", b"no")])
        .await;
    s.acl(
        "game",
        "ana",
        "**",
        "**",
        AclEffect::Allow,
        AccessLevel::Read,
    )
    .await;
    s.acl(
        "game",
        "ana",
        "**",
        "secret/**",
        AclEffect::Deny,
        AccessLevel::Read,
    )
    .await;
    (s, r2)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_browses_what_they_may_read() {
    let (s, r2) = setup().await;

    // Not signed in: the sign-in page, then back.
    let mut anon = Browser::new(s.url());
    let r = anon.get("/web/depots/game").await;
    assert_eq!(r.status, 303);
    assert_eq!(
        r.location(),
        Some("/admin/sign-in?next=%2Fweb%2Fdepots%2Fgame")
    );

    let mut b = sign_in(&s, "ana", PASSWORD).await;
    let home = b.get("/web/").await;
    assert_eq!(home.status, 200, "{}", home.body);
    let links = texts(&home, "a");
    assert!(links.contains(&"game".to_owned()), "{links:?}");
    assert!(!links.contains(&"other".to_owned()), "{links:?}");
    assert!(
        !links.contains(&"Admin".to_owned()),
        "no admin link for ana"
    );

    // The tree hides the denied directory.
    let tree = b.get("/web/depots/game").await;
    assert_eq!(tree.status, 200, "{}", tree.body);
    let names = texts(&tree, "a");
    assert!(names.contains(&"README.md".to_owned()), "{names:?}");
    assert!(names.contains(&"art/".to_owned()), "{names:?}");
    assert!(!names.iter().any(|n| n.contains("secret")), "{names:?}");
    assert!(tree.body.contains(&format!("head r{r2}")));
    let link = tree.doc().by_id("lode-link").unwrap().text();
    assert!(
        link.starts_with("lode://127.0.0.1:") && link.ends_with(&format!("/game/r{r2}?http=1")),
        "{link}"
    );

    // A text file with line numbers, its history and its link.
    let file = b
        .get("/web/depots/game/file?branch=main&path=README.md")
        .await;
    assert_eq!(file.status, 200, "{}", file.body);
    let lines: Vec<String> = file
        .doc()
        .find(&|n| n.attr("class") == Some("src"))
        .iter()
        .map(|n| n.text())
        .collect();
    assert_eq!(lines, ["Hello", "there"]);
    let link = file.doc().by_id("lode-link").unwrap().text();
    assert!(
        link.ends_with("/game/README.md?branch=main&http=1"),
        "{link}"
    );
    assert!(texts(&file, "a").contains(&format!("r{r2}")));
    // An older revision of it.
    let old = b
        .get(&format!(
            "/web/depots/game/file?branch=main&rev={}&path=README.md",
            r2 - 1
        ))
        .await;
    assert!(old.body.contains(">world<"), "{}", old.body);

    // An image shows inline; its bytes come with an image type, or as a download.
    let img = b
        .get("/web/depots/game/file?branch=main&path=art%2Flogo.png")
        .await;
    assert_eq!(img.doc().all("img").len(), 1);
    let raw = b
        .get(&format!(
            "/web/depots/game/raw?rev={r2}&path=art%2Flogo.png"
        ))
        .await;
    assert_eq!(raw.status, 200);
    assert_eq!(raw.headers["content-type"], "image/png");
    assert_eq!(raw.headers["content-disposition"], "inline");
    let dl = b
        .get(&format!(
            "/web/depots/game/raw?rev={r2}&path=art%2Flogo.png&download=1"
        ))
        .await;
    assert_eq!(dl.headers["content-type"], "application/octet-stream");
    assert!(dl.headers["content-disposition"].starts_with("attachment; filename=\"logo.png\""));

    // A binary file is offered as a download.
    let bin = b
        .get("/web/depots/game/file?branch=main&path=bin%2Fdata.bin")
        .await;
    assert!(bin.body.contains("This file is not text."), "{}", bin.body);

    // Hidden and unreadable things are not there.
    let hidden = b
        .get("/web/depots/game/file?branch=main&path=secret%2Fkeys.txt")
        .await;
    assert_eq!(hidden.status, 404, "{}", hidden.body);
    assert!(!hidden.body.contains("hunter2"));
    let raw = b
        .get(&format!(
            "/web/depots/game/raw?rev={r2}&path=secret%2Fkeys.txt"
        ))
        .await;
    assert_eq!(raw.status, 404);
    let other = b.get("/web/depots/other").await;
    assert_ne!(other.status, 200);
    assert!(!other.body.contains("private.txt"));

    // A revision page with the diff.
    let rev = b.get(&format!("/web/depots/game/r/{r2}")).await;
    assert_eq!(rev.status, 200, "{}", rev.body);
    let class_text = |class: &str| -> Vec<String> {
        rev.doc()
            .find(&|n| n.attr("class") == Some(class))
            .iter()
            .map(|n| n.text())
            .collect()
    };
    assert_eq!(class_text("del"), ["-world"]);
    assert_eq!(class_text("add"), ["+there"]);
    // The first revision's hidden file is not listed.
    let first = b.get(&format!("/web/depots/game/r/{}", r2 - 1)).await;
    assert!(first.body.contains("README.md"));
    assert!(!first.body.contains("keys.txt"), "{}", first.body);
    assert!(first.body.contains("3 files"));

    // History.
    let hist = b.get("/web/depots/game/history?branch=main").await;
    let revs = texts(&hist, "a");
    assert!(revs.contains(&format!("r{r2}")) && revs.contains(&format!("r{}", r2 - 1)));

    // Unknown pages are 404 in the browser's frame.
    assert_eq!(b.get("/web/nope").await.status, 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branches_are_picked_and_administrators_get_a_link_to_the_admin_pages() {
    let (s, r2) = setup().await;
    s.admin()
        .create_branch(
            "game",
            &lode_proto::depot::CreateBranchRequest {
                name: "dev".into(),
                from_branch: "main".into(),
                from_rev: None,
            },
        )
        .await
        .unwrap();
    let r3 = s
        .commit_files("game", "dev", &[("dev.txt", b"dev only")])
        .await;
    let mut b = sign_in(&s, "ana", PASSWORD).await;
    let main = b.get("/web/depots/game").await;
    let options: Vec<String> = texts(&main, "option");
    assert_eq!(options, ["dev", "main"]);
    assert!(!main.body.contains("dev.txt"));
    let dev = b.get("/web/depots/game?branch=dev").await;
    assert!(dev.body.contains("dev.txt"), "{}", dev.body);
    assert!(dev.body.contains(&format!("head r{r3}")));
    let fixed = b
        .get(&format!("/web/depots/game?branch=main&rev={r2}"))
        .await;
    assert!(fixed.body.contains("go to the head"));
    let none = b.get("/web/depots/game?branch=nope").await;
    assert_eq!(none.status, 404);

    // The built-in admin lands on the admin pages, and sees a link to them here.
    s.admin()
        .patch_principal(
            "admin",
            &lode_proto::admin::PatchPrincipalRequest {
                password: Some(PASSWORD.into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut a = sign_in(&s, "admin", PASSWORD).await;
    let home = a.get("/web/").await;
    let links = texts(&home, "a");
    assert!(links.contains(&"Admin".to_owned()), "{links:?}");
    assert!(
        links.contains(&"other".to_owned()),
        "an administrator sees every depot"
    );
    let admin_home = a.get("/admin/").await;
    assert!(texts(&admin_home, "a").contains(&"Browse".to_owned()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_account_page_lists_and_revokes_tokens_and_changes_the_password() {
    let (s, _) = setup().await;
    let cli = s.token_for("ana", &["read"]).await;
    let mut b = sign_in(&s, "ana", PASSWORD).await;
    let page = b.get("/web/account").await;
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("this session"));
    let forms: Vec<String> = page
        .doc()
        .all("form")
        .iter()
        .filter_map(|f| f.attr("action").map(str::to_owned))
        .filter(|a| a.ends_with("/revoke"))
        .collect();
    assert_eq!(
        forms.len(),
        1,
        "only the other token can be revoked: {forms:?}"
    );
    let r = b.submit(&page, &forms[0], &[]).await;
    assert_eq!(r.status, 303);
    let after = b.follow("/web/account").await;
    assert_eq!(note(&after), "Token revoked.");
    assert!(s.client(&cli).whoami().await.is_err(), "the token is gone");

    // A form without the session's CSRF token is refused.
    let r = b.post(&forms[0], &[("csrf", "nope")]).await;
    assert_eq!(r.status, 403);

    // The password: both new ones must match, then it changes.
    let r = b
        .submit(
            &after,
            "/web/account/password",
            &[
                ("password", PASSWORD),
                ("new_password", "a new password"),
                ("new_password_again", "another one"),
            ],
        )
        .await;
    assert_eq!(r.status, 303);
    let page = b.follow("/web/account").await;
    assert_eq!(note(&page), "The new passwords differ.");
    let r = b
        .submit(
            &page,
            "/web/account/password",
            &[
                ("password", PASSWORD),
                ("new_password", "a new password"),
                ("new_password_again", "a new password"),
            ],
        )
        .await;
    assert_eq!(r.status, 303);
    let page = b.follow("/web/account").await;
    assert_eq!(note(&page), "Password changed.");
    sign_in(&s, "ana", "a new password").await;

    // Signing out from the browser works like from the admin pages.
    let out = b.submit(&page, "/admin/sign-out", &[]).await;
    assert_eq!(out.status, 303);
    assert_eq!(b.get("/web/").await.status, 303);
}
