//! What only the S3 store does (T123), on the in-process fake S3: key layout, retries,
//! listing pages, signatures, stale temp files, uploading stored files, settings.

// Helpers outside #[test] functions unwrap too; a failure is a test failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::time::{Duration, SystemTime};

use futures::StreamExt;
use lode_core::{blob_id, DepotId, ObjectKind};
use lode_store::s3::fake::FakeS3;
use lode_store::s3::sigv4::Credentials;
use lode_store::{
    BlobStore, FsBlobStore, FsBlobStoreConfig, PutOptions, PutOutcome, S3BlobStore, S3Config,
};
use tokio::io::AsyncReadExt;

const BUCKET: &str = "bucket";
const D: DepotId = DepotId(7);

fn creds(secret: &str) -> Credentials {
    Credentials {
        access_key_id: "AKIDLODE".into(),
        secret_access_key: secret.into(),
        session_token: None,
    }
}

fn config(fake: &FakeS3, tmp: &Path, secret: &str) -> S3Config {
    S3Config {
        bucket: BUCKET.into(),
        prefix: "lode/main".into(),
        region: "eu-central-1".into(),
        endpoint: Some(fake.url().into()),
        path_style: true,
        credentials: creds(secret),
        max_object_size: u64::MAX,
        tmp_dir: tmp.join("s3-tmp"),
    }
}

async fn setup() -> (FakeS3, tempfile::TempDir, S3BlobStore) {
    let fake = FakeS3::start(creds("s3cret/KEY"), "eu-central-1", BUCKET).await;
    let tmp = tempfile::tempdir().unwrap();
    let store = S3BlobStore::open(config(&fake, tmp.path(), "s3cret/KEY"))
        .await
        .unwrap();
    (fake, tmp, store)
}

