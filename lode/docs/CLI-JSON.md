# Lode CLI: JSON output

With `--json`, every `lode` command prints exactly one JSON document to stdout and nothing
else. Progress, prompts and warnings go to stderr or are left out, and a command that would
need to ask something fails instead (exit code 2). This page lists the document of each
command. Fields may be added in later versions; existing fields keep their meaning.

The same documents, without the `ok` envelope, are what the `lode-ops` library returns to
programs that call the operations directly, such as the Lode Desk app.

## Envelope

Success:

```json
{ "ok": true, "...": "the command's fields, listed below" }
```

Failure:

```json
{ "ok": false, "error": { "code": "out_of_date", "message": "…", "details": { "paths": ["a.txt"] } } }
```

`details` is present only when there is something in it. When the error concerns files,
`details.paths` lists them.

## Exit codes (SPEC §9)

| Exit | Meaning | Typical codes |
| --- | --- | --- |
| 0 | success | |
| 1 | operation failed | `not_found`, `out_of_date`, `conflict`, `conflicted`, `locked`, `lock_required`, `local_changes`, `nothing_to_commit`, `nothing_to_shelve`, `update_incomplete`, `missing`, `aborted`, `module_disabled`, `not_a_workspace`, `already_a_workspace`, `not_empty`, `workspace_in_use`, `checkout_incomplete`, `pending_commit`, `ignored`, `outside_view`, `unsafe_path`, `changed_during_commit` |
| 2 | usage error | `usage`, `invalid_path`, `invalid_pattern`, `invalid_url`, `insecure_url` |
| 3 | authentication or permission | `unauthenticated`, `permission_denied`, `sso_failed` |
| 4 | network or server | `network`, `internal`, `protocol`, `hash_mismatch`, any 5xx |

Error codes from the server (`lode_core::ErrorCode`) are passed through unchanged.

## Common values

- Paths are depot paths: `/`-separated, relative to the depot root.
- Ids are 64 hex digits (BLAKE3).
- `status` values: `modified`, `added`, `deleted`, `missing`, `conflicted`, `untracked`, and
  `unchanged` with `status --unchanged`.
- `kind` values: `file`, `symlink`, `dir`.
- Path arguments may be relative to the working directory or absolute, with either slash. On
  Windows the workspace root is matched without regard to letter case.

### Many paths: `--paths-from FILE`

Commands that take a list of paths (`status`, `add`, `rm`, `revert`, `diff`, `commit`, `lock`,
`unlock`, `shelve`, `clean`, `changelist move`) also read them from a file, one per line, or
from standard input with `--paths-from -`. The file is UTF-8; a byte-order mark, `\r\n` line
ends and blank lines are fine. Use it when a list would not fit on a command line (about
32,000 characters on Windows). An empty list is a `usage` error, never "everything"; any other
command refuses the option with `usage`.

## Commands

### `lode --version`

