//! A throwaway server with sample data, for looking at the admin web UI and taking the
//! screenshots in the user guide (`docs/screenshots/admin/`, made by
//! `docs/screenshots/admin.mjs`).
//!
//! ```text
//! cargo run -p lode-testkit --example admin_demo
//! ```
//!
//! It prints the address and runs until stopped. Sign in as `admin` with the password
//! `demo-password`. Everything lives in a temporary folder that goes away with it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

use lode_proto::admin::{
    AccessLevel, AclEffect, AclRule, AclSubject, CreatePrincipalRequest, PatchPrincipalRequest,
};
use lode_proto::PrincipalKind;
use lode_testkit::{TestServer, TestServerOptions};

const PASSWORD: &str = "demo-password";

fn rule(
    depot: Option<&str>,
    subject: AclSubject,
    path: &str,
    effect: AclEffect,
    level: AccessLevel,
) -> AclRule {
    AclRule {
        id: None,
        depot: depot.map(str::to_owned),
        subject,
        branch: "**".into(),
        path: path.into(),
        effect,
        level,
    }
}

#[tokio::main]
async fn main() {
    // A receiver for the sample webhook, which accepts everything.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hook_url = format!("http://{}/lode", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let app = axum::Router::new().fallback(|| async { "ok" });
        axum::serve(listener, app).await.unwrap();
    });

    let s = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| {
            c.modules.webhooks = true;
            c.webhooks.allow_http = true;
        }),
    })
    .await;
    let admin = s.admin();
    let password = |p: &str| PatchPrincipalRequest {
        password: Some(p.into()),
        ..Default::default()
    };
    admin
        .patch_principal("admin", &password(PASSWORD))
        .await
        .unwrap();

    for (name, display) in [
        ("ana", "Ana Ruiz"),
        ("ben", "Ben Okafor"),
        ("chen", "Chen Wei"),
    ] {
        admin
            .create_principal(&CreatePrincipalRequest {
                kind: PrincipalKind::User,
                name: name.into(),
                display_name: Some(display.into()),
                owner: None,
                password: Some(PASSWORD.into()),
                sso: None,
            })
            .await
            .unwrap();
    }
    admin
        .patch_principal(
            "chen",
            &PatchPrincipalRequest {
                disabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    s.create_agent("build-bot", "ana", &["read", "write"]).await;
    admin.create_group("artists").await.unwrap();
    admin.add_group_member("artists", "ana").await.unwrap();
    admin.add_group_member("artists", "ben").await.unwrap();

    s.create_depot("game").await;
    s.create_depot("tools").await;
    s.commit_files(
        "game",
        "main",
        &[
            ("Content/hero.psd", b"v1"),
            ("Source/main.cpp", b"int main(){}"),
        ],
    )
    .await;
    s.commit_files(
        "game",
        "main",
        &[("Content/hero.psd", b"v2"), ("Content/level1.umap", b"map")],
    )
    .await;
    s.commit_files(
        "game",
        "main",
        &[("Source/main.cpp", b"int main(){return 0;}")],
    )
    .await;
    s.commit_files("tools", "main", &[("build.py", b"print('build')")])
        .await;
    admin
        .create_branch(
            "game",
            &lode_proto::depot::CreateBranchRequest {
                name: "release/1.0".into(),
                from_branch: "main".into(),
                from_rev: None,
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
                workspace: "studio-pc".into(),
                comment: Some("repainting the armour".into()),
                paths: vec![lode_proto::locks::LockPath {
                    path: lode_core::DepotPath::parse("Content/hero.psd").unwrap(),
                    base_id: lode_core::blob_id(&b"v2"[..]).unwrap(),
                }],
            },
        )
        .await
        .unwrap();

    for r in [
        rule(
            Some("game"),
            AclSubject::All,
            "**",
            AclEffect::Allow,
            AccessLevel::Read,
        ),
        rule(
            Some("game"),
            AclSubject::Group("artists".into()),
            "Content/**",
            AclEffect::Allow,
            AccessLevel::Write,
        ),
        rule(
            Some("game"),
            AclSubject::User("ben".into()),
            "Content/Cinematics/**",
            AclEffect::Deny,
            AccessLevel::Read,
        ),
        rule(
            Some("tools"),
            AclSubject::User("ana".into()),
            "**",
            AclEffect::Allow,
            AccessLevel::Write,
        ),
    ] {
        admin.add_acl_rule(&r).await.unwrap();
    }

    let hook = admin
        .create_webhook(
            "game",
            &lode_proto::webhooks::CreateWebhookRequest {
                url: hook_url,
                events: vec!["revision.created".into(), "lock.acquired".into()],
                branch: Some("main".into()),
            },
        )
        .await
        .unwrap();
    admin.ping_webhook("game", hook.hook.id).await.unwrap();
    // A shelf waiting for review, for the depot browser's shelf pages (T174).
    let before = b"int main(){return 0;}";
    let after = b"int main(){\n    return 1;\n}\n";
    let new_id = lode_core::blob_id(&after[..]).unwrap();
    admin
        .put_blob("game", &new_id, std::io::Cursor::new(after.to_vec()))
        .await
        .unwrap();
    let head = admin.branch("game", "main").await.unwrap().head_rev;
    let shelver = s.token_for("admin", &["read", "write", "shelve"]).await;
    s.client(&shelver)
        .create_shelf(
            "game",
            &lode_proto::shelves::CreateShelfRequest {
                branch: "main".into(),
                base_rev: head,
                description: "Return 1 when the game fails to start".into(),
                changes: vec![lode_core::Change {
                    path: lode_core::DepotPath::parse("Source/main.cpp").unwrap(),
                    action: lode_core::ChangeAction::Modify,
                    base_id: Some(lode_core::blob_id(&before[..]).unwrap()),
                    new_id: Some(new_id),
                    kind: Some(lode_core::EntryKind::File),
                    executable: false,
                    size: Some(after.len() as u64),
                    copy_from: None,
                }],
                merged_from: Vec::new(),
                provenance: None,
            },
        )
        .await
        .unwrap();
    admin.gc("game", false).await.unwrap();
    admin.fsck("game", false).await.unwrap();

    println!(
        "Lode admin demo at {}/admin/  (admin / {PASSWORD})",
        s.url()
    );
    println!("LODE_DEMO_URL={}", s.url());
    std::future::pending::<()>().await;
}
