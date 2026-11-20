//! T123 acceptance: a server whose content is in S3 (the in-process fake): commit, checkout,
//! update to an older revision, garbage collection, fsck, restart and backup.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use lode_core::blob_id;
use lode_server::{S3Settings, StorageConfig};
use lode_store::s3::fake::FakeS3;
use lode_store::s3::sigv4::Credentials;
use lode_testkit::{TestServer, TestServerOptions, TestWorkspace};
use serde_json::Value;

const BUCKET: &str = "lode-content";

fn rev_of(v: &Value) -> u64 {
    v["rev"].as_u64().unwrap()
}

async fn s3_server() -> (Arc<FakeS3>, TestServer) {
    let creds = Credentials {
        access_key_id: "AKIDSERVER".into(),
        secret_access_key: "server/secret".into(),
        session_token: None,
    };
    let fake = Arc::new(FakeS3::start(creds, "us-east-1", BUCKET).await);
    let endpoint = fake.url().to_owned();
    let server = TestServer::start_with(TestServerOptions {
        configure: Box::new(move |cfg| {
            let mut s3 = S3Settings::new(BUCKET).with_credentials("AKIDSERVER", "server/secret");
            s3.prefix = "team".into();
            s3.region = Some("us-east-1".into());
            s3.endpoint = Some(endpoint);
            s3.path_style = true;
            cfg.storage = StorageConfig::s3(s3);
        }),
    })
    .await;
    (fake, server)
}

