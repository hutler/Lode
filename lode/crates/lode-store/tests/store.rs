//! The blob store test suite (T003), run against every store (T123): the filesystem store,
//! the S3 store on the in-process fake S3, and the S3 store on a real S3-compatible service
//! when `LODE_TEST_S3` names one, as `http://KEY:SECRET@host:port/bucket` (CI starts MinIO).

// Helpers outside #[test] functions unwrap too; a failure is a test failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use futures::StreamExt;
use lode_core::{blob_id, tree_id, Codec, DepotId, ObjectHeader, ObjectId, ObjectKind};
use lode_store::s3::fake::FakeS3;
use lode_store::s3::sigv4::Credentials;
use lode_store::{
    BlobStore, FsBlobStore, FsBlobStoreConfig, PutOptions, PutOutcome, S3BlobStore, S3Config,
    StoreError,
};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

const D: DepotId = DepotId(1);
const BUCKET: &str = "lode-test";
const REGION: &str = "us-east-1";

/// A store under test with raw access to what it stored.
trait Fixture: Sized {
    type S: BlobStore + Clone + 'static;
    /// A fresh, empty store; `None` when this backend is not available here.
    async fn new(cap: Option<u64>) -> Option<Self>;
    fn store(&self) -> &Self::S;
    /// Writes bytes at a path relative to the depot's object root, replacing anything.
    async fn raw_put(&self, rel: &str, bytes: &[u8]);
    /// Reads the bytes at a path relative to the depot's object root.
    async fn raw_get(&self, rel: &str) -> Vec<u8>;
    /// Files left in the store's temporary directory.
    fn tmp_entries(&self) -> usize;
}

struct Fs {
    dir: tempfile::TempDir,
    store: FsBlobStore,
}

