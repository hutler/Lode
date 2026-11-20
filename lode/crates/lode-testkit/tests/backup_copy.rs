//! Copies of backups in an S3-compatible bucket: only new content is sent, old copies and the
//! content only they needed are removed, and a server comes back from a copy.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use lode_meta::Reads;
use lode_server::config::{BackupCopy, S3Settings};
use lode_server::maintenance::copy::{copy_backup, list_remote, open_bucket, restore};
use lode_server::maintenance::{backup_tick, copy_newest, BackupSchedule, BackupTick, FsckOptions};
use lode_server::{open_state, ServerConfig, SystemClock};
use lode_store::s3::fake::FakeS3;
use lode_store::s3::sigv4::Credentials;
use lode_testkit::TestServer;

const BUCKET: &str = "lode-backups";
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

fn copy_settings(fake: &FakeS3, keep: usize) -> BackupCopy {
    let mut s3 = S3Settings::new(BUCKET).with_credentials("AKIDBACKUP", "backup/secret");
    s3.prefix = "office".into();
    s3.region = Some("us-east-1".into());
    s3.endpoint = Some(fake.url().to_owned());
    s3.path_style = true;
    BackupCopy { s3, keep }
}

async fn fake() -> FakeS3 {
    FakeS3::start(
        Credentials {
            access_key_id: "AKIDBACKUP".into(),
            secret_access_key: "backup/secret".into(),
            session_token: None,
        },
        "us-east-1",
        BUCKET,
    )
    .await
}

fn object_keys(fake: &FakeS3) -> Vec<String> {
    fake.keys(BUCKET)
        .into_iter()
        .filter(|k| k.starts_with("office/objects/"))
        .collect()
}

