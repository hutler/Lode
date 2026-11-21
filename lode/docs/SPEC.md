# Lode — Product Specification

Status: draft v0.1 · Licence: Apache-2.0 · Language: Rust

"Lode" is a working name. The CLI binary is `lode`, the server binary is `loded`.

## 1. Purpose

Lode is a centralized, open-source version control system for small teams (up to about 100
users) that work with a mix of source code and large binary assets.

It takes Subversion's simple model as the base, adds the few features such teams miss
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
- Per-file integration history and complex client view mappings.
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

### 5.3a Streams (0.4)

- A branch can be a **stream**: `mainline` (the trunk, a branch without a parent), `development`
  (less stable than its parent) or `release` (more stable than its parent). Ordinary branches
  stay as they are, and every merge involving one follows the v1 rules.
- `lode branch create NAME --type development|release` makes a stream from a stream;
  `lode branch set-type NAME TYPE|none` changes one (depot admins). `lode branch tree` shows
  the streams with how far each is behind its parent and ahead of it.
- Changes flow down into development streams freely and up from them only once they are up to
  date with the parent ("merge down, copy up"). Release streams take only picked revisions from
  their parent and merge fixes up freely. Nothing flows sideways.
- The server enforces the rules on commit and on shelf submit; `lode merge` checks them before
  touching the workspace. A depot admin can override with `--force-flow`, which is recorded.

### 5.4 Merge (basic)

- `lode merge <source-branch>` merges all not-yet-merged source revisions into the workspace.
  `lode merge <source-branch> -r <rev>` picks one revision. Both require the workspace view
  to cover every path in the source delta before applying changes or recording a merge range.
- The resulting commit records `merged_from`: source branch plus revision range. This is stored
  from the first version so that later, smarter merge tooling has complete history to work with.
- v1 merge base rule: the highest source revision `R` such that every source revision up to `R`
  is recorded as merged into the target (or the branch point if none).

### 5.5 Identity and permissions

- Principals authenticate with bearer tokens. Humans obtain one with `lode login`: with a
  name and password, or (0.2) with single sign-on through the company's OpenID Connect
  provider (`lode login URL --sso`, §5.9).
- Tokens carry scopes. The core defines `read`, `write` and `admin`; modules add their own
  (`lock`, `shelve`, `checks`). A scope belonging to a module that is switched off has no effect.
- An explicitly created token for a user needs at least one `--scope`, for example
  `lode admin token create --for alice --name review --scope read`. When the target of
  `admin token create --for` is an agent, omitting `--scope` selects its defaults (§7.1).
- Access rules grant `read`, `write` or `admin` on `depot / branch pattern / path pattern` to a
  user, group, every agent (`agents`, 0.4), or everyone. Default is no access. Rules are `allow` or `deny`. A matching `deny`
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

Since 0.4 it may also say how files merge (read by the client from the workspace):

```toml
[merge]
json = ["**/*.json", "**/*.uproject", "**/*.uplugin"]  # the default: merged key by key
                                                         # when a line merge conflicts
[merge.tools]
"**/*.uasset" = "unreal"     # `lode resolve --tool` starts the tool named `unreal`
```

A tool name only names a tool: the command it runs is set by each person in their own
`settings.toml` (`[merge_tools.unreal]`), so a depot can never make Lode start a program.

**Shelf-only branches (0.5).** `[branches] shelf_only = ["main", "release/*"]` makes the
matching branches take changes through shelf submits only. Any other commit to them, a merge
result included, is refused with `shelf_required`: shelve it and submit the shelf. A depot
admin (a user, not an agent) can still commit directly with `lode commit --override
"REASON"`; the revision records who and why (`provenance.shelf_only_override`, shown by
`lode log`), so a broken check can never lock a branch. With the shelves module off, only
such overrides can commit to a shelf-only branch.

