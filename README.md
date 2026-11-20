<p align="center">
  <img src="docs/logo/lode-banner.png" alt="Lode: a pixel-art miner girl with a helmet lamp, a pickaxe and a glowing gem, beside the word LODE" width="592">
</p>

<p align="center">
  <b>Version control for teams that keep code and big binary files together.</b><br>
  Open source · self-hosted · one server binary · AI agents as first-class users
</p>

---

**Lode** is a centralized version control system for small teams, up to about 100 people, whose
projects mix source code with large binary assets: game studios, simulation and media teams,
anyone whose repository holds textures, models and builds next to the code. It pairs one
server and a simple linear history with the features such teams need (exclusive locks,
shelves, changelists, partial checkouts), and treats AI agents as users with their own
identity, permissions and MCP tools.

Lode is written in Rust, runs on Linux, Windows and macOS, and is licensed under Apache-2.0
([LICENSE](LICENSE)). It is part of SimpleDevOps.

## Principles

- **One simple mental model.** One server, linear history per branch, one revision number per
  depot (`r1, r2, …`). No local commits to push, no history rewriting.
- **Small core, optional modules.** The core stores data and enforces rules. Locks, shelves,
  MCP, webhooks, checks and the event feed are modules built on public extension points and
  switched on in configuration. New convenience grows around these existing primitives
  instead of adding new concepts.
- **Good with binaries.** Content is addressed by its BLAKE3 hash and deduplicated. Workspaces
  keep no pristine copies. Files from 16 MiB are stored in chunks, so an edit to a large file
  sends and stores only the chunks that changed. Unmergeable files can be locked.
- **Agents are users.** An agent has its own account and tokens, owned by a person. By default
  it can read, lock and shelve but not commit: **the agent proposes, a person accepts**, by
  submitting the agent's shelf. Every revision records who made the change and who committed
  it.
- **Easy to run anywhere.** One server binary, one data directory, SQLite by default, no other
  services required. A small VDS, an office machine or a container is enough. `loded setup`
  asks a few questions and gives you HTTPS with Let's Encrypt and daily backups.
- **Hard to corrupt.** Every object is hash-checked on write, commits are atomic and
  idempotent, and a server or client killed at any point leaves nothing half-done.
- **Machine-readable everywhere.** Every CLI command takes `--json` and answers with one JSON
  document and stable error codes, so scripts, build servers and agents never parse text.

## Core features

### Version control

- **Commits** are atomic, with optimistic per-file concurrency: a commit is refused only if a
  file it touches changed on the server since you last synced it.
- **Workspaces** with change detection by scan (no "open for edit" step), explicit moves and
  copies so history follows files, and **views** for partial checkouts that can be changed
  later.
- **Branches** are cheap and can be archived. **Streams** (mainline, development, release)
  add flow rules the server enforces: merge down, copy up, nothing sideways.
- **Merges** record their origin from the first version, follow renames, merge JSON files key
  by key, and can hand a file to an external merge tool. Binary conflicts are never merged
  blindly.
- **History tools:** log, blame, diff, cat at any revision, and **obliterate** for content
  that must really go (with a dry run first).

### Team workflow

- **Locks** give one person an exclusive claim on a file per branch. Paths can require a lock
  (`[locks].required`), and such files stay read-only on disk until locked.
- **Shelves** store uncommitted changes on the server where others can see, unshelve and
  submit them. Each change raises the shelf's version, and a submit names the version that was
  reviewed, so nobody can approve one thing and commit another.
- **Changelists** group pending changes inside a workspace; commit or shelve one at a time.
- **Checks and webhooks** connect any build server without Lode running builds itself: a CI
  system reports results on a revision or a shelf version, and `.lodeconfig` can require named
  checks to pass before a shelf is submitted. Webhooks are signed and carry identifiers only.

### AI agents and MCP

- **`lode mcp`**, a local MCP server over stdio, lets a coding agent work in a workspace like a
  person in Lode Desk: status, diff, update, switch, merge, resolve conflicts, lock and shelve.
- **`/mcp` on the server** needs no workspace: an agent reads depots, history and diffs, then
  proposes changes with `create_shelf`. Many agents can work in parallel without checkouts.
- **Event feed:** `lode events` and the `wait_for_events` tool let an agent wait for new
  revisions, shelves and checks instead of polling.
- **Guard rails:** agents never inherit their owner's access, and one rule such as
  `lode admin acl add deny write agents --depot game --branch main` keeps every agent's work
  on shelves for `main`. Provenance on each revision records the client, the endpoint used
  and the agent's reported model and session.

See [lode/docs/MCP.md](lode/docs/MCP.md).

### Running a server

- **Setup:** Linux packages (`.deb`, `.rpm`) with a systemd unit, a Windows MSI that installs a
  service, and a container image (`ghcr.io/hutler/loded`).
- **Identity:** users and groups, passwords or single sign-on through any OpenID Connect
  provider, scoped tokens (`read`, `write`, `admin`, plus module scopes), and access rules per
  depot, branch pattern and path pattern.
- **Storage:** local disk or S3-compatible blob storage; SQLite, or Postgres for one active
  server with a standby that takes over.
- **Backups:** scheduled backups, copies to an S3-compatible bucket, and `loded restore`.
- **Housekeeping:** garbage collection, `fsck`, and safe deletion of users and depots with a
  30-day trash, restore, and a forced delete when needed.
- **Remote offices:** a caching proxy keeps file content close to people far from the server.
- **Admin web UI** at `/admin/`: depots, people, access rules, tokens, webhooks, GC and
  obliterate reports, on desktop and phone.
- **Importers** bring Git and Subversion projects in with their history.
- **Versioned builds:** every build names its commit, for example `0.2.0+10.g2c321d0`.

