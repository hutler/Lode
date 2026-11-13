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
2. The CLI uses the same public API as any third party.
3. Everything that touches content streams it. No code path buffers a whole file in memory
   (tree objects and small text merges excepted).
4. The server never trusts a client-supplied hash. It always recomputes.

## 2. Crates

One Cargo workspace. Library crates never depend on binaries; arrows point downward only.

| Crate | Kind | Contents |
| --- | --- | --- |
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
| `lode-client` | lib | Typed HTTP client, blob transfer, retry |
| `lode-workspace` | lib | Workspace database, scanner, sync, merge, changelists, client side of locks and shelves |
| `lode-cli` | bin `lode` | Argument parsing, output formatting (human and `--json`), local MCP server |
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
| 5 | 1 | kind: 1 blob, 2 tree, 3 chunk list (reserved, not implemented) |
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

## 4. Metadata

### 4.1 Store

SQLite in WAL mode, one file `<data>/meta.db`, accessed with `rusqlite`: one dedicated writer
connection behind a mutex, a pool of read-only connections, all calls via `spawn_blocking`.
All writers are serialized, which is what makes commits atomic and is ample for 100 users
because blobs are uploaded before the write transaction begins.

`MetaStore` is a concrete type with a narrow method surface, written so a Postgres
implementation can be added later behind a trait without touching callers.

Migrations are numbered SQL files embedded in the binary; `schema_version` table; the server
refuses to start on a newer schema than it knows. Modules own tables prefixed `mod_<name>_`
and ship their own migrations.

### 4.2 Core schema

```sql
depot      (id INTEGER PK, name TEXT UNIQUE, next_rev INTEGER NOT NULL, created_at)
principal  (id INTEGER PK, kind TEXT CHECK(kind IN ('user','agent')), name TEXT UNIQUE,
            display_name, owner_id REFERENCES principal, password_hash, disabled INT, created_at)
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
```

`rev_change` is the per-path history index (drives `log <path>`, `blame`, rename following).
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
   on it) and `merged_from` (branch must exist; range must lie in its history).
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

## 6. HTTP API

Base path `/api/v1`. JSON unless stated. Auth: `Authorization: Bearer <token>`.
Path parameters are single URL segments: a branch name that contains `/` is sent with each `/`
percent-encoded (`release%2F1.0`).

