//! Acceptance tests for the metadata store (task T004).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use lode_core::{DepotId, ObjectId};
use lode_meta::*;

const NOW: Millis = 1_700_000_000_000;

fn open(dir: &tempfile::TempDir) -> MetaStore {
    MetaStore::open(dir.path().join("meta.db"), &[], MetaConfig::default()).unwrap()
}

#[test]
fn fresh_database_migrates_and_reopen_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("meta.db");
    let s = MetaStore::open(&path, &[], MetaConfig::default()).unwrap();
    let v = s.with_writer(|c| schema_version(c, "core")).unwrap();
    assert_eq!(v, CORE_MIGRATIONS.migrations.len() as u32);
    let (mode, fk, sync): (String, i64, i64) = s
        .with_writer(|c| {
            c.query_one(
                "SELECT (SELECT journal_mode FROM pragma_journal_mode), \
                        (SELECT foreign_keys FROM pragma_foreign_keys), \
                        (SELECT synchronous FROM pragma_synchronous)",
                vals![],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
        })
        .unwrap();
    assert_eq!((mode.as_str(), fk, sync), ("wal", 1, 2));
    drop(s);
    let s = MetaStore::open(&path, &[], MetaConfig::default()).unwrap();
    let count: i64 = s
        .with_writer(|c| c.value("SELECT count(*) FROM schema_version", vals![]))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn newer_schema_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("meta.db");
    drop(MetaStore::open(&path, &[], MetaConfig::default()).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE schema_version SET version = 99 WHERE namespace = 'core'",
        [],
    )
    .unwrap();
    drop(conn);
    let err = MetaStore::open(&path, &[], MetaConfig::default())
        .err()
        .unwrap();
    assert!(
        matches!(&err, MetaError::SchemaTooNew { namespace, found: 99, .. } if namespace == "core"),
        "{err}"
    );
    assert!(err.to_string().contains("upgrade Lode"));
}

const MOD_SET: MigrationSet = MigrationSet {
    namespace: "locks",
    migrations: &[Migration {
        version: 1,
        name: "init",
        sql: "CREATE TABLE mod_locks_lock (depot_id INTEGER, path TEXT, PRIMARY KEY (depot_id, path));",
        postgres: "CREATE TABLE mod_locks_lock (depot_id BIGINT, path TEXT, PRIMARY KEY (depot_id, path));",
    }],
};

#[test]
fn module_migration_set_applies_once_and_is_tracked_separately() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("meta.db");
    let s = MetaStore::open(&path, &[MOD_SET], MetaConfig::default()).unwrap();
    assert_eq!(s.with_writer(|c| schema_version(c, "locks")).unwrap(), 1);
    assert_eq!(
        s.with_writer(|c| schema_version(c, "core")).unwrap(),
        CORE_MIGRATIONS.migrations.len() as u32
    );
    drop(s);
    // Reopening runs nothing again (a second CREATE TABLE would fail).
    let s = MetaStore::open(&path, &[MOD_SET], MetaConfig::default()).unwrap();
    assert_eq!(s.with_writer(|c| schema_version(c, "locks")).unwrap(), 1);
    drop(s);
    // Opening without the module leaves its tables and version alone.
    let s = MetaStore::open(&path, &[], MetaConfig::default()).unwrap();
    assert_eq!(s.with_writer(|c| schema_version(c, "locks")).unwrap(), 1);
}

#[test]
fn failed_migration_leaves_no_trace() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("meta.db");
    const BAD: MigrationSet = MigrationSet {
        namespace: "broken",
        migrations: &[Migration {
            version: 1,
            name: "half",
            sql: "CREATE TABLE mod_broken_a (x INTEGER); THIS IS NOT SQL;",
            postgres: "CREATE TABLE mod_broken_a (x BIGINT); THIS IS NOT SQL;",
        }],
    };
    assert!(matches!(
        MetaStore::open(&path, &[BAD], MetaConfig::default()),
        Err(MetaError::Migration { .. })
    ));
    let s = open(&dir);
    let exists: i64 = s
        .with_writer(|c| {
            c.value(
                "SELECT count(*) FROM sqlite_master WHERE name = 'mod_broken_a'",
                vals![],
            )
        })
        .unwrap();
    assert_eq!(exists, 0);
    assert_eq!(s.with_writer(|c| schema_version(c, "broken")).unwrap(), 0);
}