`{ "version": "0.2.0", "build": "0.2.0+5.g4a33c89", "api": "1" }`. `version` is the release
version, for version checks. `build` names the exact build: the same as `version` on a
release, and on any later commit the commits since the newest `lode-v*` tag and the commit
(see [release.md](release.md#build-versions)). The text output prints `build`. `api` is the
server API version this client speaks (`/api/v1`).

### `lode login <url>`

Needs `--name` with `--password-stdin`, or `--token-stdin`, or `--sso`.

`{ "server", "name", "kind": "user"|"agent", "scopes": [..], "token_file", "sso": bool }`

With `--sso` (single sign-on, 0.2) the command opens the browser (`--no-browser`: it does
not) and prints the sign-in address to stderr, also with `--json`, then waits up to 5
minutes for the browser to come back to its loopback address. A server without single
sign-on is a `usage` error; a refused or unfinished sign-in is `sso_failed` (exit 3) with the
reason in the message, such as the provider's refusal, an unlinked identity, a domain that
is not allowed or a disabled user.

A first password that must be changed (`loded setup --generate-admin-password`) is refused
with `unauthenticated` and `details.reason` `password_change_required`; at a terminal
without `--json` the command asks for a new password, changes it and signs in with it.
After 24 hours unused the reason is `password_expired`.

### `lode passwd [url] [--name N] [--password-stdin]`

Changes one's own password through `POST /auth/password` (no token needed, so it also
replaces a first password before any sign-in). The name defaults to the signed-in account.
`--password-stdin` reads the current password, then the new one, one per line.

`{ "server", "name" }`

### `lode logout`

`{ "server", "removed": bool, "revoked": bool }` (`revoked` absent when nothing was stored)

### `lode proxy set <url>` / `clear` / `show`

The caching proxy (0.3) used for the server's file content.

- `set`: `{ "server", "proxy" }`. A URL that answers as a Lode server rather than a proxy is
  a `usage` error; one that does not answer is saved with a warning.
- `clear`: `{ "server", "removed": bool }`
- `show`: `{ "server", "proxy": url|null, "source": "LODE_PROXY"|"settings", "reachable":
  bool, "upstream": url|null }`

`status --remote` reports the proxy as `remote.proxy` (null without one).

### `lode whoami`

`{ "server", "name", "kind": "user"|"agent", "owner": name|null, "scopes": [..] }`

### `lode checkout <url>/<depot> [dir]`

`{ "workspace", "server", "depot", "branch", "rev", "files", "fetched", "downloaded", "bytes_downloaded", "bytes_reused", "obliterated", "resumed": bool }`

`files` counts the files in the view, `fetched` those placed by this run, `downloaded` the
distinct blobs fetched from the server. `bytes_downloaded` is the content received from the
server and `bytes_reused` the content found locally instead: the shared cache, and for
large files the chunks already in the workspace or left by an interrupted run (0.3).

### `lode init <url>/<depot> [--template T] [--dry-run] [-m MSG]`

Run in the folder to import. `{"depot","server","workspace","template","rev","files","resumed"}`:
`rev` is the import revision (2 for a new depot), `files` the number of files committed (`null`
when a rerun found the commit already done). With `--dry-run`:
`{"dry_run":true,"root","template","files","bytes","writes":[...],"invalid":[...]}`, where
`writes` names the template files that would be written and `invalid` the paths a depot would
refuse. Codes: `depot_not_empty` (the depot has revisions), `already_a_workspace`, `usage`
(unknown template, exit 2).

### `lode import git <repo> <url>/<depot> --branch B`

`{"depot","server","git_branch","total","already","imported","skipped","rev","last_commit",
"lfs_replaced","lfs_missing":[...],"submodules":[...]}`: `total` is the number of commits in
the branch's first-parent history, `already` how many an earlier run imported, `imported` the
revisions this run made, `skipped` the commits that change nothing a depot holds (empty or
submodule-only), `rev` the branch head and `last_commit` the last Git commit now in the depot.
`lfs_missing` lists files whose LFS content is not in the local store (imported as pointers);
`submodules` lists submodule paths, which are not imported. Each revision's `provenance` has
`imported: {"system":"git","id","author","date"}`. Codes: `git_not_found`,
`not_a_git_repository`, `unknown_git_branch`, `git_failed`, `depot_not_empty` (the depot has
revisions that were not imported), `import_diverged` (the last imported commit is not in the
branch's history), `invalid_path`, `import_mismatch`.

### `lode import svn <url> <url>/<depot>`

`{"depot","server","url","total","already","imported","skipped","rev","last_revision",
"externals":[...]}`: `total` is the number of SVN revisions that change the directory,
`already` how many an earlier run covered, `imported` the revisions this run made, `skipped`
the SVN revisions that change nothing a depot holds (empty folders, properties only), `rev` the
branch head and `last_revision` the last SVN revision now in the depot. `externals` lists
folders with `svn:externals` (relative to the directory, `.` for itself), which are not
imported. Each revision's `provenance` has `imported: {"system":"svn","id":"<revision>",
"author","date"}`. Codes: `svn_not_found`, `not_an_svn_directory`, `svn_failed`,
`depot_not_empty`, `import_diverged` (the depot was imported from a later revision than the
repository has), `invalid_path`.

### `lode self-update [--check]`

`{"current","latest","update_available","updated","path"}`: `path` is this executable,
`updated` whether it was replaced. Codes: `package_managed` (installed by winget, the MSI,
Homebrew or a Linux package; the message names the command to use), `checksum_mismatch` (the
download does not match `SHA256SUMS`; nothing changed), `update_failed`. `LODE_UPDATE_URL`
replaces the release list URL (a GitHub-style `releases` JSON).

### `lode view show|include|exclude`

- `show`: `{ "view": { "include": [..], "exclude": [..] } }`
- `include`, `exclude`: `{ "view": {..}, "added": n, "removed": n }`

### `lode status [--unchanged] [--remote] [paths]`

```json
{
  "branch": "main",
  "rev": 12,
  "merges": [ { "branch": "feat", "from_rev": 8, "to_rev": 11 } ],
  "files": [
    { "path": "c/2.txt", "status": "added", "copy_from": { "path": "a/2.txt", "rev": 7 },
      "moved": true, "changelist": null, "locked": false, "needs_lock": false,
      "lock_required": false }
  ],
  "invalid": [ { "local": "/abs/path", "reason": "…" } ],
  "remote": {
    "head_rev": 14,
    "proxy": "https://lode-office.example.com/",
    "files": [
      { "path": "Content/Hero.uasset", "newer": false, "deleted_at_head": false,
        "locked_by": { "name": "bob", "kind": "user" }, "lock_workspace": "…", "mine": false }
    ]
  }
}
```

Only changed paths are listed, unless `--unchanged` is given: then every tracked file in scope
is listed, unchanged ones with status `unchanged`. `lock_required` is true for any file that
matches `[locks].required`, whatever its state.
`remote` is present only with `--remote`. It lists the files in scope that have a newer
version at the branch head (`newer`; `deleted_at_head` when the head deleted it) or a lock
held by anyone (`mine` when the lock is this workspace's own); files with neither are left
out. It costs one request for the branch, at most one for the changes since the workspace's
revision (none when it is at the head) and one for the locks, however many paths are named.
Without the locks module, no locks are reported. `moved` is true when the origin is deleted in the same change.
`locked` is true when this workspace holds the file's lock; `needs_lock` is true for a changed
file that matches `[locks].required` and is not locked here (the commit would be refused).
`merges` lists the merge waiting for the next commit (`lode merge`), empty when there is none.

### `lode add <paths>`

`{ "added": [paths] }`

### `lode rm <paths>`

`{ "removed": [paths], "kept_on_disk": bool }`

### `lode mv <src…> <dest>` / `lode cp <src…> <dest>`

`{ "moved": [ { "from", "to" } ] }` / `{ "copied": [ { "from", "to" } ] }`

### `lode revert <paths> | --all | --merge`

`{ "reverted": [paths], "merges_abandoned": [ { "branch", "from_rev", "to_rev" } ] }`

`--merge` forgets the pending merge and leaves the files as they are (name paths too to revert
them); `--all` reverts everything and forgets the merge as well.

### `lode clean [--ignored] [--dry-run] [paths]`

`{ "removed": [paths], "dry_run": bool }`. Directories end with `/`.

### `lode diff [paths] [--stat]`

```json
{
  "files": [
    { "path", "status", "old_path": path|null, "binary": bool,
      "old_size": n|null, "new_size": n|null, "old_mode": "100644"|null, "new_mode": "100755"|null,
      "insertions": n, "deletions": n, "patch": "diff --lode a/x b/x\n…" }
  ],
  "insertions": n,
  "deletions": n
}
```

`patch` is left out with `--stat`. Modes are `100644`, `100755` or `120000` (symlink).

### `lode commit`

`{ "rev", "branch", "files", "uploaded", "bytes_uploaded", "bytes_reused", "replayed": bool, "resumed": bool }`

`uploaded` counts the blobs the server lacked; `bytes_uploaded` is the content sent and
`bytes_reused` what the server already had, which for a large file is every chunk that did
not change (0.3).

`resumed` is true when the command finished an earlier commit whose answer was lost
(`replayed` then tells whether the server had already made the revision).

`--keep-locks` keeps this workspace's locks on the committed files; without it they are
released by the commit.

With a merge pending the commit records it as `merged_from` and may have no file changes. It
must then include everything: paths and `-c` are a usage error. The default message is
`Merge <branch> r<from>..r<to>`.

### `lode lock <paths…> [-m COMMENT]` / `lode unlock <paths…> [--force]`

`{ "locked": [paths] }` and `{ "released": [paths] }`. A lock is all or nothing: if any file
is locked by someone else the command fails with `locked` (`details.paths`), and a file that is
older locally than at the branch head fails with `out_of_date` (run `lode update` first).
`--force` breaks someone else's lock and needs depot admin. On a server without the locks
module, `lock`, `unlock` and `locks` fail with `module_disabled`.

### `lode locks [--mine] [path-prefix]`

`{ "locks": [ { "path", "branch", "owner": { "name", "kind" }, "workspace", "comment",
"created_at", "mine": bool } ] }`; `comment` is left out when there is none. `mine` is true for locks held by this
workspace.

### `lode changelist new|list|move|delete`

- `new <name> [-m DESC]`: `{ "changelist" }`
- `list`: `{ "changelists": [ { "name", "description", "files": [paths] } ] }`, `default`
  first. `files` are the changed files of each changelist.
- `move <name> <paths…>`: `{ "changelist", "moved": [paths] }`. Moving one half of a move
  (`lode mv`) takes the other half along.
- `delete <name>`: `{ "deleted", "files": n }`; the files return to `default`.

`add`, `rm`, `mv`, `cp` take `-c NAME`; `commit -c NAME` commits only that changelist (use
`default` for files in none) and takes the changelist's description as the message when
neither `-m` nor `-F` is given.

### `lode shelve [paths] -m DESC [-c NAME] [--keep] [--update ID]`

`{ "shelf": id, "link": "lode://…/shelf/ID" | null, "version": n, "files": n, "uploaded": n,
"bytes_uploaded": n, "bytes_reused": n, "kept": bool, "updated": bool,
"merged_from": [ { "branch", "from_rev", "to_rev" } ] }`

`link` (T176) is the shelf's `lode://` link, to paste for a reviewer; `null` when the server
address has a path, which links cannot carry. The local MCP server's `shelve` tool returns
the same document.

A pending merge goes into the shelf's `merged_from` (and leaves the workspace unless `--keep`);
it must then be shelved whole, like a commit.

The changes are uploaded and checked against the branch head like a commit (`out_of_date`
means: run `lode update` first), then reverted in the workspace unless `--keep`. The
description defaults to the changelist's (`-c`) or, with `--update`, the shelf's.

### `lode shelves [--mine | --user U] [--branch B] [--state open|submitted|deleted]`

`{ "shelves": [ { "id", "version", "branch", "base_rev", "owner": { "name", "kind" },
"agent_owner"?, "description", "state", "submitted_rev"?, "created_at", "updated_at" } ] }`,
newest first, open shelves by default.

### `lode shelf show|diff|delete|submit <id>`

- `show`: the shelf as above plus `"changes": [ { "path", "action", "base_id", "new_id",
  "kind", "executable", "size", "copy_from" } ]`.
- `diff`: as `show`, plus `"diff": [ { "path", "patch" } | { "path", "skipped" } ]`
  (`skipped`: `binary`, `symlink`, `too_large`, `missing`).
- `delete`: `{ "deleted": id }`.
- `submit <id>@<version> [-m MSG]`: `{ "rev", "branch", "shelf", "version", "replayed": bool }`.
  The version is the one reviewed; a shelf changed since fails with `conflict` (the message
  names the current version). Without `@<version>` the shelf is shown and confirmed in a
  terminal; with `--json` or without a terminal that is a usage error. `replayed` is true when
  the shelf was already submitted at that version. The author is the shelf's owner.

A submit refused by the checks gate exits 1 with code `checks_required`; `details` is
`{ "shelf_id", "shelf_version", "missing": [ { "name", "reporter", "reason", "state"?,
"reported_by"? } ] }` (`reason`: `absent`, `not_success`, `wrong_reporter`), and the human
message lists each unmet check on its own line.

### `lode completions <shell>`

Prints the completion script for `bash`, `zsh`, `fish`, `powershell` or `elvish` to stdout,
also with `--json` (the script is the output). `loded completions` does the same for the
server.

### `lode link PATH[@REV] [-b BRANCH] | -r REV | --shelf ID [--depot D]`

Prints the `lode://` link Lode Desk would copy (T176), using the workspace's server and depot
(`--server`/`--depot` outside one). A file without `@REV` or `--branch` links the revision the
workspace is at (outside a workspace: the head of `main`); with `--branch`, the head of that
branch. Plain-HTTP servers get `?http=1`. Nothing is asked of the server.

`{ "kind": "file"|"revision"|"shelf", "server", "depot", "path"?, "rev"?, "branch"?, "id"?,
"link" }`. A server under a path is `bad_link` (exit 1).

### `lode checks <rN | ID[@VERSION]> [--depot D]`

`{ "target": { "kind": "revision"|"shelf", "id", "version"? }, "checks": [ { "target",
"name", "state", "summary"?, "url"?, "reporter": { "name", "kind", "owner"? }, "updated_at" } ] }`,
most recently reported first. `state` is `pending`, `success`, `failure`, `error` or
`cancelled`. A shelf without `@VERSION` means its current version. Outside a workspace give
`--server` (or `LODE_SERVER`) and `--depot`. A server without the checks module answers
`module_disabled` (exit 1).

### `lode unshelve <id>`

```json
{
  "shelf": 7, "version": 2,
  "files": [ { "path": "a.txt", "outcome": "merged" } ],
  "counts": { "merged": 1 },
  "merged_from": []
}
```

Outcomes: `applied`, `merged`, `conflicted`, `unchanged` (already as on the shelf), `skipped`
(outside the view). A shelf that carries a merge makes it pending again here, and is refused
(`merge_pending`) while another merge is pending. A conflict is settled with `lode resolve`; `--theirs` takes the shelf's
side. A binary conflict leaves the shelf's content in `<file>.theirs-shelf`.

### `lode update [-r REV]`

```json
{
  "rev": 12, "from": 9, "full": false,
  "files": [ { "path": "a.txt", "outcome": "merged" } ],
  "counts": { "merged": 1 },
  "downloaded": 0, "bytes_downloaded": 0, "bytes_reused": 0,
  "obliterated": []
}
```

Outcomes: `added`, `updated`, `merged`, `conflicted`, `deleted`, `kept` (no longer visible
after a full sync and modified locally: left on disk, untracked), `obliterated` (the content
at that revision was obliterated on the server: left out, and listed in `obliterated`; an
older tracked copy is removed). `full` is true when the access epoch changed and the whole
manifest was compared. `checkout` and `view` changes list obliterated files the same way.

### `lode branch list [--all]` / `create <name> [--from B[@REV]] [--switch]` / `archive <name>`

A branch is `{ "name", "head_rev", "created_rev", "parent": { "branch", "rev" } | null,
"state": "active"|"archived" }`. `list` prints `{ "current", "branches": [branch] }`;
`create` prints the branch (from this workspace's branch at its revision by default), plus
`"switch"` with the switch result when `--switch`; `archive` prints the branch.

### `lode switch <branch> [-r REV]`

The `update` result plus `"branch"`, `"switched_from"` (null when already on it) and
`"locks_left"` (paths whose locks stay on the old branch). Local changes are carried over as
an update carries them; a switch compares the target's full manifest with the workspace, so
it is right after commits too. An interrupted switch is finished by `lode switch` to the same
branch or `lode update`. `update` prints `"branch"`, `"switched_from": null` and
`"locks_left": []` too.

With `-r REV` the workspace goes straight to that revision of the branch without first
syncing its head; `REV` must be in the branch's history (`not_found` otherwise). Only blobs
of that revision are downloaded.

### `lode merge <branch> [-r REV]`

```json
{
  "source": "feat", "from_rev": 8, "to_rev": 11, "base": 7, "nothing_to_do": false,
  "files": [ { "path": "a.txt", "outcome": "merged" } ],
  "counts": { "merged": 1 }
}
```

Applies the source branch's changes from the merge base (`base`, SPEC §5.4) to its head, as
`unshelve` applies a shelf: outcomes `applied`, `merged`, `conflicted`, `unchanged`, `skipped`.
A binary conflict leaves the source's content in `<file>.theirs-r<to_rev>`; conflict markers
name it `<branch> r<to_rev>`. The range `from_rev..to_rev` is kept until the next commit
records it (see `lode status`, `lode revert --merge`). With nothing new on the source,
`nothing_to_do` is true and `from_rev`/`to_rev` are null. `-r REV` applies one revision's
change (`parent → REV`) and records `REV..REV`.

Refused with `conflicted` while conflicts remain, `merge_pending` while a merge is not yet
committed, and `pending_commit`/`update_incomplete` like `update`.

Known limit: after a single-revision pick a later full merge starts from the base before the
gap and applies the picked change again. Usually that is `unchanged`; if the file changed
since, it can conflict.

### `lode resolve <paths> --mine|--theirs|--merged`

`{ "resolved": [paths], "using": "mine"|"theirs"|"merged" }`

For a conflict left by `unshelve`, `--theirs` writes the shelf's content (or deletes the file)
over the workspace's base, so the file shows as modified (or deleted).

### `lode log [path]`

`{ "depot", "branch", "revisions": [revision], "more": bool }`

`more` is true when older revisions exist; `--before REV` lists only revisions older than
`REV`, so passing the last `rev` of one page gets the next.

Each revision is the server's revision object: `rev`, `branch`, `parent`, `author` and
`submitter` (`{ "name", "kind" }`), `created_at` (RFC 3339, UTC), `message`, `root_tree`,
`provenance`, `merged_from`, and with `-v` `changes` (`[{ "path", "action":
"add"|"modify"|"delete", "kind", "old_id", "new_id", "size", "executable", "copy_from" }]`)
and `changes_truncated`.

### `lode cat <path>[@rev]`

`{ "path", "rev", "id", "kind", "executable", "size", "content": text }`, or
`"content_base64"` in place of `content` when the content is not UTF-8. Without `--json` the
raw bytes go to stdout.

With `-o FILE` the content is written to `FILE` (through a temporary file next to it, so it
is whole or absent) and nothing else goes to stdout; with `--json` the result is
`{ "path", "rev", "id", "size", "output" }`, without content. Use it for binary files, which
cannot pass through stdout safely on Windows.

### `lode blame <path>[@rev]`

```json
{
  "path": "b.txt", "rev": 14, "id": "…",
  "lines": [ { "rev": 12, "text": "one" } ],
  "revisions": [ { "rev": 12, "branch": "main", "author": { "name", "kind" },
                   "created_at": "…", "summary": "first line of the message" } ],
  "truncated": false
}
```

Each line names the revision that last changed it. The file's history is followed through
`copy_from` (renames and copies) and the branch chain. `revisions` describes every revision
named, newest first. `truncated` (left out when false) means the walk stopped early: more than
1000 versions, a version over 1 MiB or not text, or history the caller cannot read; the oldest
version looked at then takes the remaining lines (marked `^` in human output). A binary file or
a symlink is `invalid_request`; a file over 1 MiB is `too_large`.

### `lode ls [path][@rev]`

`{ "depot", "rev", "path": dir|null, "entries": [ { "name", "kind", "id", "size", "executable" } ] }`

`executable` appears only when true. Listing a file gives its directory with that one entry.

### `lode mcp [--agent-model M] [--agent-session S] [--agent-tool T]`

Serves the workspace over MCP on stdin and stdout until stdin closes; prints nothing else to
stdout. Each tool's result is the JSON above for the matching command, without `ok`; a
failure is a tool error carrying `{code, message, details?}`. See `docs/MCP.md`.

### `lode admin …`

Server administration through the API; the token needs the `admin` scope (an agent's owner
may also manage that agent's tokens).

| Command | `--json` output |
| --- | --- |
| `depot create <name>` / `depot list` | the depot / `{ "depots": [ { "name", "created_at", "revisions"?, "last_commit_at"?, "size_bytes"? } ] }`; the last three for server administrators (T179), and the text output is then a table |
| `user create <name> [--display-name D] [--sso IDENTITY] [--password-stdin]` | the principal, with `"sso"` when linked; with `--sso` no password is asked for unless `--password-stdin` is given |
| `user sso <name> <identity>` / `user sso <name> --remove` | the principal; an identity linked to someone else is `conflict` |
| `user list` / `agent list` | `{ "principals": [principal] }`, users or agents only |
| `user disable <name> [--enable]` / `agent disable …` | the principal |
| `user passwd <name> [--password-stdin]` | the principal |
| `agent create <name> --owner <user> [--display-name D] [--grant DEPOT=LEVEL]… [--with-token [--scope S]]` | the principal, plus `"rules": [rule]` with `--grant` (allow LEVEL on every branch and path of DEPOT) and `"token"` (as `token create`) with `--with-token`; the token alone goes to stdout in text mode. A failure after the agent exists says what was done |
| `group create <name>` / `group list` | `{ "name", "members" }` / `{ "groups": [ … ] }` |
| `group add|remove <group> <member>` | `{ "group", "added"|"removed" }` |
| `token create [--for P] [--name N] [--scope S,…] [--expires-in-days D]` | `{ "token", "info": token }` |
| `token list [--for P]` / `token revoke <id> [--for P]` | `{ "tokens": [token] }` / `{ "revoked": id }` |
| `acl list [--depot D] [--subject S]` | `{ "rules": [rule] }` in evaluation order; `--depot`: only the rules that can apply in D (its own and server-wide ones; an unknown depot is `not_found`); `--subject`: only that subject's (`user:NAME`, `group:NAME`, `all`) |
| `acl add <allow|deny> <read|write|admin> <subject> [--depot D] [--branch G] [--path G] [--check P B PATH]` | the rule, plus `"check"` with `--check` |
| `acl remove <id>` | `{ "removed": id }` |
| `acl check <principal> <depot> <branch> <path>` | `{ "principal", "depot", "branch", "path", "level": "read"|"write"|"admin"|null, "depot_admin"?, "matched"?, "decided_by"? }`: `depot_admin` (only when true) means an admin rule on the depot passes over path rules; `matched` lists the ids of the rules that apply, in order; `decided_by` is the rule that set the level, absent when none did or for the built-in `admin` |
| `gc <depot> [--dry-run]` / `fsck <depot> [--full]` | `{ "objects_checked", "objects_affected", "bytes_affected", "dry_run"?, "problems"? }` |
| `obliterate <depot> <path[@rev]>… --reason R [--dry-run] [--yes]` | `{ "dry_run", "objects": [ { "id", "size", "uses": [ { "rev", "branch", "path" } ] } ], "bytes", "in_use"? }`; `in_use` (`[ { "branch", "path", "id" } ]`) only on a dry run, otherwise it is a `conflict` with `details.in_use` |
| `obliterated <depot>` | `{ "obliterated": [ { "id", "size", "path", "reason", "by", "at" } ] }`, oldest first |
| `webhook add <depot> <url> --events E,… [--branch G] [--format lode\|slack\|discord\|telegram]` | `{ "hook": hook, "secret" }`; the secret goes to stdout once. `hook.format` is present unless it is `lode` |
| `webhook list <depot>` | `{ "webhooks": [ { "id", "depot", "url", "events", "branch"?, "active", "created_by", "created_at" } ] }` |
| `webhook remove <depot> <id>` | `{ "removed": id }` |
| `webhook deliveries <depot> <id>` | `{ "deliveries": [ { "id", "hook_id", "event", "state": "pending"|"ok"|"failed", "attempts", "next_attempt_at"?, "last_status"?, "last_error"?, "created_at" } ] }`, newest first |
| `webhook ping <depot> <id>` | the queued delivery |

A subject is `user:NAME` (or a bare name), `group:NAME` or `all`. Branch and path patterns
default to `**` and are checked before anything is sent (`usage` on a bad one). Passwords are
prompted for twice in a terminal, or read from stdin with `--password-stdin` (required with
`--json`). A new token's value goes to stdout once (alone on the line in human output, the
rest on stderr); listings never show values. `fsck` exits 1 with code `fsck_failed` and the
report as `details` when it finds problems. `obliterate` without `--dry-run` needs `--reason`,
and `--yes` when nobody can be asked (`usage` otherwise). `acl check` evaluates the rules as for a token
with every scope; a real token's scopes can only narrow that.

### Unattended machines

Build agents and other scripts drive a workspace with no person at the keyboard.

**Sign-in.** Set `LODE_SERVER` and `LODE_TOKEN_FILE` (a path to a file whose first line is a
token). They win over a stored `lode login`. A missing or unreadable token file is
`unauthenticated` (exit 3); there is no fallback to a stored login. No variable carries the
token itself, and the file's path is never written into the workspace. `--server` and a
workspace's own server still come before `LODE_SERVER`.

**Shared download cache.** `LODE_CACHE_DIR`, or `cache_dir` in the user's `settings.toml`,
names a directory that checkout, update and switch read before downloading and fill with
what they download. Several workspaces of one depot on one machine then download each blob
once. Entries are named by content id, written to a temporary file and renamed, and checked
against their id on every read: a damaged entry is deleted and downloaded again. Files are
always copied into the workspace, never linked, so a build that edits a file cannot harm the
cache. The cap is 20 GiB (`cache_max_gib` in `settings.toml` changes it); the least recently
used entries go when a sync finishes over the cap. Any number of `lode` processes may share
it. A cache problem is a warning, never a failed command.

```toml
# settings.toml in the user's lode config directory
cache_dir = "D:/lode-cache"
cache_max_gib = 50
```

**Reset sequence.** To bring a reused workspace to an exact state, whatever the last job left
behind (modified, added, deleted or untracked files, conflicts, a pending merge):

```
lode revert --all                 # undo every change, clear conflicts, drop a pending merge
lode clean                        # remove untracked files (add --ignored for build output too)
lode switch <branch> -r <rev>     # exactly that revision of that branch
```

Each step exits 0 on success. After it, `lode status --json` lists no files and reports the
branch and revision asked for. `lode clean` keeps ignored files unless given `--ignored`.

### External commands

`lode foo …` runs `lode-foo …` from `PATH` when `foo` is not a built-in command. It inherits
stdin, stdout and stderr, so its output is its own; `lode` adds nothing and exits with its
exit code. The environment carries `LODE_SERVER`, `LODE_DEPOT` and `LODE_WORKSPACE` (when in a
workspace), `LODE_TOKEN_FILE` (a path to the token, never the token itself) and `LODE_JSON=1`
when `--json` was given. With no such program, the result is a `usage` error (exit 2).
