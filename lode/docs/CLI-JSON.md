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
| 1 | operation failed | `not_found`, `out_of_date`, `conflict`, `conflicted`, `locked`, `lock_required`, `local_changes`, `nothing_to_commit`, `missing`, `aborted`, `not_a_workspace`, `already_a_workspace`, `not_empty`, `workspace_in_use`, `checkout_incomplete`, `pending_commit`, `ignored`, `outside_view`, `unsafe_path`, `changed_during_commit` |
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
      "moved": true, "changelist": null, "locked": false }
  ],
  "invalid": [ { "local": "/abs/path", "reason": "…" } ]
}
```

Only changed paths are listed. `moved` is true when the origin is deleted in the same change.

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