#[test]
fn dropped_write_transaction_leaves_no_trace() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(&dir);
    let res: Result<()> = s.write_blocking(|tx| {
        tx.create_depot("ghost", NOW)?;
        assert!(tx.depot_by_name("ghost")?.is_some());
        Err(MetaError::NotFound("forced".into()))
    });
    assert!(res.is_err());
    s.write_blocking(|tx| {
        assert!(tx.depot_by_name("ghost")?.is_none());
        Ok(())
    })
    .unwrap();
}

#[tokio::test]
async fn failed_async_write_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(&dir);
    let res: Result<()> = s
        .write(|tx| {
            tx.create_depot("half", NOW)?;
            Err(MetaError::NotFound("forced".into()))
        })
        .await;
    assert!(res.is_err());
    assert!(s
        .read(|tx| tx.depot_by_name("half"))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn readers_proceed_while_a_write_transaction_is_open() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(&dir);
    s.write(|tx| tx.create_depot("main", NOW).map(|_| ()))
        .await
        .unwrap();

    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel::<()>();
    let writer = s.clone();
    let handle = std::thread::spawn(move || {
        writer
            .write_blocking(|tx| {
                tx.create_depot("pending", NOW)?;
                started_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                Ok(())
            })
            .unwrap();
    });
    started_rx.recv().unwrap();
    // The write transaction is open and holds the write lock. Reads still complete and do
    // not see its uncommitted row.
    let names = tokio::time::timeout(
        Duration::from_secs(5),
        s.read(|tx| Ok(tx.depots()?.into_iter().map(|d| d.name).collect::<Vec<_>>())),
    )
    .await
    .expect("read blocked by the writer")
    .unwrap();
    assert_eq!(names, ["main"]);
    finish_tx.send(()).unwrap();
    handle.join().unwrap();
    assert!(s
        .read(|tx| tx.depot_by_name("pending"))
        .await
        .unwrap()
        .is_some());
}

fn tree(n: u8) -> ObjectId {
    ObjectId::from_bytes([n; 32])
}

fn change(path: &str, action: char) -> ChangeRow {
    ChangeRow {
        path: path.into(),
        action,
        kind: Some(1),
        old_id: None,
        new_id: Some(tree(7)),
        size: Some(3),
        flags: 0,
        copy_from_path: None,
        copy_from_rev: None,
    }
}

/// Commits one revision touching `paths` on `branch`, the way the commit engine will.
fn commit(
    tx: &WriteTx<'_>,
    depot: DepotId,
    branch: BranchId,
    who: PrincipalId,
    paths: &[&str],
) -> u64 {
    let b = tx.branch_by_id(branch).unwrap().unwrap();
    let num = tx.allocate_rev(depot).unwrap();
    tx.insert_revision(&RevisionRow {
        depot_id: depot,
        num,
        branch_id: branch,
        parent_num: Some(b.head_rev),
        root_tree: tree(num as u8),
        author_id: who,
        submitter_id: who,
        created_at: NOW,
        message: format!("r{num}"),
        provenance: None,
        request_id: Some(format!("req-{num}")),
    })
    .unwrap();
    let changes: Vec<_> = paths.iter().map(|p| change(p, 'M')).collect();
    tx.insert_changes(depot, num, &changes).unwrap();
    tx.set_branch_head(branch, num).unwrap();
    num
}

