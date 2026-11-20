# Upgrading Lode

How to move a server and its clients from one release to the next. The user guide's
"Upgrading" chapter is the short version.

## Compatibility

- Clients and servers that speak the same API version work together, in both directions.
  The server reports it in `/api/v1/info` (`api_version`, now `1`); a release that changes it
  says so in its notes.
- The server upgrades its database on start, in place. It refuses to start on a database
  written by a newer release, so going back means restoring a backup.
- Workspaces upgrade their own small database the same way, the first time a newer `lode`
  opens them.
- `loded.toml` keys are never removed within a major version. A new release may add keys;
  the defaults keep the old behaviour unless the release notes say otherwise.

## Server

1. Read the release notes for anything marked "upgrade note".
2. Take a backup while the server runs:
   `loded backup --config /var/lib/lode/loded.toml /backups/before-0.2`
3. Stop the server: `loded service stop` (or `systemctl stop loded`, `Stop-Service loded`,
   `docker stop lode`).
4. Replace the executables:
   - Release archive: unpack it over the old `loded` and `lode`.
   - Package: install the newer one (`curl -fsSL …/install.sh | sh`, `apt install
     ./lode-server_<version>_amd64.deb`, `dnf install ./lode-server-<version>.x86_64.rpm`, or
     the newer MSI). The package stops and starts a running service itself, so steps 3 and 5
     can be skipped.
   - Container: `docker pull ghcr.io/hutler/loded:<version>`, then recreate the container with
     the same volume.
5. Start it: `loded service start`. The log shows the database migration, if any, and the
   "listening" line.
6. Check: `lode admin fsck <depot>` on one depot, and a `lode update` from a workspace.

Registered services keep working across upgrades as long as the executable stays at the same
path: `loded service install` recorded that path. After moving the executable, run
`loded service uninstall` and `loded service install` again.

### Going back

1. Stop the server.
2. Put the old executable back.
3. Replace the data directory with the backup from step 2 above (the newer database cannot be
   opened by the older server).
4. Run `loded fsck --config ...`, then start the server.

Commits made after the backup are lost on the server; workspaces that made them should be
checked out fresh and their changes re-applied.

## Clients

Run `lode self-update` for a `lode` installed from an archive, or update it the way it was
installed (winget, the MSI, Homebrew, apt or dnf; `self-update` names the command). Nothing
else is needed. Upgrade the server first:
a newer client may use requests that an older server does not know.

## Release notes

### 0.1.0

Built and tested, never published. 0.2.0 is the first public release; a server that ran a
0.1 build from source upgrades as the notes below say.

### 0.2.0: setup and packaging (M8)

- New `[connections]` table (`header_timeout_seconds`, `max_per_address`). The defaults apply
  without it; slow and idle connections are now closed after 30 seconds.
- `[tls]` accepts `acme_domain` for a Let's Encrypt certificate instead of `cert` and `key`.
- New `[backup]` table for scheduled backups; off unless `every` is set.
- `loded setup` and `loded service`. On Windows, the WinSW wrapper and
  `packaging/windows/loded-service.xml` are gone: stop and remove the WinSW service
  (`loded-service.exe uninstall`), then run `loded service install --config ...`.
- Packages: `.deb` and `.rpm` (`lode-server`, `lode`), a Windows MSI, the Linux install
  script and `packaging/compose.yaml`. The container image's first start can now run
  `loded setup` (`LODED_DOMAIN`, `LODED_EMAIL`, `LODED_ADMIN_PASSWORD_FILE`); without them it
  behaves as before.
- `lode init` (new depot from a folder, with templates) and `lode import git`.
- Client installers: a Windows MSI (also for winget), a Homebrew formula, and
  `lode self-update` for archive installs. Releases also carry the bare `lode` executables.

### 0.2.0: obliterate, importers, single sign-on, S3, Lode Desk on Slint, admin web UI (M12 to M14)

- The database gains the `obliterated` table (core schema 3) and a single sign-on column on
  principals (core schema 4), both added automatically on the first start. A 0.1 server refuses a database a 0.2 server has opened, so take a backup first if
  you may need to go back.
- New error code `obliterated` (HTTP 410). Older clients show it as an unknown error with the
  server's message; a 0.1 client fails a checkout or update that meets an obliterated file
  instead of leaving it out, so upgrade clients before obliterating content that people may
  still check out at old revisions.
- `lode import svn <url> <server>/<depot>` imports a Subversion directory's history.
- New optional `[oidc]` table in `loded.toml` for single sign-on (the commented example in a
  new `loded init` config shows every key). Without it nothing changes. Link existing users
  with `lode admin user sso NAME IDENTITY` before turning on `password_login = "admins"`.
  `GET /info` gains an optional `sso` field and principals an optional `sso` field; older
  clients ignore both. `lode login --sso` and Lode Desk's "Sign in with …" need 0.2 clients.
- New optional `[storage]` table: `kind = "s3"` keeps file content in an S3-compatible bucket.
  Without it nothing changes. Move an existing server with `loded migrate-storage --to s3`
  (see the user guide, "Keeping content in S3"). With S3 storage, `loded backup` and
  scheduled backups copy only the database and `loded.toml`; use the bucket's versioning or
  replication for the content.
- New admin web UI at `/admin/` (M14), on by default for server administrators. Its new
  `[admin_ui]` table has one key, `enabled`; set it to `false` if nobody will use the pages.
  Serve it only over HTTPS. The database gains `maintenance_run` (core schema 5), which
  records the last garbage collection and check per depot, added on the first start.
  `GET /admin/acl/check` (`lode admin acl check --json`) gains the optional fields
  `depot_admin`, `matched` and `decided_by`; older clients ignore them.

