# Working with Lode as an agent

Use this guide to take a task from a checkout or a remote session to a reviewed change. Examples use depot `game`, branch `main` and tracked file `Data/items.json`. Substitute your own values. Run workspace commands from the checkout root.

| Task | Guide |
| --- | --- |
| Edit files, prepare a shelf, review or recover after a failure | Follow the workflow below |
| Write a Python check or diagnose a blocked change | [Pre-commit and shelf validation](agent-validation.md) |
| Manage identities, CI, storage, backups or upgrades | [Server and automation operations](agent-operations.md) |
| Work with Desk or Unreal, change the product or verify a release | [Clients, development and releases](agent-clients.md) |
| Look up fields, tool inputs and permission rules | [CLI JSON](../lode/docs/CLI-JSON.md) · [MCP](../lode/docs/MCP.md) · [Specification](../lode/docs/SPEC.md) |

## Identify the working state

A **depot** has named branches. Each branch has a head, and revision numbers are allocated across the depot. A successful commit immediately creates a server revision. Concurrent edits to a touched file can cause an `out_of_date` refusal.

A **workspace** associates a directory with a server, depot and branch. It contains local edits, conflicts and named **changelists**. Its **view** determines which paths are present on disk. Inspect the view and file status before interpreting an absent file as a deletion.

A **shelf** is a proposal on the server. It has an owner, an ID and a version. Updating it increments the version. Submitting a reviewed version creates a revision recording both its author and its submitter.

## Connect and establish a baseline

| Interface | Use it for |
| --- | --- |
| CLI with `--json` | Scripted commands and editing files on a local disk |
| Local MCP, `lode mcp -C /work/game` over stdio | Workspace status, diffs, conflicts, validation and shelving |
| Remote MCP at `https://HOST/mcp` | Reading a depot and proposing a shelf without a checkout |
| Desk or Unreal | Handing off a proposal to a person or inspecting binary assets in their application |

Use an agent identity with its own token. Token scopes, enabled modules and depot ACLs determine the operations available. An agent does not inherit its owner's ACLs. Set `LODE_TOKEN_FILE` to the token file path. Record the identity from `whoami` in reports. Local MCP accepts `--agent-model`, `--agent-tool` and `--agent-session` for self-reported provenance. Ask the connected MCP server for its available tools and input schemas. Stdout on stdio is reserved for protocol messages. [MCP setup examples](../lode/docs/MCP.md).

For a new checkout, run `lode checkout https://lode.example.com/game game` and enter the new directory. In an existing checkout, start with these reads:

```sh
lode --json --version
lode --json whoami
lode --json view show
lode --json status --remote
lode --json diff --stat
```

Record the server, branch, base revision and paths already modified. Remote status helps identify newer changes and locks. Use `lode doctor` to locate a failing DNS, TLS, API or identity check. For history, use `lode log PATH`, `lode blame PATH@REV` and `lode cat PATH@REV`. Pin the revision when several reads must refer to one snapshot.

## Edit and prepare a shelf

1. If you need a newer base, run `lode update` and inspect the conflicts. For a lock-required file, check `lode locks` before editing. For example, `lode lock Content/Hero.uasset -m "Update collision"` requests a branch lock.
2. Edit tracked files normally. Use `lode add` for new files, `lode rm` for intentional deletions, and `lode mv` or `lode cp` to preserve ancestry. Investigate a `missing` status before scheduling a deletion.
3. Group the paths belonging to this task. Here the tracked file has already been edited:

```sh
lode changelist new content-fix -m "Fix item definitions"
lode changelist move content-fix Data/items.json
lode --json diff Data/items.json
lode --json validate -c content-fix
lode --json shelve -c content-fix --keep -m "Fix invalid item definitions"
```

Save the returned `shelf`, `version` and `link` when available. The example uses `--keep` to retain edits locally. Check status afterward. To revise a proposal, use `shelve --update ID` with the intended changes and description, then give the reviewer the new version.

## Change the view, switch branches and merge

