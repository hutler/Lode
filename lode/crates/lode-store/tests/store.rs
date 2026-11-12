//! Acceptance tests for the filesystem blob store (task T003).

// Helpers outside #[test] functions unwrap too; a failure is a test failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::StreamExt;
use lode_core::{blob_id, tree_id, Codec, DepotId, ObjectHeader, ObjectId, ObjectKind};
use lode_store::{BlobStore, FsBlobStore, FsBlobStoreConfig, PutOptions, PutOutcome, StoreError};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

const D: DepotId = DepotId(1);

async fn store(dir: &tempfile::TempDir) -> FsBlobStore {
    FsBlobStore::open(dir.path(), FsBlobStoreConfig::default())
        .await
        .unwrap()
}

async fn put(s: &FsBlobStore, data: &[u8]) -> Result<PutOutcome, StoreError> {
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

async fn read_all(s: &FsBlobStore, id: &ObjectId) -> Result<Vec<u8>, io::Error> {
    let mut r = s.get(D, id).await.unwrap();
    let mut out = Vec::new();
    r.reader.read_to_end(&mut out).await?;
    assert_eq!(out.len() as u64, r.raw_len);
    Ok(out)
}

fn object_file(dir: &tempfile::TempDir, id: &ObjectId) -> std::path::PathBuf {
    let h = id.to_hex();
    dir.path()
        .join("depots/1/objects")
        .join(&h[..2])
        .join(&h[2..4])
        .join(h)
}

fn tmp_entries(dir: &tempfile::TempDir) -> usize {
    std::fs::read_dir(dir.path().join("depots/1/tmp"))
        .map(|r| r.count())
        .unwrap_or(0)
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

fn stored_codec(dir: &tempfile::TempDir, id: &ObjectId) -> Codec {
    let bytes = std::fs::read(object_file(dir, id)).unwrap();
    ObjectHeader::decode(&bytes).unwrap().codec
}

#[tokio::test]
async fn round_trips_all_sizes() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(&dir).await;
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
        assert_eq!(put(&s, &data).await.unwrap(), PutOutcome::Created, "{name}");
        assert!(s.contains(D, &id).await.unwrap(), "{name}");
        assert_eq!(stored_codec(&dir, &id), codec, "{name}");
        assert_eq!(read_all(&s, &id).await.unwrap(), data, "{name}");
        let stored = s.get_stored(D, &id).await.unwrap();
        assert_eq!(stored.codec, codec, "{name}");
        assert_eq!(stored.raw_len, data.len() as u64, "{name}");
    }
    assert_eq!(tmp_entries(&dir), 0);
}

#[tokio::test]
async fn trees_are_hashed_in_their_own_domain() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(&dir).await;
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

#[tokio::test]
async fn wrong_expected_id_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(&dir).await;
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
    assert_eq!(tmp_entries(&dir), 0);
    assert_eq!(s.list(D).count().await, 0);
}

