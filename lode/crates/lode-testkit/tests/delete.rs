//! Deleting users and depots: a deleted user keeps their name in history; a deleted depot
//! goes to the trash, can be restored, and is erased after 30 days or at once with force.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_core::{blob_id, DepotPath};
use lode_meta::Reads;
use lode_proto::admin::{AccessLevel, AclEffect, CreatePrincipalRequest};
use lode_proto::locks::{LockPath, LockRequest, LocksQuery};
use lode_proto::read::RevisionListQuery;
use lode_proto::PrincipalKind;
use lode_server::Clock;
use lode_testkit::TestServer;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

#[track_caller]
fn code<T: std::fmt::Debug>(r: Result<T, lode_client::Error>) -> String {
    r.expect_err("expected an error")
        .api()
        .expect("an answer from the server")
        .code
        .clone()
}

async fn depot_id(s: &TestServer, name: &str) -> Option<i64> {
    let name = name.to_owned();
    s.state()
        .meta()
        .read(move |tx| tx.depot_by_name(&name))
        .await
        .unwrap()
        .map(|d| d.id.0)
}

fn depot_dir(s: &TestServer, id: i64) -> std::path::PathBuf {
    s.data_dir()
        .join(lode_server::server::DEPOTS_DIR)
        .join(id.to_string())
}

