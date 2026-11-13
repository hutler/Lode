# Lode — Product Specification

Status: draft v0.1 · Licence: Apache-2.0 · Language: Rust

"Lode" is a working name. The CLI binary is `lode`, the server binary is `loded`.

## 1. Purpose

Lode is a centralized, open-source version control system for small teams (up to about 100
users) that work with a mix of source code and large binary assets.

It takes Subversion's simple model as the base, adds the few Perforce features such teams miss
(exclusive locks, shelves, changelists), and treats AI agents as first-class users through MCP.

## 2. Goals

1. **Simple mental model.** One server, linear history per branch, one global revision number.
2. **Good with binaries.** Content-addressed storage, deduplication, no local pristine copies,
   exclusive locks for unmergeable files.
3. **Small core, optional modules.** The core stores data and enforces rules. Workflow features
   are modules built only on public extension points.
4. **Agents are users.** Agents have their own identity, scoped permissions and recorded
   provenance. By default their work lands in shelves that a human submits.
5. **Easy to run.** One server binary, one data directory, SQLite metadata, no external services.
6. **Hard to corrupt.** Every object is hash-verified on write. Commits are atomic.

## 3. Non-goals (v1)

- Offline or distributed commits. A commit needs the server.
- Perforce-style per-file integration history and complex client view mappings.
- Built-in code review UI, issue tracking, or server-side scripting.
- Replication, edge servers, clustering.
- Keyword expansion, line-ending conversion, versioned empty directories.
- A dynamic plugin ABI (loadable libraries, WASM).
- Git or SVN wire compatibility. Import tools may come later as modules.

## 4. Concepts

| Term | Meaning |
| --- | --- |
| **Depot** | One repository on a server. A server hosts one or more depots. |
| **Revision** | An atomic commit. Numbered `r1, r2, …` per depot, across all branches. Each revision belongs to exactly one branch and holds a full tree snapshot. |
| **Branch** | A named line of revisions. Creating a branch is itself a revision whose parent is the source revision. |
| **Workspace** | A local checkout of one branch, possibly partial (see View). |
| **View** | Include/exclude path patterns that select which part of the depot a workspace contains. |
| **Blob** | Immutable file content, addressed by the BLAKE3 hash of its bytes. |
| **Principal** | An identity: a **user** (human) or an **agent** (non-human, owned by a user). |
| **Lock** | An exclusive claim on a path in a branch. Only the holder may commit changes to it. |
| **Shelf** | A set of uncommitted changes stored on the server, visible to others. |
| **Changelist** | A named local grouping of pending changes inside a workspace. |
| **Module** | An optional feature compiled into the server and/or CLI and switched on by configuration. |

Only files and symlinks are versioned. Directories exist implicitly while they contain files.

## 5. Core features

### 5.1 Commits

- A commit is a set of file changes (add, modify, delete) applied to a branch head, atomically.
- Concurrency is optimistic and per file: a commit is rejected as `out_of_date` only if a file
  it touches changed on the branch since the workspace last synced that file. Unrelated commits
  by others do not block it.
- Each commit records: author, submitter, time, message, client name/version, provenance
  (section 7.3), per-file copy/rename origin, and merge origin (section 5.4).
- Commits are idempotent: a retried request with the same request id returns the same revision.

### 5.2 Workspaces

- Change detection is by scan (size and mtime, re-hash on mismatch). There is no mandatory
  "open for edit" step.
- New files must be added explicitly (`lode add`, or `lode commit -A`).
- Renames and copies are explicit (`lode mv`, `lode cp`) so that history follows the file.
- `lode revert <paths>` discards changes to the named paths; `lode revert --all` discards every
  pending change, abandons a pending merge and clears conflicts. Reverting an added file leaves
  it on disk, untracked. `lode clean` removes untracked files (ignored files only with
  `--ignored`). `revert --all` followed by `clean` returns a workspace to a known state.
- No pristine copies are kept. Base content needed for diff or merge is fetched from the server
  and held in a size-capped local cache.
- `lode update` brings the workspace to a branch head. Text files changed on both sides are
  merged three-way. Binary files changed on both sides become conflicts.
- Views make partial checkouts possible. A view can be changed after checkout.

### 5.3 Branches

- `lode branch create <name> [--from <branch>[@rev]]` creates a branch on the server. It is cheap:
  the new branch shares the source tree.
