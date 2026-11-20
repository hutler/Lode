# Lode — Architecture

Status: draft v0.1. Read [SPEC.md](SPEC.md) first. Where this document and the spec disagree,
the spec wins on behaviour and this document wins on mechanism.

## 1. Overview

```
  workstation                                   server (single process: loded)
 ┌──────────────────────────┐                  ┌──────────────────────────────────────┐
 │ lode (CLI)   lode mcp    │                  │  HTTP API (axum)        /mcp         │
 │      │          │        │   HTTPS + JSON   │      │                    │          │
 │   lode-workspace         │ ───────────────▶ │  auth → permissions → handlers       │
 │   (.lode/workspace.db)   │   blobs: raw     │      │                               │
 │   lode-client            │   zstd streams   │  commit engine ◀── modules           │
 └──────────────────────────┘                  │      │        (locks, shelves, mcp)  │
                                               │  ┌───┴─────────┐  ┌───────────────┐  │
                                               │  │ MetaStore   │  │ BlobStore     │  │
                                               │  │ (SQLite)    │  │ (filesystem)  │  │
                                               │  └─────────────┘  └───────────────┘  │
                                               └──────────────────────────────────────┘
```

Design rules:

1. The core knows nothing about locks, shelves or MCP. Modules use only the extension points
   in section 8. If a module needs a back door, the core is wrong — fix the extension point.
2. The CLI uses the same public API as any third party. Its commands live in `lode-ops`, so
   the CLI and the desktop app run the same code.
3. Everything that touches content streams it. No code path buffers a whole file in memory
   (tree objects and small text merges excepted).
4. The server never trusts a client-supplied hash. It always recomputes.

## 2. Crates

One Cargo workspace. Library crates never depend on binaries; arrows point downward only.

| Crate | Kind | Contents |
| --- | --- | --- |
| `lode-build` | lib | The build version (`0.2.0`, or `0.2.0+5.g4a33c89` after a release), worked out from Git by its `build.rs`; what `lode --version`, `loded --version`, `/info` and the admin pages show |
| `lode-core` | lib, no I/O | `ObjectId`, `DepotPath` + validation, tree encoding, object header, change types, error codes, glob matching, `.lodeconfig` parsing |
| `lode-proto` | lib | Request/response types of the HTTP API (serde). Shared by server and client |
| `lode-store` | lib | `BlobStore` trait and filesystem implementation |
| `lode-meta` | lib | `MetaStore`: SQLite schema, migrations, queries |
| `lode-server` | lib | Config, auth, permissions, commit engine, read queries, module framework, HTTP routes, GC, fsck |
| `lode-mod-locks` | lib | Locks module (server part) |
| `lode-mod-shelves` | lib | Shelves module (server part) |
| `lode-mod-mcp` | lib | Server MCP endpoint |
| `lode-mod-webhooks` | lib | Webhooks module (optional, off by default) |
| `lode-mod-checks` | lib | Checks module and submit gate (optional, off by default) |
| `lode-mod-feed` | lib | Event feed: events of a depot from a cursor, long-poll (0.4) |
| `lode-client` | lib | Typed HTTP client, blob transfer, retry |
| `lode-workspace` | lib | Workspace database, scanner, sync, merge, changelists, client side of locks and shelves |
| `lode-import` | lib | Imports history from other systems: Git through the `git` command (first-parent replay, renames, LFS content) |
| `lode-ops` | lib | Every client operation (context, sign-in resolution, workspace helpers, the commands). Each returns the command's `--json` document and reports through a `Ui` trait (lines, warnings, progress, questions), so it never touches the terminal. Used by `lode` and by the Lode Desk app |
| `lode-cli` | bin `lode` | Argument parsing, the terminal `Ui` (printing, progress, prompts, editor), the `--json` envelope, local MCP server, `self-update`, external subcommands |
| `lode-desk` | bin `lode-desk`, in `desk/` beside the core | Lode Desk, the desktop client, a separate subproject that depends on `lode-ops`, `lode-client` and `lode-core` at a pinned commit or tag of this repository (see `desk/README.md`), so the core can change without fixing Desk in the same change. `desk.rs` runs `lode-ops` operations on a thread each; the Slint window (`app.rs`, screens in `ui/slint/`) awaits them on the UI thread and receives progress through `slint::invoke_from_event_loop`. One copy runs per user (`instance.rs`, a lock file and a loopback socket that takes `lode://` links from later starts); updates come from a signed feed (`update.rs`, `minisign-verify`); installers are made by cargo-packager. Not part of the core workspace |
| `loded` | bin | Wires `lode-server` and enabled modules together |
| `lode-testkit` | lib (dev) | Starts an in-process server on a temp directory; helpers to drive CLI workspaces |

Dependency allow-list is in [../AGENTS.md](../AGENTS.md).

## 3. Objects

### 3.1 Identifiers

`ObjectId` is 32 bytes, shown as 64 lower-case hex characters.

- **Blob id** = `BLAKE3(content)`. A client can compute it with any stock BLAKE3 tool.
- **Tree id** = `BLAKE3` in derive-key mode with context `"lode 2026 tree v1"` over the tree
  encoding. The separate domain means a tree can never collide with a blob.

### 3.2 Tree encoding

A tree describes one directory. Canonical binary form:

```
u8      version = 1
varint  entry_count
entry*  sorted by name bytes, strictly ascending, no duplicates
  u8      kind      1 = file, 2 = dir, 3 = symlink
  u8      flags     bit 0 = executable (files only); other bits must be 0
  varint  name_len, bytes name   (one path segment, validated per SPEC 5.7)
  [32]    id        file: blob id · dir: tree id · symlink: blob id of the target string
  varint  size      file: content length · symlink: target length · dir: 0
```

Varints are unsigned LEB128. Decoding must reject non-canonical input (unsorted, duplicate,
trailing bytes, over-long varints). Empty directories are never stored: a tree with zero
entries is only valid as the root of an empty depot.

### 3.3 Stored object format

Each object is one file: a 16-byte header followed by the payload.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | magic `LODE` |
| 4 | 1 | format version = 1 |
| 5 | 1 | kind: 1 blob, 2 tree, 3 chunk list (§3.5) |
| 6 | 1 | codec: 0 none, 1 zstd |
| 7 | 1 | reserved = 0 |
| 8 | 8 | raw length, little-endian |

Codec `none` is used when zstd saves less than 3 %. Readers enforce `raw length`: decompression
that would exceed it is an error (protects against decompression bombs).

### 3.4 BlobStore

```rust
#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn contains(&self, depot: DepotId, id: &ObjectId) -> Result<bool>;
    /// Streams `raw` (uncompressed) in; verifies the hash; returns Created or AlreadyPresent.
    async fn put(&self, depot: DepotId, kind: ObjectKind, expect: &ObjectId,
                 raw: impl AsyncRead + Send + Unpin) -> Result<PutOutcome>;
    async fn get(&self, depot: DepotId, id: &ObjectId) -> Result<ObjectReader>; // header + raw stream
    async fn delete(&self, depot: DepotId, id: &ObjectId) -> Result<()>;        // GC only
    fn list(&self, depot: DepotId) -> BoxStream<'_, Result<(ObjectId, SystemTime)>>;
}
```

Filesystem layout: `<data>/depots/<depot-id>/objects/<h0h1>/<h2h3>/<64-hex>`.

Write path: stream to `<data>/depots/<depot-id>/tmp/<random>` while hashing the raw bytes and
compressing → compare hash to `expect` (mismatch: delete temp, `hash_mismatch`) → fsync file →
publish with a **no-replace** operation (`link` + `unlink` on Unix, `MoveFileExW` without
`MOVEFILE_REPLACE_EXISTING` on Windows) → fsync directory on Unix. If the target exists the temp
file is dropped and the result is `AlreadyPresent`. Objects are never overwritten.

Blobs are namespaced per depot, so dedup works within a depot only. This keeps depots fully
separable (backup, deletion, permissions).