Before narrowing a view, inspect `view show`, `status` and the diff for paths the new view excludes. If Lode reports pending edits, preserve them in a shelf or coordinate their disposition with their author. Verify both the view and the remaining edits after changing it.

Use `lode branch tree` to inspect branch relationships. `switch BRANCH` changes the workspace branch and carries pending changes. `merge SOURCE` integrates SOURCE into the current branch. Typed branches enforce stream direction. If a merge returns `conflict` with `details.reason = "outside_view"`, include the affected paths or use a full checkout of the target branch, then retry.

After merge, switch, update or unshelve, inspect conflicts. The local MCP tool `conflicts` with a path shows its sides. For text, compare base, mine and theirs, edit the result and run `lode resolve PATH --merged`. For binaries, choose the correct `--mine` or `--theirs` and open the result in its application. Explain that choice in the proposal. A pending merge must be shelved as a whole so the shelf preserves `merged_from`.

For a development stream, merge changes down from its parent first. Once the child is current, integrate its changes back up. A release stream accepts picked revisions from its parent and can send fixes back. Use `lode merge SOURCE -r REV` for a single source revision. `branch tree` shows parent relationships and how far streams are ahead or behind. Keep the tree and intended direction in the handoff if a flow rule refuses the operation.

For a text conflict in JSON, the configured JSON patterns allow a key-wise merge after a line merge conflicts. For an asset that requires a dedicated merge tool, inspect `lode merge-tool list` and use `lode resolve PATH --tool` when the tool is configured. The depot maps path patterns to tool names, while the local user's settings specify the executable. Inspect the resulting file before shelving it.

## Propose remotely and review a version

Without a checkout, use `list_dir` and `read_file` at a specific revision. Check for truncation before preparing a replacement. Give `create_shelf` the `base_rev` you read and the complete new content of each edited file. Use `update_shelf` for subsequent versions. Each call accepts up to 500 edits and 16 MiB of inline content by default. [Remote MCP limits](../lode/docs/MCP.md).

For example, if you receive shelf 17 version 2:

```sh
lode --json shelf show 17
lode --json shelf diff 17
lode --json checks 17@2
```

Confirm that the diff belongs to version 2. An authorized reviewer can run `lode --json shelf submit 17@2`. A version conflict calls for a fresh review of the updated diff and checks. A proposal-only task ends with the shelf ID, version, results and link. An empty check list does not establish that a check passed.

## Recover from errors and resume watching

In `--json` mode the CLI emits one JSON document on stdout. Read `ok`, the process exit code and, on failure, `error.code` and `error.details`. Keep stderr separate. Supported commands accept a UTF-8 path list through `--paths-from FILE`, one path per line. An empty list is refused. [CLI contract](../lode/docs/CLI-JSON.md).

| Signal | Next action |
| --- | --- |
| `out_of_date`, `conflicted` | Compare revisions, update, resolve conflicts and revalidate |
| `locked`, `lock_required` | Identify the branch and owner, then acquire a lock or coordinate with the owner |
| `permission_denied` | Inspect identity, scopes and ACLs. Use a shelf if permitted |
| `shelf_required`, `checks_required` | Create a shelf or inspect required results for its exact version |
| `validation_failed`, `validation_untrusted`, `python_missing` | Follow the [validation diagnostic table](agent-validation.md) |
| Connection lost during a write | Check history, status and owned shelves first. Inspect `replayed` when retrying commit or submit |
| `module_disabled` | Identify the needed module and report the limitation to the operator |

With the feed enabled, call `wait_for_events` without `after` to obtain a cursor. Pass each returned `next` as the next `after`. Supply the depot on remote MCP. Events contain identifiers. Read history, shelf details or checks separately. On `truncated: true`, reconcile history and current shelf states.

## Hand off the result

Report the depot, branch, base revision, changed paths and a shelf **with its version** or the committed revision. Include local and server results separately. Identify skipped checks, unresolved conflicts and retained edits. For example:

> game/main, based on r41. Edited Data/items.json. Shelf 17@2 is ready for review. Local json passed and server validate/json succeeded on version 2. Edits remain in the workspace.
