# Lode CLI: JSON output

With `--json`, every `lode` command prints exactly one JSON document to stdout and nothing
else. Progress, prompts and warnings go to stderr or are left out, and a command that would
need to ask something fails instead (exit code 2). This page lists the document of each
command. Fields may be added in later versions; existing fields keep their meaning.

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
| 3 | authentication or permission | `unauthenticated`, `permission_denied` |
| 4 | network or server | `network`, `internal`, `protocol`, `hash_mismatch`, any 5xx |

Error codes from the server (`lode_core::ErrorCode`) are passed through unchanged.

## Common values

- Paths are depot paths: `/`-separated, relative to the depot root.
- Ids are 64 hex digits (BLAKE3).
- `status` values: `modified`, `added`, `deleted`, `missing`, `conflicted`, `untracked`.
- `kind` values: `file`, `symlink`, `dir`.

## Commands

### `lode --version`

`{ "version": "0.1.0" }`

### `lode login <url>`

Needs `--name` with `--password-stdin`, or `--token-stdin`.

`{ "server", "name", "kind": "user"|"agent", "scopes": [..], "token_file" }`

### `lode logout`

`{ "server", "removed": bool, "revoked": bool }` (`revoked` absent when nothing was stored)

### `lode whoami`

`{ "server", "name", "kind": "user"|"agent", "owner": name|null, "scopes": [..] }`

### `lode checkout <url>/<depot> [dir]`

`{ "workspace", "server", "depot", "branch", "rev", "files", "fetched", "downloaded", "resumed": bool }`

`files` counts the files in the view, `fetched` those placed by this run, `downloaded` the
distinct blobs fetched from the server.

### `lode view show|include|exclude`

- `show`: `{ "view": { "include": [..], "exclude": [..] } }`
- `include`, `exclude`: `{ "view": {..}, "added": n, "removed": n }`

### `lode status [paths]`

```json
{
  "branch": "main",
  "rev": 12,
  "merges": [ { "branch": "feat", "from_rev": 8, "to_rev": 11 } ],
  "files": [
    { "path": "c/2.txt", "status": "added", "copy_from": { "path": "a/2.txt", "rev": 7 },
      "moved": true, "changelist": null, "locked": false, "needs_lock": false }
  ],
  "invalid": [ { "local": "/abs/path", "reason": "…" } ]
}
```

Only changed paths are listed. `moved` is true when the origin is deleted in the same change.
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

`{ "rev", "branch", "files", "uploaded", "replayed": bool, "resumed": bool }`

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

`{ "shelf": id, "version": n, "files": n, "uploaded": n, "kept": bool, "updated": bool,
"merged_from": [ { "branch", "from_rev", "to_rev" } ] }`

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
  "downloaded": 0
}
```

Outcomes: `added`, `updated`, `merged`, `conflicted`, `deleted`, `kept` (no longer visible
after a full sync and modified locally: left on disk, untracked). `full` is true when the
access epoch changed and the whole manifest was compared.

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

Each revision is the server's revision object: `rev`, `branch`, `parent`, `author` and
`submitter` (`{ "name", "kind" }`), `created_at` (RFC 3339, UTC), `message`, `root_tree`,
`provenance`, `merged_from`, and with `-v` `changes` (`[{ "path", "action":
"add"|"modify"|"delete", "kind", "old_id", "new_id", "size", "executable", "copy_from" }]`)
and `changes_truncated`.

### `lode cat <path>[@rev]`

`{ "path", "rev", "id", "kind", "executable", "size", "content": text }`, or
`"content_base64"` in place of `content` when the content is not UTF-8. Without `--json` the
raw bytes go to stdout.

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
| `depot create <name>` / `depot list` | the depot / `{ "depots": [ { "name", "created_at" } ] }` |
| `user create <name> [--display-name D] [--password-stdin]` | the principal |
| `user list` / `agent list` | `{ "principals": [principal] }`, users or agents only |
| `user disable <name> [--enable]` / `agent disable …` | the principal |
| `user passwd <name> [--password-stdin]` | the principal |
| `agent create <name> --owner <user> [--display-name D]` | the principal |
| `group create <name>` / `group list` | `{ "name", "members" }` / `{ "groups": [ … ] }` |
| `group add|remove <group> <member>` | `{ "group", "added"|"removed" }` |
| `token create [--for P] [--name N] [--scope S,…] [--expires-in-days D]` | `{ "token", "info": token }` |
| `token list [--for P]` / `token revoke <id> [--for P]` | `{ "tokens": [token] }` / `{ "revoked": id }` |
| `acl list` | `{ "rules": [rule] }` in evaluation order |
| `acl add <allow|deny> <read|write|admin> <subject> [--depot D] [--branch G] [--path G] [--check P B PATH]` | the rule, plus `"check"` with `--check` |
| `acl remove <id>` | `{ "removed": id }` |
| `acl check <principal> <depot> <branch> <path>` | `{ "principal", "depot", "branch", "path", "level": "read"|"write"|"admin"|null }` |
| `gc <depot> [--dry-run]` / `fsck <depot> [--full]` | `{ "objects_checked", "objects_affected", "bytes_affected", "dry_run"?, "problems"? }` |
| `webhook add <depot> <url> --events E,… [--branch G]` | `{ "hook": hook, "secret" }`; the secret goes to stdout once |
| `webhook list <depot>` | `{ "webhooks": [ { "id", "depot", "url", "events", "branch"?, "active", "created_by", "created_at" } ] }` |
| `webhook remove <depot> <id>` | `{ "removed": id }` |
| `webhook deliveries <depot> <id>` | `{ "deliveries": [ { "id", "hook_id", "event", "state": "pending"|"ok"|"failed", "attempts", "next_attempt_at"?, "last_status"?, "last_error"?, "created_at" } ] }`, newest first |
| `webhook ping <depot> <id>` | the queued delivery |

A subject is `user:NAME` (or a bare name), `group:NAME` or `all`. Branch and path patterns
default to `**` and are checked before anything is sent (`usage` on a bad one). Passwords are
prompted for twice in a terminal, or read from stdin with `--password-stdin` (required with
`--json`). A new token's value goes to stdout once (alone on the line in human output, the
rest on stderr); listings never show values. `fsck` exits 1 with code `fsck_failed` and the
report as `details` when it finds problems. `acl check` evaluates the rules as for a token
with every scope; a real token's scopes can only narrow that.

### External commands

`lode foo …` runs `lode-foo …` from `PATH` when `foo` is not a built-in command. It inherits
stdin, stdout and stderr, so its output is its own; `lode` adds nothing and exits with its
exit code. The environment carries `LODE_SERVER`, `LODE_DEPOT` and `LODE_WORKSPACE` (when in a
workspace), `LODE_TOKEN_FILE` (a path to the token, never the token itself) and `LODE_JSON=1`
when `--json` was given. With no such program, the result is a `usage` error (exit 2).