**S3 storage** (T123): `[storage] kind = "s3"` puts objects in an S3-compatible bucket with
`S3BlobStore`; the database stays local. Keys are `<prefix>/<depot-id>/<h0h1>/<h2h3>/<64-hex>`
and hold the stored format of §3.3 unchanged. A put is encoded into
`<data>/s3-tmp/<random>` exactly as the filesystem write path does (hash check, compression,
cap), then sent as one `PUT` with `If-None-Match: *`: `412` is `AlreadyPresent`, `409` (a
concurrent conditional write) is tried again, so an object is never replaced. Single `PUT`s
limit objects to 5 GiB. Reads stream the `GET` body (the header is read and checked first);
`list` pages through ListObjectsV2 and skips keys that are not objects; `delete` sends `HEAD`
first so a missing object is `NotFound`. Requests are signed with Signature Version 4,
written in `lode-store` over `reqwest` and `sha2` (no AWS SDK; HMAC is a few lines), with the
payload hash `UNSIGNED-PAYLOAD` for bodies. Transport errors and 5xx answers are tried three
times. Credentials come from `access_key_id` plus `secret_access_key_file`, or else
`AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/`AWS_SESSION_TOKEN`; the region from `region`,
`AWS_REGION`, `AWS_DEFAULT_REGION`, then `us-east-1`. `open_state` lists the bucket once, so
wrong settings stop the start with S3's error code. GC and fsck use the `BlobStore` trait and
work unchanged. `loded migrate-storage --to s3` copies `depots/*/objects` into the bucket with
`put_stored_file` (no re-encoding), skipping what a listing shows is there, checks the bucket
has every local object, and then sets `kind = "s3"` in `loded.toml` (keeping
`loded.toml.before-s3`); `--copy-only` leaves the config alone for a first pass beside a live
server. `lode-store`'s feature `test-util` adds an in-process fake S3 that checks signatures,
and the store suite runs against the filesystem store, the fake and, with `LODE_TEST_S3`, a
real service (MinIO in CI).

### 3.5 Chunked files (T150)

A file of at least 16 MiB (`CHUNK_THRESHOLD`, the same cut-off as text versus binary) may be
stored as a **chunk list** instead of whole. The file keeps its id, `BLAKE3(content)`, so
trees, revisions, locks, shelves, caches and older clients do not see a difference. Each
chunk is an ordinary blob (kind 1) under its own id; the list is a kind-3 object stored
under the **file's** id. Its payload:

```
u8      version = 1
varint  count            at least 2
chunk*  in file order
  [32]    id            blob id of the chunk
  varint  size          256 KiB ..= 4 MiB; only the last chunk may be smaller
```

Decoding is strict, like trees: a wrong version, a count under 2, a size out of range, a
non-canonical varint or trailing bytes are rejected.

**Chunker.** Cut points come from FastCDC (Xia et al., "The Design of Fast Content-Defined
Chunking", 2020) with normalized chunking at level 2: minimum 256 KiB, average 1 MiB, maximum
4 MiB. For each byte after the first 256 KiB of a chunk, `hash = (hash << 1) + GEAR[byte]`
(wrapping, 64 bits, starting at 0 for each chunk); the chunk ends after that byte when
`hash & mask == 0`, with the 22 highest bits as the mask up to 1 MiB into the chunk and the
18 highest bits after it, or when it reaches 4 MiB. `GEAR[i]` is the first 8 bytes,
little-endian, of `BLAKE3("lode gear v1" || u8 i)`. The scheme is named
`fastcdc-2020-lode-v1` and `GET /info` reports it with its sizes (`chunking`), so every client
cuts the same content at the same places and their uploads share chunks. Test vector: the
24 MiB xorshift stream of `lode-core`'s `chunk::tests::noise(0x10de, …)` ends its first four
chunks at 1,394,431, 2,789,073, 3,920,034 and 5,066,432 and has 20 chunks.

**Upload.** The client uploads the chunks the server lacks (`objects/missing` with the chunk
ids), then `PUT /depots/{d}/blobs/{id}/chunks` with the list. The server checks every chunk
is stored with the size given (`missing_objects` names the others), streams them in order
through BLAKE3 and compares with `id` (`hash_mismatch`), and only then stores the list with
the same no-replace rule as any object. A file under the threshold is refused; a file already
stored, whole or chunked, is `AlreadyPresent`. The check holds the GC fence shared, like a
commit's object checks.

**Reading.** The server wraps its store in `ChunkedStore`: `get` of a chunk list streams the
chunks one after the other, so download, `files`, blame, MCP, shelves and fsck's re-hash see
the whole file without knowing. `get_stored` still shows the list. `get_from(offset)` opens
only the chunks from an offset on, which serves `Range`. `GET …/blobs/{id}/chunks` returns the
list of a chunked file and `not_found` with `details.reason = "not_chunked"` for one stored
whole. Chunks need not be stored as chunks only: a 1 MiB file whose content equals a chunk is
the same object.

Chunked files also lift the S3 store's 5 GiB single-`PUT` limit, since no object is bigger
than a chunk or its list.

## 4. Metadata

### 4.1 Store

SQLite in WAL mode, one file `<data>/meta.db`, accessed with `rusqlite`: one dedicated writer
connection behind a mutex, a pool of read-only connections, all calls via `spawn_blocking`.
All writers are serialized, which is what makes commits atomic and is ample for 100 users
because blobs are uploaded before the write transaction begins.

`MetaStore` is a concrete type with a narrow method surface. Every statement, in the core
and in modules, goes through the object-safe trait `lode_meta::Sql` (T152): `execute`,
`query`, `query_opt`, `query_one` and `value` on `&dyn Sql`, with parameters built by
`vals![…]` (`i64`, `&str`, `&[u8]`, `Option<…>`, `bool`) and results read from an owned
`Row` with `row.get::<T>(i)`. `ReadTx::conn()`, `WriteTx::conn()` and `Reads::db()` return
`&dyn Sql`; no crate but `lode-meta` names a database driver (a test enforces it; the
client's local SQLite in `lode-workspace` and `lode-ops` is separate). Statements are
written in a portable subset: numbered `?1` parameters (rewritten to `$1` for Postgres),
`RETURNING id` instead of `last_insert_rowid()`, `ON CONFLICT … DO NOTHING/UPDATE` instead
of `INSERT OR IGNORE/REPLACE`, no SQLite-only functions, and a parameter whose type
Postgres cannot infer from its first use (`?2 IS NULL OR …`) written as `CAST(?2 AS BIGINT)`. Where a statement must differ,
`Sql::dialect()` tells which one to run.

Migrations are numbered SQL files embedded in the binary; `schema_version` table; the server
refuses to start on a newer schema than it knows. Modules own tables prefixed `mod_<name>_`
and ship their own migrations. Each step has an SQLite and a Postgres form
(`Migration::sql`, `Migration::postgres`; core files `NNNN_name.sql` and
`NNNN_name.postgres.sql`).

**Postgres (T153).** `[database] kind = "postgres"` with `url` or `url_file` puts the same
schema in Postgres 14+: `BIGINT` for every integer, `BYTEA` for blobs, identity columns for
ids, `TEXT COLLATE "C"` so text orders by bytes as in SQLite (the path range queries rely on
it), and an explicit `rowid` column where SQLite's implicit one gave an order (`rev_merge`).
The driver is `tokio-postgres` with `tokio-postgres-rustls` (ring), the maintained pure-Rust
client; no OpenSSL. The store keeps its shape: one writer connection behind a mutex (so
commits serialize exactly as with SQLite), a pool of reader connections whose transactions
are `REPEATABLE READ READ ONLY` snapshots, and statements prepared once per connection.
Connections are driven by a two-thread runtime inside `lode-meta`; the blocking threads wait
on the client's futures with `futures::executor::block_on`, which is safe on any thread.
Migrations run under a transaction-level advisory lock, so two servers starting together
take turns.

*One active server.* `loded serve` opens one more connection and takes the session-level
advisory lock `hashtextextended('lode-server:' || current_database() || '.' ||
current_schema(), 0)`, with `application_name = "loded <host> pid <n>"`. While another
session holds it, `serve` refuses and names the holder (from `pg_locks` and
`pg_stat_activity`); with `--wait-for-lock` it retries every second. Postgres frees the lock
when the holder's connection ends, so a standby takes over when the active server stops or
dies. The holder pings its lock connection every 3 seconds (10 second limit); on failure the
server shuts down, since a standby may already be active. Both must share the object store
(S3 or a shared filesystem).

*Moving.* `loded migrate-db --to postgres --url-file <f>` opens the SQLite database (bringing
it up to date), opens the empty Postgres database with the same modules (creating the
schema), copies every table in foreign-key order inside one Postgres transaction, compares
row counts, moves the identity sequences past the copied ids, and only then rewrites
`loded.toml` (keeping `loded.toml.before-postgres`).

*Tests.* With `LODE_TEST_POSTGRES` set to a server where tests may create schemas, `loded
init` (and so every test server) puts its metadata in a fresh schema and records the URL in
`<data>/test-postgres-url`, which the server then opens instead of `meta.db`. CI runs the
server, module, client and CLI suites a second time this way.

### 4.2 Core schema

```sql
depot      (id INTEGER PK, name TEXT UNIQUE, next_rev INTEGER NOT NULL, created_at,
            deleted_at NULL, purged_at NULL)   -- in the trash / erased tombstone (schema 6)
principal  (id INTEGER PK, kind TEXT CHECK(kind IN ('user','agent')), name TEXT UNIQUE,
            display_name, owner_id REFERENCES principal, password_hash, disabled INT, created_at,
            sso_id TEXT NULL,   -- UNIQUE INDEX principal_sso_id; lower case (§7, schema 4)
            deleted_at NULL,    -- deleted: row kept for history, name stays taken (schema 6)
            password_change_by NULL) -- set: a first password, good only for choosing a new
                                     -- one, and only until this time (schema 9)
grp        (id INTEGER PK, name TEXT UNIQUE)
grp_member (grp_id, principal_id, PRIMARY KEY (grp_id, principal_id))
token      (id INTEGER PK, principal_id, name, hash BLOB UNIQUE, scopes TEXT,
            expires_at, last_used_at, created_at)
acl        (id INTEGER PK, depot_id NULL, subject_kind TEXT CHECK(subject_kind IN ('all','user','group')),
            subject_id NULL, branch_glob TEXT, path_glob TEXT,
            effect TEXT CHECK(effect IN ('allow','deny')), level TEXT CHECK(level IN ('read','write','admin')))
branch     (id INTEGER PK, depot_id, name, head_rev, created_rev,
            parent_branch_id NULL, parent_rev NULL, state TEXT DEFAULT 'active',
            UNIQUE (depot_id, name))
revision   (depot_id, num, branch_id, parent_num NULL, root_tree BLOB, author_id, submitter_id,
            created_at, message TEXT, provenance TEXT /*json*/, request_id TEXT,
            PRIMARY KEY (depot_id, num), UNIQUE (depot_id, request_id))
rev_change (depot_id, rev, path TEXT, action TEXT CHECK(action IN ('A','M','D')), kind INT,
            old_id BLOB NULL, new_id BLOB NULL, size INT, flags INT,
            copy_from_path TEXT NULL, copy_from_rev INT NULL,
            PRIMARY KEY (depot_id, rev, path))
            -- INDEX rev_change_path (depot_id, path, rev)
rev_merge  (depot_id, rev, source_branch_id, from_rev, to_rev)   -- inclusive range
rev_meta   (depot_id, rev, key TEXT, value TEXT, PRIMARY KEY (depot_id, rev, key))
outbox     (id INTEGER PK AUTOINCREMENT, depot_id, kind TEXT, payload TEXT /*json*/, created_at)
outbox_cursor (subscriber TEXT PK, last_id INT)
counter    (name TEXT PK, value INT)   -- 'access_epoch' (§6), raised with every acl or grp_member change
obliterated (depot_id, id BLOB, size, path, reason, by_id, at, PRIMARY KEY (depot_id, id))  -- §11
maintenance_run (depot_id, kind TEXT /*gc|fsck|fsck-full*/, at, objects_checked, objects_affected,
            bytes_affected, problems, PRIMARY KEY (depot_id, kind))   -- §11, schema 5
```

`rev_change` is the per-path history index (drives `log <path>`, `blame`, rename following).

**Deleted principals and depots (schema 6).** Deleting a principal sets `deleted_at` and
`disabled`, clears `password_hash` and `sso_id`, and removes its tokens, `grp_member` rows
and `acl` rows with it as subject, raising the access epoch. Revisions keep pointing at the
row, so history shows the name; lookups by name (`principal_by_name`, `principal_by_sso`)
and `principals()` leave deleted rows out. A deleted depot first only gets `deleted_at`
("the trash"): `depot_by_name` and `depots()` leave it out, its name stays taken, and
clearing the column restores it. Purging (after 30 days, checked hourly, or at once with
`force`) removes the depot's objects from storage, then in one transaction every module's
rows (`Module::purge_depot`) and the core rows keyed by `depot_id`; the depot row stays as a
tombstone named `#<id>` with `purged_at`, so the id is never reused. Content first, rows
second: an interrupted purge leaves the depot in the trash and the next attempt finishes.
`rev_meta` keys are namespaced `<module>.<key>`.

### 4.3 Branch ancestry

A branch's history is its own revisions, then its parent branch's revisions up to `parent_rev`,
recursively. Path-history queries first resolve this chain into `(branch_id, max_rev)` segments
and filter `rev_change ⨝ revision` by them.

## 5. Commit engine

### 5.1 Request

`POST /api/v1/depots/{depot}/branches/{branch}/commits`

```json
{ "request_id": "uuid",
  "message": "…",
  "changes": [
    { "path": "a/b.txt", "action": "modify", "base_id": "<hex>", "new_id": "<hex>",
      "kind": "file", "executable": false, "size": 123 },
    { "path": "a/new.png", "action": "add", "new_id": "<hex>", "kind": "file", "size": 9,
      "copy_from": { "path": "a/old.png", "rev": 41 } },
    { "path": "a/old.png", "action": "delete", "base_id": "<hex>" } ],
  "merged_from": [ { "branch": "feature-x", "from_rev": 30, "to_rev": 44 } ],
  "provenance": { "client": "lode-cli/0.1.0" },
  "extensions": { "locks.keep": true } }
```

`base_id` is the blob id the client had for that path (absent for `add`). The conflict check
is by content, so workspaces need not be at a single revision.

### 5.2 Steps

Before the transaction (no lock held): authenticate; validate every path; reject duplicate or
nested-conflicting paths (a path that is both a file and a directory prefix); check that all
`new_id` blobs exist (`missing_objects` lists the absent ones); if the change set adds or
modifies `.lodeconfig`, parse the new blob and reject an invalid file (`invalid_request`), so
that a branch head never holds an unparseable configuration.

Inside one write transaction:

1. If `request_id` already produced a revision, return it.
2. Load the branch and its head root tree. Reject if the branch is archived. Obtain the
   **branch configuration**: the parsed `.lodeconfig` of the head tree, from an in-memory
   cache keyed by blob id (empty when the file is absent). On a cache miss the engine releases
   the transaction, loads and parses the blob, and starts again; modules never read blobs.
3. For each change compare with the head tree: `add` requires the path to be absent;
   `modify`/`delete` require the head entry's id to equal `base_id`. Any failure →
   `out_of_date` with the list of paths. Also enforce case-insensitive uniqueness per directory.
4. Check `write` permission for every path.
5. Resolve `copy_from` (source revision must exist and contain that path; caller needs `read`
   on it) and `merged_from` (branch must exist; range must lie in its history). Between
   streams, every `merged_from` range must also pass the flow rules (§5.4), inside the branch
   lock so the copy-up check sees the parent's current head.
6. Run every module's `pre_commit` check. Any rejection aborts with the module's error code.
7. Build new tree objects bottom-up for changed directories only; put them in the blob store.
   (Object writes are not transactional; unreferenced trees left by an aborted commit are
   collected by GC.)
8. Allocate `num = depot.next_rev++`; insert `revision`, `rev_change`, `rev_merge`; set
   `branch.head_rev`; let modules run `in_commit` hooks (same transaction); append a
   `revision.created` row to `outbox`.
9. Commit the transaction. Then wake the outbox dispatcher.