- `lode switch <branch>` moves a workspace to another branch.
- Branches can be archived (hidden, read-only), never deleted.

### 5.4 Merge (basic)

- `lode merge <source-branch>` merges all not-yet-merged source revisions into the workspace.
  `lode merge <source-branch> -r <rev>` picks one revision.
- The resulting commit records `merged_from`: source branch plus revision range. This is stored
  from the first version so that later, smarter merge tooling has complete history to work with.
- v1 merge base rule: the highest source revision `R` such that every source revision up to `R`
  is recorded as merged into the target (or the branch point if none).

### 5.5 Identity and permissions

- Principals authenticate with bearer tokens. Humans obtain one with `lode login`
  (name + password in v1; single sign-on is a later module).
- Tokens carry scopes. The core defines `read`, `write` and `admin`; modules add their own
  (`lock`, `shelve`, `checks`). A scope belonging to a module that is switched off has no effect.
- Access rules grant `read`, `write` or `admin` on `depot / branch pattern / path pattern` to a
  user, group, or everyone. Default is no access. Rules are `allow` or `deny`. A matching `deny`
  at level L refuses L and every higher level, whatever other rules say (`deny write` still
  permits `read`). Otherwise the highest matching `allow` applies.
- Known limit in v1: blob download is authorized per depot, not per path. A principal with any
  read access who knows a blob's hash can fetch it.

### 5.6 Versioned configuration

A branch may contain two special files at its root:

- `.lodeignore` — ignore patterns for the workspace scanner (gitignore syntax).
- `.lodeconfig` — TOML read by server and client from the branch head:

```toml
[locks]
required = ["**/*.psd", "Content/**/*.uasset"]   # files that need a lock to be changed

[files]
binary = ["**/*.dat"]                             # force binary handling
text   = ["**/*.shader"]                          # force text handling
```

With the checks module on, it may also name checks that must pass before a shelf is submitted
to certain branches (section 6.6):

```toml
[checks]
branches = ["main", "release/*"]
required = [{ name = "ci/build", reporter = "ci" }]
```

A commit that would leave `.lodeconfig` unparseable is refused. Rules are always taken from
the branch head, never from the change being committed.

### 5.7 Path rules

Paths are UTF-8, NFC-normalized, `/`-separated and relative to the depot root. Rejected:
empty segments, `.` and `..`, control characters, backslash, the characters Windows forbids in
file names (`< > : " | ? *`), segments ending in a dot or space, Windows reserved device names
(including `COM¹`–`COM³` and `LPT¹`–`LPT³`), and any path that would collide with another path
in the same directory when compared case-insensitively. Every path in a depot can therefore be
checked out on Windows, macOS and Linux.

## 6. Default modules

### 6.1 Locks

- `lode lock <path>…` takes exclusive locks; all-or-nothing. A lock is per branch and path.
- A lock can only be taken on a file the workspace has at the branch head version.
- A commit touching a path locked by someone else is rejected (`locked`).
- A commit touching a path matching `[locks].required` without holding its lock is rejected
  (`lock_required`). The client keeps such files read-only on disk until locked.
- Locks are released on commit unless `--keep-locks` is given. `lode unlock` releases manually.
  Admins can break locks (`lode unlock --force`).
- `lode locks` lists locks with owner, time and comment.

### 6.2 Shelves

- `lode shelve [-c <changelist>] -m <description>` uploads pending changes as a shelf and,
  unless `--keep` is given, reverts them locally.
- `lode shelves`, `lode shelf show <id>`, `lode shelf diff <id>` inspect shelves of any user
  (subject to read permission).
- `lode unshelve <id>` applies a shelf to the workspace, merging like an update.
- `lode shelf submit <id>` commits a shelf directly on the server. Author is the shelf owner,
  submitter is the caller. This is the approval path for agent work.
- Every change to a shelf raises its **version**. A submit names the version that was reviewed
  and is refused if the shelf has changed since, so nobody can approve one thing and commit
  another.
- A shelf made during a pending merge carries the merge origin, and submitting it records
  `merged_from` exactly as a direct commit would.
- `lode shelf delete <id>`.

### 6.3 Changelists