### 0.3.0: chunked large files, Postgres, caching proxy (M15)

Nothing has to be done to upgrade: replace the programs and restart. The database gains the
columns for deleting users and depots (core schema 6: `lode admin user delete`, `lode admin
depot delete` and the trash), added on the first start. A 0.2 server refuses a database a 0.3 server has opened,
so take a backup first.

- Files of 16 MiB and more are stored as content-defined chunks (T150, T151). The file id
  does not change, so history, trees and older clients are unaffected: an 0.2 client still
  uploads and downloads whole files, and the server serves whole files to it. 0.3 clients
  send and fetch only the chunks the other side lacks. `/info` gains `chunking`.
- New optional `[database]` table (T153): `kind = "postgres"` keeps the metadata in Postgres
  14 or newer. Without it nothing changes. Move a stopped server with
  `loded migrate-db --to postgres --url-file <file>` (see the user guide, "Keeping the
  metadata in Postgres"). With Postgres, backups do not contain the database; use `pg_dump`.
- Modules declare a Postgres form of each migration (`Migration::postgres`); a module without
  one cannot be enabled on a Postgres server. All modules shipped with Lode have one.
- New `loded proxy` (T154): a caching proxy for remote offices, configured in its own
  `proxy.toml` (see the user guide, "A caching proxy for a remote office"; `loded proxy
  --example` prints a starting file). Clients use it after `lode proxy set <url>` or with
  `LODE_PROXY`; nothing changes for anyone else. The server gains `GET /depots/{d}/obliterated-ids` for proxies,
  open to the depot's readers (ids only).

### 0.4.0: streams and smarter merge (M16)

Nothing has to be done to upgrade: replace the programs and restart. The database gains
`branch.stream_type` (core schema 7), added on the first start; every existing branch has no
type and keeps the 0.3 merge rules. A 0.3 server refuses a database a 0.4 server has opened,
so take a backup first.

- Streams (T160, T161) are opt-in. To turn a depot into streams, a depot admin gives the
  trunk a type first (`lode branch set-type main mainline`), then its children
  (`lode branch set-type dev/ui development`, `... rel/1.0 release`). From then on the
  server refuses merges between streams that go the wrong way (`conflict` with
  `details.reason = "stream_flow"`); `lode branch tree` shows what each stream could merge.
  Merges where either side has no type are unchanged. Older clients can still commit; a
  refused merge reaches them as an ordinary `conflict` error.
- `GET …/merge-info` gains `flow`, `BranchView` gains `stream`, and there is a new webhook
  event `branch.stream_changed`. Clients that ignore unknown fields are unaffected.
- `lode merge` follows renames (T162), merges JSON files key by key (T163, `[merge] json` in
  `.lodeconfig`, on by default for `*.json`, `*.uproject` and `*.uplugin`), and names a
  merge tool for conflicted files (T164, `[merge.tools]`); a tool runs only through
  `lode resolve --tool` and only as set up in your own `settings.toml`.
- Backups can be copied to an S3-compatible bucket and restored from it
  (`[backup.copy_to]`, `loded restore`; see the user guide, "Copies on another machine").

#### Also in 0.4.0: the agent as a desktop client (M18)

The database also widens the access-rule subject check (core schema 8) and gains the feed
module's tables, on the same first start.

- `lode mcp` gains `branches`, `locks`, `conflicts`, `switch`, `merge`, `resolve`,
  `changelist` and `wait_for_events` (T190, T191, T193); `status` takes `remote` and, like
  `lode status --json`, answers a `summary` of counts.
- New module `feed`, on by default (`[modules] feed = false` turns it off; T192): `GET
  /depots/{d}/events` with long-poll, `lode events`, and `wait_for_events` on `/mcp`. Events
  are kept 7 days (at most 100 000 per depot). A reverse proxy in front of `loded` must allow
  requests to take up to 60 seconds; most do by default.
- Access rules take the subject `agents` (T194). A bare `agents` on the command line now means
  this subject; a user named `agents` is written `user:agents`. A 0.3 client cannot list
  rules once one exists.

### 0.5.0: content validation and shelf-only branches (M19)

Nothing has to be done to upgrade: replace the programs and restart. No core schema change;
the new `validate` module adds its own table when it is turned on.

- `[branches] shelf_only` in `.lodeconfig` (T200) makes a branch take changes through shelf
  submits only. A refused commit answers `shelf_required` (409); an older client shows it as
  an ordinary error. A depot admin can commit anyway with `lode commit --override "REASON"`,
  recorded on the revision.
- `[validate]` and `[[validate.script]]` in `.lodeconfig` (T201) list Python validation
  scripts kept in the depot. `lode shelve` and `lode commit` run them first (T202,
  `--no-verify` skips them), `lode validate` runs them on demand, and `lode mcp` has a
  `validate` tool (T203). Each machine trusts a script's content once before it runs.
  An older client ignores the section; a 0.4 server accepts it without checking it.
- New module `validate`, off by default (T205). It needs `checks`, `shelves` and
  `[validate] python` in `loded.toml`, runs the scripts on every shelf version and reports
  `validate/<name>` checks that gate submits on `[checks].branches`. The server must have
  Python 3; the container image does not include it yet. Read the HARDENING note before
  turning it on: the scripts run as the `loded` user.