impl Fixture for Fs {
    type S = FsBlobStore;
    async fn new(cap: Option<u64>) -> Option<Self> {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = FsBlobStoreConfig::default();
        if let Some(c) = cap {
            cfg.max_object_size = c;
        }
        let store = FsBlobStore::open(dir.path(), cfg).await.unwrap();
        Some(Self { dir, store })
    }
    fn store(&self) -> &FsBlobStore {
        &self.store
    }
    async fn raw_put(&self, rel: &str, bytes: &[u8]) {
        let path = self.dir.path().join("depots/1/objects").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    async fn raw_get(&self, rel: &str) -> Vec<u8> {
        std::fs::read(self.dir.path().join("depots/1/objects").join(rel)).unwrap()
    }
    fn tmp_entries(&self) -> usize {
        count_dir(self.dir.path().join("depots/1/tmp"))
    }
}

fn count_dir(p: PathBuf) -> usize {
    std::fs::read_dir(p).map(|r| r.count()).unwrap_or(0)
}

fn test_creds() -> Credentials {
    Credentials {
        access_key_id: "AKIDLODETEST".into(),
        secret_access_key: "lode/test/secret+KEY".into(),
        session_token: None,
    }
}

fn s3_config(
    endpoint: &str,
    bucket: &str,
    prefix: String,
    creds: Credentials,
    tmp: PathBuf,
) -> S3Config {
    S3Config {
        bucket: bucket.into(),
        prefix,
        region: REGION.into(),
        endpoint: Some(endpoint.into()),
        path_style: true,
        credentials: creds,
        max_object_size: u64::MAX,
        tmp_dir: tmp,
    }
}

struct FakeS3Fixture {
    tmp: tempfile::TempDir,
    _fake: FakeS3,
    store: S3BlobStore,
}

impl Fixture for FakeS3Fixture {
    type S = S3BlobStore;
    async fn new(cap: Option<u64>) -> Option<Self> {
        let fake = FakeS3::start(test_creds(), REGION, BUCKET).await;
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = s3_config(
            fake.url(),
            BUCKET,
            "depots".into(),
            test_creds(),
            tmp.path().join("s3-tmp"),
        );
        if let Some(c) = cap {
            cfg.max_object_size = c;
        }
        let store = S3BlobStore::open(cfg).await.unwrap();
        Some(Self {
            tmp,
            _fake: fake,
            store,
        })
    }
    fn store(&self) -> &S3BlobStore {
        &self.store
    }
    async fn raw_put(&self, rel: &str, bytes: &[u8]) {
        self.store.raw_put(D, rel, bytes).await.unwrap();
    }
    async fn raw_get(&self, rel: &str) -> Vec<u8> {
        self.store.raw_get(D, rel).await.unwrap()
    }
    fn tmp_entries(&self) -> usize {
        count_dir(self.tmp.path().join("s3-tmp"))
    }
}

/// A real S3-compatible service from `LODE_TEST_S3`; each fixture gets its own prefix.
struct RealS3 {
    tmp: tempfile::TempDir,
    store: S3BlobStore,
}

impl Fixture for RealS3 {
    type S = S3BlobStore;
    async fn new(cap: Option<u64>) -> Option<Self> {
        let Ok(spec) = std::env::var("LODE_TEST_S3") else {
            eprintln!("LODE_TEST_S3 is not set; skipping the real S3 run");
            return None;
        };
        let url =
            reqwest::Url::parse(&spec).expect("LODE_TEST_S3 is http://KEY:SECRET@host:port/bucket");
        let creds = Credentials {
            access_key_id: url.username().to_owned(),
            secret_access_key: url.password().unwrap_or_default().to_owned(),
            session_token: None,
        };
        let bucket = url.path().trim_matches('/').to_owned();
        let endpoint = format!(
            "{}://{}{}",
            url.scheme(),
            url.host_str().unwrap(),
            url.port().map(|p| format!(":{p}")).unwrap_or_default()
        );
        let tmp = tempfile::tempdir().unwrap();
        let prefix = format!("lode-test/{}", uuid::Uuid::new_v4().simple());
        let mut cfg = s3_config(&endpoint, &bucket, prefix, creds, tmp.path().join("s3-tmp"));
        if let Some(c) = cap {
            cfg.max_object_size = c;
        }
        let store = S3BlobStore::open(cfg).await.unwrap();
        store.ensure_bucket().await.unwrap();
        Some(Self { tmp, store })
    }
    fn store(&self) -> &S3BlobStore {
        &self.store
    }
    async fn raw_put(&self, rel: &str, bytes: &[u8]) {
        self.store.raw_put(D, rel, bytes).await.unwrap();
    }
    async fn raw_get(&self, rel: &str) -> Vec<u8> {
        self.store.raw_get(D, rel).await.unwrap()
    }
    fn tmp_entries(&self) -> usize {
        count_dir(self.tmp.path().join("s3-tmp"))
    }
}

macro_rules! suite {
    ($($name:ident),* $(,)?) => {
        mod fs {
            $(#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
            async fn $name() { super::$name::<super::Fs>().await })*
        }
        mod fake_s3 {
            $(#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
            async fn $name() { super::$name::<super::FakeS3Fixture>().await })*
        }
        mod real_s3 {
            $(#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
            async fn $name() { super::$name::<super::RealS3>().await })*
        }
    };
}

suite!(
    round_trips_all_sizes,
    trees_are_hashed_in_their_own_domain,
    wrong_expected_id_leaves_nothing_behind,
    size_cap_and_announced_length,
    concurrent_puts_of_same_content,
    existing_object_is_never_modified,
    rejects_payload_longer_than_header_claims,
    rejects_truncated_and_damaged_objects,
    list_skips_stray_files_and_delete_works,
    one_gib_stream,
    chunked_files,
);

async fn put<S: BlobStore>(s: &S, data: &[u8]) -> Result<PutOutcome, StoreError> {
    let id = blob_id(data).unwrap();
    s.put(
        D,
        ObjectKind::Blob,
        &id,
        &mut &data[..],
        PutOptions::default(),
    )
    .await
}

async fn read_all<S: BlobStore>(s: &S, id: &ObjectId) -> Result<Vec<u8>, io::Error> {
    let mut r = s.get(D, id).await.unwrap();
    let mut out = Vec::new();
    r.reader.read_to_end(&mut out).await?;
    assert_eq!(out.len() as u64, r.raw_len);
    Ok(out)
}

fn rel(id: &ObjectId) -> String {
    let h = id.to_hex();
    format!("{}/{}/{h}", &h[..2], &h[2..4])
}

/// Deterministic pseudo-random bytes (xorshift), which zstd cannot compress.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut s = seed | 1;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s as u8
        })
        .collect()
}

async fn stored_codec<F: Fixture>(f: &F, id: &ObjectId) -> Codec {
    let bytes = f.raw_get(&rel(id)).await;
    ObjectHeader::decode(&bytes).unwrap().codec
}