Server-set fields: `author_id` (= caller, or shelf owner for a shelf submit), `submitter_id`
(= caller), `created_at`. `provenance.via` is set by the server from
the endpoint used (`api` or `mcp`). `provenance.client` is copied from the `User-Agent` header
and is therefore self-reported, like the rest of `provenance`, which is stored as given, capped
at 4 KiB. Only `author`, `submitter` and `via` may be used for trust decisions.

### 5.3 Branch creation

`POST /depots/{d}/branches` `{ name, from_branch, from_rev? }` runs the same transaction shape
with no changes: a new revision on the new branch whose `parent_num` is the source revision and
whose `root_tree` is the source's root tree. The same transaction appends `revision.created`
and `branch.created` to the outbox. Archiving a branch appends `branch.archived`.

Branch names use `[A-Za-z0-9._/-]`, at most 200 characters, and are unique per depot ignoring
case. Every `/`-separated segment is non-empty and not `.` or `..`, which rules out a leading,
trailing or doubled `/`. Creating needs scope `write`, a `write` allow rule on the new name and
read access to the source; archiving (`PATCH {"state":"archived"}`) needs scope `admin` and
admin on the depot. `GET …/branches` leaves archived branches out unless `all=true`.

### 5.4 Streams (0.4)

A branch may carry a stream type in `branch.stream_type` (migration 7): `mainline`,
`development` or `release`; NULL is an ordinary branch. The stream parent is the branch's
`parent_branch_id`. `POST …/branches` takes `stream` (`development` or `release`; the source
must be a stream); `PATCH …/branches/{b}` takes `stream` (`mainline` only on a branch without
a parent, `none` clears it and is refused while a child is a stream). Breaking these is
`invalid_request` with `details.reason = "stream_parent"`. Setting or clearing the type
appends `branch.stream_changed`.

Flow rules, for a range of `merged_from` whose source and target are both streams (a range of
one revision counts as a pick):

| Direction | When | Full merge | Pick |
| --- | --- | --- | --- |
| `merge_down` | source is the target's parent, target is development | yes | yes |
| `merge_down` | source is the target's parent, target is release | no | yes |
| `merge_up` | source is a release child of the target | yes | yes |
| `copy_up` | source is a development child of the target | only when the merge base of parent→child is the parent's head (`behind = 0`) | yes |
| `sideways` | anything else | no | no |
| `untyped` | either side is an ordinary branch | yes | yes |

A refusal is `conflict` with `details: {reason: "stream_flow", direction, source, target,
behind?}`. `CommitRequest.extensions.force_flow = true` (and `force_flow` on a shelf submit)
skips the rules for a caller with scope `admin` who is admin on the depot, and is otherwise
`permission_denied`; a forced revision has `forced_flow: true` in its `revision.created` data.
`GET …/merge-info` adds `flow: {direction, allowed, pick_allowed, reason?, behind?}`.

## 6. HTTP API

Base path `/api/v1`. JSON unless stated. Auth: `Authorization: Bearer <token>`.
Path parameters are single URL segments: a branch name that contains `/` is sent with each `/`
percent-encoded (`release%2F1.0`).

| Method and path | Purpose |
| --- | --- |
| `GET /info` | Server version, API version, enabled modules |
| `GET /info` with single sign-on | Adds `sso: {label, password_login}` when `[oidc]` is configured |
| `POST /auth/login` · `GET /auth/whoami` | Password login → token; identity, scopes |
| `POST /auth/password` | Change one's own password given the current one (no token); the only use of a must-change first password |
| `GET /auth/sso/start?redirect_uri=&state=&code_challenge=[&token_name=]` · `GET /auth/sso/callback` · `POST /auth/sso/token {code, code_verifier}` | Single sign-on (§7). Start and callback are browser pages; the token call answers like a password login |
| `GET/POST/DELETE /tokens[/{id}]` | Own tokens |
| `GET/POST /depots` | List; create (admin). For server administrators each depot also carries `revisions` (newest revision number), `last_commit_at` and `size_bytes` (stored content, measured at most every ten minutes; absent with S3) (T179) |
| `DELETE /admin/depots/{d}` `{confirm, force?}` · `POST /admin/depots/{d}/restore` · `GET /admin/trash` | Delete a depot: `confirm` must repeat its name. Without `force` it goes to the trash, answering `{name, purged: false, purge_after}`; with `force` it is erased at once (also from the trash), `{name, purged: true}`. A depot already in the trash is `conflict` without `force`. Restore answers the depot; the trash lists `{depots: [{name, created_at, deleted_at, purge_after}]}`. Server admin only |
| `GET/POST /depots/{d}/branches` · `GET/PATCH …/{b}` | List, create, show, archive |
| `GET /depots/{d}/branches/{b}/merge-info?source=<branch>` | Merging `source` into `b`: `{target, source, branch_point, merged: [{rev, from_rev, to_rev}], base, source_head, pending}`. `branch_point` is the newest revision both histories share; `merged` the ranges from `source` recorded in `b`'s history (§4.3); `base` the merge base of SPEC §5.4; `pending` how many source revisions come after it. Needs read on both branches |
| `POST /depots/{d}/objects/missing` | `{ids}` → `{missing}`; at most 10,000 ids per call. Needs scope `write` or `shelve`, like upload, because the answer reveals whether content exists |
| `PUT /depots/{d}/blobs/{id}` | Upload. Body is the raw content, optionally `Content-Encoding: zstd` |
| `GET /depots/{d}/blobs/{id}` | Download, whole or chunked (§3.5); honours `Accept-Encoding: zstd` for an object stored compressed and whole; one `Range: bytes=` range on the raw content answers `206` (`416` when it cannot be met) |
| `PUT /depots/{d}/blobs/{id}/chunks` · `GET …` | Store a large file as the chunk list in the body (needs scope `write` or `shelve`); read a chunked file's list (§3.5) |
| `POST /depots/{d}/branches/{b}/commits` | Commit (section 5) |
| `GET /depots/{d}/revisions?branch=&path=&before=&after=&own=&changes=&author=&author_kind=&limit=` | Revision list, paged. `author=<name>` and `author_kind=user|agent` filter by author. Default order is newest first, paging with `before`. With `after=N` the order is ascending from `N+1`, for polling. Without `branch` the list covers the whole depot; every item names its branch. With `branch`, inherited history (§4.3) is included unless `own=true`. `changes=true` adds each revision's changed paths, capped per revision (`[limits] max_changes_listed`, default 1,000) with a `changes_truncated` flag. With `path`, `follow=true` continues through `copy_from` into the source path's history |
| `GET /depots/{d}/revisions/{n}` | One revision with its changes |
| `GET /depots/{d}/manifest?rev=N[&base=M][&include=…][&exclude=…]` | NDJSON stream. Without `base`: every file at `N`. With `base`: only differences `M → N`, each line `{"op":"put"|"del", …}` |
| `GET /depots/{d}/tree?rev=&path=` | List one directory |
| `GET /depots/{d}/files?rev=&path=` | File content at a revision, with headers `X-Lode-Id`, `X-Lode-Kind` (`file` or `symlink`) and `X-Lode-Executable` (`true` or `false`). A missing, unreadable or directory path is `not_found` |
| `GET /depots/{d}/blame?rev=&path=` | Per-line last-change revision of a text file: `{path, rev, id, lines: [{rev, text}], revisions: [{rev, branch, author, created_at, summary}], truncated?}`. Walks `rev_change` for the path along the branch chain and through `copy_from`, diffing successive versions line by line (Myers, 2 s per pair before settling for a less minimal diff). At most 1000 versions and 1 MiB per version; history the caller cannot read ends the walk. Either sets `truncated`, and the oldest version looked at takes the remaining lines. Binary or symlink is `invalid_request`, over 1 MiB `too_large`. Results are cached in memory (64 entries) by depot, path and the revision of the newest version, which fixes the history walked; walks cut short by access are not cached |
| `GET /admin/acl?depot=&subject=` | The rules in evaluation order; `depot` keeps those that can apply there (its own and the server-wide ones; unknown depot `not_found`), `subject` (`user:NAME`, `group:NAME`, `all`) that subject's (T172). Server admin only |
| `GET /admin/acl/check?principal=&depot=&branch=&path=` | The level the rules give a principal there, `{principal, depot, branch, path, level: read|write|admin|null, depot_admin?, matched?, decided_by?}`, evaluated as for a token with every scope; `matched` are the ids of the rules that apply and `decided_by` the one that set the level (T142). Server admin only |
| `…/admin/principals[/{name}]` (`GET`/`POST` with optional `sso`; `PATCH` `{disabled?, password?, sso?}`, an empty `sso` removes the link; `DELETE` deletes a user with the agents they own, or an agent, answering `{name, agents}`, refused for `admin` and for the caller), `…/admin/principals/{name}/tokens[/{id}]` (`GET`/`POST`/`DELETE`, tokens of another principal; an agent's owner may use these for the agent), `/groups` (`GET`/`POST`), `/groups/{g}/members/{name}` (`PUT`/`DELETE`), `/acl`, `/depots/{d}/gc`, `/depots/{d}/fsck` | Administration. Needs scope `admin` and server-admin standing |
| `/depots/{d}/locks`, `/depots/{d}/shelves…`, `/mcp` | Modules, section 9 |

Manifests and trees are filtered by read permission and by `include`/`exclude` (repeatable
query parameters). A delta manifest filters each side with the rules of that revision's branch,
so applying it to the full manifest of `M` gives the full manifest of `N`. A directory appears in
a tree listing only if it holds at least one visible file. A revision on a branch the caller
cannot read at all is `not_found` and is left out of revision lists.

**Access epoch.** A delta manifest cannot tell a client that a path it holds has become
unreadable, because the filter uses current permissions on both sides. The server therefore
keeps a counter that is raised whenever an access rule or a group membership changes, and
returns it on every manifest response as `X-Lode-Access-Epoch`. A client that sees a different
value from the one it stored at its last sync must do a full-manifest sync (§10.4).

Errors use one shape with a stable `code`:

```json
{ "code": "out_of_date", "message": "…", "details": { "paths": ["a/b.txt"] } }
```

Codes: `unauthenticated` 401 · `permission_denied` 403 · `not_found` 404 · `invalid_request`,
`invalid_path` 400 · `out_of_date`, `conflict`, `locked`, `lock_required`, `missing_objects`,
`branch_archived` 409 · `obliterated` 410 · `hash_mismatch` 422 · `too_large` 413 · `internal` 500.
Modules may define further codes for their own rejections; the checks module adds
`checks_required` 409.

Limits (configurable): request JSON 64 MiB, single blob 64 GiB, changes per commit 500,000.

### 6.1 Admin web UI (M14)

`lode-server/src/admin/` serves HTML pages for server administrators under `/admin/`, merged
into the same router after the API and module routes; `[admin_ui] enabled = false` leaves
them out, so `/admin/…` falls through to the API's 404.

- **Rendering.** Pages are built in Rust with the `html!` macro (`admin/html.rs`): a format
  string whose every argument is escaped unless it is already `Html`. No template crate and
  no client-side build. One stylesheet and one small script are compiled in and served from
  `/admin/static/`; every admin response carries a content security policy with no inline
  script or style, `X-Frame-Options: DENY`, `nosniff` and `Cache-Control: no-store`. Pages
  work without the script, which adds confirmations, copy buttons and live refresh (it
  fetches the page and swaps `<main>`).
- **Actions.** A page acts by sending an ordinary API request, with the session's token, to
  the API router in process (`tower::ServiceExt::oneshot`, `admin/api.rs`), so permissions,
  validation, events and errors are the API's. The overview also reads what has no API:
  uptime, the bound address, database and storage sizes (cached 10 minutes), the last
  scheduled backup (kept in memory) and `maintenance_run` (written by every GC and fsck).
- **Sessions.** Signing in (password through the API's own check and rate limit, or single
  sign-on with a `Back::Admin` flow whose result is a one-time code redeemed only by
  `/admin/sign-in/sso-done`) creates a token named `web admin` with every registered scope
  and an 8-hour expiry (named `web` for a user who is not a server administrator, T173).
  The cookie `lode_admin` holds that token: `HttpOnly`, `SameSite=Strict`, `Path=/` (since
  T173; the API reads only `Authorization`, never cookies), `Secure` when `[tls]` is set.
  Each request resolves it like a bearer token; admin pages (`Admin` extractor) also require
  server-admin standing, so disabling the user, revoking the token or removing the rule ends
  the session. Sign-out deletes the token. A browser that still holds a cookie from before
  T173 (`Path=/admin`) sends both; each is tried, and the old one is cleared at sign-in.
- **Depot browser (T173).** `admin/web.rs` serves `/web/` to any session (`Viewer`
  extractor): depots, a directory at a branch head or revision (`GET …/tree`), a file
  (`GET …/files` streamed through `Api::raw`, text up to 2 MiB, images inline from
  `/web/depots/{d}/raw`, other files as an attachment), a revision with unified diffs
  (`diffy`, each side read through `…/files` at the parent revision or copy source, at most
  50 files and 1 MiB per side) and the account (`GET/DELETE /tokens`, password change through
  the same function as `POST /auth/password`). Everything is read with the session's token,
  so access rules and path visibility apply as for any client. `lode://` links are built
  with `lode_proto::links` from the request's `Host` (https with `[tls]` or
  `X-Forwarded-Proto: https`).