/// Object files the server keeps on its own disk.
fn local_objects(server: &TestServer) -> usize {
    fn walk(p: &std::path::Path) -> usize {
        std::fs::read_dir(p)
            .map(|rd| {
                rd.flatten()
                    .map(|e| {
                        if e.path().is_dir() {
                            walk(&e.path())
                        } else {
                            1
                        }
                    })
                    .sum()
            })
            .unwrap_or(0)
    }
    walk(&server.data_dir().join("depots"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_depot_lives_in_s3() {
    let (fake, mut server) = s3_server().await;
    server.create_depot("game").await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    let big: Vec<u8> = b"level data "
        .iter()
        .cycle()
        .take(3 * 1024 * 1024)
        .copied()
        .collect();
    ws.write("readme.txt", "first\n");
    ws.write("maps/level1.bin", &big);
    ws.write("empty.txt", "");
    ws.json(&["add", "."]);
    let r1 = rev_of(&ws.json(&["commit", "-m", "First"]));
    ws.write("readme.txt", "second\n");
    ws.json(&["rm", "maps/level1.bin"]);
    let r2 = rev_of(&ws.json(&["commit", "-m", "Second"]));
    assert!(r2 > r1);

    // The content is in the bucket, under the prefix, and not on the server's disk.
    let keys = fake.keys(BUCKET);
    assert!(keys.len() >= 4, "{keys:?}");
    assert!(keys.iter().all(|k| k.starts_with("team/1/")), "{keys:?}");
    assert_eq!(local_objects(&server), 0);

    // Another workspace checks out the head, then goes back to r1.
    let other = TestWorkspace::new(&server).with_token(server.admin_token());
    other.json(&["checkout", &format!("{}/game", server.url()), "."]);
    assert_eq!(other.read("readme.txt"), b"second\n");
    assert!(!other.exists("maps/level1.bin"));
    other.json(&["update", "-r", &r1.to_string()]);
    assert_eq!(other.read("readme.txt"), b"first\n");
    assert_eq!(other.read("maps/level1.bin"), big);
    assert_eq!(other.read("empty.txt"), b"");

    // An upload no revision uses is collected once it is past the grace period.
    let stray = b"uploaded, never committed\n".to_vec();
    let stray_id = blob_id(&stray[..]).unwrap();
    server
        .admin()
        .put_blob("game", &stray_id, std::io::Cursor::new(stray))
        .await
        .unwrap();
    let before = fake.keys(BUCKET).len();
    let fsck = ws.json(&["admin", "fsck", "game"]);
    assert!(fsck.get("problems").is_none(), "{fsck}");
    server.clock().advance_ms(2 * 24 * 3600 * 1000);
    let gc = ws.json(&["admin", "gc", "game"]);
    assert_eq!(gc["objects_affected"], 1, "{gc}");
    let after = fake.keys(BUCKET);
    assert_eq!(after.len(), before - 1);
    assert!(!after.iter().any(|k| k.ends_with(&stray_id.to_hex())));
    let fsck = ws.json(&["admin", "fsck", "game", "--full"]);
    assert!(fsck.get("problems").is_none(), "{fsck}");

    // A restart opens the same bucket.
    server.restart().await;
    let third = TestWorkspace::new(&server).with_token(server.admin_token());
    third.json(&["checkout", &format!("{}/game", server.url()), "."]);
    third.json(&["update", "-r", &r1.to_string()]);
    assert_eq!(third.read("maps/level1.bin"), big);

    // A backup holds the database; the content stays in the bucket.
    let dest = tempfile::tempdir().unwrap();
    let report = lode_server::maintenance::backup(server.state(), &dest.path().join("b"))
        .await
        .unwrap();
    assert!(report.content_in_bucket);
    assert_eq!(report.objects, 0);
    let database = if report.database_external {
        lode_server::maintenance::DATABASE_NOTE_FILE
    } else {
        "meta.db"
    };
    assert!(dest.path().join("b").join(database).is_file());
}

#[tokio::test]
async fn a_server_with_wrong_s3_credentials_does_not_start() {
    let fake = FakeS3::start(
        Credentials {
            access_key_id: "AKID".into(),
            secret_access_key: "right".into(),
            session_token: None,
        },
        "us-east-1",
        BUCKET,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = lode_server::ServerConfig::with_data_dir(dir.path());
    let mut s3 = S3Settings::new(BUCKET).with_credentials("AKID", "wrong-secret");
    s3.endpoint = Some(fake.url().to_owned());
    s3.path_style = true;
    cfg.storage = StorageConfig::s3(s3);
    let err = lode_server::open_blob_store(&cfg)
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(
        err.contains("403") && err.contains("SignatureDoesNotMatch"),
        "{err}"
    );
    assert!(!err.contains("wrong-secret"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_filesystem_depot_moves_to_s3() {
    use lode_server::maintenance::{migrate_to_s3, MigrateOptions};
    use lode_server::{ServerConfig, StorageKind};

    let server = TestServer::start().await;
    server.create_depot("game").await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    for i in 0..5 {
        ws.write(&format!("f{i}.txt"), format!("file {i}\n"));
    }
    ws.json(&["add", "."]);
    let r = rev_of(&ws.json(&["commit", "-m", "Files"]));
    let local = local_objects(&server);
    assert!(local >= 6, "five files and a tree");

    let fake = FakeS3::start(
        Credentials {
            access_key_id: "AKIDMOVE".into(),
            secret_access_key: "move-secret".into(),
            session_token: None,
        },
        "us-east-1",
        BUCKET,
    )
    .await;
    let config = server.data_dir().join("loded.toml");
    std::fs::write(server.data_dir().join("s3-secret.txt"), "move-secret\n").unwrap();
    // Prepared: S3 settings in [storage], still on files.
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(&format!(
        "\n[storage]\nkind = \"fs\"\nbucket = \"{BUCKET}\"\nendpoint = \"{}\"\npath_style = true\n\
         access_key_id = \"AKIDMOVE\"\nsecret_access_key_file = \"s3-secret.txt\"\n",
        fake.url()
    ));
    std::fs::write(&config, &text).unwrap();

    // A first pass while the server runs copies everything and leaves the config alone.
    let first = migrate_to_s3(&config, MigrateOptions { copy_only: true })
        .await
        .unwrap();
    assert_eq!(first.copied as usize, local);
    assert_eq!(first.switched, None);
    assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
    assert_eq!(fake.keys(BUCKET).len(), local);

    // As if interrupted: one object did not make it, and new content arrived meanwhile.
    let lost = fake.keys(BUCKET)[0].clone();
    assert!(fake.remove_raw(BUCKET, &lost));
    ws.write("late.txt", "late\n");
    ws.json(&["add", "late.txt"]);
    ws.json(&["commit", "-m", "Late"]);

    let second = migrate_to_s3(&config, MigrateOptions::default())
        .await
        .unwrap();
    assert!(second.copied >= 2, "{second:?}");
    assert_eq!(
        second.copied + second.already_there,
        local_objects(&server) as u64
    );
    assert_eq!(second.switched.as_deref(), Some(config.as_path()));
    assert!(server.data_dir().join("loded.toml.before-s3").is_file());

    // The switched configuration opens on S3 and is consistent.
    let cfg = ServerConfig::load(&config).unwrap();
    assert_eq!(cfg.storage.kind, StorageKind::S3);
    let state = lode_server::open_state(cfg, Arc::new(lode_server::SystemClock), Vec::new())
        .await
        .unwrap();
    let report = lode_server::maintenance::fsck(
        &state,
        lode_core::DepotId(1),
        lode_server::maintenance::FsckOptions { full: true },
    )
    .await
    .unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(report.objects_checked as usize, fake.keys(BUCKET).len());
    let id = blob_id(&b"file 3\n"[..]).unwrap();
    assert!(state
        .blobs()
        .contains(lode_core::DepotId(1), &id)
        .await
        .unwrap());
    assert!(r > 0);

    // Once on S3, a repeat is refused.
    let err = migrate_to_s3(&config, MigrateOptions::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("already"), "{err}");
}