async fn round_trips_all_sizes<F: Fixture>() {
    let Some(f) = F::new(None).await else { return };
    let s = f.store();
    let compressible: Vec<u8> = b"lode stores binaries well. "
        .iter()
        .cycle()
        .take(5 * 1024 * 1024)
        .copied()
        .collect();
    let cases: Vec<(&str, Vec<u8>, Codec)> = vec![
        ("empty", vec![], Codec::None),
        ("small", b"hello".to_vec(), Codec::None),
        ("small compressible", vec![b'a'; 10_000], Codec::Zstd),
        ("multi-MiB compressible", compressible, Codec::Zstd),
        (
            "multi-MiB incompressible",
            noise(3 * 1024 * 1024 + 17, 7),
            Codec::None,
        ),
    ];
    for (name, data, codec) in cases {
        let id = blob_id(&data[..]).unwrap();
        assert_eq!(put(s, &data).await.unwrap(), PutOutcome::Created, "{name}");
        assert!(s.contains(D, &id).await.unwrap(), "{name}");
        assert_eq!(stored_codec(&f, &id).await, codec, "{name}");
        assert_eq!(read_all(s, &id).await.unwrap(), data, "{name}");
        let stored = s.get_stored(D, &id).await.unwrap();
        assert_eq!(stored.codec, codec, "{name}");
        assert_eq!(stored.raw_len, data.len() as u64, "{name}");
    }
    assert_eq!(f.tmp_entries(), 0);
}

async fn trees_are_hashed_in_their_own_domain<F: Fixture>() {
    let Some(f) = F::new(None).await else { return };
    let s = f.store();
    let bytes = lode_core::Tree::default().encode();
    let tid = tree_id(&bytes);
    let wrong = blob_id(&bytes[..]).unwrap();
    let err = s
        .put(
            D,
            ObjectKind::Tree,
            &wrong,
            &mut &bytes[..],
            PutOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::HashMismatch { .. }));
    s.put(
        D,
        ObjectKind::Tree,
        &tid,
        &mut &bytes[..],
        PutOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(s.get(D, &tid).await.unwrap().kind, ObjectKind::Tree);
}

async fn wrong_expected_id_leaves_nothing_behind<F: Fixture>() {
    let Some(f) = F::new(None).await else { return };
    let s = f.store();
    let wrong = blob_id(&b"other"[..]).unwrap();
    let err = s
        .put(
            D,
            ObjectKind::Blob,
            &wrong,
            &mut &b"content"[..],
            PutOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), lode_core::ErrorCode::HashMismatch);
    assert!(!s.contains(D, &wrong).await.unwrap());
    assert_eq!(f.tmp_entries(), 0);
    assert_eq!(s.list(D).count().await, 0);
}

async fn size_cap_and_announced_length<F: Fixture>() {
    let Some(f) = F::new(Some(100)).await else {
        return;
    };
    let s = f.store();
    let data = [0u8; 101];
    let id = blob_id(&data[..]).unwrap();
    let err = s
        .put(
            D,
            ObjectKind::Blob,
            &id,
            &mut &data[..],
            PutOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), lode_core::ErrorCode::TooLarge);
    let err = s
        .put(
            D,
            ObjectKind::Blob,
            &id,
            &mut &data[..10],
            PutOptions {
                expected_len: Some(1000),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), lode_core::ErrorCode::TooLarge);
    let small = b"12345";
    let sid = blob_id(&small[..]).unwrap();
    let err = s
        .put(
            D,
            ObjectKind::Blob,
            &sid,
            &mut &small[..],
            PutOptions {
                expected_len: Some(4),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::LengthMismatch { .. }));
    assert_eq!(f.tmp_entries(), 0);
    assert_eq!(s.list(D).count().await, 0);
}

async fn concurrent_puts_of_same_content<F: Fixture>() {
    let Some(f) = F::new(None).await else { return };
    let s = f.store();
    let data = noise(2 * 1024 * 1024, 3);
    let (a, b) = futures::join!(put(s, &data), put(s, &data));
    let mut outcomes = vec![a.unwrap(), b.unwrap()];
    outcomes.sort_by_key(|o| *o == PutOutcome::Created);
    assert_eq!(outcomes, [PutOutcome::AlreadyPresent, PutOutcome::Created]);
    assert_eq!(s.list(D).count().await, 1);
    assert_eq!(f.tmp_entries(), 0);
}

async fn listed<S: BlobStore>(s: &S) -> Vec<(ObjectId, SystemTime)> {
    s.list(D).map(|r| r.unwrap()).collect().await
}

async fn existing_object_is_never_modified<F: Fixture>() {
    let Some(f) = F::new(None).await else { return };
    let s = f.store();
    let data = vec![b'x'; 5000];
    let id = blob_id(&data[..]).unwrap();
    put(s, &data).await.unwrap();
    let before = f.raw_get(&rel(&id)).await;
    let mtime = listed(s).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(put(s, &data).await.unwrap(), PutOutcome::AlreadyPresent);
    assert_eq!(f.raw_get(&rel(&id)).await, before);
    assert_eq!(listed(s).await, mtime);
}