#[tokio::test]
async fn size_cap_and_announced_length() {
    let dir = tempfile::tempdir().unwrap();
    let s = FsBlobStore::open(
        dir.path(),
        FsBlobStoreConfig {
            max_object_size: 100,
        },
    )
    .await
    .unwrap();
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
    assert_eq!(tmp_entries(&dir), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_puts_of_same_content() {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(store(&dir).await);
    let data = Arc::new(noise(2 * 1024 * 1024, 3));
    let tasks: Vec<_> = (0..2)
        .map(|_| {
            let s = s.clone();
            let data = data.clone();
            tokio::spawn(async move { put(&s, &data).await.unwrap() })
        })
        .collect();
    let mut outcomes = Vec::new();
    for t in tasks {
        outcomes.push(t.await.unwrap());
    }
    outcomes.sort_by_key(|o| *o == PutOutcome::Created);
    assert_eq!(outcomes, [PutOutcome::AlreadyPresent, PutOutcome::Created]);
    assert_eq!(s.list(D).count().await, 1);
    assert_eq!(tmp_entries(&dir), 0);
}

#[tokio::test]
async fn existing_object_is_never_modified() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(&dir).await;
    let data = vec![b'x'; 5000];
    let id = blob_id(&data[..]).unwrap();
    put(&s, &data).await.unwrap();
    let path = object_file(&dir, &id);
    let before = std::fs::read(&path).unwrap();
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(put(&s, &data).await.unwrap(), PutOutcome::AlreadyPresent);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
}

fn write_raw_object(dir: &tempfile::TempDir, id: &ObjectId, header: ObjectHeader, payload: &[u8]) {
    let path = object_file(dir, id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = header.encode().to_vec();
    bytes.extend_from_slice(payload);
    std::fs::write(path, bytes).unwrap();
}

#[tokio::test]
async fn rejects_payload_longer_than_header_claims() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(&dir).await;
    let raw = vec![7u8; 100_000];
    let id = blob_id(&raw[..]).unwrap();
    let payload = zstd::bulk::compress(&raw, 3).unwrap();
    // Header claims 1,000 bytes; the frame expands to 100,000.
    write_raw_object(
        &dir,
        &id,
        ObjectHeader {
            kind: ObjectKind::Blob,
            codec: Codec::Zstd,
            raw_len: 1000,
        },
        &payload,
    );
    let err = read_all(&s, &id).await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    let inner = err.get_ref().unwrap().downcast_ref::<StoreError>().unwrap();
    assert!(matches!(inner, StoreError::Corrupt { .. }), "{inner}");
}

#[tokio::test]
async fn rejects_truncated_and_damaged_objects() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(&dir).await;

    // zstd payload cut short.
    let raw = noise(50_000, 9).repeat(4);
    let id = blob_id(&raw[..]).unwrap();
    let payload = zstd::bulk::compress(&raw, 3).unwrap();
    let header = ObjectHeader {
        kind: ObjectKind::Blob,
        codec: Codec::Zstd,
        raw_len: raw.len() as u64,
    };
    write_raw_object(&dir, &id, header, &payload[..payload.len() / 2]);
    assert_eq!(
        read_all(&s, &id).await.unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );

    // zstd payload that decodes to fewer bytes than claimed.
    let header_long = ObjectHeader {
        raw_len: raw.len() as u64 + 1,
        ..header
    };
    write_raw_object(&dir, &id, header_long, &payload);
    assert_eq!(
        read_all(&s, &id).await.unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );

    // Data after the frame.
    let mut trailing = payload.clone();
    trailing.extend_from_slice(b"junk");
    write_raw_object(&dir, &id, header, &trailing);
    assert_eq!(
        read_all(&s, &id).await.unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );

    // Uncompressed payload shorter than claimed: detected when opening.
    let small = b"abcdef";
    let sid = blob_id(&small[..]).unwrap();
    write_raw_object(
        &dir,
        &sid,
        ObjectHeader {
            kind: ObjectKind::Blob,
            codec: Codec::None,
            raw_len: 10,
        },
        small,
    );
    assert!(matches!(
        s.get(D, &sid).await,
        Err(StoreError::Corrupt { .. })
    ));

    // Bad header.
    std::fs::write(object_file(&dir, &sid), b"NOPE").unwrap();
    assert!(matches!(
        s.get(D, &sid).await,
        Err(StoreError::Corrupt { .. })
    ));
    let mut bad = header.encode();
    bad[0] = b'X';
    std::fs::write(object_file(&dir, &sid), bad).unwrap();
    assert!(matches!(
        s.get(D, &sid).await,
        Err(StoreError::Corrupt { .. })
    ));

    let missing = ObjectId::from_bytes([9; 32]);
    assert!(matches!(
        s.get(D, &missing).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn list_skips_stray_files_and_delete_works() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(&dir).await;
    put(&s, b"one").await.unwrap();
    put(&s, b"two").await.unwrap();
    let objects = dir.path().join("depots/1/objects");
    std::fs::write(objects.join("README"), b"x").unwrap();
    std::fs::create_dir_all(objects.join("zz/zz")).unwrap();
    std::fs::write(objects.join("zz/zz/whatever"), b"x").unwrap();
    let one = blob_id(&b"one"[..]).unwrap();
    let h = one.to_hex();
    std::fs::write(objects.join(&h[..2]).join(&h[2..4]).join("tmp.part"), b"x").unwrap();
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
async fn open_removes_stale_temp_files_only() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join("depots/1/tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let old = tmp.join("old");
    let fresh = tmp.join("fresh");
    std::fs::write(&old, b"x").unwrap();
    std::fs::write(&fresh, b"x").unwrap();
    let f = std::fs::File::options().write(true).open(&old).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 3600))
        .unwrap();
    drop(f);
    let _s = store(&dir).await;
    assert!(!old.exists());
    assert!(fresh.exists());
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
#[tokio::test(flavor = "multi_thread")]
async fn one_gib_stream() {
    const LEN: u64 = 1024 * 1024 * 1024;
    let dir = tempfile::tempdir().unwrap();
    let s = store(&dir).await;
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