async fn put(s: &S3BlobStore, depot: DepotId, data: &[u8]) -> PutOutcome {
    let id = blob_id(data).unwrap();
    s.put(
        depot,
        ObjectKind::Blob,
        &id,
        &mut &data[..],
        PutOptions::default(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn objects_live_under_prefix_depot_and_fan_out() {
    let (fake, _tmp, s) = setup().await;
    s.check().await.unwrap();
    put(&s, D, b"hello").await;
    let id = blob_id(&b"hello"[..]).unwrap();
    let h = id.to_hex();
    let key = format!("lode/main/7/{}/{}/{h}", &h[..2], &h[2..4]);
    assert_eq!(s.key(D, &id), key);
    assert_eq!(fake.keys(BUCKET), vec![key.clone()]);
    // The stored format of ARCHITECTURE §3.3: header, then payload.
    let bytes = fake.get_raw(BUCKET, &key).unwrap();
    assert_eq!(&bytes[lode_core::ObjectHeader::LEN..], b"hello");
}

#[tokio::test]
async fn server_errors_are_tried_again() {
    let (fake, _tmp, s) = setup().await;
    fake.fail_next(2);
    assert_eq!(put(&s, D, b"retry me").await, PutOutcome::Created);
    let id = blob_id(&b"retry me"[..]).unwrap();
    fake.fail_next(2);
    let mut back = Vec::new();
    s.get(D, &id)
        .await
        .unwrap()
        .reader
        .read_to_end(&mut back)
        .await
        .unwrap();
    assert_eq!(back, b"retry me");

    fake.fail_next(3);
    let err = s.contains(D, &id).await.unwrap_err().to_string();
    assert!(err.contains("503"), "{err}");
    fake.fail_next(3);
    let err = s.get(D, &id).await.err().unwrap().to_string();
    assert!(err.contains("503") && err.contains("SlowDown"), "{err}");
    assert!(s.contains(D, &id).await.unwrap());
}

#[tokio::test]
async fn listing_follows_continuation_tokens_and_keeps_depots_apart() {
    let (fake, _tmp, s) = setup().await;
    fake.set_page_size(2);
    let mut want = Vec::new();
    for i in 0..7u32 {
        let data = format!("object {i}");
        put(&s, D, data.as_bytes()).await;
        want.push(blob_id(data.as_bytes()).unwrap());
    }
    put(&s, DepotId(70), b"other depot").await;
    let before = fake.requests();
    let mut got: Vec<_> = s.list(D).map(|r| r.unwrap().0).collect().await;
    got.sort();
    want.sort();
    assert_eq!(got, want);
    assert_eq!(fake.requests() - before, 4, "7 keys in pages of 2");
    assert_eq!(s.list(DepotId(70)).count().await, 1);
    assert_eq!(s.list(DepotId(8)).count().await, 0);
}

#[tokio::test]
async fn a_wrong_secret_is_refused_and_never_shown() {
    let (fake, tmp, _s) = setup().await;
    let bad = S3BlobStore::open(config(&fake, tmp.path(), "not-the-secret"))
        .await
        .unwrap();
    let err = bad.check().await.unwrap_err().to_string();
    assert!(
        err.contains("403") && err.contains("SignatureDoesNotMatch"),
        "{err}"
    );
    assert!(!err.contains("not-the-secret"));
    assert!(
        !format!("{:?}", config(&fake, tmp.path(), "not-the-secret")).contains("not-the-secret")
    );
}

#[tokio::test]
async fn open_removes_stale_temp_files_only() {
    let fake = FakeS3::start(creds("k"), "eu-central-1", BUCKET).await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s3-tmp");
    std::fs::create_dir_all(&dir).unwrap();
    let (old, fresh) = (dir.join("old"), dir.join("fresh"));
    std::fs::write(&old, b"x").unwrap();
    std::fs::write(&fresh, b"x").unwrap();
    let f = std::fs::File::options().write(true).open(&old).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(25 * 3600))
        .unwrap();
    drop(f);
    let _s = S3BlobStore::open(config(&fake, tmp.path(), "k"))
        .await
        .unwrap();
    assert!(!old.exists());
    assert!(fresh.exists());
}

#[tokio::test]
async fn stored_files_upload_once() {
    let (fake, tmp, s) = setup().await;
    let fs_dir = tmp.path().join("fs");
    let fs = FsBlobStore::open(&fs_dir, FsBlobStoreConfig::default())
        .await
        .unwrap();
    let data = vec![b'z'; 50_000];
    let id = blob_id(&data[..]).unwrap();
    fs.put(
        D,
        ObjectKind::Blob,
        &id,
        &mut &data[..],
        PutOptions::default(),
    )
    .await
    .unwrap();
    let h = id.to_hex();
    let file = fs_dir.join(format!("depots/7/objects/{}/{}/{h}", &h[..2], &h[2..4]));
    assert_eq!(
        s.put_stored_file(D, &id, &file).await.unwrap(),
        PutOutcome::Created
    );
    assert_eq!(
        s.put_stored_file(D, &id, &file).await.unwrap(),
        PutOutcome::AlreadyPresent
    );
    assert_eq!(
        fake.get_raw(BUCKET, &s.key(D, &id)).unwrap(),
        std::fs::read(&file).unwrap()
    );
    let mut back = Vec::new();
    s.get(D, &id)
        .await
        .unwrap()
        .reader
        .read_to_end(&mut back)
        .await
        .unwrap();
    assert_eq!(back, data);

    let junk = tmp.path().join("junk");
    std::fs::write(&junk, b"not an object").unwrap();
    assert!(s.put_stored_file(D, &id, &junk).await.is_err());
}

#[tokio::test]
async fn settings_are_checked() {
    let fake = FakeS3::start(creds("k"), "eu-central-1", BUCKET).await;
    let tmp = tempfile::tempdir().unwrap();
    for endpoint in ["ftp://example.com", "http://example.com/path", "not a url"] {
        let mut cfg = config(&fake, tmp.path(), "k");
        cfg.endpoint = Some(endpoint.into());
        assert!(S3BlobStore::open(cfg).await.is_err(), "{endpoint}");
    }
    let mut cfg = config(&fake, tmp.path(), "k");
    cfg.bucket = String::new();
    assert!(S3BlobStore::open(cfg).await.is_err());
    // A bucket that does not exist yet is created.
    let mut cfg = config(&fake, tmp.path(), "k");
    cfg.bucket = "fresh".into();
    let s = S3BlobStore::open(cfg).await.unwrap();
    assert!(s.check().await.is_err());
    s.ensure_bucket().await.unwrap();
    s.ensure_bucket().await.unwrap();
    s.check().await.unwrap();
}