## Clients

| Client | What it is |
| --- | --- |
| `lode` | The command-line client. Every command has `--json`. |
| **Lode Desk** | A native desktop app (Slint): changes and changelists, history, blame, text and image diffs, branches and merges with conflict resolution, locks, shelves and their checks. It updates itself. Guide: [desk/docs/guide.md](desk/docs/guide.md). |
| **Lode for Unreal** | An Unreal Editor plugin that makes Lode a revision control provider (Unreal Engine 5.8). It runs `lode --json`. Guide and spec: [unreal/docs/SPEC.md](unreal/docs/SPEC.md). |
| Any MCP agent | `lode mcp` on a developer machine, or `/mcp` on the server. |

## Quick start

On a Linux server with a domain name pointing at it and port 443 open:

```sh
curl -fsSL https://raw.githubusercontent.com/hutler/Lode/main/lode/packaging/install.sh | sh
sudo -u lode loded setup            # domain, email, backups, admin password
sudo systemctl enable --now loded
```

On Windows, run `lode-server-<version>-x64.msi` from the releases page and keep "Set up the Lode
server now" ticked. With Docker, use `packaging/compose.yaml` from the release archive. Then,
on your own machine:

```sh
lode login https://lode.example.com
cd MyGame
lode init https://lode.example.com/mygame --template unreal                 # the folder as it is
lode import git ~/src/mygame https://lode.example.com/mygame --branch main  # or a Git history
```

Templates exist for `unreal`, `unity`, `godot`, `rust`, `node` and `generic`. The full guide is
[lode/docs/user-guide.html](lode/docs/user-guide.html) ([PDF](lode/docs/Lode-User-Guide.pdf)),
and AI agents deploying a server can follow
[lode/docs/deploy-linux-for-agents.md](lode/docs/deploy-linux-for-agents.md).

For agents working with Lode, see the Russian [system guide](docs/agent-guide.ru.md)
and [pre-commit and shelf validation guide](docs/agent-validation.ru.md).

## What Lode is not

Lode stays small on purpose. It has no built-in CI runners (it stores and enforces check
results that your build server reports), no issue tracker, no inline code review, and it does
not orchestrate agents (the loop that waits for work lives in the agent's runtime). It has no
offline or distributed commits, and no Git or SVN wire compatibility; importers bring history
in instead.

## Status

Implemented on `main`: Lode v1 (M0 to M6), packaging and easy setup (M7, M8), Lode Desk (M9 to
M13, now on Slint), Lode 0.2 features such as obliterate, single sign-on, S3 storage and the
SVN importer (M12), the admin web UI (M14), chunked large files, Postgres and the caching
proxy (M15), streams and smarter merges (M16), and the agent as a desktop client with the event
feed (M18), plus deleting users and depots and backup copies to S3. Lode Desk now lives in its
own `desk/` folder with a pinned core version, its own CI and its own `desk-v*` releases.

Releases: **0.2.0** is published on the
[releases page](https://github.com/hutler/Lode/releases/tag/lode-v0.2.0). **0.3.0** and
**0.4.0** are prepared on `main` and waiting for their tags.

In review now:

- **M17, field feedback** ([#5](https://github.com/hutler/Lode/pull/5)): a read-only depot
  browser and shelf review at `/web/` for every signed-in user, `lode doctor` and
  `loded doctor`, chat-friendly webhooks for Slack, Discord and Telegram, `lode link`, shell
  completions, a safer first admin password, and optional Prometheus `/metrics`.
- **M19, content validation** ([#21](https://github.com/hutler/Lode/pull/21)): Python
  validation scripts kept in the depot that run before shelve and commit, for people and agents
  alike, plus branches that accept only shelf submits.

Status boards: [lode/docs/tasks/README.md](lode/docs/tasks/README.md),
[unreal/docs/TASKS.md](unreal/docs/TASKS.md).

## Repository layout

| Folder | What it is | Language | Start reading at |
| --- | --- | --- | --- |
| [lode/](lode/) | The core: the server `loded`, the client `lode`, the modules and the `lode-ops` library | Rust | [lode/README.md](lode/README.md), [lode/docs/SPEC.md](lode/docs/SPEC.md), [lode/docs/ARCHITECTURE.md](lode/docs/ARCHITECTURE.md) |
| [desk/](desk/) | Lode Desk, the desktop client, built on a pinned version of the core | Rust | [desk/README.md](desk/README.md), [desk/docs/guide.md](desk/docs/guide.md) |
| [unreal/](unreal/) | Lode for Unreal, the editor plugin | C++ | [unreal/docs/SPEC.md](unreal/docs/SPEC.md) |
| [docs/logo/](docs/logo/) | The mascot and banner, with the scripts that draw them | Python | |

Lode never names Unreal: the plugin only runs the `lode` command and reads its JSON output.
Lode Desk uses the core's `lode-ops` library at a version pinned in `desk/Cargo.toml`, so the
core changes and settles first and Desk moves to it in its own change; Desk has its own
version, CI and `desk-v*` releases.

## Building and contributing

```sh
cd lode
cargo build --workspace
cargo test --workspace
```

Lode Desk builds on its own, in `desk/` (`cargo build`, `cargo test`), and needs Rust 1.92.
Building the core needs Rust 1.88 or newer and, on Windows, the MSVC C++ build tools. More in
[lode/README.md](lode/README.md). Rules for people and AI agents working in this repository
are in [AGENTS.md](AGENTS.md).

## The mascot

The miner on the banner is Lode's own pixel-art mascot: a lode is a vein of ore, so she brings
a helmet lamp, a pickaxe and a gem. She is drawn by
[docs/logo/source/mascot.py](docs/logo/source/mascot.py) and is under the same Apache-2.0
licence as the rest of the project.
