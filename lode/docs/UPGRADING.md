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