A commit that would leave `.lodeconfig` unparseable is refused. Rules are always taken from
the branch head, never from the change being committed.

### 5.7 Path rules

Paths are UTF-8, NFC-normalized, `/`-separated and relative to the depot root. Rejected:
empty segments, `.` and `..`, control characters, backslash, the characters Windows forbids in
file names (`< > : " | ? *`), segments ending in a dot or space, Windows reserved device names
(including `COM¹`–`COM³` and `LPT¹`–`LPT³`), and any path that would collide with another path
in the same directory when compared case-insensitively. Every path in a depot can therefore be
checked out on Windows, macOS and Linux.

### 5.8 Obliterate

A server administrator can remove file content from a depot for good, for a leaked secret or a
huge file committed by mistake: every version of a file or of every file under a folder, or
only the version a revision had (`lode admin obliterate <depot> <path[@rev]>… --reason TEXT`).
Content is removed, history is not: revisions, messages and paths stay, and the depot keeps a
record of what was removed, by whom, when and why. From then on that content answers
`obliterated` wherever it would be read, a checkout or update leaves those files out and says
so, and uploading or committing the same content again is refused.

Content still in the head of an active branch cannot be obliterated; commit its removal or a
fixed file first. Copies outside the server are not reached: backups, workspaces and shared
caches on people's computers, and webhook payloads already sent.

### 5.8a Deleting users and depots

- `lode admin user delete NAME` (and `lode admin agent delete NAME`) deletes a principal.
  A deleted user cannot sign in, every token stops working, and they disappear from lists;
  their group memberships, access rules that name them and their locks are removed, and
  the agents they own are deleted with them. History keeps showing their name, so the
  name stays taken and cannot be given to someone else. The built-in `admin` cannot be
  deleted, nor can an administrator delete themselves. To stop access only for a while,
  disable instead.
- `lode admin depot delete NAME` moves a depot to the trash after the name is typed again
  as confirmation (`--yes` for scripts). It disappears for everyone at once and keeps its
  name; `lode admin depot restore NAME` brings it back unchanged within 30 days, and
  `lode admin depot trash` lists what is there. After 30 days the server erases it: its
  stored content, history, access rules and module data (locks, shelves, checks,
  webhooks). Its name is then free for a new depot.
- `--force` erases at once, for a depot that is live or already in the trash. This
  cannot be undone. As with obliterate, copies outside the server (backups, workspaces,
  shared caches) are not reached.

### 5.8b Backups on another machine

- Scheduled backups (`[backup]`) can also go to an S3-compatible bucket
  (`[backup.copy_to]`): Backblaze B2, Wasabi, AWS, Hetzner, or MinIO on a second server.
  Each copy uploads only new content plus the database; copies beyond `keep` are removed
  with the content only they needed.
- `loded restore --config FILE [--name NAME] DEST` brings a data directory back from the
  bucket, also on a new machine; `--list` shows the copies. `loded backup-copy` sends the
  newest backup at once. The admin overview shows the last copy.

### 5.9 Single sign-on (0.2)

- A server with `[oidc]` in `loded.toml` signs people in through an OpenID Connect provider
  (Google, Microsoft Entra ID, Okta, Keycloak and others). The server is the OIDC client:
  authorization code flow with PKCE; the ID token's signature (RS256 or ES256), issuer,
  audience, expiry and nonce are checked against the provider's discovery document and keys.
- A sign-in ends with an ordinary Lode token with the default scopes, like a password login.
- Users are linked to the identity the provider names them by (the `email` claim by default,
  compared without case) with `lode admin user create NAME --sso IDENTITY` or
  `lode admin user sso NAME IDENTITY`. An unlinked identity is refused unless
  `auto_create` is on, which creates the user on first sign-in. `allowed_domains` limits who
  may sign in at all. A disabled Lode user stays refused whatever the provider says.
- `password_login = "admins"` turns password sign-in off for everyone but server
  administrators.