- **Shelf review (T174).** `admin/web_shelves.rs` adds `/web/depots/{d}/shelves[/{id}]`
  when the shelves module is on: the list (`GET …/shelves?state=open`, checks per current
  version from `GET …/checks`), a shelf (`GET …/shelves/{id}?diff=true` for the patches;
  images from the base revision's file and from the shelf's blob, served only for a path the
  shelf changes) and two forms posting to `POST …/shelves/{id}/submit` (with
  `expected_version` from the page) and `DELETE …/shelves/{id}`. A refused submit is shown
  on the shelf page with the API's message and, for `checks_required`, each entry of
  `details.missing`.
- **CSRF.** Every signed-in form carries `csrf`, the first 16 bytes of
  SHA-256(`"lode admin csrf\n"` + token) in hex, compared in constant time before the handler
  runs. The sign-in form uses a double-submit value in a short-lived `lode_admin_signin`
  cookie, which is also the PKCE verifier of an admin single sign-on.
- **Tests.** `lode-testkit/tests/admin_ui.rs` drives a real server with
  `lode_testkit::web::Browser` (cookies, no redirect following, `submit` sends a form as it
  stands) and parses pages as XML. `cargo run -p lode-testkit --example admin_demo` starts a
  server with sample data; `docs/admin-guide/shoot.mjs` takes the guide's screenshots.

### 6.2 Metrics (T181)

`[metrics] enabled = true` mounts `GET /metrics` (outside `/api/v1`, on the same port) and a
middleware around every route that counts requests by a fixed route class (`api_read`,
`api_write`, `commit`, `blob_upload`, `blob_download`, `admin_ui`, `web_ui`, `metrics`,
`other`) and status code, accepted commits, and blob body bytes as they stream. These
counters live in `AppState` and reset with the process. Gauges are read per scrape: depots
and head revisions from the metadata, module samples from `Module::metrics(tx)` (locks
held and open shelves per depot, webhook deliveries by state), store and SQLite sizes
(through the size cache), certificate expiry and the newest backup. The text format is
written by `lode_server::metrics::render`; `check_text_format` is the test's checker. Access:
the peer address in `allow_from` (CIDR), else a token with the `metrics` scope, which is
registered only while metrics are on. Labels carry depot names and states, never paths.

## 7. Security

- **Transport.** `loded` terminates TLS itself with rustls, with a certificate and key from
  files or one it obtains over ACME (T081), or listens on plain HTTP bound to localhost behind
  a reverse proxy. With `[tls] acme_domain`, `loded` gets the certificate from Let's Encrypt
  (or `acme_directory`) with the TLS-ALPN-01 challenge on its own HTTPS port: during a
  challenge, a handshake offering only the `acme-tls/1` protocol gets the challenge
  certificate and the connection ends there. The account, certificate and key are cached in
  `<data_dir>/acme/<id of the directory URL>/`; a restart serves the cached certificate at
  once, and it is renewed after two thirds of its lifetime (failed attempts are retried after
  5 minutes, doubling up to 6 hours). Over TLS, ALPN picks HTTP/2 or
  HTTP/1.1; plain HTTP is HTTP/1.1 only. `loded` runs its own connection loop on hyper (not
  `axum::serve`), so it can face the internet directly: request headers must arrive within
  `[connections] header_timeout_seconds` (30, also the limit for an idle kept-alive HTTP/1.1
  connection and for the first request on an HTTP/2 connection), and one client address (an
  IPv6 /64 network) may hold at most `[connections] max_per_address` (64) connections; more
  are closed at once. The TLS handshake must finish within 10 s. The client refuses plain HTTP to
  non-loopback hosts unless `--insecure` is given.
- **Passwords** are hashed with Argon2id. **Tokens** are 256 bits of randomness, shown once,
  stored as SHA-256. Login is rate-limited per name and per address.
- **Single sign-on** (T122, `[oidc]`). The server is the OpenID Connect client, with the
  authorization code flow and PKCE on both legs. A client (CLI or Desk) binds `127.0.0.1:0`
  and opens `GET /auth/sso/start` in the browser with that loopback address, a state and an
  S256 challenge; only `http` loopback addresses with a port are accepted. The server keeps
  the sign-in in memory for 10 minutes (at most 10,000 at once, oldest dropped) and sends the
  browser to the provider with its own state, nonce and PKCE pair. The callback address is
  `public_url` + `/api/v1/auth/sso/callback`, or the request's `Host` when `public_url` is not
  set. At the callback the server trades the code at the token endpoint (client secret by
  HTTP Basic when the provider allows it, else in the form; the secret file is read on each
  use), then checks the ID token: compact JWS, `RS256` (`RSA_PKCS1_2048_8192_SHA256`) or
  `ES256` (P-256) against the provider's keys, verified with `ring` directly; `iss` equal to
  the discovery document's issuer, `aud` holding the client id (and `azp`, when present,
  equal to it), `exp` and `iat`/`nbf` with 120 s of clock skew, and `nonce` equal to the one
  this sign-in sent. Discovery and keys are cached for an hour; a token naming an unknown key
  fetches the keys again (at most every 30 s). The provider must be `https`, or `http` on a
  loopback address. The user claim (`email` by default) is matched in lower case against
  `principal.sso_id`; `email_verified: false` is refused, as are domains outside
  `allowed_domains`, agents, and disabled users. With `auto_create` an unknown identity
  becomes a user named after the part before `@` (made unique with `-2`, `-3`, …). The
  browser then goes back to the client with a one-time code (2 minutes, one use), which the
  client trades with its verifier at `POST /auth/sso/token` for a token with the default
  scopes. Every refusal goes back to the client as `error=access_denied` with a description,
  and is logged with its reason (never a code or token). With `password_login = "admins"`
  a password login succeeds only for a principal that would be a server administrator.
- **Scopes** are checked before permissions: `read` for all reads, `write` for commit and branch
  operations, `admin`. These three are the core scopes. Modules declare their own (`lock`,
  `shelve`, `checks`) through `Module::scopes()`. A token may be given any core scope and any
  scope of an enabled module; a scope whose module is disabled is kept but has no effect.
  A token may grant only scopes its maker's token carries, except that a server administrator
  may grant any registered scope: the token `loded init` prints carries only the core scopes,
  because modules are not known at that point.
- **Agents.** An agent's tokens are minted by an administrator or by the agent's owner. The
  owner may grant only `read` and the agent-default module scopes (`ScopeDef::agent_default`:
  `shelve`, `lock`); `write`, `admin` and `checks` need an administrator. A token asked for
  without scopes gets exactly those defaults, computed from the enabled modules. An agent's
  token stops working while its owner is disabled. Agents have no access through their owner:
  ACL rules name them like users. Principal references in views (`author`, `submitter`, lock
  and shelf owners) carry `owner` for agents.
- **Provenance** (SPEC §7.3). The server sets `via` and copies `client` from `User-Agent`;
  `agent` must be an object of string fields and is stored as given. The whole record is
  capped at 4 KiB (`invalid_request` otherwise).
- **Client materialization.** Every path from the server is re-validated on the client
  (`DepotPath` rules, and a first segment of `.lode` in any case is refused). Before a file is
  written, each directory component is checked with `lstat`: a symlink or non-directory there
  stops the command with `unsafe_path`, and a missing one is created with a plain `mkdir`. The
  file is downloaded into `.lode/tmp`, verified, and renamed into place, replacing a symlink
  rather than writing through it. A hostile or buggy server therefore cannot write outside the
  workspace root. A local process racing the client between the check and the rename is out of
  scope.
- **Limits against hostile peers** (T054, details in [HARDENING.md](HARDENING.md)). The
  server fails a request body after `[limits] body_idle_seconds` (60) without data, and
  bounds JSON bodies, blob sizes (after decompression) and changes per commit. The client
  times out a response after 300 s without data, stops a download once it passes the size the
  manifest gave, caps JSON answers at 64 MiB and manifest lines at 1 MiB. Slow request
  headers and many connections from one address are handled by the connection limits above
  (T080). The client keeps idle pooled connections for 20 s, below the server's header
  timeout, so it does not reuse one the server is closing.
- **Bootstrap.** `loded init` creates the data directory and prints a one-time admin token.
  `loded reset-admin` (offline, on the server machine) revokes the `admin` user's tokens and
  prints a new one.

## 8. Module framework

Modules are Rust crates compiled into `loded` and enabled in `loded.toml`:

```toml
[modules]
locks = true
shelves = true
mcp = true
webhooks = false
checks = false
feed = true
validate = false
```

```rust
pub trait Module: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn scopes(&self) -> &'static [ScopeDef] { &[] }                      // { name, agent_default }
    fn migrations(&self) -> &'static [Migration] { &[] }
    fn routes(&self, ctx: ModuleCtx) -> Option<axum::Router> { None }   // mounted by the server
    fn pre_commit(&self, tx: &dyn Reads, c: &CommitCtx) -> Result<(), ApiError> { Ok(()) } // read-only
    fn in_commit(&self, tx: &WriteTx, c: &CommittedCtx) -> Result<(), ApiError> { Ok(()) } // error rolls back
    fn on_event<'a>(&'a self, ctx: &'a ModuleCtx, ev: &'a Event) -> BoxFuture<'a, Result<(), BoxError>> { … } // after commit, at-least-once
    fn worker(&self, ctx: ModuleCtx, stop: Stop) -> Option<BoxFuture<'static, ()>> { None } // background task
    fn gc_roots(&self, tx: &dyn Reads, depot: DepotId, out: &mut dyn FnMut(ObjectId)) -> Result<(), MetaError> { Ok(()) }
    fn mcp_tools(&self, ctx: ModuleCtx) -> Vec<ToolDef> { vec![] }       // tools this module offers to agents
    fn purge_depot(&self, tx: &WriteTx, depot: DepotId) -> Result<(), MetaError> { Ok(()) } // depot erased: drop its rows
    fn principal_deleted(&self, tx: &WriteTx, p: PrincipalId, now: Millis) -> Result<(), MetaError> { Ok(()) } // e.g. release locks
}
```

- `ModuleCtx` gives a module the public server services only: `Auth` (authenticate a request,
  check scope and permission), `Depots` (read trees, files, revisions, diffs), `Commits`
  (run the commit engine), `Blobs`, `Meta` (its own tables plus `rev_meta`).
