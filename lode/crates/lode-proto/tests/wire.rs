//! Wire-format tests (task T005). Each type's JSON form is pinned by a snapshot in
//! `tests/snapshots/`, so an accidental wire change fails CI. Review a changed snapshot with
//! `cargo insta review` and commit it only if the change is intended.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use insta::assert_json_snapshot;
use lode_proto::admin::*;
use lode_proto::auth::*;
use lode_proto::commit::*;
use lode_proto::depot::*;
use lode_proto::error::ErrorBody;
use lode_proto::locks::*;
use lode_proto::read::*;
use lode_proto::shelves::*;
use lode_proto::*;
use serde::de::DeserializeOwned;
use serde::Serialize;
use time::macros::datetime;

const T: Timestamp = datetime!(2026-11-12 10:30:00 UTC);

fn id(b: u8) -> ObjectId {
    ObjectId::from_bytes([b; 32])
}

fn p(s: &str) -> DepotPath {
    DepotPath::parse(s).unwrap()
}

fn ana() -> PrincipalRef {
    PrincipalRef {
        name: "ana".into(),
        kind: PrincipalKind::User,
        owner: None,
    }
}

fn bot() -> PrincipalRef {
    PrincipalRef {
        name: "fixer-bot".into(),
        kind: PrincipalKind::Agent,
        owner: Some("ana".into()),
    }
}

/// Checks that the value survives a JSON round trip unchanged.
fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(v: &T) {
    let json = serde_json::to_string(v).unwrap();
    let back: T = serde_json::from_str(&json).unwrap();
    assert_eq!(&back, v, "{json}");
}

fn changes() -> Vec<Change> {
    vec![
        Change {
            path: p("a/b.txt"),
            action: ChangeAction::Modify,
            base_id: Some(id(1)),
            new_id: Some(id(2)),
            kind: Some(EntryKind::File),
            executable: false,
            size: Some(123),
            copy_from: None,
        },
        Change {
            path: p("a/new.png"),
            action: ChangeAction::Add,
            base_id: None,
            new_id: Some(id(3)),
            kind: Some(EntryKind::File),
            executable: false,
            size: Some(9),
            copy_from: Some(CopyFrom {
                path: p("a/old.png"),
                rev: 41,
            }),
        },
        Change {
            path: p("a/old.png"),
            action: ChangeAction::Delete,
            base_id: Some(id(4)),
            new_id: None,
            kind: None,
            executable: false,
            size: None,
            copy_from: None,
        },
    ]
}

fn provenance() -> Provenance {
    Provenance {
        client: Some("lode-cli/0.1.0".into()),
        via: Some(Via::Mcp),
        agent: Some(AgentProvenance::from([
            ("model".to_owned(), "model-x".to_owned()),
            ("session".to_owned(), "s-1".to_owned()),
            ("tool".to_owned(), "create_shelf".to_owned()),
        ])),
        submitted_shelf: None,
    }
}

macro_rules! snap {
    ($name:literal, $value:expr) => {{
        let v = $value;
        round_trip(&v);
        assert_json_snapshot!($name, v);
    }};
}

#[test]
fn auth_types() {
    snap!(
        "info",
        Info {
            server_version: "0.1.0".into(),
            api_version: lode_proto::consts::API_VERSION.into(),
            modules: vec!["locks".into(), "shelves".into(), "mcp".into()],
        }
    );
    snap!(
        "login_request",
        LoginRequest {
            name: "ana".into(),
            password: "secret".into(),
            token_name: Some("workstation".into()),
        }
    );
    let view = PrincipalView {
        name: "fixer-bot".into(),
        kind: PrincipalKind::Agent,
        display_name: None,
        owner: Some("ana".into()),
        disabled: false,
    };
    snap!(
        "login_response",
        LoginResponse {
            token: "lode_abc".into(),
            principal: view.clone(),
        }
    );
    snap!(
        "whoami",
        WhoAmI {
            principal: view,
            scopes: vec!["read".into(), "shelve".into()],
            token_id: 7,
        }
    );
    let tv = TokenView {
        id: 7,
        name: "ci".into(),
        scopes: vec!["read".into()],
        created_at: T,
        expires_at: Some(T + time::Duration::days(30)),
        last_used_at: None,
    };
    snap!(
        "token_list",
        TokenList {
            tokens: vec![tv.clone()]
        }
    );
    snap!(
        "create_token_request",
        CreateTokenRequest {
            name: "ci".into(),
            scopes: vec!["read".into()],
            expires_at: None,
        }
    );
    snap!(
        "create_token_response",
        CreateTokenResponse {
            token: "lode_xyz".into(),
            info: tv,
        }
    );
}