/// Creates a branch the way §5.3 describes: a revision on the new branch whose parent is
/// the source revision.
fn branch_from(
    tx: &WriteTx<'_>,
    depot: DepotId,
    name: &str,
    from: BranchId,
    from_rev: u64,
    who: PrincipalId,
) -> BranchId {
    let num = tx.allocate_rev(depot).unwrap();
    let id = tx
        .create_branch(depot, name, num, Some((from, from_rev)))
        .unwrap();
    tx.insert_revision(&RevisionRow {
        depot_id: depot,
        num,
        branch_id: id,
        parent_num: Some(from_rev),
        root_tree: tree(0),
        author_id: who,
        submitter_id: who,
        created_at: NOW,
        message: format!("create {name}"),
        provenance: None,
        request_id: None,
    })
    .unwrap();
    id
}

#[tokio::test]
async fn path_history_follows_a_three_level_branch_chain() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(&dir);
    let (depot, main, dev, feat) = s
        .write(|tx| {
            let d = tx.create_depot("game", NOW)?.id;
            let who = tx.create_principal(
                &NewPrincipal {
                    kind: PrincipalKind::User,
                    name: "ana".into(),
                    display_name: None,
                    owner_id: None,
                    password_hash: None,
                },
                NOW,
            )?;
            // r1 creates main.
            let r1 = tx.allocate_rev(d)?;
            let main = tx.create_branch(d, "main", r1, None)?;
            tx.insert_revision(&RevisionRow {
                depot_id: d,
                num: r1,
                branch_id: main,
                parent_num: None,
                root_tree: tree(0),
                author_id: who,
                submitter_id: who,
                created_at: NOW,
                message: "init".into(),
                provenance: None,
                request_id: None,
            })?;
            let r2 = commit(tx, d, main, who, &["a.txt"]); // r2
            let _r3 = commit(tx, d, main, who, &["b.txt"]); // r3
            let dev = branch_from(tx, d, "dev", main, r2, who); // r4, branched at r2
            let r5 = commit(tx, d, dev, who, &["a.txt"]); // r5
            let _r6 = commit(tx, d, main, who, &["a.txt"]); // r6: main after the branch point
            let feat = branch_from(tx, d, "feat", dev, r5, who); // r7, branched at r5
            let _r8 = commit(tx, d, dev, who, &["a.txt"]); // r8: dev after feat's point
            let _r9 = commit(tx, d, feat, who, &["a.txt"]); // r9
            Ok((d, main, dev, feat))
        })
        .await
        .unwrap();

    let chain = s.read(move |tx| tx.branch_chain(feat)).await.unwrap();
    assert_eq!(
        chain,
        [
            HistorySegment {
                branch_id: feat,
                max_rev: None
            },
            HistorySegment {
                branch_id: dev,
                max_rev: Some(5)
            },
            HistorySegment {
                branch_id: main,
                max_rev: Some(2)
            },
        ]
    );

    let revs = |b: BranchId| {
        let s = s.clone();
        async move {
            s.read(move |tx| tx.path_history(depot, b, "a.txt", None, 100))
                .await
                .unwrap()
                .into_iter()
                .map(|e| e.rev)
                .collect::<Vec<_>>()
        }
    };
    // feat sees its own r9, dev's r5 (not r8), and main's r2 (not r6).
    assert_eq!(revs(feat).await, [9, 5, 2]);
    assert_eq!(revs(dev).await, [8, 5, 2]);
    assert_eq!(revs(main).await, [6, 2]);

    let paged = s
        .read(move |tx| tx.path_history(depot, feat, "a.txt", Some(9), 1))
        .await
        .unwrap();
    assert_eq!(paged.iter().map(|e| e.rev).collect::<Vec<_>>(), [5]);
    assert_eq!(paged[0].change.new_id, Some(tree(7)));
}