- `CommitCtx` exposes the **author** and the **submitter** (the same principal for a direct
  commit; different when a module commits on someone's behalf, as shelf submit does), the
  branch, the change list, the client-supplied `extensions` map, the parsed branch
  configuration (§5.2 step 2) with `config_section(name)` for a module's own `[name]` table,
  the message, and `head_entry(path)`, the head-tree entry of each changed path. `extensions`
  comes from the request body and must never be treated as proof of anything.
  `CommittedCtx` adds the new revision number and its time (`created_at`), for rows and
  events a module writes in `in_commit`.
- `CommitCtx.origin` says how the commit reached the engine and is set only by server code:
  `Direct` for a commit request from a client, or `Module { name, attrs }` when a module
  calls `Commits` (shelf submit passes `name = "shelves"`,
  `attrs = { shelf_id, shelf_version }`). A hook that needs to know "is this a shelf submit,
  and of which shelf version" reads `origin`. No client can set it.
- `ToolDef` is a transport-neutral tool description: name, description, input schema, the
  scope it needs, and an async handler taking the caller and JSON input. The server collects
  `mcp_tools()` from every enabled module into a registry that `ModuleCtx` exposes. The MCP
  module hosts that registry; it does not know which module a tool came from.
- `pre_commit` and `in_commit` run inside the commit transaction and must be fast and
  synchronous: no network, no blob I/O.
- `worker` returns a task the server runs for as long as it runs (`spawn_workers` in
  `loded serve`); it must return soon after `Stop` fires. `ModuleCtx::config()` gives a
  module its own `loded.toml` section, such as `[webhooks]`.
- `on_event` is fed by a dispatcher that reads `outbox` after each commit and on startup,
  advancing a per-subscriber cursor only after success. After each pass it deletes outbox rows
  that every registered subscriber has passed; cursors of subscribers that are no longer
  registered (a disabled module) are ignored and removed. A subscriber must not hold the
  cursor back for slow external work: it records what it needs in its own tables and returns.
- Third-party extension without recompiling: the HTTP API, and CLI subcommands found on `PATH`
  as `lode-<name>` (the CLI passes server URL and token through environment variables).

**Module surface (reviewed in T056).** The locks, shelves and MCP modules reach the server only
through `ModuleCtx` (auth, depots, commits, blobs, meta, tools, limits, clock), the commit
hooks (`CommitCtx`, `CommittedCtx`) and the `Module` trait. Their SQL touches only their own
`mod_<name>_` tables and runs through `lode_meta::Sql` in the portable subset of §4.1; core data is read through `lode_meta::Reads` and written only through
`Commits`, `set_rev_meta` and the event outbox. No module uses `AppState`, the permission
engine's internals or the read handlers. `Blobs::store()` and `Meta::store()` hand out the raw
stores, so the rule is kept by review rather than by the type system; a narrower surface is a
candidate for a later version.

### 8.1 Events

Every outbox event has the envelope `{ id, event, depot, created_at, data }`. `data` carries
identifiers only — never file content and never paths, so an event cannot leak what its
receiver may not read. Receivers fetch details through the API with their own permissions.

| Event | Written by | `data` |
| --- | --- | --- |
| `revision.created` | core | `rev, branch, author, author_kind, submitter, origin`, and `forced_flow: true` when a depot admin skipped the stream rules (0.4) |
| `branch.created` | core | `branch, rev, from_branch, from_rev`, and `stream` for a stream (0.4) |
| `branch.archived` | core | `branch` |
| `branch.stream_changed` | core | `branch, stream` (0.4; `null` when cleared) |
| `lock.acquired`, `lock.released` | locks | `branch, owner, count` |
| `shelf.created`, `shelf.updated`, `shelf.deleted` | shelves | `shelf_id, version, branch, base_rev, owner, owner_kind` |
| `shelf.submitted` | shelves | as above, plus `rev` |
| `check.updated` | checks | `target_kind, target_id, target_version, name, state, reporter` |

## 9. Modules

Locks, shelves and MCP (9.1–9.3) are on by default. Webhooks and checks (9.4–9.5) are off by
default.

### 9.1 Locks (`lode-mod-locks`)

```sql
mod_locks_lock (depot_id, branch_id, path TEXT, owner_id, workspace TEXT, comment TEXT, created_at,
                PRIMARY KEY (depot_id, branch_id, path))
```

- `POST /depots/{d}/locks` `{branch, workspace, comment, paths:[{path, base_id}]}` —
  all-or-nothing. Fails `locked` (with holder) or `out_of_date` (head id ≠ `base_id`).
  Needs scope `lock` and `write` permission on each path.
- `DELETE /depots/{d}/locks` `{branch, paths, force}` — `force` needs `admin`.
- `GET /depots/{d}/locks?branch=&prefix=&owner=`.
- `pre_commit`: reject `locked` if any changed path is locked by a principal other than the
  commit's author; reject `lock_required` if a modified or deleted path matches
  `[locks].required` in the branch configuration from `CommitCtx` and the author holds no lock
  on it. Adds are exempt. The module reads no blobs.
- `in_commit`: delete the author's locks on committed paths unless `extensions["locks.keep"]`.
- `mcp_tools`: `list_locks`.
- `scopes`: `lock` (agent default).

Details of the implementation:

- A lock request names 1 to 10,000 paths; each must be a file or symlink at the branch head
  (`not_found` otherwise, also for paths the caller cannot read) and the caller needs `write`
  on it. The paths are looked up outside the database transaction; the transaction checks
  that the branch head is still the one looked at and starts over if a commit moved it.
- A path already locked by the caller from the same workspace stays as it is, so a repeated
  request is harmless. The answer is `LockList` with the locks of the requested paths.
- `locked` carries `details.paths` and `details.locks` (holder, workspace, comment, time).
  It names only paths the caller sent, and for commits paths in the commit.
- `DELETE` releases the caller's own locks; any other lock on a named path refuses the whole
  request with `locked`, unless `force` is set, which needs admin on the depot. Paths that
  are not locked are skipped. The answer is `{ released: [paths] }`.
- `GET` filters by `branch` (`not_found` if the caller cannot read it), `owner` (principal
  name) and `prefix`, a plain string prefix of the path (`Content/` for a directory). It
  lists only locks on paths the caller can read and needs scope `read`.
- Events are written in the same transaction: `lock.acquired` once per request that took new
  locks, `lock.released` once per owner whose locks were released (by `DELETE` or a commit).
- `pre_commit` reads the branch's locks once when there are no more of them than changed
  paths, and looks up each changed path otherwise.

### 9.2 Shelves (`lode-mod-shelves`)

```sql
mod_shelves_shelf  (id INTEGER PK, depot_id, branch_id, base_rev, owner_id, description TEXT,
                    state TEXT /*open|submitted|deleted*/, submitted_rev NULL, provenance TEXT,
                    version INTEGER NOT NULL DEFAULT 1, merged_from TEXT /*json, as in a commit*/,
                    created_at, updated_at)
mod_shelves_change (shelf_id, path, action, kind, base_id, new_id, size, flags,
                    copy_from_path, copy_from_rev, PRIMARY KEY (shelf_id, path))
```

A shelf stores the same change list a commit would send. Blobs are uploaded through the normal
blob API first.

- `POST /depots/{d}/shelves` (create) · `PUT …/{id}` (replace changes; owner only) ·
  `GET …` (list; filter by owner, branch, state) · `GET …/{id}` · `DELETE …/{id}` (owner or admin).
- `PUT` replaces the change list, `base_rev`, description and `merged_from` wholesale and
  raises `version` by one. `version` identifies one exact state of a shelf.
- `POST …/{id}/submit` `{expected_version, message?}` — fails with `conflict` if
  `expected_version` is not the shelf's current version, so nobody can submit content other
  than what they reviewed. Otherwise runs the commit engine with the shelf's changes and
  `merged_from`, `author` = shelf owner, `submitter` = caller. Caller needs scope `write` and `write` permission on every path. On
  success the shelf becomes `submitted`. On `out_of_date` nothing changes.
- `gc_roots`: every `new_id` of shelves in state `open`.
- `mcp_tools`: `list_shelves`, `get_shelf`, `create_shelf`, `update_shelf`, `submit_shelf`
  (the last requires `expected_version`).
- `scopes`: `shelve` (agent default).
- Submit calls `Commits` with `origin = Module { "shelves", { shelf_id, shelf_version } }` and
  writes `shelf.submitted` in the commit transaction (`in_commit`).
- Shelf responses include `version`, the owner's `kind` and, for agents, `owner`.

Details of the implementation:

- Create and `PUT` run the commit engine's checks through `Commits::check` (`ModuleCtx`):
  change-set rules, blob existence and sizes, a parseable `.lodeconfig`, each `base_id`
  against the file at `base_rev` (which must lie in the branch's history), `copy_from`,
  `merged_from` and the provenance size, with `read` instead of `write` on every path. The
  GC fence is held shared from the check until the rows are written.
- A shelf is visible to a caller who can read its branch and every path in it; to anyone
  else it does not exist (`not_found`, left out of lists).
- `GET …` lists open shelves unless `state` is given, newest first, without changes.
  `GET …/{id}` includes the changes; `?diff=true` adds `diff`, one entry per change with a
  unified diff (`--- a/x`, `+++ b/x`, `/dev/null` for adds and deletes) for text files, or
  `skipped` (`binary`, `symlink`, `too_large`, `missing`). Files over 1 MiB are not
  diffed, and no more patches are made once 4 MiB of them are written.
- `PUT` and `DELETE` work on open shelves only (`conflict` otherwise, with `details.state`).
  Deleting keeps the shelf row with state `deleted` and drops its changes, so its blobs are
  collected once the grace period has passed. Deleting a deleted shelf answers its view.
- For an agent owner, the view carries `agent_owner`, the agent's owning user.
- Submit answers a `CommitResponse` (`{rev, branch, replayed}`). The commit's request id is
  unique per attempt; whether a version was submitted is the shelf's state, which `in_commit`
  sets with `WHERE state = 'open' AND version = …` in the commit transaction, together with
  `rev_meta shelves.shelf_id` and the `shelf.submitted` event. A submit that loses a race to
  another of the same version answers that revision (`replayed`); a submitted shelf asked for
  again at its version does the same. The revision's provenance is the shelf's plus
  `submitted_shelf`, which the engine drops from direct commits.

### 9.3 MCP (`lode-mod-mcp`)

Streamable-HTTP MCP endpoint at `/mcp`, built on the official Rust MCP SDK. Each tool is a thin
wrapper over the same services the HTTP handlers use and carries a JSON Schema for input and
output. Tool list: SPEC 6.4. `create_shelf` accepts
`{depot, branch, base_rev, description, edits:[{path, action, text | base64, executable?, copy_from?}]}`,
stores the blobs, and creates a shelf. It is defined by the shelves module and reaches the MCP
endpoint through the tool registry (§8), as do the lock tools; the MCP module itself defines
only the tools that read core data (branches, history, files, diff, blame). A tool is listed
only when its module is enabled and the token has the scope the tool declares. `read_file` caps output (default 256 KiB)
and reports truncation; binary content is returned as a summary (size, id), not bytes.

- The endpoint is stateless (no MCP sessions; protocol `2026-07-28` is stateless anyway):
  every request carries its bearer token, which a middleware resolves before the transport
  sees the request. No token or a bad one gets `401` with `WWW-Authenticate: Bearer`.
  Responses are plain JSON where possible. The SDK's `Host` allow-list (a DNS-rebinding
  guard that defaults to loopback) is off: a browser page cannot send the bearer token.
- The caller's `via` is `mcp` on this endpoint, so commits and shelves made through it
  record `via = "mcp"` in provenance.
- Results are `structuredContent` plus the same JSON as text. A Lode error is a tool error
  (`isError`) whose content is `{code, message, details?}`; a tool the token lacks the scope
  for answers `permission_denied`; an unknown tool name is a protocol error.
- Core tools and their limits: `read_file` takes `rev` or `branch` (default `main`) and an
  optional line range, returns at most 256 KiB (up to 1 MiB on request) with `truncated` and
  the last line returned; content with a NUL byte or invalid UTF-8 in its first 8 KiB is
  `{binary: true, size, id}`. `diff` takes `to_rev` and optional `from_rev` (default: the
  parent) and `path`, lists up to 1000 changed files, compares only text files up to
  256 KiB per side (others get `skipped: binary | too_large`) and caps the text at 256 KiB
  (up to 1 MiB). `search_history` matches message and changed paths case-insensitively and
  looks through at most 2000 revisions per call, returning `next_before` to continue.
  `blame` takes `path`, `rev` or `branch` and an optional line range, returns at most 2000
  lines (`next_line` continues) with the revisions they name.
- Built on `rmcp` 3.x, which needs Rust 1.88.
- Shelf tools (shelves module): `create_shelf` (`base_rev` defaults to the branch head) and
  `update_shelf` (scope `shelve`) take whole-file edits; `update_shelf` replaces the full
  edit set and keeps the description, base revision and `agent` unless given. Each edit's
  `base_id` comes from the base revision, which also makes a path the caller cannot read
  `not_found`. `add` with `copy_from` and no content copies the source exactly. An optional
  `agent` object is stored as provenance. At most `[limits] mcp_max_edits` (500) edits and
  `mcp_max_inline_bytes` (16 MiB) of content per call, else `too_large`. `submit_shelf`
  (scope `write`) needs `expected_version`.
- `lode mcp` (in `lode-cli`) serves one workspace over stdio. Each tool builds the matching
  command's arguments and runs the same function with `--json`, on a worker thread of its
  own (one call at a time; the workspace database handle cannot cross threads). Paths are
  normalized against the workspace root and refused (`outside_workspace`) if they leave
  it, including through a link. `commit` is listed only if `whoami` reports `write`.
  `--agent-*` flags fill `Globals.agent`, which commit and shelve send as provenance. Setup
  snippets: `docs/MCP.md`.
- A shelf's provenance gets `via` from the server, like a commit's. A submitted shelf's
  revision records the submitter's `via`; the shelf keeps the one it was made with.

### 9.4 Webhooks (`lode-mod-webhooks`)

Tells outside systems that something happened, so they need not poll. Built only on
`on_event`, a background `worker` and its own tables.

```sql
mod_webhooks_hook     (id INTEGER PK, depot_id, url TEXT, secret BLOB, events TEXT /*json list*/,
                       branch_glob TEXT NULL, active INT, created_by, created_at,
                       format TEXT DEFAULT 'lode' /*module schema 2, T175*/)
mod_webhooks_delivery (id INTEGER PK, hook_id, outbox_id NULL /*NULL for ping*/, event TEXT,
                       body TEXT, state TEXT /*pending|ok|failed*/, attempts INT,
                       next_attempt_at NULL, first_attempt_at NULL, last_status INT NULL,
                       last_error TEXT NULL, created_at, UNIQUE (hook_id, outbox_id))
```

- API for depot administrators (scope `admin` and `admin` on the depot):
  `GET/POST /depots/{d}/webhooks`, `DELETE …/{id}` (the hook and its deliveries),
  `GET …/{id}/deliveries` (the 100 most recent, newest first), `POST …/{id}/ping` (queues a
  `ping` event, `202`). `POST` takes `{url, events, branch?}`; `events` are names from §8.1,
  at least one. The secret is 32 random bytes from the OS, shown once as 64 hex characters
  in the create answer (`{hook, secret}`) and never again; the HMAC key is that text.
- `on_event` only inserts one `pending` delivery row per active hook whose event list and
  branch pattern match, then returns. It never does network I/O, so one dead receiver cannot
  hold back the outbox cursor or other hooks. `UNIQUE (hook_id, outbox_id)` makes a
  redelivered event a no-op. Events without a `branch` in `data` (`check.updated`) are not
  filtered by the pattern.
- The worker (started with the server, stopped on shutdown, resuming whatever is pending)
  sends each delivery: `POST` with the event envelope (§8.1) as JSON and headers
  `X-Lode-Event`, `X-Lode-Delivery` (the delivery id, stable across retries, for
  de-duplication) and `X-Lode-Signature: sha256=<hex HMAC-SHA256 of the body with the secret>`.
  Any 2xx within 10 s is success. Redirects are not followed and count as failures. After a
  failure the next attempt waits 10 s, 1 min, 5 min, 30 min, then an hour each time, measured
  on the server clock; an attempt that would come more than 24 h after the first one is not
  made and the delivery becomes `failed`. Deliveries to one hook are sent one at a time,
  oldest first; different hooks proceed in parallel.
- URLs must be `https://` unless `[webhooks] allow_http = true` in `loded.toml`, and may not
  carry a user name or password.
- Formats (T175). `POST` also takes `format`: `lode` (the default, the event envelope above;
  left out of JSON when it is the default) or `slack`, `discord`, `telegram`. A chat format's
  body is built when the delivery is queued (`chat::body`): one line of text from the event's
  identifiers, `{"text"}`, `{"content"}` or `{"chat_id", "text",
  "disable_web_page_preview": true}` with `chat_id` from the hook URL's query (required at
  create). Revision and shelf messages end with a `/web/` link when
  `ServerConfig::public_base` knows the address (ACME domain, else `[oidc] public_url`).
  Headers and signing are the same for every format.