async fn write_raw_object<F: Fixture>(f: &F, id: &ObjectId, header: ObjectHeader, payload: &[u8]) {
    let mut bytes = header.encode().to_vec();
    bytes.extend_from_slice(payload);
    f.raw_put(&rel(id), &bytes).await;
}

async fn rejects_payload_longer_than_header_claims<F: Fixture>() {
    let Some(f) = F::new(None).await else { return };
    let s = f.store();
    let raw = vec![7u8; 100_000];
    let id = blob_id(&raw[..]).unwrap();
    let payload = zstd::bulk::compress(&raw, 3).unwrap();
    // Header claims 1,000 bytes; the frame expands to 100,000.
    write_raw_object(
        &f,
        &id,
        ObjectHeader {
            kind: ObjectKind::Blob,
            codec: Codec::Zstd,
            raw_len: 1000,
        },
        &payload,
    )
    .await;
    let err = read_all(s, &id).await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    let inner = err.get_ref().unwrap().downcast_ref::<StoreError>().unwrap();
    assert!(matches!(inner, StoreError::Corrupt { .. }), "{inner}");
}

async fn rejects_truncated_and_damaged_objects<F: Fixture>() {
    let Some(f) = F::new(None).await else { return };
    let s = f.store();

    // zstd payload cut short.
    let raw = noise(50_000, 9).repeat(4);
    let id = blob_id(&raw[..]).unwrap();
    let payload = zstd::bulk::compress(&raw, 3).unwrap();
    let header = ObjectHeader {
        kind: ObjectKind::Blob,
        codec: Codec::Zstd,
        raw_len: raw.len() as u64,
    };
    write_raw_object(&f, &id, header, &payload[..payload.len() / 2]).await;
    assert_eq!(
        read_all(s, &id).await.unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );

    // zstd payload that decodes to fewer bytes than claimed.
    let header_long = ObjectHeader {
        raw_len: raw.len() as u64 + 1,
        ..header
    };
    write_raw_object(&f, &id, header_long, &payload).await;
    assert_eq!(
        read_all(s, &id).await.unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );

    // Data after the frame.
    let mut trailing = payload.clone();
    trailing.extend_from_slice(b"junk");
    write_raw_object(&f, &id, header, &trailing).await;
    assert_eq!(
        read_all(s, &id).await.unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );

    // Uncompressed payload shorter than claimed: detected when opening.
    let small = b"abcdef";
    let sid = blob_id(&small[..]).unwrap();
    write_raw_object(
        &f,
        &sid,
        ObjectHeader {
            kind: ObjectKind::Blob,
            codec: Codec::None,
            raw_len: 10,
        },
        small,
    )
    .await;
    assert!(matches!(
        s.get(D, &sid).await,
        Err(StoreError::Corrupt { .. })
    ));

    // Bad header.
    f.raw_put(&rel(&sid), b"NOPE").await;
    assert!(matches!(
        s.get(D, &sid).await,
        Err(StoreError::Corrupt { .. })
    ));
    let mut bad = header.encode();
    bad[0] = b'X';
    f.raw_put(&rel(&sid), &bad).await;
    assert!(matches!(
        s.get(D, &sid).await,
        Err(StoreError::Corrupt { .. })
    ));

    let missing = ObjectId::from_bytes([9; 32]);
    assert!(matches!(
        s.get(D, &missing).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        s.get_stored(D, &missing).await,
        Err(StoreError::NotFound(_))
    ));
}