- Client-side only. `lode changelist new|list|move|delete`.
- `lode commit -c <name>` and `lode shelve -c <name>` operate on one changelist.
- Files not assigned belong to the `default` changelist.

### 6.4 MCP

Two MCP servers, both speaking the standard Model Context Protocol:

- **Server endpoint** (`/mcp`, streamable HTTP, bearer token). Works without a workspace. Tools:
  `whoami`, `list_depots`, `list_branches`, `log`, `show_revision`, `list_dir`, `read_file`,
  `diff`, `blame`, `search_history`, `list_locks`, `list_shelves`, `get_shelf`, `create_shelf`,
  `update_shelf`, and `submit_shelf` for tokens with `write`.
- **Local endpoint** (`lode mcp`, stdio). Operates on a workspace. Tools: `status`, `diff`,
  `add`, `remove`, `move`, `revert`, `update`, `lock`, `unlock`, `shelve`, `unshelve`, `commit`.

Every tool returns structured JSON. Tools that write are only listed when the token allows them.
Each module supplies its own tools (the lock tools come from the locks module, the shelf tools
from the shelves module); the MCP module hosts them and adds the tools that read core data.

### 6.5 Webhooks (off by default)

- A depot administrator registers a URL per depot, with the events it wants and an optional
  branch pattern: `lode admin webhook add|list|remove|deliveries|ping`. The URL must be
  `https://` unless the server allows plain HTTP (`[webhooks] allow_http`).
- Lode sends a signed `POST` for each matching event: new revision, branch created or
  archived, shelf created, updated, submitted or deleted, lock taken or released, check updated.
- Events carry identifiers only, never file content or paths. The receiver fetches details
  with its own token, so a webhook cannot reveal more than its receiver may read.
- Delivery is retried for up to 24 hours and may repeat. Receivers treat an event as a hint
  to read the revision list from where they left off.

### 6.6 Checks (off by default)

- A check is a named result (`pending`, `success`, `failure`, `error`, `cancelled`, with a
  summary and a link) that an outside system such as a build server attaches to a revision or
  to one version of a shelf.
- Reporting needs the `checks` scope. It grants nothing else, so a build server can report
  results without being able to commit.
- `lode checks r214` and `lode checks 37@3` list the checks on a revision or a shelf version;
  the MCP tool `list_checks` does the same for agents.
- **Submit gate.** `.lodeconfig` may list checks that must be `success`, each reported by a
  named principal, before a shelf can be submitted to matching branches. A submit that does
  not meet them is refused (`checks_required`). Because checks belong to a shelf version,
  changing the shelf means it must pass again.
- Direct commits are not gated. Lode stores and enforces results; it does not run builds and
  has no review workflow.

## 7. AI support

### 7.1 Agent identities

- An agent is a principal of kind `agent`, owned by a user or an admin, with its own tokens.
- Default agent scopes are `read`, `shelve`, `lock`. `write` (direct commit) must be granted
  explicitly.
- Access rules apply to agents exactly as to users. An agent never inherits its owner's access.

### 7.2 Workspace-free operation

An agent can do a full read-modify-propose cycle with only the server MCP endpoint: read files at
a revision, then call `create_shelf` with a base revision and a list of edits. No checkout is
needed, so many agents can work in parallel at low cost.

### 7.3 Provenance

Every revision and shelf stores a provenance record:

```json
{ "client": "lode-cli/0.1.0", "via": "api",
  "agent": { "model": "…", "session": "…", "tool": "…" } }
```

What can be trusted: `via` is set by the server and says which server endpoint was used —
`mcp` for the server MCP endpoint, `api` for everything else, including the CLI and the local
`lode mcp` server. `client` and everything under `agent` are reported by the client and stored
as given; show them, but do not base decisions on them.

A revision made by `shelf submit` takes `via` from the submit request (usually `api`), keeps
the shelf's `client` and `agent`, and adds `submitted_shelf` with the shelf's id.

`author` and `submitter` come from the authenticated token and are always reliable.
`author` is who made the change, `submitter` is who committed it. They differ when a human
submits an agent's shelf. `lode log` shows both.

### 7.4 Machine-readable CLI

Every CLI command accepts `--json` and then prints exactly one JSON document to stdout. Errors
are JSON on stdout with a stable `code`; the process exit code is non-zero.

## 8. Optional modules (after v1)