| Method and path | Purpose |
| --- | --- |
| `GET /info` | Server version, API version, enabled modules |
| `POST /auth/login` · `GET /auth/whoami` | Password login → token; identity, scopes |
| `GET/POST/DELETE /tokens[/{id}]` | Own tokens |
| `GET/POST /depots` | List; create (admin) |
| `GET/POST /depots/{d}/branches` · `GET/PATCH …/{b}` | List, create, show, archive |
| `POST /depots/{d}/objects/missing` | `{ids}` → `{missing}`; at most 10,000 ids per call. Needs scope `write` or `shelve`, like upload, because the answer reveals whether content exists |
| `PUT /depots/{d}/blobs/{id}` | Upload. Body is the raw content, optionally `Content-Encoding: zstd` |
| `GET /depots/{d}/blobs/{id}` | Download; honours `Accept-Encoding: zstd`; supports `Range` on raw bytes only when stored uncompressed, otherwise full body |
| `POST /depots/{d}/branches/{b}/commits` | Commit (section 5) |
| `GET /depots/{d}/revisions?branch=&path=&before=&after=&own=&changes=&author=&author_kind=&limit=` | Revision list, paged. `author=<name>` and `author_kind=user|agent` filter by author. Default order is newest first, paging with `before`. With `after=N` the order is ascending from `N+1`, for polling. Without `branch` the list covers the whole depot; every item names its branch. With `branch`, inherited history (§4.3) is included unless `own=true`. `changes=true` adds each revision's changed paths, capped per revision (`[limits] max_changes_listed`, default 1,000) with a `changes_truncated` flag. With `path`, `follow=true` continues through `copy_from` into the source path's history |
| `GET /depots/{d}/revisions/{n}` | One revision with its changes |
| `GET /depots/{d}/manifest?rev=N[&base=M][&include=…][&exclude=…]` | NDJSON stream. Without `base`: every file at `N`. With `base`: only differences `M → N`, each line `{"op":"put"|"del", …}` |
| `GET /depots/{d}/tree?rev=&path=` | List one directory |
| `GET /depots/{d}/files?rev=&path=` | File content at a revision, with headers `X-Lode-Id`, `X-Lode-Kind` (`file` or `symlink`) and `X-Lode-Executable` (`true` or `false`). A missing, unreadable or directory path is `not_found` |
| `GET /depots/{d}/blame?rev=&path=` | Per-line last-change revision (M5) |
| `…/admin/principals[/{name}]` (`GET`/`POST`; `PATCH` `{disabled?, password?}`), `…/admin/principals/{name}/tokens[/{id}]` (`GET`/`POST`/`DELETE`, tokens of another principal; an agent's owner may use these for the agent), `/groups` (`GET`/`POST`), `/groups/{g}/members/{name}` (`PUT`/`DELETE`), `/acl`, `/depots/{d}/gc`, `/depots/{d}/fsck` | Administration. Needs scope `admin` and server-admin standing |
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
`branch_archived` 409 · `hash_mismatch` 422 · `too_large` 413 · `internal` 500.
Modules may define further codes for their own rejections; the checks module adds
`checks_required` 409.

Limits (configurable): request JSON 64 MiB, single blob 64 GiB, changes per commit 500,000.

## 7. Security

- **Transport.** `loded` terminates TLS itself with rustls (cert and key in config) or listens
  on plain HTTP bound to localhost behind a reverse proxy. The client refuses plain HTTP to
  non-loopback hosts unless `--insecure` is given.
- **Passwords** are hashed with Argon2id. **Tokens** are 256 bits of randomness, shown once,
  stored as SHA-256. Login is rate-limited per name and per address.
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
```

```rust
pub trait Module: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn scopes(&self) -> &'static [ScopeDef] { &[] }                      // { name, agent_default }
    fn migrations(&self) -> &'static [Migration] { &[] }
    fn routes(&self, ctx: ModuleCtx) -> Option<axum::Router> { None }   // mounted by the server
    fn pre_commit(&self, tx: &dyn Reads, c: &CommitCtx) -> Result<(), ApiError> { Ok(()) } // read-only
    fn in_commit(&self, tx: &WriteTx, c: &CommittedCtx) -> Result<(), ApiError> { Ok(()) } // error rolls back
    fn on_event<'a>(&'a self, ev: &'a Event) -> BoxFuture<'a, Result<(), BoxError>> { … } // after commit, at-least-once
    fn gc_roots(&self, tx: &dyn Reads, depot: DepotId, out: &mut dyn FnMut(ObjectId)) -> Result<(), MetaError> { Ok(()) }
    fn mcp_tools(&self, ctx: ModuleCtx) -> Vec<ToolDef> { vec![] }       // tools this module offers to agents
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
- `on_event` is fed by a dispatcher that reads `outbox` after each commit and on startup,
  advancing a per-subscriber cursor only after success. After each pass it deletes outbox rows
  that every registered subscriber has passed; cursors of subscribers that are no longer
  registered (a disabled module) are ignored and removed. A subscriber must not hold the
  cursor back for slow external work: it records what it needs in its own tables and returns.
- Third-party extension without recompiling: the HTTP API, and CLI subcommands found on `PATH`
  as `lode-<name>` (the CLI passes server URL and token through environment variables).

### 8.1 Events

Every outbox event has the envelope `{ id, event, depot, created_at, data }`. `data` carries
identifiers only — never file content and never paths, so an event cannot leak what its
receiver may not read. Receivers fetch details through the API with their own permissions.

| Event | Written by | `data` |
| --- | --- | --- |
| `revision.created` | core | `rev, branch, author, author_kind, submitter, origin` |
| `branch.created` | core | `branch, rev, from_branch, from_rev` |
| `branch.archived` | core | `branch` |
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

### 9.4 Webhooks (`lode-mod-webhooks`)

Tells outside systems that something happened, so they need not poll. Built only on
`on_event` and its own tables.

```sql
mod_webhooks_hook     (id INTEGER PK, depot_id, url TEXT, secret BLOB, events TEXT /*json list*/,
                       branch_glob TEXT NULL, active INT, created_by, created_at)
mod_webhooks_delivery (id INTEGER PK, hook_id, outbox_id, event TEXT, body TEXT,
                       state TEXT /*pending|ok|failed*/, attempts INT, next_attempt_at,
                       last_status INT NULL, last_error TEXT NULL, created_at,
                       UNIQUE (hook_id, outbox_id))
```

- Admin-only API: `GET/POST /depots/{d}/webhooks`, `DELETE …/{id}`,
  `GET …/{id}/deliveries` (recent attempts), `POST …/{id}/ping` (sends a `ping` event).
  The secret is generated by the server, at least 32 random bytes, and shown once.
- `on_event` only inserts one `pending` delivery row per active hook whose event list and
  branch pattern match, then returns. It never does network I/O, so one dead receiver cannot
  hold back the outbox cursor or other hooks.
- A worker sends each delivery: `POST` with the event envelope (§8.1) as JSON and headers
  `X-Lode-Event`, `X-Lode-Delivery` (the delivery id, stable across retries, for
  de-duplication) and `X-Lode-Signature: sha256=<hex HMAC-SHA256 of the body with the secret>`.
  Any 2xx within 10 s is success. Otherwise retry with exponential backoff for up to 24 h,
  then mark `failed`. Redirects are not followed. Deliveries to one hook are sent one at a
  time, oldest first; different hooks proceed independently.
- URLs must be `https://` unless `[webhooks] allow_http = true` in `loded.toml`.
- Delivery rows are kept for 7 days, then deleted.
- Delivery is at-least-once and, under retry, not strictly ordered. Receivers should treat an
  event as a hint and read the revision list with `after=` from their own cursor.

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
- `GET /depots/{d}/checks?target_kind=&target_id=&target_version=` lists; needs scope `read`.
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
  commit is rejected `checks_required` with the missing names in `details`.
- The rule is read from the branch head (the configuration in `CommitCtx`), never from the
  shelf being submitted, so a shelf cannot remove the rule it must satisfy.
- Binding each name to a reporter is what makes the gate meaningful: holding the `checks`
  scope is not enough to pass someone else's required check.
- Direct commits are not gated. Making a branch accept shelf submits only is a separate
  feature, not in this version.

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
store downloaded blobs there. It is content-addressed, written with temp-plus-rename, verified
by hash on read, size-capped with LRU eviction, and safe for concurrent `lode` processes.
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
without moving the base, and a binary conflict puts it in `<file>.theirs-shelf`. Pending merge
state (`merged_from`) is not carried yet (T051).

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

Compute the merge base (SPEC 5.4) from `rev_merge` data returned by the server, fetch the
manifest diff `base → source`, and apply it to the workspace with the update algorithm. Record
the range in workspace config; the next commit sends it as `merged_from`.

## 11. Maintenance

- **GC** (`lode admin gc`): mark from every revision's root tree (walking tree objects, memoized
  by id) plus module `gc_roots`; sweep objects that are unmarked **and** older than a grace
  period (`[gc] grace_seconds`, default 24 h, protects uploads whose commit has not started).
  Runs online: deletion happens in batches under an exclusive fence that every commit holds
  shared from its blob checks to its transaction, and each batch first marks the revisions
  committed since the previous mark, so a finished commit never loses an object.
  `POST /admin/depots/{d}/gc[?dry_run=true]` returns objects checked, objects and bytes removed.
