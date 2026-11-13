# Lode and MCP

Lode speaks the Model Context Protocol in two places (SPEC §6.4):

- **`lode mcp`**, a local server over stdio. It works on one workspace, for a coding agent
  on a developer's machine: status, diff, add, remove, move, revert, update, lock, unlock,
  shelve, unshelve, commit, log and read_file_at_revision.
- **`/mcp` on the server**, streamable HTTP with a bearer token. It needs no workspace: read
  depots, branches, history, files and diffs, list locks and shelves, and propose changes as
  shelves (`create_shelf`, `update_shelf`), which a person reviews and submits.

Every tool answers structured JSON. Errors carry the same stable `code` as the HTTP API and
`--json` (`not_found`, `permission_denied`, `out_of_date`, …).

## Tokens for agents

Give an agent its own identity rather than your token, so revisions show who did the work:

```sh
# An admin creates the agent once, and lets it read the depot:
lode admin agent create fixer --owner ana
lode admin acl add allow read user:fixer --depot game
# Its owner (or an admin) makes it a token; no scopes means read, shelve and lock.
# The token is printed once:
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