Caching proxy · S3 blob backend · chunked storage for very large files · streams
(branch hierarchy with merge direction) · smarter merge using `merged_from` · single sign-on
(OIDC) · Postgres metadata backend · obliterate · importers (SVN, Git).

## 9. CLI summary

```
lode login <server-url>
lode checkout <server-url>/<depot> [dir] [--branch B] [--include P]… [--exclude P]…
lode status | add | rm | mv | cp | diff
lode revert <paths…> | revert --all        lode clean [--ignored] [--dry-run]
lode commit -m MSG [-A] [-c CHANGELIST] [--keep-locks] [paths…]
lode update [-r REV]
lode resolve <path> --mine|--theirs|--merged
lode log [path] | cat <path>[@rev] | ls [path][@rev] | blame <path>
lode branch list|create|archive      lode switch <branch> [-r REV]
lode merge <branch> [-r REV]
lode lock|unlock <path>…             lode locks
lode shelve | unshelve <id>          lode shelves | shelf show|diff|delete <id>
lode shelf submit <id>[@version]
lode changelist new|list|move|delete
lode view show|include|exclude
lode checks <rN | shelf-id[@version]>          (planned, with the checks module)
lode mcp
lode admin depot|user|agent|group|token|acl|gc|fsck …
lode admin webhook add|list|remove|deliveries|ping …   (with the webhooks module)
```

Every command honours the environment variables `LODE_SERVER` and `LODE_TOKEN_FILE` (a path
to a file holding a token) for sign-in without a stored login, and `LODE_CACHE_DIR` for a
download cache shared by several workspaces on one machine. `lode --version --json` reports
the client version in machine-readable form.

Exit codes: `0` success, `1` operation failed (conflict, rejected, not found), `2` usage error,
`3` authentication or permission error, `4` network or server error.

## 10. Quality requirements

- A killed server or client at any point must not leave a depot or workspace inconsistent;
  rerunning the command must complete or cleanly report.
- A commit of 10,000 small files completes in under 30 s on a local network.
- `lode status` on a 200,000-file workspace with no changes completes in under 5 s warm.
- Server memory use is independent of file size (all content is streamed).
- Supported platforms: Linux, Windows, macOS, for both client and server.

## 10.1 Deviations in 0.1

What the first build does differently from this specification, found in the T056 review.
Everything else in sections 5–7 and 9 is covered by the scenario suite
(`crates/lode-cli/tests/scenarios.rs`, one scenario per section) and the feature tests.

| Section | Deviation |
| --- | --- |
| 6.6 Checks | Reports, listing over HTTP and the submit gate work; `lode checks` and the MCP tool `list_checks` are not built yet (T063). |
| 5.4 Merge | After a single-revision pick (`-r`), a later full merge starts again from the merge base before the gap and re-applies the picked change. Usually `unchanged`; a conflict if the file changed since. Follows from the v1 base rule. |
| 6.1 Locks | `lode switch` leaves this workspace's locks on the old branch (they are per branch) and reports them; it does not release them. |
| 6.4 MCP | The server endpoint also offers `list_depots`, and `submit_shelf` to tokens with `write`. Large files, diffs and blame are cut off at fixed sizes (`truncated`, `next_line`). |
| 7.3 Provenance | A submitted shelf's revision has `via` of the submit request and an extra `submitted_shelf` field (see 7.3). |
| 10 Quality | `loded` times out stalled request bodies but not slow request headers. A server reachable from the internet should sit behind a reverse proxy; see `docs/HARDENING.md`. |
| 10 Quality | The performance targets were measured on Linux only (`docs/BENCHMARKS.md`); Windows and macOS run the same tests in CI but not the benchmarks. |

## 11. Milestones

| # | Outcome |
| --- | --- |
| M0 | Foundations: workspace, core types, blob store, metadata store |
| M1 | Server core: auth, permissions, commit engine, read API, modules framework |
| M2 | Client: checkout, status, commit, update, log, diff |
| M3 | Locks, shelves, changelists |
| M4 | Agents, provenance, MCP (server and local) |
| M5 | Branch switching, merge, blame, admin tooling, hardening, packaging |
| M6 | Integration: webhooks, checks and submit gate, build-agent client features |

Task breakdown: [tasks/README.md](tasks/README.md). Design: [ARCHITECTURE.md](ARCHITECTURE.md).