- Delivery rows that are no longer pending are deleted 7 days after they were queued.
- Delivery is at-least-once and, under retry, not strictly ordered. Receivers should treat an
  event as a hint and read the revision list with `after=` from their own cursor.
- CLI: `lode admin webhook add <depot> <url> --events e1,e2 [--branch GLOB] [--format F]` (prints the
  secret once on stdout), `list <depot>`, `remove <depot> <id>`, `deliveries <depot> <id>`,
  `ping <depot> <id>`.

### 9.5 Checks (`lode-mod-checks`)

A check is a named result that an outside system attaches to a revision or to one version of
a shelf. Lode stores and lists checks and can require them before a shelf is submitted. It
does not run anything and has no review workflow.

```sql
mod_checks_check (depot_id, target_kind TEXT /*revision|shelf*/, target_id INT, target_version INT,
                  name TEXT, state TEXT /*pending|success|failure|error|cancelled*/,
                  summary TEXT, url TEXT, reporter_id, updated_at,
                  PRIMARY KEY (depot_id, target_kind, target_id, target_version, name))
```

- `scopes`: `checks`. It allows reporting results and nothing else; it is not an agent default.
- `POST /depots/{d}/checks` `{ target: { kind, id, version? }, name, state, summary?, url? }`
  creates or replaces the check for that target and name, recording the caller as reporter.
  Needs scope `checks` and `read` on the depot. `target_version` is the shelf version, and 0
  for a revision. Revision targets are validated through `Depots`; shelf targets are stored as
  given, so this module needs nothing from the shelves module. Limits: name
  `[a-z0-9._/-]{1,100}`, summary 1 KiB, url 2 KiB and `http(s)` only.
- `GET /depots/{d}/checks?target_kind=&target_id=&target_version=` lists the checks of one
  target, most recently reported first, each with its reporter (name, kind, owner for an
  agent); needs scope `read`. `target_version` is required for a shelf and left out for a
  revision. A revision the caller cannot read is `not_found`. Shelf checks are visible to
  anyone who can see the depot, since the module does not know the shelf.
- `POST` answers `200` with the stored check. A report replaces state, summary, url,
  reporter and time; leaving `summary` or `url` out clears it.
- Writes `check.updated`. `mcp_tools`: `list_checks`.
- **Submit gate.** The branch configuration may require checks:

  ```toml
  [checks]
  branches = ["main", "release/*"]
  required = [{ name = "ci/build", reporter = "ci" }]
  ```

  In `pre_commit`, if `origin` is `Module { "shelves", … }` and the branch matches, then for
  every required entry there must be a check on that shelf id and shelf version with that
  name, state `success`, and a reporter whose principal name equals `reporter`. Otherwise the
  commit is rejected `checks_required` with `details: {shelf_id, shelf_version, missing}`;
  each `missing` entry has `name`, the required `reporter` and `reason`: `absent`,
  `not_success` (with `state`) or `wrong_reporter` (with `state` and `reported_by`). A shelf
  submit whose origin lacks the two attributes is refused the same way. The gate reads only
  the module's own table; the coupling to the shelves module is the origin name `shelves`
  and the attribute keys `shelf_id` and `shelf_version`, so with the shelves module off the
  gate never fires.
- The rule is read from the branch head (the configuration in `CommitCtx`), never from the
  shelf being submitted, so a shelf cannot remove the rule it must satisfy.
- Binding each name to a reporter is what makes the gate meaningful: holding the `checks`
  scope is not enough to pass someone else's required check.
- Direct commits are not gated by checks. `[branches].shelf_only` (0.5) closes that gap in
  the commit engine itself, not in this module: after reading the head configuration and
  before writing anything, a commit to a matching branch whose origin is not
  `Module { "shelves", … }` is refused `shelf_required` (`details: {branch}`), unless the
  request carries the extension `override_shelf_only` with a reason (1 to 500 bytes) and the
  caller is a user with the `admin` scope and admin on the depot (otherwise
  `permission_denied`). The engine then sets `provenance.shelf_only_override = {by, reason}`
  (stripped from anything a client sends) and `revision.created` carries
  `shelf_only_override: true`.

### 9.6 Feed (`lode-mod-feed`)

Keeps a copy of each outbox event so readers can ask "what happened since cursor N".

```sql
mod_feed_event (id INT PRIMARY KEY /*outbox id*/, depot_id, created_at, body TEXT /*envelope*/)
mod_feed_depot (depot_id PRIMARY KEY, pruned_through INT)
```

- `on_event` stores the envelope of §8.1 under its outbox id (`ON CONFLICT DO NOTHING`, so a
  repeated delivery is harmless), then wakes waiting requests. A request is woken only after
  the row is committed, so a woken request finds what woke it.
- `GET /depots/{d}/events?after=&kinds=&wait=` reads rows of the depot after `after`, drops
  events whose `data.branch` the caller can read nothing on, and answers at most 500 with
  `next` = the last id scanned (or the newest id anywhere once the depot has nothing more).
  With `wait` it registers for a wake-up before each look, so no event slips between the look
  and the wait. At most 256 requests wait at once; beyond that a request answers at once.
- The worker prunes on start and hourly: rows older than 7 days and beyond 100 000 per
  depot. `pruned_through` per depot lets an answer say `truncated` when `after` is older.
- `mcp_tools`: `wait_for_events` (scope `read`, wait at most 50 s). `purge_depot` deletes the
  depot's rows.

### 9.7 Validate (`lode-mod-validate`, 0.5)

Runs the depot's validation scripts (SPEC §6.8) on shelves. Needs `checks` and `shelves`;
`loded` refuses to start with `[modules] validate = true` without them or without
`[validate] python`.

```sql
mod_validate_run (depot_id, shelf_id, shelf_version /*PK*/, state /*queued|running|done|superseded*/,
                  results TEXT /*JSON ScriptResult[]*/, created_at, updated_at)
```

- `on_event` for `shelf.created` and `shelf.updated` inserts a `queued` row, marks older
  queued or running rows of the shelf `superseded` and aborts their tasks.
- The worker, at start: creates the service user `lode-validate` (disabled, so it cannot
  sign in), puts `running` rows back to `queued` (a run cut short by a restart starts over),
  probes the configured Python and creates `work_dir`. It then starts queued runs, at most
  `[validate] workers` at a time.
- A run reads the shelf (`lode_mod_shelves::shelf_for_module`) and the target branch head's
  `.lodeconfig`. Scripts whose `paths` match no changed file are recorded `skipped`. For the
  rest it reports `validate/<name>` `pending`, writes a fresh directory under `work_dir`
  with the shelf's added and edited files, then the head's `support` files and scripts on top
  (so a shelf cannot replace them; at most `max_input_mib`), runs each script through
  `lode-validate` with `phase: "server"`. One metadata transaction publishes `done`, the
  results, the final check states with their summaries (up to 1 KiB), and their outbox
  events. A superseded run publishes none of them. A storage failure rolls the transaction
  back and leaves the run eligible for retry on server restart. A run that cannot collect
  its inputs completes without results and cannot satisfy required validation.