- **fsck** (`lode admin fsck`): every revision's tree is reachable and decodes; every referenced
  object exists; optional `--full` re-hashes every object; branch heads and `next_rev` are
  consistent. `POST /admin/depots/{d}/fsck[?full=true]` returns every problem with its
  revision, path and object id; `loded fsck --config <file> [--full] [depot…]` does the same
  offline and exits with 1 when anything is wrong.
- **Backup:** SQLite online backup of `meta.db` first, then copy `depots/` (objects are
  immutable; any object referenced by the copied database is already on disk).
  `loded backup --config <file> <dest>` runs beside a live server; `dest` must be missing or
  empty and becomes a data directory, including `loded.toml`.

## 12. Testing strategy

- Unit tests beside the code. Property tests (`proptest`) for tree encoding round-trips and for
  "apply changes to tree == rebuild tree from scratch".
- Integration tests through `lode-testkit`: real server on a temp directory, real CLI workspaces.
- Concurrency test: N clients committing overlapping and disjoint changes; history must be a
  valid serial order and no update lost.
- Crash tests: abort the server at injected points in the commit path; `fsck` must pass after
  restart. The points are named failpoints on `AppState::failpoints()`
  (`commit.before_trees`, `commit.in_transaction`, `commit.after_transaction`); nothing arms
  them outside tests.
- `TestWorkspace` runs `lode` with `LODE_CONFIG_DIR` and the home directories pointed at a
  temporary directory, so tests never read or write the developer's credentials.
- CI on Linux, Windows and macOS: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`.