- `lode login URL --sso` opens the browser and receives the result on a loopback address;
  `--no-browser` prints the address instead. Lode Desk offers "Sign in with …" when
  `GET /info` names single sign-on.
- Not in 0.2: SAML, group sync from the provider, SCIM.

### 5.10 Admin web UI (0.2)

- `loded` serves web pages for server administrators at `/admin/` on its own listener and
  TLS; nothing else to install. `[admin_ui] enabled = false` turns them off (404).
- Sign-in with a password or single sign-on; only server administrators get in. A session
  lasts 8 hours and is an ordinary `web admin` token, so revoking it, disabling the user or
  removing their admin rule ends it. Every form is protected against cross-site requests.
- Pages: an overview of the server's health (version, uptime, TLS and certificate expiry,
  storage and database size, counts, backups, last maintenance per depot, recent commits);
  depots with branches, revisions, locks, garbage collection, checks and obliterate (dry run
  first, and deleting a depot to the trash or for good, with the trash to restore from);
  users, agents, groups and tokens (a new token's value shown once; deleting a user); access rules
  with a check that names the deciding rule; webhooks and their deliveries (with the
  module); the effective settings with secrets hidden.
- Every action goes through the API, so permissions, validation and errors match
  `lode admin`. Pages work without JavaScript, follow the system light or dark theme, and
  fit a phone.
- Not in 0.2: browsing file contents, a web UI for people who are not administrators.

## 6. Default modules

### 6.1 Locks

- `lode lock <path>…` takes exclusive locks; all-or-nothing. A lock is per branch and path.
- A lock can only be taken on a file the workspace has at the branch head version.
- A commit touching a path locked by someone else is rejected (`locked`).
- Modifying or deleting an existing path matching `[locks].required` requires its lock
  (`lock_required`). The client keeps the tracked file read-only until locked. The first
  add may commit without a lock because the path has no version at branch head yet.
  If two workspaces add the same path, the first commit creates it and the other receives
  `out_of_date` for that path.
- Locks are released on commit unless `--keep-locks` is given. `lode unlock` releases manually.
  Admins can break locks (`lode unlock --force`).
- `lode locks` lists locks with owner, time and comment.

### 6.2 Shelves

- `lode shelve [-c <changelist>] -m <description>` uploads pending changes as a shelf and,
  unless `--keep` is given, reverts them locally.
- `lode shelves`, `lode shelf show <id>`, `lode shelf diff <id>` inspect shelves of any user
  (subject to read permission). In a workspace they default to its depot. With `--depot NAME`
  they use the configured server and can run outside a workspace.
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
  `update_shelf`, `list_checks` (with the checks module), and `submit_shelf` for tokens with
  `write`.
- **Local endpoint** (`lode mcp`, stdio). Operates on a workspace. Tools: `status` (with
  `remote` and a `summary`), `diff`, `add`, `remove`, `move`, `revert`, `update`, `lock`,
  `unlock`, `shelve`, `unshelve`, `log`, `read_file_at_revision`, `commit`, and (0.4)
  `branches`, `locks`, `conflicts`, `switch`, `merge`, `resolve`, `changelist` and
  `wait_for_events`. An agent can do through it what a person does in Lode Desk: see what
  changed locally and on the server, switch and merge, settle conflicts and shelve the
  result for review.
- With the feed module (§6.7) both endpoints offer `wait_for_events`.

For `create_shelf` and `update_shelf`, every edit requires `path` and `action` (`add`,
`modify` or `delete`). A supplied `copy_from` requires both `path` and `rev`. Invalid or
missing actions return `invalid_request` with the array index and the allowed actions.
HTTP shelf and commit requests use the same diagnostic for their `changes` array.

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
- Direct commits are not gated by checks. To make every change on a branch go through a
  gated shelf, list it in `[branches].shelf_only` (§5.6, 0.5). Lode stores and enforces
  results; it does not run builds (except the validation scripts of §6.8) and has no review
  workflow.

### 6.7 Event feed (0.4, on by default)

- The server keeps every event of §6.5 for 7 days, at most 100 000 per depot, so a client can
  see what happened in a depot since it last looked without polling every list.
- `GET /depots/{d}/events?after=<id>&kinds=<k,…>&wait=<s>` answers
  `{ "events": [envelope…], "next": <id>, "truncated": bool }`, at most 500 events, oldest
  first. Envelopes carry identifiers only, as for webhooks. Without `after` it answers no
  events and the current cursor. `wait` (at most 55 seconds) holds the request until an event
  arrives. `truncated` says events after the cursor were dropped: read the history again.
- Needs scope `read` and read access to the depot; events naming a branch the caller can read
  nothing on are left out.
- `lode events [--depot D] [--after ID] [--kinds K,…] [--wait S]` and the MCP tool
  `wait_for_events` read it. Lode does not run agents: the loop that waits lives in the agent
  runtime.

### 6.8 Validation scripts (0.5)

- A depot keeps its own content checks as Python scripts in the depot, listed in
  `.lodeconfig`. They reach every client and the server with an ordinary `lode update` or
  submit, so there is nothing to install per machine.

  ```toml
  [validate]
  python  = "3.8"                  # oldest Python the scripts need (optional)
  support = ["tools/validate/**"]  # files the scripts import or read

  [[validate.script]]
  name    = "json"                 # check name on the server: validate/json
  run     = "tools/validate/json_valid.py"
  paths   = ["Data/**/*.json"]     # the changed files it is given
  timeout = "60s"                  # default 60 s, at most 10 min
  server  = true                   # also run by the server (default true)
  ```

  A commit whose `.lodeconfig` names a script that is not in the new revision, or a setting
  that does not parse, is refused (`invalid_request`, `details.paths`).
- **Contract.** The script runs as `python -X utf8 <run>` with the workspace (or the
  server's temporary tree) as working directory and a cleared environment plus
  `LODE_VALIDATE=1`. Standard input is one JSON object: `{ "version": 1, "phase":
  "shelve"|"commit"|"manual"|"server", "depot", "branch", "files": [ { "path", "action":
  "add"|"edit"|"delete"|"move", "from"? } ] }`. Exit 0 passes, exit 1 fails, anything else,
  a timeout or an uncaught exception is an error ("the check broke"). Each standard output
  line that is a JSON object `{ "path"?, "line"?, "level": "error"|"warning", "message" }`
  is a problem; other lines are kept as text.
- **Clients.** `lode shelve` and `lode commit` run the scripts whose `paths` match the
  changes first and stop with `validation_failed` (`details.results`) when one fails;
  `--no-verify` skips them. `lode validate [paths] [-c NAME] [--script NAME]` runs them on
  demand. A script runs on a machine only after its owner trusted that exact content once
  (`lode validate --trust`, or a prompt in a terminal); otherwise `validation_untrusted`.
  Without a suitable Python the client warns and goes on, or refuses with `python_missing`
  when `[validate] missing_python = "refuse"` is set in `settings.toml`.
- **Server (module `validate`, off by default).** On every new shelf version the server
  runs the scripts with `server = true` that match the shelf's changes and reports each as
  the check `validate/<name>` (§6.6) from the service user `lode-validate`. Scripts and
  support files come from the target branch head, never from the shelf. On branches in
  `[checks].branches` every such script must have passed on the submitted shelf version, in
  addition to `[checks].required`; with `[branches].shelf_only` that covers every change.
  `GET /depots/{d}/validate/{shelf}/{version}` (scope `read`) answers the full results.
  When validation is enabled, the admin depot page lists active and recent runs. Select a shelf
  version to read each script’s outcome, diagnostics and captured output.
- Client runs are a convenience; the server run is the enforcement. Neither is a sandbox:
  a script runs with the rights of whoever runs it.

## 7. AI support

### 7.1 Agent identities

- An agent is a principal of kind `agent`, owned by a user or an admin, with its own tokens.
- An agent token defaults to `read` plus the agent-default scopes of enabled modules
  (`shelve` for shelves, `lock` for locks). `write` (direct commit) must be granted explicitly.
- Access rules apply to agents exactly as to users. An agent never inherits its owner's access.
- One rule can cover every agent (0.4): `lode admin acl add deny write agents --depot game
  --branch main` keeps agents' work on shelves for `main` even when a token has `write`.
  A deny at `write` also refuses locks there, since a lock needs write access; agents still
  read and shelve, and a person submits.

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

A revision made by `lode import git` adds `imported`:
`{ "system": "git", "id": "<commit id>", "author": "Name <email>", "date": "<RFC 3339>" }`,
the original commit, its author and its author date. It is reported by the importer like
`agent`; the importer reads it back from the branch head to continue an interrupted import.

A revision made by `shelf submit` takes `via` from the submit request (usually `api`), keeps
the shelf's `client` and `agent`, and adds `submitted_shelf` with the shelf's id.

`author` and `submitter` come from the authenticated token and are always reliable.
`author` is who made the change, `submitter` is who committed it. They differ when a human
submits an agent's shelf. `lode log` shows both.

### 7.4 Machine-readable CLI

Every CLI command accepts `--json` and then prints exactly one JSON document to stdout. Errors
are JSON on stdout with a stable `code`; the process exit code is non-zero.

## 8. Optional modules (after v1)

Streams with flow rules (§5.3a, 0.4), merges that follow renames, merge JSON files key by
key and start external merge tools (0.4), the caching proxy and the Postgres metadata
backend (0.3), chunked storage of files from 16 MiB, so an edit to a large file stores and
sends only the changed chunks (0.3), obliterate (§5.8, 0.2), single sign-on (§5.9, 0.2), S3-compatible blob storage (0.2), the Git importer (0.1)
and the SVN importer (0.2) are built.

## 9. CLI summary

```
lode login <server-url> [--sso [--no-browser]]
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
lode checks <rN | shelf-id[@version]>          (with the checks module)
lode mcp
lode admin depot|user|agent|group|token|acl|gc|fsck …
lode admin depot delete <name> [--force] | restore <name> | trash
lode admin user|agent delete <name>
lode admin webhook add|list|remove|deliveries|ping …   (with the webhooks module)
```

For history, `lode log -n 4` and `lode log --max-count 4` return at most four revisions,
newest first. The default maximum is 50; zero requests the entire matching history.
Pass the last revision number to `--before REV` to continue with older entries.

Every command honours the environment variables `LODE_SERVER` and `LODE_TOKEN_FILE` (a path
to a file holding a token) for sign-in without a stored login, and `LODE_CACHE_DIR` (or the
user setting `cache_dir`) for a download cache shared by several workspaces on one machine.
`lode --version --json` reports the client and API versions in machine-readable form. A
reused workspace is reset with `lode revert --all`, `lode clean` and
`lode switch <branch> -r <rev>` (docs/CLI-JSON.md, "Unattended machines").

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
| 5.4 Merge | After a single-revision pick (`-r`), a later full merge starts again from the merge base before the gap and re-applies the picked change. Usually `unchanged`; a conflict if the file changed since. Follows from the v1 base rule. |
| 6.1 Locks | `lode switch` leaves this workspace's locks on the old branch (they are per branch) and reports them; it does not release them. |
| 6.4 MCP | The server endpoint also offers `list_depots`, and `submit_shelf` to tokens with `write`. Large files, diffs and blame are cut off at fixed sizes (`truncated`, `next_line`). |
| 7.3 Provenance | A submitted shelf's revision has `via` of the submit request and an extra `submitted_shelf` field (see 7.3). |
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
