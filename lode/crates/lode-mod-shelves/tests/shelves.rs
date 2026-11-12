//! T032 acceptance: shelves created, replaced, listed, read and deleted over HTTP.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use lode_core::blob_id;
use lode_meta::{Reads, TokenId};
use lode_mod_shelves::Shelves;
use lode_server::module::Module;
use lode_server::{init, open_state, AppState, Caller, ManualClock, ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

struct H {
    state: AppState,
    clock: ManualClock,
    admin: String,
    ana: String,
    bob: String,
    rita: String,
    _dir: tempfile::TempDir,
}

fn id(content: &[u8]) -> String {
    blob_id(content).unwrap().to_string()
}

/// `(path, old content or None for an add, new content or None for a delete)`.
type FileChange<'a> = (&'a str, Option<&'a [u8]>, Option<&'a [u8]>);

impl H {
    async fn send(&self, method: &str, uri: &str, token: &str, body: Value) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"));
        let body = if body.is_null() {
            Body::empty()
        } else {
            b = b.header("content-type", "application/json");
            Body::from(body.to_string())
        };
        let resp = lode_server::router(self.state.clone())
            .oneshot(b.body(body).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn upload(&self, content: &[u8]) -> String {
        let id = id(content);
        let req = Request::put(format!("/api/v1/depots/game/blobs/{id}"))
            .header("authorization", format!("Bearer {}", self.admin))
            .body(Body::from(content.to_vec()))
            .unwrap();
        let resp = lode_server::router(self.state.clone())
            .oneshot(req)
            .await
            .unwrap();
        assert!(resp.status().is_success());
        id
    }

    async fn has_blob(&self, content: &[u8]) -> bool {
        let (s, _) = self
            .send(
                "GET",
                &format!("/api/v1/depots/game/blobs/{}", id(content)),
                &self.admin,
                Value::Null,
            )
            .await;
        s == StatusCode::OK
    }

    /// The change list of `files`, uploading new content unless `upload` is false.
    async fn changes(&self, files: &[FileChange<'_>], upload: bool) -> Vec<Value> {
        let mut out = Vec::new();
        for (path, old, new) in files {
            let mut c = json!({ "path": path });
            if let Some(n) = new {
                let nid = if upload { self.upload(n).await } else { id(n) };
                c["new_id"] = json!(nid);
                c["kind"] = json!("file");
                c["size"] = json!(n.len());
            }
            if let Some(o) = old {
                c["base_id"] = json!(id(o));
            }
            c["action"] = json!(match (old, new) {
                (None, _) => "add",
                (Some(_), Some(_)) => "modify",
                (Some(_), None) => "delete",
            });
            out.push(c);
        }
        out
    }

    async fn commit(&self, files: &[FileChange<'_>]) -> u64 {
        let changes = self.changes(files, true).await;
        let (s, body) = self
            .send(
                "POST",
                "/api/v1/depots/game/branches/main/commits",
                &self.admin,
                json!({ "request_id": format!("r{}", next()), "message": "m", "changes": changes }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
        body["rev"].as_u64().unwrap()
    }

    async fn create(
        &self,
        token: &str,
        base_rev: u64,
        files: &[FileChange<'_>],
    ) -> (StatusCode, Value) {
        let changes = self.changes(files, true).await;
        self.send(
            "POST",
            "/api/v1/depots/game/shelves",
            token,
            json!({ "branch": "main", "base_rev": base_rev, "description": "Fix the door",
                    "changes": changes,
                    "provenance": { "agent": { "model": "m", "tool": "t" } } }),
        )
        .await
    }

    async fn replace(
        &self,
        token: &str,
        shelf: u64,
        base_rev: u64,
        files: &[FileChange<'_>],
    ) -> (StatusCode, Value) {
        let changes = self.changes(files, true).await;
        self.send(
            "PUT",
            &format!("/api/v1/depots/game/shelves/{shelf}"),
            token,
            json!({ "base_rev": base_rev, "description": "Fix the door, again", "changes": changes }),
        )
        .await
    }

    async fn get(&self, token: &str, shelf: u64, query: &str) -> (StatusCode, Value) {
        self.send(
            "GET",
            &format!("/api/v1/depots/game/shelves/{shelf}{query}"),
            token,
            Value::Null,
        )
        .await
    }

    async fn list(&self, token: &str, query: &str) -> Vec<u64> {
        let (s, body) = self
            .send(
                "GET",
                &format!("/api/v1/depots/game/shelves{query}"),
                token,
                Value::Null,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{body}");
        body["shelves"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_u64().unwrap())
            .collect()
    }

    async fn events(&self, kind: &str) -> Vec<Value> {
        let kind = kind.to_owned();
        self.state
            .meta()
            .read(move |tx| tx.outbox_after(0, 10_000))
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == kind)
            .map(|e| serde_json::from_str(&e.payload).unwrap())
            .collect()
    }

    async fn principal(&self, name: &str, kind: &str, scopes: &[&str]) -> String {
        let mut req = json!({ "kind": kind, "name": name });
        if kind == "agent" {
            req["owner"] = json!("ana");
        }
        let (s, body) = self
            .send("POST", "/api/v1/admin/principals", &self.admin, req)
            .await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
        let (s, body) = self
            .send(
                "POST",
                &format!("/api/v1/admin/principals/{name}/tokens"),
                &self.admin,
                json!({ "name": "t", "scopes": scopes }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
        body["token"].as_str().unwrap().to_owned()
    }

    async fn acl(&self, rule: Value) {
        let (s, body) = self
            .send("POST", "/api/v1/admin/acl", &self.admin, rule)
            .await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
    }
}

fn next() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::SeqCst)
}

/// Depot `game` at r2 with `a.txt`, `b.txt` and `secret/s.txt`. `ana` may write, `bob` may
/// read and shelve, `rita` may read everything but `secret/**`.
async fn setup() -> H {
    let dir = tempfile::tempdir().unwrap();
    let admin = init(dir.path()).await.unwrap().admin_token;
    let mut config = ServerConfig::with_data_dir(dir.path());
    config.gc.grace_seconds = 3600;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let clock = ManualClock::new(now);
    let modules: Vec<Arc<dyn Module>> = vec![Shelves::module()];
    let state = open_state(config, Arc::new(clock.clone()), modules)
        .await
        .unwrap();
    let mut h = H {
        state,
        clock,
        admin,
        ana: String::new(),
        bob: String::new(),
        rita: String::new(),
        _dir: dir,
    };
    let (s, _) = h
        .send(
            "POST",
            "/api/v1/depots",
            &h.admin,
            json!({ "name": "game" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    h.ana = h
        .principal("ana", "user", &["read", "write", "shelve"])
        .await;
    h.bob = h.principal("bob", "user", &["read", "shelve"]).await;
    h.rita = h.principal("rita", "user", &["read", "shelve"]).await;
    h.acl(
        json!({ "depot": "game", "subject": { "kind": "user", "name": "ana" },
                  "branch": "*", "path": "**", "effect": "allow", "level": "write" }),
    )
    .await;
    h.acl(json!({ "depot": "game", "subject": { "kind": "all" },
                  "branch": "*", "path": "**", "effect": "allow", "level": "read" }))
        .await;
    h.acl(
        json!({ "depot": "game", "subject": { "kind": "user", "name": "rita" },
                  "branch": "*", "path": "secret/**", "effect": "deny", "level": "read" }),
    )
    .await;
    let rev = h
        .commit(&[
            ("a.txt", None, Some(b"one\ntwo\n")),
            ("b.txt", None, Some(b"b1\n")),
            ("secret/s.txt", None, Some(b"s1\n")),
        ])
        .await;
    assert_eq!(rev, 2);
    h
}

#[tokio::test]
async fn create_replace_list_and_delete() {
    let h = setup().await;
    // bob has no write access; a shelf needs only read.
    let (s, body) = h
        .create(
            &h.bob,
            2,
            &[("a.txt", Some(b"one\ntwo\n"), Some(b"one\n2\n"))],
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    let shelf = body["id"].as_u64().unwrap();
    assert_eq!(body["version"], 1);
    assert_eq!(body["state"], "open");
    assert_eq!(body["owner"], json!({ "name": "bob", "kind": "user" }));
    assert!(body.get("agent_owner").is_none());
    assert_eq!(body["changes"][0]["path"], "a.txt");
    assert_eq!(body["provenance"]["agent"]["model"], "m");

    // Reads do not change the version.
    let (_, body) = h.get(&h.ana, shelf, "").await;
    assert_eq!(body["version"], 1);
    assert_eq!(body["description"], "Fix the door");

    // Only the owner replaces; each replace raises the version.
    let (s, body) = h
        .replace(&h.ana, shelf, 2, &[("b.txt", Some(b"b1\n"), Some(b"b2\n"))])
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
    let (s, body) = h
        .replace(&h.bob, shelf, 2, &[("b.txt", Some(b"b1\n"), Some(b"b2\n"))])
        .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["version"], 2);
    assert_eq!(body["changes"].as_array().unwrap().len(), 1);
    assert_eq!(body["changes"][0]["path"], "b.txt");

    // A new base: fine when the changes match it, out_of_date otherwise.
    let r3 = h.commit(&[("b.txt", Some(b"b1\n"), Some(b"b9\n"))]).await;
    let (s, body) = h
        .replace(
            &h.bob,
            shelf,
            r3,
            &[("b.txt", Some(b"b1\n"), Some(b"b2\n"))],
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "out_of_date");
    assert_eq!(body["details"]["paths"], json!(["b.txt"]));
    let (s, body) = h
        .replace(
            &h.bob,
            shelf,
            r3,
            &[("b.txt", Some(b"b9\n"), Some(b"b2\n"))],
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["version"], 3);
    assert_eq!(body["base_rev"], r3);

    // Lists and filters.
    let (s, body) = h
        .create(&h.ana, 2, &[("new.txt", None, Some(b"n\n"))])
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    let other = body["id"].as_u64().unwrap();
    assert_eq!(h.list(&h.bob, "").await, vec![other, shelf]);
    assert_eq!(h.list(&h.bob, "?owner=bob").await, vec![shelf]);
    assert_eq!(h.list(&h.bob, "?branch=main&owner=ana").await, vec![other]);
    assert!(h.list(&h.bob, "?state=submitted").await.is_empty());
    assert!(h.list(&h.bob, "?owner=nobody").await.is_empty());

    // Delete: not by others, by the owner, and by an administrator.
    let (s, _) = h
        .send(
            "DELETE",
            &format!("/api/v1/depots/game/shelves/{shelf}"),
            &h.ana,
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, body) = h
        .send(
            "DELETE",
            &format!("/api/v1/depots/game/shelves/{shelf}"),
            &h.bob,
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "deleted");
    let (s, body) = h
        .send(
            "DELETE",
            &format!("/api/v1/depots/game/shelves/{other}"),
            &h.admin,
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert!(h.list(&h.bob, "").await.is_empty());
    assert_eq!(h.list(&h.bob, "?state=deleted").await, vec![other, shelf]);
    // A deleted shelf cannot be changed.
    let (s, body) = h
        .replace(
            &h.bob,
            shelf,
            r3,
            &[("b.txt", Some(b"b9\n"), Some(b"b2\n"))],
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "conflict");

    // Events: identifiers only.
    let created = h.events("shelf.created").await;
    assert_eq!(
        created[0],
        json!({ "shelf_id": shelf, "version": 1, "branch": "main", "base_rev": 2,
                "owner": "bob", "owner_kind": "user" })
    );
    assert_eq!(h.events("shelf.updated").await.len(), 2);
    assert_eq!(h.events("shelf.deleted").await.len(), 2);
}

#[tokio::test]
async fn bad_content_is_refused() {
    let h = setup().await;
    let (s, body) = h
        .create(&h.bob, 2, &[("a.txt", Some(b"stale\n"), Some(b"x\n"))])
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "out_of_date");

    let changes = h
        .changes(
            &[("a.txt", Some(b"one\ntwo\n"), Some(b"never uploaded\n"))],
            false,
        )
        .await;
    let (s, body) = h
        .send(
            "POST",
            "/api/v1/depots/game/shelves",
            &h.bob,
            json!({ "branch": "main", "base_rev": 2, "description": "d", "changes": changes }),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "missing_objects");

    let (s, body) = h.create(&h.bob, 99, &[("c.txt", None, Some(b"c\n"))]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    let (s, body) = h
        .create(
            &h.rita,
            2,
            &[("secret/s.txt", Some(b"s1\n"), Some(b"s2\n"))],
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "permission_denied");

    // Without the scope.
    let reader = h.principal("reader", "user", &["read"]).await;
    let (s, body) = h.create(&reader, 2, &[("c.txt", None, Some(b"c\n"))]).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn shelves_touching_unreadable_paths_are_hidden() {
    let h = setup().await;
    let (_, body) = h
        .create(
            &h.ana,
            2,
            &[
                ("a.txt", Some(b"one\ntwo\n"), Some(b"1\n")),
                ("secret/s.txt", Some(b"s1\n"), Some(b"s2\n")),
            ],
        )
        .await;
    let hidden = body["id"].as_u64().unwrap();
    let (_, body) = h
        .create(&h.ana, 2, &[("a.txt", Some(b"one\ntwo\n"), Some(b"1\n"))])
        .await;
    let open = body["id"].as_u64().unwrap();
    assert_eq!(h.list(&h.bob, "").await, vec![open, hidden]);
    assert_eq!(h.list(&h.rita, "").await, vec![open]);
    let (s, _) = h.get(&h.rita, hidden, "").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h
        .send(
            "DELETE",
            &format!("/api/v1/depots/game/shelves/{hidden}"),
            &h.rita,
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn diff_and_agent_owner() {
    let h = setup().await;
    let bot = h.principal("bot", "agent", &["read", "shelve"]).await;
    let (s, body) = h
        .create(
            &bot,
            2,
            &[
                ("a.txt", Some(b"one\ntwo\n"), Some(b"one\n2\n")),
                ("b.txt", Some(b"b1\n"), None),
                ("bin.dat", None, Some(b"\0\x01")),
                ("c.txt", None, Some(b"c\n")),
            ],
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    assert_eq!(body["owner"], json!({ "name": "bot", "kind": "agent" }));
    assert_eq!(body["agent_owner"], "ana");
    let shelf = body["id"].as_u64().unwrap();
    assert!(body.get("diff").is_none());

    let (s, body) = h.get(&h.bob, shelf, "?diff=true").await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let diff = body["diff"].as_array().unwrap();
    assert_eq!(
        diff[0]["patch"],
        "--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+2\n"
    );
    assert_eq!(
        diff[1]["patch"],
        "--- a/b.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-b1\n"
    );
    assert_eq!(diff[2]["path"], "bin.dat");
    assert_eq!(diff[2]["skipped"], "binary");
    assert_eq!(
        diff[3]["patch"],
        "--- /dev/null\n+++ b/c.txt\n@@ -0,0 +1 @@\n+c\n"
    );
    let events = h.events("shelf.created").await;
    assert_eq!(events[0]["owner_kind"], "agent");

    // The same through the tools, as an agent would see them.
    let get = h.state.tools().get("get_shelf").unwrap();
    let list = h.state.tools().get("list_shelves").unwrap();
    assert_eq!((get.scope.as_str(), list.scope.as_str()), ("read", "read"));
    let principal = h
        .state
        .meta()
        .read(|tx| tx.principal_by_name("bob"))
        .await
        .unwrap()
        .unwrap();
    let caller = Caller {
        principal,
        scopes: vec!["read".into()],
        token_id: TokenId(0),
        groups: Vec::new(),
    };
    let out = (list.handler)(caller.clone(), json!({ "depot": "game", "owner": "bot" }))
        .await
        .unwrap();
    assert_eq!(out["shelves"][0]["id"], shelf);
    let out = (get.handler)(
        caller,
        json!({ "depot": "game", "id": shelf, "diff": true }),
    )
    .await
    .unwrap();
    assert_eq!(out["diff"][2]["skipped"], "binary");
    assert_eq!(h.state.scopes().agent_defaults(), vec!["shelve"]);
}

#[tokio::test]
async fn open_shelves_keep_their_blobs_through_gc() {
    let h = setup().await;
    let admin = h.admin.clone();
    let gc = || async {
        let (s, body) = h
            .send("POST", "/api/v1/admin/depots/game/gc", &admin, Value::Null)
            .await;
        assert_eq!(s, StatusCode::OK, "{body}");
    };
    let (_, body) = h
        .create(&h.bob, 2, &[("only-shelved.txt", None, Some(b"shelved\n"))])
        .await;
    let shelf = body["id"].as_u64().unwrap();
    // Well past the grace period: only the shelf keeps the blob.
    h.clock.advance_ms(2 * 3600 * 1000);
    gc().await;
    assert!(h.has_blob(b"shelved\n").await);

    let (s, _) = h
        .send(
            "DELETE",
            &format!("/api/v1/depots/game/shelves/{shelf}"),
            &h.bob,
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    gc().await;
    assert!(!h.has_blob(b"shelved\n").await);
    // Committed content is untouched.
    assert!(h.has_blob(b"b1\n").await);
}

impl H {
    async fn submit(&self, token: &str, shelf: u64, body: Value) -> (StatusCode, Value) {
        self.send(
            "POST",
            &format!("/api/v1/depots/game/shelves/{shelf}/submit"),
            token,
            body,
        )
        .await
    }

    async fn revision(&self, rev: u64) -> Value {
        let (s, body) = self
            .send(
                "GET",
                &format!("/api/v1/depots/game/revisions/{rev}"),
                &self.admin,
                Value::Null,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{body}");
        body
    }

    async fn file(&self, rev: u64, path: &str) -> Vec<u8> {
        let req = Request::get(format!("/api/v1/depots/game/files?rev={rev}&path={path}"))
            .header("authorization", format!("Bearer {}", self.admin))
            .body(Body::empty())
            .unwrap();
        let resp = lode_server::router(self.state.clone())
            .oneshot(req)
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec()
    }

    async fn head(&self) -> u64 {
        let (_, body) = self
            .send(
                "GET",
                "/api/v1/depots/game/branches/main",
                &self.admin,
                Value::Null,
            )
            .await;
        body["head_rev"].as_u64().unwrap()
    }
}

#[tokio::test]
async fn submit_commits_as_the_owner() {
    let h = setup().await;
    let bot = h.principal("bot", "agent", &["read", "shelve"]).await;
    let (s, body) = h
        .create(
            &bot,
            2,
            &[
                ("a.txt", Some(b"one\ntwo\n"), Some(b"one\n2\n")),
                ("c.txt", None, Some(b"c\n")),
            ],
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    let shelf = body["id"].as_u64().unwrap();

    // Submitting needs scope `write` and the version that was reviewed.
    let (s, body) = h
        .submit(&h.bob, shelf, json!({ "expected_version": 1 }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
    let (s, body) = h.submit(&h.ana, shelf, json!({})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    let (s, body) = h
        .submit(&h.ana, shelf, json!({ "expected_version": 2 }))
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        (body["code"].clone(), body["details"]["version"].clone()),
        (json!("conflict"), json!(1))
    );
    assert_eq!(h.head().await, 2);

    let (s, body) = h
        .submit(&h.ana, shelf, json!({ "expected_version": 1 }))
        .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "rev": 3, "branch": "main" }));
    let rev = h.revision(3).await;
    assert_eq!(rev["author"], json!({ "name": "bot", "kind": "agent" }));
    assert_eq!(rev["submitter"], json!({ "name": "ana", "kind": "user" }));
    assert_eq!(rev["message"], "Fix the door");
    assert_eq!(rev["provenance"]["submitted_shelf"], shelf);
    assert_eq!(rev["provenance"]["agent"]["model"], "m");
    let meta = h
        .state
        .meta()
        .read(|tx| {
            let d = tx.depot_by_name("game")?.unwrap();
            tx.rev_meta(d.id, 3)
        })
        .await
        .unwrap();
    assert_eq!(
        meta,
        vec![("shelves.shelf_id".to_owned(), shelf.to_string())]
    );
    let (_, view) = h.get(&h.bob, shelf, "").await;
    assert_eq!(
        (view["state"].clone(), view["submitted_rev"].clone()),
        (json!("submitted"), json!(3))
    );
    let events = h.events("shelf.submitted").await;
    assert_eq!((events.len(), events[0]["rev"].clone()), (1, json!(3)));
    let created = h.events("revision.created").await;
    assert_eq!(created.last().unwrap()["origin"], "shelves");
    // The contents are the shelf's.
    assert_eq!(h.file(3, "a.txt").await, b"one\n2\n");
    assert_eq!(h.file(3, "c.txt").await, b"c\n");

    // A second submit of the same version answers the same revision; another version is a
    // conflict, and so is changing a submitted shelf.
    let (s, body) = h
        .submit(
            &h.ana,
            shelf,
            json!({ "expected_version": 1, "message": "again" }),
        )
        .await;
    assert_eq!(
        (s, body),
        (
            StatusCode::OK,
            json!({ "rev": 3, "branch": "main", "replayed": true })
        )
    );
    let (s, body) = h
        .submit(&h.ana, shelf, json!({ "expected_version": 2 }))
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["details"]["state"], "submitted");
    assert_eq!(h.head().await, 3);
    // A direct commit cannot claim to come from a shelf.
    let changes = h.changes(&[("d.txt", None, Some(b"d\n"))], true).await;
    let (s, body) = h
        .send(
            "POST",
            "/api/v1/depots/game/branches/main/commits",
            &h.ana,
            json!({ "request_id": "claim", "message": "m", "changes": changes,
                    "provenance": { "submitted_shelf": shelf } }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    assert!(h.revision(4).await["provenance"]
        .get("submitted_shelf")
        .is_none());
}

#[tokio::test]
async fn refused_submits_leave_the_shelf_open() {
    let h = setup().await;
    let (s, body) = h
        .create(&h.bob, 2, &[("a.txt", Some(b"one\ntwo\n"), Some(b"bob\n"))])
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    let shelf = body["id"].as_u64().unwrap();

    // Updated after review: the reviewed version cannot be submitted.
    let (s, body) = h
        .replace(
            &h.bob,
            shelf,
            2,
            &[("a.txt", Some(b"one\ntwo\n"), Some(b"bob 2\n"))],
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let (s, body) = h
        .submit(&h.ana, shelf, json!({ "expected_version": 1 }))
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["details"]["version"], 2);
    assert_eq!(h.head().await, 2);

    // A submitter who may not write a path of the shelf.
    let wes = h.principal("wes", "user", &["read", "write"]).await;
    h.acl(
        json!({ "depot": "game", "subject": { "kind": "user", "name": "wes" },
                  "branch": "*", "path": "b.txt", "effect": "allow", "level": "write" }),
    )
    .await;
    let (s, body) = h
        .submit(&wes, shelf, json!({ "expected_version": 2 }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "permission_denied");

    // The branch moved on a touched file.
    h.commit(&[("a.txt", Some(b"one\ntwo\n"), Some(b"ana\n"))])
        .await;
    let (s, body) = h
        .submit(&h.ana, shelf, json!({ "expected_version": 2 }))
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "out_of_date");
    let (_, view) = h.get(&h.bob, shelf, "").await;
    assert_eq!(view["state"], "open");
    assert!(h.events("shelf.submitted").await.is_empty());
}

#[tokio::test]
async fn a_merge_shelf_submits_its_merge_record() {
    let h = setup().await;
    let (s, body) = h
        .send(
            "POST",
            "/api/v1/depots/game/branches",
            &h.admin,
            json!({ "name": "feature", "from_branch": "main" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    let changes = h.changes(&[("f.txt", None, Some(b"f\n"))], true).await;
    let (s, body) = h
        .send(
            "POST",
            "/api/v1/depots/game/branches/feature/commits",
            &h.admin,
            json!({ "request_id": "feature-1", "message": "f", "changes": changes }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    let feature_rev = body["rev"].as_u64().unwrap();
    let merged = json!([{ "branch": "feature", "from_rev": feature_rev, "to_rev": feature_rev }]);
    let changes = h.changes(&[("f.txt", None, Some(b"f\n"))], true).await;
    let (s, body) = h
        .send(
            "POST",
            "/api/v1/depots/game/shelves",
            &h.ana,
            json!({ "branch": "main", "base_rev": 2, "description": "Merge feature",
                    "changes": changes, "merged_from": merged }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    let shelf = body["id"].as_u64().unwrap();
    let (s, body) = h
        .submit(&h.ana, shelf, json!({ "expected_version": 1 }))
        .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let rev = h.revision(body["rev"].as_u64().unwrap()).await;
    assert_eq!(rev["merged_from"], merged);
    assert_eq!(rev["message"], "Merge feature");
}
