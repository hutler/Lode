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

### `lode add <paths>`

`{ "added": [paths] }`

### `lode rm <paths>`

`{ "removed": [paths], "kept_on_disk": bool }`

### `lode mv <src…> <dest>` / `lode cp <src…> <dest>`

`{ "moved": [ { "from", "to" } ] }` / `{ "copied": [ { "from", "to" } ] }`

### `lode revert <paths> | --all`

`{ "reverted": [paths] }`

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

`{ "shelf": id, "version": n, "files": n, "uploaded": n, "kept": bool, "updated": bool }`

The changes are uploaded and checked against the branch head like a commit (`out_of_date`
means: run `lode update` first), then reverted in the workspace unless `--keep`. The
description defaults to the changelist's (`-c`) or, with `--update`, the shelf's.

### `lode shelves [--mine | --user U] [--branch B] [--state open|submitted|deleted]`

`{ "shelves": [ { "id", "version", "branch", "base_rev", "owner": { "name", "kind" },
"agent_owner"?, "description", "state", "submitted_rev"?, "created_at", "updated_at" } ] }`,
newest first, open shelves by default.

### `lode shelf show|diff|delete <id>`

- `show`: the shelf as above plus `"changes": [ { "path", "action", "base_id", "new_id",
  "kind", "executable", "size", "copy_from" } ]`.
- `diff`: as `show`, plus `"diff": [ { "path", "patch" } | { "path", "skipped" } ]`
  (`skipped`: `binary`, `symlink`, `too_large`, `missing`).
- `delete`: `{ "deleted": id }`.

### `lode unshelve <id>`

```json
{
  "shelf": 7, "version": 2,
  "files": [ { "path": "a.txt", "outcome": "merged" } ],
  "counts": { "merged": 1 }
}
```

Outcomes: `applied`, `merged`, `conflicted`, `unchanged` (already as on the shelf), `skipped`
(outside the view). A conflict is settled with `lode resolve`; `--theirs` takes the shelf's
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

### `lode ls [path][@rev]`

`{ "depot", "rev", "path": dir|null, "entries": [ { "name", "kind", "id", "size", "executable" } ] }`

`executable` appears only when true. Listing a file gives its directory with that one entry.

### External commands

`lode foo …` runs `lode-foo …` from `PATH` when `foo` is not a built-in command. It inherits
stdin, stdout and stderr, so its output is its own; `lode` adds nothing and exits with its
exit code. The environment carries `LODE_SERVER`, `LODE_DEPOT` and `LODE_WORKSPACE` (when in a
workspace), `LODE_TOKEN_FILE` (a path to the token, never the token itself) and `LODE_JSON=1`
when `--json` was given. With no such program, the result is a `usage` error (exit 2).