async fn tick(s: &TestServer, schedule: &BackupSchedule) -> std::path::PathBuf {
    match backup_tick(s.state(), schedule).await.unwrap() {
        BackupTick::Done { path, .. } => path,
        other => panic!("a backup was due: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backups_are_copied_pruned_and_restored() {
    let fake = fake().await;
    let s = TestServer::start().await;
    let copy = copy_settings(&fake, 2);
    let dir = tempfile::tempdir().unwrap();
    let schedule = BackupSchedule {
        every: Duration::from_secs(24 * 60 * 60),
        keep: 7,
        dir: dir.path().join("backups"),
        copy_to: Some(copy.clone()),
    };
    let bucket = open_bucket(&copy, &s.data_dir()).await.unwrap();
    let tmp = dir.path().join("tmp");

    s.create_depot("game").await;
    s.commit_files("game", "main", &[("a.txt", b"first")]).await;
    s.create_depot("scratch").await;
    s.commit_files("scratch", "main", &[("junk.bin", b"only in scratch")])
        .await;

    // The first copy sends everything; the server records it for the overview.
    tick(&s, &schedule).await;
    let mut copied = None;
    copy_newest(s.state(), &schedule, &copy, &mut copied).await;
    let run = s.state().last_backup_copy().unwrap();
    assert!(run.outcome.is_ok(), "{:?}", run.outcome);
    let first = copied.clone().unwrap();
    let objects_after_first = object_keys(&fake).len();
    assert!(objects_after_first >= 4, "{:?}", fake.keys(BUCKET));
    for f in ["meta.db", "objects.zst", "done.json"] {
        assert!(
            fake.get_raw(BUCKET, &format!("office/{first}/{f}"))
                .is_some(),
            "{f}"
        );
    }
    // Asking again does nothing: the copy is there.
    let requests = fake.requests();
    copy_newest(s.state(), &schedule, &copy, &mut copied).await;
    assert_eq!(fake.requests(), requests);

    // The next day: one new file, so only its content (and the new trees) go up.
    s.clock().advance_ms(DAY_MS);
    s.commit_files("game", "main", &[("b.txt", b"second")])
        .await;
    let path = tick(&s, &schedule).await;
    let r = copy_backup(&copy, &bucket, &path, &tmp).await.unwrap();
    assert!(r.uploaded >= 1 && r.uploaded < r.objects, "{r:?}");
    assert!(r.removed.is_empty());

    // Erase the scratch depot; two backups later its content has left the bucket.
    s.admin()
        .delete_depot("scratch", "scratch", true)
        .await
        .unwrap();
    let junk = lode_core::blob_id(&b"only in scratch"[..])
        .unwrap()
        .to_hex();
    assert!(object_keys(&fake).iter().any(|k| k.ends_with(&junk)));
    s.clock().advance_ms(DAY_MS);
    let path = tick(&s, &schedule).await;
    let r = copy_backup(&copy, &bucket, &path, &tmp).await.unwrap();
    assert_eq!(r.uploaded, 0, "nothing new: {r:?}");
    assert_eq!(r.removed, std::slice::from_ref(&first));
    assert!(
        object_keys(&fake).iter().any(|k| k.ends_with(&junk)),
        "still kept by a copy"
    );
    s.clock().advance_ms(DAY_MS);
    let path = tick(&s, &schedule).await;
    let r = copy_backup(&copy, &bucket, &path, &tmp).await.unwrap();
    assert_eq!(r.removed.len(), 1);
    assert!(r.objects_removed >= 1, "{r:?}");
    assert!(!object_keys(&fake).iter().any(|k| k.ends_with(&junk)));
    let remote = list_remote(&bucket).await.unwrap();
    assert_eq!(remote.len(), 2);
    assert!(fake
        .keys(BUCKET)
        .iter()
        .all(|k| !k.starts_with(&format!("office/{first}/"))));

    // Restore the newest copy into a new data directory: it opens and checks clean.
    let restored = dir.path().join("restored");
    let rr = restore(&bucket, None, &restored).await.unwrap();
    assert_eq!(rr.name, remote[1].name);
    assert!(rr.objects >= 4);
    let state = open_state(
        ServerConfig::with_data_dir(&restored),
        Arc::new(SystemClock),
        Vec::new(),
    )
    .await
    .unwrap();
    let depots = state.meta().read(|tx| tx.depots()).await.unwrap();
    assert_eq!(
        depots.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        ["game"]
    );
    let report = lode_server::maintenance::fsck(&state, depots[0].id, FsckOptions { full: true })
        .await
        .unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);

    // An older copy by name; a destination that is not empty is refused.
    let older = dir.path().join("older");
    assert_eq!(
        restore(&bucket, Some(&remote[0].name), &older)
            .await
            .unwrap()
            .name,
        remote[0].name
    );
    assert!(restore(&bucket, None, &older).await.is_err());
    assert!(restore(
        &bucket,
        Some("lode-backup-20200101T000000Z"),
        &dir.path().join("x")
    )
    .await
    .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_copy_is_recorded_and_tried_again() {
    let fake = fake().await;
    let s = TestServer::start().await;
    let mut copy = copy_settings(&fake, 3);
    let good = copy.clone();
    copy.s3 = S3Settings::new(BUCKET).with_credentials("AKIDBACKUP", "wrong");
    copy.s3.endpoint = Some(fake.url().to_owned());
    copy.s3.path_style = true;
    let dir = tempfile::tempdir().unwrap();
    let mut schedule = BackupSchedule {
        every: Duration::from_secs(24 * 60 * 60),
        keep: 7,
        dir: dir.path().join("backups"),
        copy_to: Some(copy.clone()),
    };
    s.create_depot("game").await;
    tick(&s, &schedule).await;
    let mut copied = None;
    copy_newest(s.state(), &schedule, &copy, &mut copied).await;
    assert!(copied.is_none());
    let err = s.state().last_backup_copy().unwrap().outcome.unwrap_err();
    assert!(err.contains("403") || err.contains("Signature"), "{err}");
    // With the right key the next check sends it.
    schedule.copy_to = Some(good.clone());
    copy_newest(s.state(), &schedule, &good, &mut copied).await;
    assert!(copied.is_some());
    assert!(s.state().last_backup_copy().unwrap().outcome.is_ok());
}

#[test]
fn copy_to_is_read_from_loded_toml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("loded.toml");
    std::fs::write(
        &path,
        "data_dir = 'data'\n[backup]\nevery = '24h'\nkeep = 5\n[backup.copy_to]\n\
         bucket = 'lode-backups'\nendpoint = 'https://s3.example.com'\n\
         access_key_id = 'AK'\nsecret_access_key_file = 's.txt'\n",
    )
    .unwrap();
    let cfg = ServerConfig::load(&path).unwrap();
    let c = cfg.backup.copy_to.unwrap();
    assert_eq!(c.keep, 5, "defaults to [backup] keep");
    assert_eq!(c.s3.bucket, "lode-backups");
    assert_eq!(
        c.s3.secret_access_key_file.unwrap(),
        dir.path().join("s.txt")
    );
    std::fs::write(&path, "[backup.copy_to]\nbucket = 'Bad_Name'\n").unwrap();
    assert!(ServerConfig::load(&path).is_err());
    std::fs::write(&path, "[backup.copy_to]\nprefix = 'x'\n").unwrap();
    assert!(ServerConfig::load(&path).is_err());
}
