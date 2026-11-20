# Lode

**Lode** is an open source, self-hosted, centralized version control system for teams that keep
code and large binary files together, such as game studios: file locks, shelves, changelists,
partial checkouts, and AI agents as first-class users (an MCP endpoint and token scopes). Part
of SimpleDevOps. Apache-2.0 ([LICENSE](LICENSE)).

| Folder | What it is | Language | Start reading at |
| --- | --- | --- | --- |
| [lode/](lode/) | **Lode**, the core: the server `loded`, the command-line client `lode`, and the `lode-ops` library they share | Rust | [lode/README.md](lode/README.md), [lode/docs/SPEC.md](lode/docs/SPEC.md) |
| [desk/](desk/) | **Lode Desk**, the desktop client, built on a pinned version of the core | Rust | [desk/README.md](desk/README.md), [desk/docs/guide.md](desk/docs/guide.md) |
| [unreal/](unreal/) | **Lode for Unreal**, an Unreal Editor plugin that makes Lode a revision control provider | C++ | [unreal/docs/SPEC.md](unreal/docs/SPEC.md) |

Rules for agents working here are in [AGENTS.md](AGENTS.md).

## Status

Lode v1 (milestones M0 to M6), its packaging and 0.2 features (M7 to M12), Lode Desk with a
native Slint UI (M13), the server's admin web UI (M14), and chunked large files, Postgres and
a caching proxy (M15) are implemented. The current release is 0.3.0. Lode for Unreal
(T01 to T11) is implemented and builds on Unreal Engine 5.8. Status boards: [lode/docs/tasks/README.md](lode/docs/tasks/README.md),
[unreal/docs/TASKS.md](unreal/docs/TASKS.md).

## How the parts fit

- **Lode** is complete on its own: `loded` serves depots over HTTPS, `lode` works with
  workspaces, and webhooks and checks connect any build server.
- **Lode Desk** uses the core's `lode-ops` library, at a core version pinned in
  `desk/Cargo.toml`. The core changes and settles first; Desk moves to the new core in its own
  change. It has its own version, CI (`desk.yml`) and release (`desk-v*` tags). Lode never
  depends on Desk.
- **Lode for Unreal** runs the `lode` command and reads its JSON output. The contract is
  section 5 of [unreal/docs/SPEC.md](unreal/docs/SPEC.md). Lode never names Unreal.
