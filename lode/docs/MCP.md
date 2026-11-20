# Lode and MCP

Lode speaks the Model Context Protocol in two places (SPEC §6.4):

- **`lode mcp`**, a local server over stdio. It works on one workspace, for a coding agent
  on a developer's machine: status, diff, add, remove, move, revert, update, lock, unlock,
  shelve, unshelve, commit, log and read_file_at_revision, and the tools that let it act as a
  desktop client: branches, locks, conflicts, switch, merge, resolve, changelist and
  wait_for_events.
- **`/mcp` on the server**, streamable HTTP with a bearer token. It needs no workspace: read
  depots, branches, history, files and diffs, list locks and shelves, and propose changes as
  shelves (`create_shelf`, `update_shelf`), which a person reviews and submits.

Every tool answers structured JSON. Errors carry the same stable `code` as the HTTP API and
`--json` (`not_found`, `permission_denied`, `out_of_date`, …).

## Tokens for agents

Give an agent its own identity rather than your token, so revisions show who did the work:

```sh
# An admin creates the agent, lets it read the depot and prints its token (once);
# no --scope means read, shelve and lock:
lode admin agent create fixer --owner ana --grant game=read --with-token
# Later its owner (or an admin) can make more tokens:
lode admin token create --for fixer --name laptop
```

An agent's owner can create and revoke its tokens. By default an agent can read, lock and
shelve but not commit: its work arrives as shelves for a person to review. Only an admin can
grant it `write`. The agent needs access rules of its own; it never inherits its owner's.

## `lode mcp` (local, stdio)

Run it in a workspace, or point it at one with `-C`. It uses the token the workspace already
uses (`lode login`, or `LODE_TOKEN_FILE`). `commit` is listed only when that token has the
`write` scope.

```sh
lode mcp -C /work/game --agent-model <model> --agent-tool <tool name>
```

`--agent-model`, `--agent-session` and `--agent-tool` (or `LODE_AGENT_MODEL`,
`LODE_AGENT_SESSION`, `LODE_AGENT_TOOL`) are recorded as the `agent` provenance of every
shelf and commit made through it. They are self-reported and shown, never trusted.

Paths in tool input are relative to the workspace root; anything that leaves the workspace
(`..`, an absolute path, a link pointing out) is refused with `outside_workspace`. Stdout
carries only protocol messages.

**Claude Code**

```sh
claude mcp add lode -- lode mcp -C /work/game --agent-tool claude-code
```

**Claude Desktop, Cursor and others using an `mcpServers` file**

```json
{
  "mcpServers": {
    "lode": {
      "command": "lode",
      "args": ["mcp", "-C", "/work/game", "--agent-tool", "desktop"],
      "env": { "LODE_TOKEN_FILE": "/home/ana/.config/lode/fixer.token" }
    }
  }
}
```

**VS Code** (`.vscode/mcp.json` in the workspace)

```json
{
  "servers": {
    "lode": {
      "type": "stdio",
      "command": "lode",
      "args": ["mcp", "-C", "${workspaceFolder}", "--agent-tool", "vscode"]
    }
  }
}
```

### Tools that make the agent a desktop client

| Tool | Input | What it does |
| --- | --- | --- |
| `status` | `paths?`, `remote?` | Local changes and a `summary` of counts; with `remote`, also what is newer at the branch head and who holds locks |
| `branches` | `archived?` | The depot's branches and which one the workspace is on |
| `locks` | `paths?` (one), `mine?` | Locks on the workspace's branch |
| `conflicts` | `path?`, `max_bytes?` | Without `path`, the conflicted files; with it, `base`, `mine`, `theirs` and `result`. Text sides come as `text` (cut at 256 KiB, then `cut: true`); binary sides as `size` and `binary: true` only, with the incoming copy on disk at `theirs_file` |
| `switch` | `branch`, `rev?` | Moves the workspace to another branch, carrying local changes |
| `merge` | `branch`, `rev?` | Merges another branch; what cannot be merged is left in conflict |
| `resolve` | `path`, and `take` (`mine`, `theirs`, `as_is`) or `text`/`base64` | Settles one conflicted file (content at most 16 MiB) |
| `changelist` | `action` (`new`, `list`, `move`, `delete`), `name?`, `description?`, `paths?` | Named groups of pending changes |
| `wait_for_events` | `after?`, `kinds?`, `timeout_s?`, `depot?` | See "Watching a depot" below |

