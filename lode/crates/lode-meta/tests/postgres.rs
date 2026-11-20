//! The metadata store on Postgres (T153). Runs only when `LODE_TEST_POSTGRES` names a server
//! where the tests may create schemas, such as `postgres://lode:lode@localhost/lode_test`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use lode_core::{DepotId, ObjectId};
use lode_meta::*;

const NOW: Millis = 1_700_000_000_000;

/// Opens on a blocking thread: the Postgres client must not block inside the runtime.
async fn open(url: &str) -> MetaStore {
    let url = url.to_owned();
    tokio::task::spawn_blocking(move || {
        MetaStore::open_postgres(&url, &[], MetaConfig::default()).unwrap()
    })
    .await
    .unwrap()
}

async fn pg() -> Option<(String, MetaStore)> {
    let url = test_postgres_url()?.unwrap();
    let store = open(&url).await;
    Some((url, store))
}

#[tokio::test]
async fn core_queries_work_on_postgres() {
    let Some((url, s)) = pg().await else { return };
    assert_eq!(s.dialect(), Dialect::Postgres);
    let (depot, admin) = s
        .write(|tx| {
            let d = tx.create_depot("main", NOW)?;
            let p = tx.create_principal(
                &NewPrincipal {
                    kind: PrincipalKind::User,
                    name: "admin".into(),
                    display_name: Some("Admin".into()),
                    owner_id: None,
                    password_hash: None,
                },
                NOW,
            )?;
            Ok((d.id, p))
        })
        .await
        .unwrap();
    // A duplicate name is AlreadyExists, as on SQLite.
    let dup = s.write(|tx| tx.create_depot("main", NOW).map(|_| ())).await;
    assert!(matches!(dup, Err(MetaError::AlreadyExists(_))), "{dup:?}");
    let rev = s.write(move |tx| tx.allocate_rev(depot)).await.unwrap();
    assert_eq!(rev, 1);
    let epoch = s.read(|tx| tx.access_epoch()).await.unwrap();
    s.write(move |tx| {
        let g = tx.create_group("devs")?;
        tx.add_group_member(g, admin)?;
        tx.add_group_member(g, admin)
    })
    .await
    .unwrap();
    assert!(s.read(|tx| tx.access_epoch()).await.unwrap() > epoch);
    let info = s.info().await.unwrap();
    assert_eq!(info.kind, "Postgres");
    assert!(!info.location.contains("lode:lode@"), "{}", info.location);
    // Reopening runs no migration twice.
    drop(s);
    let s = open(&url).await;
    let names = s
        .read(|tx| Ok(tx.depots()?.into_iter().map(|d| d.name).collect::<Vec<_>>()))
        .await
        .unwrap();
    assert_eq!(names, ["main"]);
    let _ = DepotId(0);
    let _ = ObjectId::from_bytes([0; 32]);
}

#[tokio::test]
async fn a_second_server_is_refused_until_the_first_lets_go() {
    let Some((_, s)) = pg().await else { return };
    let first = match s.try_server_lock("loded test-a").await.unwrap() {
        LockAttempt::Held(l) => l,
        LockAttempt::InUse(h) => panic!("in use by {h}"),
    };
    match s.try_server_lock("loded test-b").await.unwrap() {
        LockAttempt::InUse(h) => assert!(h.contains("loded test-a"), "{h}"),
        LockAttempt::Held(_) => panic!("two servers hold the lock"),
    }
    drop(first);
    let mut held = None;
    for _ in 0..50 {
        if let LockAttempt::Held(l) = s.try_server_lock("loded test-b").await.unwrap() {
            held = Some(l);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(held.is_some(), "the lock was not released");
}

#[test]
fn sqlite_copies_into_postgres() {
    let Some(url) = test_postgres_url() else {
        return;
    };
    let url = url.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let src = MetaStore::open(dir.path().join("meta.db"), &[], MetaConfig::default()).unwrap();
    src.write_blocking(|tx| {
        let d = tx.create_depot("main", NOW)?;
        let p = tx.create_principal(
            &NewPrincipal {
                kind: PrincipalKind::User,
                name: "ann".into(),
                display_name: None,
                owner_id: None,
                password_hash: Some("x".into()),
            },
            NOW,
        )?;
        tx.create_token(p, "t", &[1, 2, 3], &["read".into()], None, NOW)?;
        tx.append_outbox(Some(d.id), "test", "{}", NOW)?;
        Ok(())
    })
    .unwrap();
    let dst = MetaStore::open_postgres(&url, &[], MetaConfig::default()).unwrap();
    let report = src.copy_into(&dst).unwrap();
    assert!(report.rows() >= 4, "{report:?}");
    // New rows continue after the copied ids.
    dst.write_blocking(|tx| {
        let d = tx.create_depot("second", NOW)?;
        assert_eq!(d.id.0, 2);
        Ok(())
    })
    .unwrap();
    // A second copy into the same database is refused.
    assert!(src.copy_into(&dst).is_err());
}