#[test]
fn depot_types() {
    snap!(
        "depot_list",
        DepotList {
            depots: vec![DepotView {
                name: "game".into(),
                created_at: T,
            }],
        }
    );
    snap!(
        "create_depot_request",
        CreateDepotRequest {
            name: "game".into()
        }
    );
    snap!(
        "branch_list",
        BranchList {
            branches: vec![
                BranchView {
                    name: "main".into(),
                    head_rev: 1042,
                    created_rev: 1,
                    parent: None,
                    state: BranchState::Active,
                },
                BranchView {
                    name: "release/1.0".into(),
                    head_rev: 990,
                    created_rev: 900,
                    parent: Some(BranchPoint {
                        branch: "main".into(),
                        rev: 899,
                    }),
                    state: BranchState::Archived,
                },
            ],
        }
    );
    snap!(
        "create_branch_request",
        CreateBranchRequest {
            name: "feature-x".into(),
            from_branch: "main".into(),
            from_rev: Some(1040),
        }
    );
    snap!(
        "patch_branch_request",
        PatchBranchRequest {
            state: Some(BranchState::Archived),
        }
    );
    snap!(
        "missing_objects_request",
        MissingObjectsRequest {
            ids: vec![id(1), id(2)],
        }
    );
    snap!(
        "missing_objects_response",
        MissingObjectsResponse {
            missing: vec![id(2)]
        }
    );
    snap!(
        "put_blob_response",
        PutBlobResponse {
            id: id(2),
            created: true
        }
    );
}

#[test]
fn commit_types() {
    let mut extensions = BTreeMap::new();
    extensions.insert("locks.keep".to_string(), serde_json::json!(true));
    snap!(
        "commit_request",
        CommitRequest {
            request_id: "6f1c1c1e-0a43-4b52-9a0e-5b8f3d0f7c11".into(),
            message: "Fix door".into(),
            changes: changes(),
            merged_from: vec![MergedFrom {
                branch: "feature-x".into(),
                from_rev: 30,
                to_rev: 44,
            }],
            provenance: Some(Provenance {
                client: Some("lode-cli/0.1.0".into()),
                via: None,
                agent: None,
                submitted_shelf: None,
            }),
            extensions,
        }
    );
    snap!(
        "commit_response",
        CommitResponse {
            rev: 1043,
            branch: "main".into(),
            replayed: false,
        }
    );
}

#[test]
fn read_types() {
    let rev = RevisionView {
        rev: 1043,
        branch: "main".into(),
        parent: Some(1042),
        author: bot(),
        submitter: ana(),
        created_at: T,
        message: "Fix door".into(),
        root_tree: id(9),
        provenance: Some(provenance()),
        merged_from: vec![],
        changes: Some(vec![ChangeView {
            path: p("Source/Door.cpp"),
            action: ChangeAction::Modify,
            kind: Some(EntryKind::File),
            old_id: Some(id(1)),
            new_id: Some(id(2)),
            size: Some(2048),
            executable: false,
            copy_from: None,
        }]),
        changes_truncated: true,
    };
    snap!(
        "revision_list",
        RevisionList {
            revisions: vec![rev],
            more: true,
        }
    );
    snap!(
        "revision_list_query",
        RevisionListQuery {
            after: Some(1042),
            changes: true,
            limit: Some(100),
            ..Default::default()
        }
    );
    snap!(
        "manifest_query",
        ManifestQuery {
            rev: 1043,
            base: Some(1042),
            include: vec!["Source/**".into()],
            exclude: vec![],
        }
    );
    let entry = ManifestEntry {
        path: p("tools/build.sh"),
        kind: EntryKind::File,
        id: id(5),
        size: 77,
        executable: true,
    };
    snap!("manifest_entry", entry.clone());
    snap!("manifest_delta_put", ManifestDelta::Put(entry));
    snap!(
        "manifest_delta_del",
        ManifestDelta::Del { path: p("old.txt") }
    );
    snap!(
        "tree_listing",
        TreeListing {
            rev: 1043,
            path: Some(p("Source")),
            entries: vec![
                TreeEntryView {
                    name: "Door.cpp".into(),
                    kind: EntryKind::File,
                    id: id(2),
                    size: 2048,
                    executable: false,
                },
                TreeEntryView {
                    name: "Engine".into(),
                    kind: EntryKind::Dir,
                    id: id(6),
                    size: 0,
                    executable: false,
                },
            ],
        }
    );
    snap!(
        "path_at_rev_query",
        PathAtRevQuery {
            rev: 1043,
            path: Some(p("Source/Door.cpp")),
        }
    );
    snap!(
        "blame",
        Blame {
            lines: vec![BlameLine {
                rev: 1001,
                author: "ana".into(),
            }],
        }
    );
}