`wait_for_events` runs on a worker of its own, so other tools answer while it waits.

### A working day

What a person asks for in chat, and the calls an agent makes:

- "Check out the release branch and tell me what came in": `status` (nothing pending), `switch`
  to `release`, then `log` for the revisions since the old one.
- "Lock everything I will edit today": `status` with `remote` (who holds what, what is newer),
  `update` if something is, then `lock` the files, with a comment.
- "Assemble today's changes into a shelf with a proper description": `status`, `diff`,
  `changelist` to set aside what does not belong, then `shelve` with a description. The
  person reviews the shelf in Lode Desk and submits it.

### Merging and conflicts

The agent proposes a merge the same way it proposes any change, as a shelf:

1. `merge` (or `update`, `switch`, `unshelve`); its answer lists the conflicted files.
2. `conflicts` lists them; `conflicts` with a `path` gives the three sides.
3. `resolve` each file, with new content or by taking a side. For a binary file it can only
   take `mine` or `theirs`.
4. `shelve` with a description of each choice. A shelf made during a merge carries the merge,
   and when a person submits it the revision records `merged_from` (SPEC §6.2).

### Watching a depot

With the feed module on (the default), `wait_for_events` answers the events of the depot after
a cursor: `revision.created`, `branch.created`, `branch.archived`, `lock.acquired`,
`lock.released`, `shelf.created`, `shelf.updated`, `shelf.submitted`, `shelf.deleted` and
`check.updated`, each with identifiers only. Call it once without `after` to get the current
cursor, then pass each answer's `next` as `after`. It answers at once when there are events,
otherwise it waits up to `timeout_s` (default 25, at most 50). Read details with `log`,
`locks`, `get_shelf` or `list_checks`. `truncated: true` means events after the cursor were
dropped (they are kept 7 days): read the history again.

The loop belongs to the agent runtime, not to Lode: Lode answers one call at a time.

### Keeping agents on shelves

An agent token without `write` can only propose. If an agent must have `write` (for example to
commit on its own branch), keep its work on shelves for protected branches with one rule for
every agent:

```sh
lode admin acl add deny write agents --depot game --branch main
```

On `main` agents can still read and shelve, but not commit, submit or lock (a lock needs
write access); a person submits their shelves.

## `/mcp` (server, streamable HTTP)

The endpoint is `https://<server>/mcp` with `Authorization: Bearer <token>`. It is on unless
`[modules] mcp = false`. Tools are listed by the token's scopes and by which modules are
enabled; changes made through it record `via = "mcp"`.

**Claude Code**

```sh
claude mcp add --transport http lode https://lode.example.com/mcp \
  --header "Authorization: Bearer lode_…"
```

**Clients that take a URL and headers**

```json
{
  "mcpServers": {
    "lode": {
      "url": "https://lode.example.com/mcp",
      "headers": { "Authorization": "Bearer lode_…" }
    }
  }
}
```

Client configuration formats change; check your client's documentation if a snippet does not
work.

### The intended workflow

1. The agent reads with `list_dir`, `read_file`, `log`, `diff`, `blame` and `search_history`.
2. It proposes changes with `create_shelf`, giving whole new file contents and the revision it
   read them at as `base_rev`, and refines them with `update_shelf`.
3. A person reviews: `lode shelf diff <id>` or `lode unshelve <id>` to try it locally.
4. The person submits the version they reviewed: `lode shelf submit <id>@<version>`. The
   revision's author is the agent, its submitter the person.

Limits per `create_shelf`/`update_shelf` call: 500 edits and 16 MiB of inline content
(`[limits] mcp_max_edits`, `mcp_max_inline_bytes`). Larger work goes through the HTTP blob API.
`read_file` and `diff` output is capped (256 KiB by default) and says when it was cut.

`wait_for_events` works on `/mcp` as in `lode mcp`, with `depot` required.