- `pre_commit`: a shelf submit to a branch in `[checks].branches` needs a `done` row for that
  shelf version where every `server = true` script passed or was skipped; otherwise
  `checks_required` with `missing` entries in the checks module's format (`reporter:
  "lode-validate"`). Other commits are not gated here.
- Route `GET /api/v1/depots/{d}/validate/{shelf}/{version}`: `{ shelf, version, state,
  results }`. `purge_depot` deletes the depot's rows.

## 10. Client

### 10.1 Workspace layout

```
<root>/.lode/workspace.db     SQLite
<root>/.lode/cache/           blob cache for diff/merge bases, LRU, default cap 1 GiB
<root>/.lode/tmp/             downloads in progress (same filesystem as the files)
<root>/.lode/conflicts/       local content set aside by text conflicts, by id, until resolved
<root>/.lode/lock             held (an exclusive SQLite lock) by a command that changes the workspace
```

`lode checkout` stores the revision it is filling in as `checkout_rev` before downloading,
records a `file` row as each file lands, and removes `checkout_rev` after setting
`synced_rev`. Rerunning `lode checkout` with the same target and directory finds
`checkout_rev`, skips files whose row and on-disk hash already match, and fetches the rest.
Other commands refuse a workspace that still has `checkout_rev`.

A command that changes the workspace takes `.lode/lock` and fails at once with "workspace is
in use" if another process holds it; the operating system releases it if `lode` is killed.
Read-only commands such as `status` do not take it.

**Shared download cache (optional).** When `LODE_CACHE_DIR` or the user setting `cache_dir`
names a directory, checkout, update and switch look there for a blob before downloading and
store downloaded blobs there; merges and reverts that need a base use it too. It is
content-addressed, written with temp-plus-rename, verified by hash on read (a damaged entry is
deleted and downloaded again), size-capped (default 20 GiB, user setting `cache_max_gib`)
with LRU eviction once at the end of each sync, and safe for concurrent `lode` processes:
eviction skips what another process removed or holds open, and a cache error only warns.
Files are always copied into workspaces, never hard-linked, so a build that modifies a file
cannot corrupt the cache. Several workspaces of one depot on one machine then download each
blob once.

**Sign-in from the environment.** Every command honours `LODE_SERVER` (default server URL)
and `LODE_TOKEN_FILE` (path to a file holding a token), taking precedence over a stored
login. There is deliberately no variable holding the token itself: it would be inherited by
every process a build starts.

**User config.** The user's config directory is the platform one (`~/.config/lode`,
`%APPDATA%\lode\config`, `~/Library/Application Support/lode`), or `LODE_CONFIG_DIR` when
set. It holds `settings.toml` (`server`, the default server, written by `lode login`) and
`credentials/`. The server URL is taken from `--server`, then the workspace, then
`LODE_SERVER`, then `settings.toml`.

```sql
config     (key TEXT PK, value TEXT)   -- server_url, depot, branch, workspace_id, synced_rev, access_epoch, view json, checkout_rev, pending_commit, update_rev
file       (path TEXT PK, base_id BLOB NULL, kind INT, flags INT, size INT, mtime_ns INT,
            state TEXT /*normal|added|deleted|conflicted*/, copy_from_path, copy_from_rev,
            changelist_id INT NULL, lock_held INT DEFAULT 0)
changelist (id INTEGER PK, name TEXT UNIQUE, description TEXT)
conflict   (path TEXT PK, base_id BLOB, theirs_id BLOB, theirs_rev INT, mine_id BLOB)
```

Credentials are stored per server URL in the user's config directory with owner-only
permissions, never in the workspace: one token file per server (scheme, host, port and path
prefix) under `<config dir>/credentials/`. On Unix the directory is `0700` and the files
`0600`; on Windows they sit under the user's profile and rely on its ACL.

### 10.2 Status scan

Walk the root honouring `.lodeignore` and the view. For each tracked file: if size and mtime
match the row, it is unchanged; otherwise hash it and compare to `base_id` (and refresh
size/mtime if the content is in fact unchanged). Untracked files are reported as `?`.
Hashing runs on a thread pool. An mtime less than 2 s before the scan is stored as 0, so a
write within the same timestamp tick cannot hide behind a matching stat. A tracked file stays
tracked even if an ignore pattern matches it. Local names that are not valid depot paths are
listed and skipped; symlinks are reported as symlinks and never followed.

**Local change commands.** `lode mv` and `lode cp` record the origin (`copy_from`, the source
path at `synced_rev`) on an `added` row; `mv` also marks the source `deleted`, so a commit
sends add-with-copy-from plus delete. Moving or copying an added file carries its origin
along; moving a file back onto its deleted origin turns the pair back into an edit of the
origin. `lode diff` prints a git-style unified diff (`--- a/x`, `+++ b/x`, `/dev/null` for
adds and deletes) that `patch -p1` applies; binary files, and files over 16 MiB, are reported
with their sizes only. Base content for diff comes from the blob cache or the server and is
cached; `revert` restores from the cache or downloads straight into `.lode/tmp`, without
filling the cache with large files. `lode clean` removes untracked (and with `--ignored`,
ignored) files in the view and then the directories left empty, never `.lode/`.

**History commands.** `lode log`, `cat` and `ls` read from the server. In a workspace, paths
are local (relative to the working directory) and `cat`/`ls` default to the workspace's
`synced_rev` on its own branch; with `--depot` (and `--server` or `LODE_SERVER`) they work
anywhere, with depot paths and the branch head. A revision is written `path@12` or
`path@r12`.

**Diff and merge library: `diffy` 0.5.** It produces unified patches and does three-way text
merges with conflict markers, which `lode update` needs (§10.4). `imara-diff` is faster on
very large inputs but has no three-way merge, so it would mean a second crate.

### 10.3 Commit

Scan → select changes (paths, changelist) → refuse if any selected file is `conflicted` →
hash → `objects/missing` → upload missing blobs in parallel (default 8) → `POST commits` →
on success update `base_id`, size, mtime, state for committed files and clear lock flags.

Missing files (gone without `lode rm`) stop the commit, as do conflicted ones. Each sent file
is hashed with a stat before and after, and stat-checked again after the upload; a file that
changed in between aborts the commit. The request, with a fresh `request_id`, is stored as
`pending_commit` before it is sent. A definite refusal (4xx) removes it; a lost answer (a
timeout, a crash, a 5xx) keeps it, and the next `lode commit` sends the same request again
before anything else. The server answers a known `request_id` with the revision it already
made, so a retry never makes a second revision. The message comes from `-m`, `-F` or
`$VISUAL`/`$EDITOR` (only in a terminal, never with `--json`); `#` lines are dropped and an
empty message aborts.

**Changelists.** A changelist is the `changelist_id` of `file` rows; files in none are in
`default`. `lode commit -c` selects the changed files of one changelist. Committed files
leave their changelist, and a changelist that the commit left without files is deleted (one
that had no files in the commit stays). `revert` takes a file out of its changelist. `mv`
puts both halves of a move into the same changelist, and `changelist move` moves them
together, so `commit -c` sends a rename whole.

**Shelves in the workspace.** `lode shelve` gathers and uploads changes as `commit` does and
sends them to the shelves module with the branch head as `base_rev`, so the touched files must
be up to date. Without `--keep` it then reverts them (adds are removed from disk). `unshelve`
applies each change against the workspace's current state: unchanged since the shelf's base →
written; changed here → three-way text merge with the shelf's `base_id` as ancestor, or a
conflict. An unshelve conflict keeps the workspace's base in the row and records the shelf's
side in `conflict.theirs_id` with `theirs_rev = 0`, so `resolve --theirs` writes that side
without moving the base, and a binary conflict puts it in `<file>.theirs-shelf`. A pending
merge travels with the shelf as its `merged_from` and becomes pending again on unshelve (T051).

**Locks in the workspace.** `file.lock_held` records which files this workspace has locked;
`lock`, `unlock`, commit and `locks` set it, and `update` reconciles it with the server and
warns about a lock that was broken. Files that match `[locks].required` in the workspace's
`.lodeconfig` are read-only on disk (Windows attribute, Unix write bits) while they are
unchanged and not locked here: after checkout, update, revert, commit and unlock. `lock` makes
them writable. The bit is a reminder only; the server enforces the rule at commit. A file
whose recorded mtime is too recent to trust is compared by content before it is made
read-only, and writes by `lode` itself clear the bit first.

### 10.4 Update

`GET manifest?rev=target&base=synced_rev` (full manifest when switching branch, changing the
view, or when the server's access epoch differs from the stored one; after a full-manifest sync
a tracked file absent from the manifest is removed if unmodified and reported if modified). For each entry, compared with the `file` row and the working file:

| Local state | Incoming | Result |
| --- | --- | --- |
| unmodified | changed / deleted | replace / delete |
| any | id equals local `base_id` | nothing (already have it) |
| modified, text | changed | three-way merge (base = `base_id` content); clean → modified with new `base_id`; else conflict markers + `conflicted` |
| modified, binary | changed | keep local file, save incoming as `<name>.theirs-r<N>`, `conflicted` |
| modified or deleted | deleted or changed (other combinations) | `conflicted` |
| untracked file in the way | added | `conflicted`, local file kept |

New content is downloaded to `.lode/tmp`, verified against its id, then renamed into place. The
`file` row is updated per file, so an interrupted update resumes when rerun. `synced_rev` is
set when all entries are processed.

Text or binary: `.lodeconfig` `[files]` patterns first, otherwise binary if the first 8 KiB
contains a NUL byte. Files over 16 MiB and symlinks are handled as binary.

Details of the client implementation:

- The target revision is stored as `update_rev` before anything changes. A rerun continues to
  it with the same delta; rows already at the incoming id are skipped, and a conflict already
  recorded for that revision is kept as it is. Merges and conflicts are written first, then
  deletions (with directories left empty), then downloads.
- An update refuses to start while conflicts are unresolved or a commit is pending. Going to
  an older revision is refused if any affected file has local changes.
- A file missing on disk (without `rm`) counts as unmodified, so it is replaced or deleted.
  A local add meeting an incoming add with the same content becomes tracked; with other
  content it is a conflict, like an untracked file in the way.
- On a conflict the row's `base_id` becomes the incoming id (none for an incoming delete),
  and the `conflict` row keeps the old base, the incoming id and revision. A text conflict
  writes the markers into the file and keeps the local version in `.lode/conflicts/`
  (`mine_id`).
- `lode resolve --theirs` restores the incoming version (or deletes the file);
  `--mine` restores the kept local version (or keeps the file as it is); `--merged` keeps the
  file as it is. A file kept against an incoming delete becomes an add; a local delete kept
  against an incoming change stays a delete. Resolving removes `.theirs-r<N>` and the kept
  copy. `revert` of a conflicted file takes theirs.
- In a full-manifest sync, a tracked file absent from the manifest that has local changes is
  untracked and left on disk (`kept`).

### 10.5 Merge

Compute the merge base (SPEC 5.4) from `rev_merge` data returned by the server
(`merge-info`), fetch the manifest diff `base → source` (and `source → base` for the base side
of each changed path), and apply it to the workspace with the three-way rules `unshelve` uses:
rows keep the workspace's base, so the result is ordinary local changes. Record the range in
workspace config (`pending_merge`); the next commit sends it as `merged_from`, a shelve moves
it into the shelf and `unshelve` brings it back. `-r N` uses the diff `parent(N) → N` and
records `N..N`. Only ranges whose source is the merged branch count toward the base; content
that reached the source through other merges is merged again, which three-way merging mostly
absorbs.

The client checks the full source delta before applying any file. A change outside the workspace view
refuses the merge or single-revision pick with `conflict` (reason `outside_view`).

Before applying, `lode merge` reads `flow` from `merge-info` and stops when the stream rules
refuse the merge (§5.4), unless `--force-flow`, which it keeps in `pending_force_flow` for the
commit to send as `extensions.force_flow`.

**Renames (0.4).** A rename is an add with `copy_from` whose source the same revision deletes.
`lode merge` lists the source range's revisions with their changes and composes the renames
(`a→b` then `b→c` is `a→c`; a renamed file deleted later drops out). In the manifest diff a
rename is a delete of the old path and an add of the new one; the add gets `copy_from = old @
synced revision`, so committing the merge records a rename. Renames on this side are those
committed on the target after the branch point plus the workspace's uncommitted ones (an
added row copied from a path whose row is deleted), kept only when the new name is in the
workspace. Then, per file (shared with `unshelve`, whose change sets carry `copy_from`):