#[test]
fn error_body() {
    snap!(
        "error_out_of_date",
        ErrorBody::new(ErrorCode::OutOfDate, "2 files changed on the branch")
            .with_details(serde_json::json!({ "paths": ["a/b.txt", "c.txt"] }))
    );
    let plain = ErrorBody::new(ErrorCode::NotFound, "no such depot");
    snap!("error_not_found", plain.clone());
    assert_eq!(plain.known_code(), Some(ErrorCode::NotFound));
    let module: ErrorBody =
        serde_json::from_str(r#"{"code":"future_code","message":"m","extra":1}"#).unwrap();
    assert_eq!(module.known_code(), None);
    assert_eq!(module.code, "future_code");
}

#[test]
fn admin_types() {
    snap!(
        "create_principal_request",
        CreatePrincipalRequest {
            kind: PrincipalKind::Agent,
            name: "ci".into(),
            display_name: Some("Build server".into()),
            owner: Some("svc-ci".into()),
            password: None,
        }
    );
    snap!(
        "patch_principal_request",
        PatchPrincipalRequest {
            disabled: Some(true),
            password: None,
        }
    );
    snap!(
        "create_group_request",
        CreateGroupRequest {
            name: "artists".into(),
        }
    );
    snap!(
        "group_list",
        GroupList {
            groups: vec![GroupView {
                name: "artists".into(),
                members: vec!["ana".into()],
            }],
        }
    );
    snap!(
        "acl_list",
        AclList {
            rules: vec![
                AclRule {
                    id: Some(1),
                    depot: None,
                    subject: AclSubject::All,
                    branch: "*".into(),
                    path: "**".into(),
                    effect: AclEffect::Allow,
                    level: AccessLevel::Read,
                },
                AclRule {
                    id: Some(2),
                    depot: Some("game".into()),
                    subject: AclSubject::Group("artists".into()),
                    branch: "release/*".into(),
                    path: "Source/**".into(),
                    effect: AclEffect::Deny,
                    level: AccessLevel::Write,
                },
            ],
        }
    );
    snap!(
        "maintenance_report",
        MaintenanceReport {
            objects_checked: 1000,
            objects_affected: 1,
            bytes_affected: 0,
            dry_run: false,
            problems: vec!["object 0a… is missing".into()],
        }
    );
}

#[test]
fn locks_types() {
    snap!(
        "lock_request",
        LockRequest {
            branch: "main".into(),
            workspace: "ws-1".into(),
            comment: Some("editing the door".into()),
            paths: vec![LockPath {
                path: p("Content/Door.uasset"),
                base_id: id(1),
            }],
        }
    );
    snap!(
        "unlock_request",
        UnlockRequest {
            branch: "main".into(),
            paths: vec![p("Content/Door.uasset")],
            force: true,
        }
    );
    snap!(
        "unlock_response",
        UnlockResponse {
            released: vec![p("Content/Door.uasset")],
        }
    );
    snap!(
        "locks_query",
        LocksQuery {
            branch: Some("main".into()),
            prefix: Some("Content/".into()),
            owner: None,
        }
    );
    snap!(
        "lock_list",
        LockList {
            locks: vec![LockView {
                branch: "main".into(),
                path: p("Content/Door.uasset"),
                owner: ana(),
                workspace: "ws-1".into(),
                comment: None,
                created_at: T,
            }],
        }
    );
}

#[test]
fn shelves_types() {
    snap!(
        "create_shelf_request",
        CreateShelfRequest {
            branch: "main".into(),
            base_rev: 1042,
            description: "Fix door".into(),
            changes: changes(),
            merged_from: vec![],
            provenance: Some(provenance()),
        }
    );
    snap!(
        "update_shelf_request",
        UpdateShelfRequest {
            base_rev: 1043,
            description: "Fix door, take 2".into(),
            changes: changes()[..1].to_vec(),
            merged_from: vec![],
            provenance: None,
        }
    );
    snap!(
        "shelves_query",
        ShelvesQuery {
            owner: Some("fixer-bot".into()),
            branch: None,
            state: Some(ShelfState::Open),
        }
    );
    snap!(
        "shelf_view",
        ShelfView {
            id: 57,
            version: 3,
            branch: "main".into(),
            base_rev: 1042,
            owner: bot(),
            agent_owner: Some("ana".into()),
            description: "Fix door".into(),
            state: ShelfState::Submitted,
            submitted_rev: Some(1044),
            created_at: T,
            updated_at: T + time::Duration::minutes(5),
            changes: Some(changes()[..1].to_vec()),
            merged_from: vec![],
            provenance: Some(provenance()),
            diff: Some(vec![
                ShelfFileDiff {
                    path: p("Source/Door.cpp"),
                    patch: Some(
                        "--- a/Source/Door.cpp\n+++ b/Source/Door.cpp\n@@ -1 +1 @@\n-a\n+b\n"
                            .into()
                    ),
                    skipped: None,
                },
                ShelfFileDiff {
                    path: p("Content/Door.uasset"),
                    patch: None,
                    skipped: Some("binary".into()),
                },
            ]),
        }
    );
    snap!(
        "submit_shelf_request",
        SubmitShelfRequest {
            expected_version: 3,
            message: None,
        }
    );
}

#[test]
fn architecture_commit_example_round_trips() {
    // ARCHITECTURE §5.1, with "<hex>" replaced by real ids.
    let h1 = id(1).to_hex();
    let h2 = id(2).to_hex();
    let h3 = id(3).to_hex();
    let h4 = id(4).to_hex();
    let text = format!(
        r#"{{ "request_id": "uuid",
  "message": "…",
  "changes": [
    {{ "path": "a/b.txt", "action": "modify", "base_id": "{h1}", "new_id": "{h2}",
      "kind": "file", "executable": false, "size": 123 }},
    {{ "path": "a/new.png", "action": "add", "new_id": "{h3}", "kind": "file", "size": 9,
      "copy_from": {{ "path": "a/old.png", "rev": 41 }} }},
    {{ "path": "a/old.png", "action": "delete", "base_id": "{h4}" }} ],
  "merged_from": [ {{ "branch": "feature-x", "from_rev": 30, "to_rev": 44 }} ],
  "provenance": {{ "client": "lode-cli/0.1.0" }},
  "extensions": {{ "locks.keep": true }} }}"#
    );
    let req: CommitRequest = serde_json::from_str(&text).unwrap();
    assert_eq!(req.changes, changes());
    let mut original: serde_json::Value = serde_json::from_str(&text).unwrap();
    // `"executable": false` is the default and is left out on output.
    original["changes"][0]
        .as_object_mut()
        .unwrap()
        .remove("executable");
    assert_eq!(serde_json::to_value(&req).unwrap(), original);
}

#[test]
fn unknown_fields_are_ignored_and_times_are_utc() {
    let v: DepotView = serde_json::from_str(
        r#"{"name":"game","created_at":"2026-11-12T13:30:00+03:00","future":true}"#,
    )
    .unwrap();
    assert_eq!(v.created_at, T);
    assert_eq!(
        serde_json::to_value(&v).unwrap()["created_at"],
        "2026-11-12T10:30:00Z"
    );
    let bad: Result<DepotView, _> =
        serde_json::from_str(r#"{"name":"g","created_at":"yesterday"}"#);
    assert!(bad.is_err());
    let bad_path: Result<ManifestEntry, _> = serde_json::from_str(&format!(
        r#"{{"path":"../x","kind":"file","id":"{}","size":1}}"#,
        id(1).to_hex()
    ));
    assert!(bad_path.is_err());
}