#[tokio::test]
async fn typed_rows_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(&dir);
    s.write(|tx| {
        let d = tx.create_depot("game", NOW)?;
        assert_eq!(d.next_rev, 1);
        assert!(matches!(
            tx.create_depot("game", NOW),
            Err(MetaError::AlreadyExists(_))
        ));
        assert_eq!(tx.allocate_rev(d.id)?, 1);
        assert_eq!(tx.allocate_rev(d.id)?, 2);

        let user = tx.create_principal(
            &NewPrincipal {
                kind: PrincipalKind::User,
                name: "ana".into(),
                display_name: Some("Ana".into()),
                owner_id: None,
                password_hash: Some("$argon2id$x".into()),
            },
            NOW,
        )?;
        let agent = tx.create_principal(
            &NewPrincipal {
                kind: PrincipalKind::Agent,
                name: "fixer-bot".into(),
                display_name: None,
                owner_id: Some(user),
                password_hash: None,
            },
            NOW,
        )?;
        let a = tx.principal_by_name("fixer-bot")?.unwrap();
        assert_eq!((a.kind, a.owner_id), (PrincipalKind::Agent, Some(user)));
        tx.set_principal_disabled(agent, true)?;
        assert!(tx.principal(agent)?.unwrap().disabled);

        let g = tx.create_group("artists")?;
        tx.add_group_member(g, user)?;
        tx.add_group_member(g, user)?;
        assert_eq!(tx.groups_of(user)?, [g]);
        tx.add_group_member(g, agent)?;
        let names: Vec<_> = tx.group_members(g)?.into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["ana", "fixer-bot"]);
        let b = tx.create_group("builders")?;
        let groups: Vec<_> = tx.groups()?.into_iter().map(|g| g.name).collect();
        assert_eq!(groups, ["artists", "builders"]);
        assert!(tx.group_members(b)?.is_empty());
        tx.remove_group_member(g, agent)?;
        tx.remove_group_member(g, user)?;
        assert!(tx.groups_of(user)?.is_empty());

        let scopes = vec!["read".to_string(), "shelve".to_string()];
        let t = tx.create_token(agent, "ci", &[1, 2, 3], &scopes, Some(NOW + 1), NOW)?;
        let row = tx.token_by_hash(&[1, 2, 3])?.unwrap();
        assert_eq!((row.id, row.scopes.clone()), (t, scopes));
        tx.touch_token(t, NOW + 5)?;
        assert_eq!(
            tx.token_by_hash(&[1, 2, 3])?.unwrap().last_used_at,
            Some(NOW + 5)
        );
        assert_eq!(tx.tokens_of(agent)?.len(), 1);
        tx.delete_token(t)?;
        assert!(tx.token_by_hash(&[1, 2, 3])?.is_none());

        let rule = AclRow {
            id: AclId(0),
            depot_id: Some(d.id),
            subject: AclSubject::Group(g),
            branch_glob: "*".into(),
            path_glob: "Content/**".into(),
            effect: AclEffect::Deny,
            level: AccessLevel::Write,
        };
        let id = tx.insert_acl(&rule)?;
        tx.insert_acl(&AclRow {
            depot_id: None,
            subject: AclSubject::All,
            effect: AclEffect::Allow,
            level: AccessLevel::Read,
            ..rule.clone()
        })?;
        let rules = tx.acl_for_depot(d.id)?;
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0], AclRow { id, ..rule });
        assert!(AccessLevel::Admin > AccessLevel::Write);

        tx.append_outbox(Some(d.id), "revision.created", r#"{"rev":1}"#, NOW)?;
        let second = tx.append_outbox(None, "x.y", "{}", NOW)?;
        assert_eq!(tx.outbox_after(0, 10)?.len(), 2);
        tx.set_outbox_cursor("webhooks", second)?;
        assert_eq!(tx.outbox_cursors()?, [("webhooks".to_string(), second)]);
        assert_eq!(tx.prune_outbox(second)?, 2);
        tx.delete_outbox_cursor("webhooks")?;
        assert!(tx.outbox_cursors()?.is_empty());
        assert!(matches!(
            tx.set_rev_meta(d.id, 1, "nokey", "v"),
            Err(MetaError::BadValue(_))
        ));
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn idempotency_lookup_merges_and_meta() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(&dir);
    s.write(|tx| {
        let d = tx.create_depot("game", NOW)?.id;
        let who = tx.create_principal(
            &NewPrincipal {
                kind: PrincipalKind::User,
                name: "ana".into(),
                display_name: None,
                owner_id: None,
                password_hash: None,
            },
            NOW,
        )?;
        let r1 = tx.allocate_rev(d)?;
        let main = tx.create_branch(d, "main", r1, None)?;
        tx.insert_revision(&RevisionRow {
            depot_id: d,
            num: r1,
            branch_id: main,
            parent_num: None,
            root_tree: tree(1),
            author_id: who,
            submitter_id: who,
            created_at: NOW,
            message: "init".into(),
            provenance: Some(r#"{"via":"api"}"#.into()),
            request_id: Some("abc".into()),
        })?;
        let r = tx.revision_by_request_id(d, "abc")?.unwrap();
        assert_eq!((r.num, r.root_tree), (r1, tree(1)));
        tx.insert_merge(
            d,
            r1,
            &MergeRow {
                source_branch_id: main,
                from_rev: 1,
                to_rev: 1,
            },
        )?;
        assert_eq!(tx.merges(d, r1)?.len(), 1);
        tx.set_rev_meta(d, r1, "locks.kept", "2")?;
        tx.set_rev_meta(d, r1, "locks.kept", "3")?;
        assert_eq!(
            tx.rev_meta(d, r1)?,
            [("locks.kept".to_string(), "3".to_string())]
        );
        tx.insert_changes(d, r1, &[change("x", 'A')])?;
        assert_eq!(tx.changes(d, r1)?, [change("x", 'A')]);
        assert_eq!(tx.revisions_after(d, 0, 10)?.len(), 1);
        tx.set_branch_state(main, BranchState::Archived)?;
        assert_eq!(tx.branch(d, "main")?.unwrap().state, BranchState::Archived);
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn online_backup_copies_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(&dir);
    s.write(|tx| tx.create_depot("game", NOW).map(|_| ()))
        .await
        .unwrap();
    let dest = dir.path().join("backup.db");
    s.backup_to(&dest).await.unwrap();
    let b = MetaStore::open(&dest, &[], MetaConfig::default()).unwrap();
    assert!(b
        .read(|tx| tx.depot_by_name("game"))
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn access_epoch_rises_with_rules_and_membership() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(&dir);
    let epochs = s
        .write(|tx| {
            let mut seen = vec![tx.access_epoch()?];
            let user = tx.create_principal(
                &NewPrincipal {
                    kind: PrincipalKind::User,
                    name: "ana".into(),
                    display_name: None,
                    owner_id: None,
                    password_hash: None,
                },
                NOW,
            )?;
            let g = tx.create_group("artists")?;
            seen.push(tx.access_epoch()?);
            tx.add_group_member(g, user)?;
            seen.push(tx.access_epoch()?);
            tx.remove_group_member(g, user)?;
            seen.push(tx.access_epoch()?);
            let id = tx.insert_acl(&AclRow {
                id: AclId(0),
                depot_id: None,
                subject: AclSubject::All,
                branch_glob: "*".into(),
                path_glob: "**".into(),
                effect: AclEffect::Allow,
                level: AccessLevel::Read,
            })?;
            seen.push(tx.access_epoch()?);
            tx.delete_acl(id)?;
            seen.push(tx.access_epoch()?);
            Ok(seen)
        })
        .await
        .unwrap();
    assert_eq!(epochs, [1, 1, 2, 3, 4, 5]);
    // A failed transaction leaves the epoch where it was.
    let r: Result<(), _> = s
        .write(|tx| {
            tx.bump_access_epoch()?;
            Err(MetaError::BadValue("abort".into()))
        })
        .await;
    assert!(r.is_err());
    assert_eq!(s.read(|tx| tx.access_epoch()).await.unwrap(), 5);
}