| There | Here | Result |
| --- | --- | --- |
| renamed `a→b` (maybe edited) | `a` unchanged | rename applied: `a` deleted, `b` added as a copy |
| renamed `a→b` (maybe edited) | `a` edited | the local file moves to `b`, then a three-way merge with `a`'s base as ancestor |
| edited `a` | renamed `a→b` | the change lands on `b` |
| renamed `a→b` | renamed `a→b` | as an ordinary change of `b`, `a`'s base as ancestor |
| renamed `a→c` | renamed `a→b` | edits merged into `b`; `c` is a conflict with no local file (`rename_conflict`): `resolve --mine` drops it, `--theirs` keeps it too |

Each file in the JSON result names the path the change was made to in `from` when that
differs. Renames inferred from content alone (no `copy_from`) are not followed.

**JSON (0.4).** Every three-way text merge (update, switch, merge, unshelve) goes through
`lode_ops::structured::merge3`: by lines first; when that conflicts and the path matches
`[merge].json` (default `**/*.json`, `**/*.uproject`, `**/*.uplugin`), the three sides are
parsed and objects merged key by key (a key changed on one side takes that side; the same
change on both is fine; anything else, arrays included, is a conflict). The result keeps
mine's key order, puts keys new on the other side after their neighbour there, and is
written with mine's indentation, line endings and final newline. Any side that does not
parse, or a structural conflict, leaves the line merge's markers.

**Merge tools (0.4).** `.lodeconfig` `[merge.tools]` maps patterns to tool names (the longest
matching pattern wins); `settings.toml` `[merge_tools.NAME]` holds `command` (argv with
`{base}`, `{mine}`, `{theirs}`, `{merged}`) and `trust_exit_code`. Only `lode resolve --tool`
runs one, on copies under `.lode/tmp/merge/`, then resolves the file as `--merged` with the
tool's result.

### 10.6 Large files (T151)

When the server reports `chunking` in `/info` with the parameters this build uses, a file of
at least 16 MiB travels in chunks (§3.5); otherwise, and against older servers, it travels
whole. `LODE_TEST_NO_CHUNKING` forces whole transfers for tests.

- **Upload** (commit, shelve, import): the file is cut and hashed in one read; a file that no
  longer hashes to the id being committed stops the commit as changed. `objects/missing` is
  asked for the chunk ids, the missing chunks are sent as blobs (four at a time, each read
  from its offset in the file), then the list. If a GC removed a chunk in between, the list is
  refused with `missing_objects` and the round runs once more. A rerun after a crash needs no
  state: the server keeps what was sent.
- **Download** (checkout, update, switch, view changes): the client asks for the list
  (`not_chunked` means the file is stored whole and is downloaded whole). Chunks come, in
  order of preference, from `.lode/tmp/chunks/` (left by an interrupted run), from the files
  now at the paths being replaced (cut with the same chunker; whatever matches a chunk id is
  copied, whether or not the file was modified), from the shared cache, and only then from the
  server. The file is put together in `.lode/tmp`, checked against its id and renamed into
  place; its chunks are then removed from `.lode/tmp/chunks/`.
- **Shared cache:** for large files it holds chunks, not whole files, so several versions of
  a big asset share most of their space.
- Results report `bytes_uploaded`/`bytes_downloaded` and `bytes_reused` (CLI-JSON).

### 10.7 Caching proxy (T154)

`loded proxy --config proxy.toml` (crate `lode-proxy`) serves file content to a remote office
from a disk cache and fetches each object from the main server ("upstream") once.

- **Routes.** `GET /api/v1/depots/{d}/blobs/{id}` (whole, or one `Range`) and `…/chunks`,
  answered as the server would (raw bytes, never zstd). `GET /api/v1/info` is the upstream's
  plus `proxy: {upstream, version}`. `/admin/` and `/admin/metrics.json` show counters.
  Anything else is `not_found` with `details.reason = "caching_proxy"`.
- **Access.** No tokens are stored. Per (SHA-256 of the bearer token, depot) the proxy calls
  the upstream's `GET /depots/{d}/obliterated-ids?after=` with the caller's token; success
  means the caller may read the depot, a 4xx is passed on as it came. The answer, allowed or
  refused, is kept for 60 s (injected clock). The same call refreshes the depot's obliterated
  ids when they are older than 60 s: new ids are refused with `obliterated` and their cached
  copies deleted. `after` is the previous answer's `now_ms` less five minutes, so an
  obliteration committed just before that answer is not missed. Refreshes happen on demand;
  a depot nobody asks for keeps its copies until someone does.
- **Fetching.** On a miss the proxy asks for the blob; a raw answer of 16 MiB or more is
  dropped unread and the chunk list asked for instead, so chunked files are cached chunk by
  chunk and never whole. Chunks are fetched as a response reaches them. Each object is checked
  against its BLAKE3 id before it is kept (`Client::fetch_content`, `download_blob_max`).
  Requests for the same object wait on one per-object lock and then find it cached. A chunk
  list cannot be checked on its own; when a whole chunked file is served, it is hashed on the
  way and a mismatch drops the list and breaks the response off.
- **Cache.** `<cache_dir>/objects/<depot as hex>/<ab>/<id>` (`.chunks` for lists), written
  under `tmp/` and renamed in; `tmp/` is cleared at start and the index rebuilt from the files
  in modification-time order (touched on each use). Entries are checked by hash on every read
  and dropped if damaged. After each fill, the least recently used entries are removed until
  the cache fits `cache_max_gib`.
- **Client.** `ClientBuilder::proxy` sends blob and chunk-list downloads to the proxy and
  everything else to the server. A transport error, a 5xx, or content that fails its hash
  marks the proxy failed for that client (shared by its clones): one warning, then the
  server. The proxy comes from `LODE_PROXY`, else `[proxies]` in `settings.toml` keyed by
  server URL (`lode proxy set|clear|show`).

## 11. Maintenance

- **GC** (`lode admin gc`): mark from every revision's root tree (walking tree objects, memoized
  by id) plus module `gc_roots`; sweep objects that are unmarked **and** older than a grace
  period (`[gc] grace_seconds`, default 24 h, protects uploads whose commit has not started).
  Runs online: deletion happens in batches under an exclusive fence that every commit holds
  shared from its blob checks to its transaction, and each batch first marks the revisions
  committed since the previous mark, so a finished commit never loses an object.
  `POST /admin/depots/{d}/gc[?dry_run=true]` returns objects checked, objects and bytes removed.
- **Chunked files** (T150): the mark adds the chunks of every reachable chunk list (a file
  entry of at least 16 MiB is looked up once), and of a module root that is one. fsck checks
  that each chunk of a referenced list is stored with its size; `--full` re-hashes the
  assembled file. Obliterating a chunked file also obliterates (records and deletes) the
  chunks no other reachable file uses, found with a mark that skips the selected files, so
  the content cannot be put together again; chunks shared with other files stay.
- **fsck** (`lode admin fsck`): every revision's tree is reachable and decodes; every referenced
  object exists; optional `--full` re-hashes every object; branch heads and `next_rev` are
  consistent. `POST /admin/depots/{d}/fsck[?full=true]` returns every problem with its
  revision, path and object id; `loded fsck --config <file> [--full] [depot…]` does the same
  offline and exits with 1 when anything is wrong.
- **Backup:** SQLite online backup of `meta.db` first, then copy `depots/` (objects are
  immutable; any object referenced by the copied database is already on disk).
  `loded backup --config <file> <dest>` runs beside a live server; `dest` must be missing or
  empty and becomes a data directory. Its `loded.toml` contains the settings loaded from
  `--config`, with `data_dir = "."` for restore. Relative references to external certificate
  and credential files resolve beside the restored configuration. With S3 storage only the
  database and `loded.toml` are copied (`content_in_bucket` in the report); the bucket's
  versioning or replication protects the content.
- **Obliterate** (T120, SPEC §5.8): `POST /admin/depots/{d}/obliterate` with
  `{targets: [{path, rev?}], reason, dry_run}`, server admins only. A target without `rev`
  selects every `new_id` that `rev_change` recorded for the path or under it; with `rev`, the
  entries of that revision's tree. The empty blob and ids already obliterated are dropped.
  The answer lists each object with every `(rev, branch, path)` that stored it (a scan of the
  depot's `rev_change`) and, on a dry run, `in_use`: entries of active branch heads holding a
  selected id, found by walking the heads (trees free of the ids are remembered, so the second
  walk only reads new trees). A real run refuses `in_use` with `conflict`, then takes
  `gc_running` and the exclusive `gc_fence`, walks the heads again, writes the `obliterated`
  rows, drops the blame cache and deletes the objects. Trees keep naming the ids, so no tree
  or revision changes. The server keeps each depot's obliterated ids in memory: upload,
  download, file reads, blame, MCP and shelf diffs answer `obliterated` (410) for them, and a
  commit naming one is refused with its paths. A blame walk stops at an obliterated older
  version (truncated). Clients leave such files out of a sync (`obliterated` in the result).
  GC deletes any obliterated object still stored, whatever the mark says; fsck accepts a
  referenced obliterated object as missing and reports one that is still stored.
  `GET /admin/depots/{d}/obliterated` lists the records.
- **Scheduled backups** (T082): with `[backup] every`, `loded serve` runs the same backup on
  a schedule into `[backup] dir`, as `lode-backup-<UTC time>` (written as `.partial` and
  renamed when complete). The newest complete backup's name says when the next is due, so
  restarts keep the schedule; complete backups beyond `keep` (7) are removed, oldest first.
  The scheduler checks at least hourly, by the server clock.
- **Copies elsewhere**: with `[backup.copy_to]` (an S3-compatible bucket, the same keys as
  `[storage]` plus `keep`), the scheduler sends the newest complete backup to the bucket
  after making it, and at each later check until one copy succeeds. Under the prefix:
  `objects/<depot-id>/<h0h1>/<h2h3>/<hex>` (stored objects, shared by all copies, uploaded
  with `If-None-Match: *` and only when missing, so each copy sends only new content), and
  per copy `<name>/meta.db`, `loded.toml`, `objects.zst` (the copy's `<depot-id>/<hex>`
  list, zstd) and `done.json` last. A copy without `done.json` is incomplete. After each
  copy, complete copies beyond `keep` go (default `[backup] keep`), then incomplete ones
  older than the newest complete copy, then every object no kept copy lists, which is how
  obliterated content and erased depots leave the bucket. `loded backup-copy` sends the
  newest backup now; `loded restore --config <file> [--name N] <dest>` downloads a copy
  into an empty directory as a data directory (`--list` names the copies). The overview
  shows the last copy's outcome. Files over 5 GiB (one S3 `PUT`) are refused.
- **Setup and services** (T082): `loded setup` writes a checked `loded.toml` from the answers
  and initializes the data directory with an admin password. `loded service install` writes a
  systemd unit on Linux, or registers a native Windows service whose process is
  `loded serve --config <file> --service` (stop and shutdown requests stop the server
  gracefully; the log goes to `<data_dir>/logs/loded.log`).

## 12. Testing strategy

- Unit tests beside the code. Property tests (`proptest`) for tree encoding round-trips and for
  "apply changes to tree == rebuild tree from scratch".
- Integration tests through `lode-testkit`: real server on a temp directory, real CLI workspaces.
- Concurrency test: N clients committing overlapping and disjoint changes, locking and
  shelving; history must be a valid serial order, no update lost, locks exclusive.
- Crash tests: abort the server at injected points in the commit path and in GC; `fsck` must
  pass after restart (`TestServer::restart`). The points are named failpoints on
  `AppState::failpoints()` (`commit.before_trees`, `commit.in_transaction`,
  `commit.after_transaction`, `gc.after_mark`, `gc.in_batch`); nothing arms them outside
  tests. The client has named crash points too (`LODE_TEST_CRASH_AT=<point>` exits with code
  86), and a test kills `checkout`, `update` and `commit` after random delays.
- Soak and fuzz: `lode-cli/tests/soak.rs` (bounded by `LODE_SOAK_SECS`) and property tests on
  every decoder that sees outside bytes. [HARDENING.md](HARDENING.md) lists them and their
  long forms.
- `TestWorkspace` runs `lode` with `LODE_CONFIG_DIR` and the home directories pointed at a
  temporary directory, so tests never read or write the developer's credentials.
- CI on Linux, Windows and macOS: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`.
