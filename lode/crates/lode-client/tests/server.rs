//! The client against a real in-process `lode-server` over TCP: the calls the CLI relies on
//! round-trip with the server's actual types and encodings.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use futures::TryStreamExt;
use lode_client::{Client, Error};
use lode_core::{blob_id, Change, ChangeAction, DepotPath, EntryKind, ErrorCode};
use lode_proto::auth::LoginRequest;
use lode_proto::commit::CommitRequest;
use lode_proto::depot::{BranchState, CreateBranchRequest, PatchBranchRequest};
use lode_proto::read::{ManifestDelta, RevisionListQuery};
use lode_server::{init, open_state, router, BoundServer, ServerConfig, SystemClock};

async fn serve() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let admin = init(dir.path()).await.unwrap().admin_token;
    let mut config = ServerConfig::with_data_dir(dir.path());
    config.listen = "127.0.0.1:0".parse().unwrap();
    let state = open_state(config.clone(), Arc::new(SystemClock), vec![])
        .await
        .unwrap();
    let bound = BoundServer::bind(&config, router(state)).await.unwrap();
    let url = format!("http://{}", bound.local_addr());
    tokio::spawn(bound.run(std::future::pending()));
    (dir, url, admin)
}

fn add(path: &str, content: &[u8]) -> Change {
    Change {
        path: DepotPath::parse(path).unwrap(),
        action: ChangeAction::Add,
        base_id: None,
        new_id: Some(blob_id(content).unwrap()),
        kind: Some(EntryKind::File),
        executable: path.ends_with(".sh"),
        size: Some(content.len() as u64),
        copy_from: None,
    }
}

fn request(id: &str, changes: Vec<Change>) -> CommitRequest {
    CommitRequest {
        request_id: id.into(),
        message: format!("commit {id}"),
        changes,
        merged_from: vec![],
        provenance: None,
        extensions: Default::default(),
    }
}

#[tokio::test]
async fn round_trip_through_a_real_server() {
    let (_dir, url, admin) = serve().await;
    let anon = Client::builder(&url).build().unwrap();
    assert_eq!(anon.info().await.unwrap().api_version, "1");
    let err = anon.whoami().await.unwrap_err();
    assert!(err.is_code(ErrorCode::Unauthenticated), "{err:?}");
    let bad = anon
        .login(&LoginRequest {
            name: "admin".into(),
            password: "wrong".into(),
            token_name: None,
        })
        .await
        .unwrap_err();
    assert!(bad.is_code(ErrorCode::Unauthenticated), "{bad:?}");

    let c = anon.with_token(admin);
    assert_eq!(c.whoami().await.unwrap().principal.name, "admin");
    c.create_depot("game").await.unwrap();
    assert_eq!(c.depots().await.unwrap().depots.len(), 1);

    // Upload what is missing, then commit.
    let readme = b"hello\n".to_vec();
    let script = b"#!/bin/sh\necho hi\n".repeat(500);
    let ids = [blob_id(&readme[..]).unwrap(), blob_id(&script[..]).unwrap()];
    assert_eq!(c.missing_objects("game", &ids).await.unwrap(), ids.to_vec());
    for (id, content) in ids.iter().zip([&readme, &script]) {
        c.put_blob("game", id, std::io::Cursor::new(content.clone()))
            .await
            .unwrap();
    }
    assert!(c.missing_objects("game", &ids).await.unwrap().is_empty());
    let req = request(
        "r1",
        vec![add("README.md", &readme), add("bin/run.sh", &script)],
    );
    let r1 = c.commit("game", "main", &req).await.unwrap();
    assert!(!r1.replayed);
    // The same request again is a replay, not a second revision.
    let again = c.commit("game", "main", &req).await.unwrap();
    assert!(again.replayed);
    assert_eq!(again.rev, r1.rev);

    // Read it back.
    let entries: Vec<_> = c
        .manifest("game", r1.rev, &[], &[])
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries
        .iter()
        .any(|e| e.path.as_str() == "bin/run.sh" && e.executable));
    let file = c
        .file("game", r1.rev, &DepotPath::parse("bin/run.sh").unwrap())
        .await
        .unwrap();
    assert!(file.executable);
    assert_eq!(file.kind, EntryKind::File);
    let mut out = Vec::new();
    file.write_to(&mut out).await.unwrap();
    assert_eq!(out, script);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("run.sh");
    c.download_blob("game", &ids[1], &dest).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), script);
    let root = c.tree("game", r1.rev, None).await.unwrap();
    assert_eq!(root.entries.len(), 2);

    // A branch with `/` in its name, and a delta between revisions.
    c.create_branch(
        "game",
        &CreateBranchRequest {
            name: "release/1.0".into(),
            from_branch: "main".into(),
            from_rev: None,
        },
    )
    .await
    .unwrap();
    let more = b"more".to_vec();
    c.put_blob(
        "game",
        &blob_id(&more[..]).unwrap(),
        std::io::Cursor::new(more.clone()),
    )
    .await
    .unwrap();
    let r2 = c
        .commit(
            "game",
            "release/1.0",
            &request("r2", vec![add("notes.txt", &more)]),
        )
        .await
        .unwrap();
    assert_eq!(r2.branch, "release/1.0");
    let delta: Vec<ManifestDelta> = c
        .manifest_delta("game", r1.rev, r2.rev, &[], &[])
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(delta.len(), 1);
    let log = c
        .revisions(
            "game",
            &RevisionListQuery {
                branch: Some("release/1.0".into()),
                changes: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(log.revisions[0].rev, r2.rev);
    assert_eq!(
        c.revision("game", r2.rev).await.unwrap().message,
        "commit r2"
    );
    let archived = c
        .patch_branch(
            "game",
            "release/1.0",
            &PatchBranchRequest {
                state: Some(BranchState::Archived),
            },
        )
        .await
        .unwrap();
    assert_eq!(archived.state, BranchState::Archived);
    let err = c
        .commit("game", "release/1.0", &request("r3", vec![]))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Api(_)), "{err:?}");

    // Maintenance.
    assert!(c.fsck("game", true).await.unwrap().problems.is_empty());
    assert!(c.gc("game", true).await.unwrap().dry_run);
}