async fn lock(s: &TestServer, token: &str, depot: &str, path: &str, content: &[u8]) {
    s.client(token)
        .lock(
            depot,
            &LockRequest {
                branch: "main".into(),
                workspace: "ws".into(),
                comment: None,
                paths: vec![LockPath {
                    path: DepotPath::parse(path).unwrap(),
                    base_id: blob_id(content).unwrap(),
                }],
            },
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_user_is_gone_but_history_keeps_the_name() {
    let s = TestServer::start().await;
    s.create_depot("game").await;
    let ana = s.create_user("ana", &["read", "write", "lock"]).await;
    let bot = s.create_agent("ana-bot", "ana", &["read"]).await;
    s.acl(
        "game",
        "ana",
        "**",
        "**",
        AclEffect::Allow,
        AccessLevel::Write,
    )
    .await;
    let admin = s.admin();
    admin.create_group("artists").await.unwrap();
    admin.add_group_member("artists", "ana").await.unwrap();
    let rev = s
        .commit_files_as(&s.client(&ana), "game", "main", &[("a.txt", b"one")])
        .await;
    lock(&s, &ana, "game", "a.txt", b"one").await;

    // Guards: not the built-in administrator, not yourself.
    assert_eq!(
        code(admin.delete_principal("admin").await),
        "invalid_request"
    );
    let r = admin.delete_principal("ana").await.unwrap();
    assert_eq!(r.name, "ana");
    assert_eq!(r.agents, ["ana-bot"]);

    // Neither can authenticate any more.
    assert_eq!(code(s.client(&ana).whoami().await), "unauthenticated");
    assert_eq!(code(s.client(&bot).whoami().await), "unauthenticated");
    // Gone from lists, groups, rules and locks.
    let names: Vec<String> = admin
        .principals()
        .await
        .unwrap()
        .principals
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert!(
        !names.iter().any(|n| n == "ana" || n == "ana-bot"),
        "{names:?}"
    );
    let groups = admin.groups().await.unwrap().groups;
    assert!(groups[0].members.is_empty());
    let rules = admin.acl().await.unwrap().rules;
    assert!(rules
        .iter()
        .all(|r| !format!("{:?}", r.subject).contains("ana")));
    let locks = admin.locks("game", &LocksQuery::default()).await.unwrap();
    assert!(locks.locks.is_empty());
    // History still names the author.
    let revs = admin
        .revisions("game", &RevisionListQuery::default())
        .await
        .unwrap()
        .revisions;
    let r = revs.iter().find(|r| r.rev == rev).unwrap();
    assert_eq!(r.author.name, "ana");
    // The name stays taken; the deleted user cannot be changed or deleted again.
    let again = admin
        .create_principal(&CreatePrincipalRequest {
            kind: PrincipalKind::User,
            name: "ana".into(),
            display_name: None,
            owner: None,
            password: None,
            sso: None,
        })
        .await;
    assert_eq!(code(again), "conflict");
    assert_eq!(code(admin.delete_principal("ana").await), "not_found");
    assert_eq!(
        code(admin.add_group_member("artists", "ana").await),
        "not_found"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_administrator_cannot_delete_themselves() {
    let s = TestServer::start().await;
    let boss = s.create_user("boss", &["read", "admin"]).await;
    s.admin()
        .add_acl_rule(&lode_proto::admin::AclRule {
            id: None,
            depot: None,
            subject: lode_proto::admin::AclSubject::User("boss".into()),
            branch: "**".into(),
            path: "**".into(),
            effect: AclEffect::Allow,
            level: AccessLevel::Admin,
        })
        .await
        .unwrap();
    let c = s.client(&boss);
    assert_eq!(code(c.delete_principal("boss").await), "invalid_request");
    // Someone without server-admin standing cannot delete anyone.
    let ana = s.create_user("ana", &["read", "admin"]).await;
    assert_eq!(
        code(s.client(&ana).delete_principal("boss").await),
        "permission_denied"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_depot_waits_in_the_trash_then_is_erased() {
    let s = TestServer::start().await;
    s.create_depot("game").await;
    s.commit_files("game", "main", &[("a.txt", b"one")]).await;
    let id = depot_id(&s, "game").await.unwrap();
    assert!(depot_dir(&s, id).exists());
    let admin = s.admin();

    // The name must be typed again.
    assert_eq!(
        code(admin.delete_depot("game", "gmae", false).await),
        "invalid_request"
    );
    let r = admin.delete_depot("game", "game", false).await.unwrap();
    assert!(!r.purged);
    let after = r.purge_after.unwrap();
    assert!(after.unix_timestamp() * 1000 > s.clock().now_ms() + 29 * DAY_MS);

    // Gone for everyone, but kept.
    assert!(admin.depots().await.unwrap().depots.is_empty());
    assert_eq!(code(admin.branches("game", true).await), "not_found");
    assert_eq!(code(admin.create_depot("game").await), "conflict");
    assert_eq!(
        code(admin.delete_depot("game", "game", false).await),
        "conflict"
    );
    let trash = admin.trash().await.unwrap().depots;
    assert_eq!(trash.len(), 1);
    assert_eq!(trash[0].name, "game");
    assert!(depot_dir(&s, id).exists());

    // Restored with its history.
    admin.restore_depot("game").await.unwrap();
    assert_eq!(
        admin.branches("game", false).await.unwrap().branches[0].head_rev,
        2
    );
    assert!(admin.trash().await.unwrap().depots.is_empty());

    // Deleted again: nothing happens before 30 days, then it is erased.
    admin.delete_depot("game", "game", false).await.unwrap();
    s.clock().advance_ms(29 * DAY_MS);
    assert!(lode_server::trash::purge_due(s.state())
        .await
        .unwrap()
        .is_empty());
    s.clock().advance_ms(2 * DAY_MS);
    assert_eq!(
        lode_server::trash::purge_due(s.state()).await.unwrap(),
        ["game"]
    );
    assert!(admin.trash().await.unwrap().depots.is_empty());
    assert!(!depot_dir(&s, id).exists());
    assert_eq!(code(admin.restore_depot("game").await), "not_found");

    // The name is free; the new depot starts empty with a new id.
    s.create_depot("game").await;
    assert_ne!(depot_id(&s, "game").await.unwrap(), id);
    assert_eq!(
        admin.branches("game", false).await.unwrap().branches[0].head_rev,
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forced_delete_erases_at_once() {
    let s = TestServer::start().await;
    s.create_depot("game").await;
    s.commit_files("game", "main", &[("a.txt", b"one")]).await;
    let ana = s.create_user("ana", &["read", "write", "lock"]).await;
    s.acl(
        "game",
        "ana",
        "**",
        "**",
        AclEffect::Allow,
        AccessLevel::Write,
    )
    .await;
    lock(&s, &ana, "game", "a.txt", b"one").await;
    let id = depot_id(&s, "game").await.unwrap();
    let admin = s.admin();

    let r = admin.delete_depot("game", "game", true).await.unwrap();
    assert!(r.purged);
    assert!(r.purge_after.is_none());
    assert!(!depot_dir(&s, id).exists());
    assert!(admin.trash().await.unwrap().depots.is_empty());
    // Every row that named the depot is gone: history, rules, module rows.
    let left = s
        .state()
        .meta()
        .read(move |tx| {
            let db = tx.db();
            let count = |sql: &str| -> i64 { db.value(sql, lode_meta::vals![id]).unwrap() };
            Ok([
                count("SELECT count(*) FROM revision WHERE depot_id = ?1"),
                count("SELECT count(*) FROM branch WHERE depot_id = ?1"),
                count("SELECT count(*) FROM acl WHERE depot_id = ?1"),
                count("SELECT count(*) FROM mod_locks_lock WHERE depot_id = ?1"),
            ])
        })
        .await
        .unwrap();
    assert_eq!(left, [0, 0, 0, 0]);
    // The name is free at once.
    s.create_depot("game").await;
    assert_ne!(depot_id(&s, "game").await.unwrap(), id);

    // A depot already in the trash can be erased now too.
    s.create_depot("old").await;
    admin.delete_depot("old", "old", false).await.unwrap();
    assert!(admin.delete_depot("old", "old", true).await.unwrap().purged);
    assert!(admin.trash().await.unwrap().depots.is_empty());

    // Only server administrators delete depots.
    assert_eq!(
        code(s.client(&ana).delete_depot("game", "game", true).await),
        "permission_denied"
    );
    assert_eq!(
        code(admin.delete_depot("nope", "nope", false).await),
        "not_found"
    );
}