async fn list_skips_stray_files_and_delete_works<F: Fixture>() {
    let Some(f) = F::new(None).await else { return };
    let s = f.store();
    put(s, b"one").await.unwrap();
    put(s, b"two").await.unwrap();
    f.raw_put("README", b"x").await;
    f.raw_put("zz/zz/whatever", b"x").await;
    let one = blob_id(&b"one"[..]).unwrap();
    let h = one.to_hex();
    f.raw_put(&format!("{}/{}/tmp.part", &h[..2], &h[2..4]), b"x")
        .await;
    let mut ids: Vec<_> = s.list(D).map(|r| r.unwrap().0).collect().await;
    ids.sort();
    let mut want = vec![one, blob_id(&b"two"[..]).unwrap()];
    want.sort();
    assert_eq!(ids, want);

    s.delete(D, &one).await.unwrap();
    assert!(!s.contains(D, &one).await.unwrap());
    assert!(matches!(
        s.delete(D, &one).await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(s.list(DepotId(99)).count().await, 0);
}

#[tokio::test]
async fn fs_open_removes_stale_temp_files_only() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join("depots/1/tmp");
    let (old, fresh) = stale_and_fresh(&tmp);
    let _s = FsBlobStore::open(dir.path(), FsBlobStoreConfig::default())
        .await
        .unwrap();
    assert!(!old.exists());
    assert!(fresh.exists());
}

fn stale_and_fresh(tmp: &std::path::Path) -> (PathBuf, PathBuf) {
    std::fs::create_dir_all(tmp).unwrap();
    let old = tmp.join("old");
    let fresh = tmp.join("fresh");
    std::fs::write(&old, b"x").unwrap();
    std::fs::write(&fresh, b"x").unwrap();
    let f = std::fs::File::options().write(true).open(&old).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(25 * 3600))
        .unwrap();
    (old, fresh)
}

/// Produces `len` bytes of a repeating pattern without holding them.
struct Generated {
    left: u64,
    counter: u64,
}

impl AsyncRead for Generated {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let n = (buf.remaining() as u64).min(self.left) as usize;
        let dst = buf.initialize_unfilled_to(n);
        for b in dst.iter_mut() {
            // Mostly compressible, with variation so zstd has real work.
            self.counter = self.counter.wrapping_add(1);
            *b = (self.counter % 251) as u8 ^ ((self.counter >> 20) as u8);
        }
        buf.advance(n);
        self.left -= n as u64;
        Poll::Ready(Ok(()))
    }
}

/// Memory bound, by construction: `put` keeps at most the 1 MiB probe plus one 64 KiB
/// chunk and the encoder's output buffer, whatever the stream length. This test checks
/// that a 1 GiB stream that is never held in memory stores and reads back correctly.
async fn one_gib_stream<F: Fixture>() {
    const LEN: u64 = 1024 * 1024 * 1024;
    let Some(f) = F::new(None).await else { return };
    let s = f.store();
    let mut hasher = blake3::Hasher::new();
    let mut g = Generated {
        left: LEN,
        counter: 0,
    };
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = g.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let id = ObjectId::from_bytes(*hasher.finalize().as_bytes());
    let mut src = Generated {
        left: LEN,
        counter: 0,
    };
    let outcome = s
        .put(
            D,
            ObjectKind::Blob,
            &id,
            &mut src,
            PutOptions {
                expected_len: Some(LEN),
            },
        )
        .await
        .unwrap();
    assert_eq!(outcome, PutOutcome::Created);
    let mut r = s.get(D, &id).await.unwrap();
    let mut back = blake3::Hasher::new();
    let mut total = 0u64;
    loop {
        let n = r.reader.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        total += n as u64;
        back.update(&buf[..n]);
    }
    assert_eq!(total, LEN);
    assert_eq!(back.finalize().as_bytes(), id.as_bytes());
}

/// T150: a chunked file over this store reads back whole and from any offset, and a chunk
/// list is refused until its chunks are stored.
async fn chunked_files<F: Fixture>() {
    use lode_store::{ChunkListOutcome, ChunkedStore};
    let Some(f) = F::new(None).await else { return };
    let store = ChunkedStore::new(std::sync::Arc::new(f.store().clone()), u64::MAX);
    let data = noise(17 * 1024 * 1024 + 5, 99);
    let mut h = lode_core::ChunkingHasher::new();
    h.update(&data);
    let r = h.finish();
    let list = r.chunks.unwrap();
    assert!(matches!(
        store.put_chunk_list(D, &r.id, &list).await.unwrap(),
        ChunkListOutcome::Missing(_)
    ));
    let mut off = 0;
    for c in &list.chunks {
        let part = &data[off..off + c.size as usize];
        off += c.size as usize;
        put(&store, part).await.unwrap();
    }
    assert_eq!(
        store.put_chunk_list(D, &r.id, &list).await.unwrap(),
        ChunkListOutcome::Stored(PutOutcome::Created)
    );
    assert_eq!(read_all(&store, &r.id).await.unwrap(), data);
    let start = list.chunks[0].size as u64 + 10;
    let mut rd = store.get_from(D, &r.id, start).await.unwrap().reader;
    let mut tail = Vec::new();
    rd.read_to_end(&mut tail).await.unwrap();
    assert_eq!(tail, data[start as usize..]);
    assert_eq!(
        f.store().get(D, &r.id).await.unwrap().kind,
        ObjectKind::ChunkList
    );
}
