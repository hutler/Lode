# Lode

**Lode** is an open source, self-hosted, centralized version control system for teams that keep
code and large binary files together, such as game studios: file locks, shelves, changelists,
partial checkouts, and AI agents as first-class users (an MCP endpoint and token scopes). Part
of SimpleDevOps. Apache-2.0 ([LICENSE](LICENSE)).

| Folder | What it is | Language | Start reading at |
| --- | --- | --- | --- |
| [lode/](lode/) | **Lode**: the server `loded`, the command-line client `lode`, and **Lode Desk**, the desktop client | Rust | [lode/README.md](lode/README.md), [lode/docs/SPEC.md](lode/docs/SPEC.md) |
| [unreal/](unreal/) | **Lode for Unreal**, an Unreal Editor plugin that makes Lode a revision control provider (planned, no code yet) | C++ | [unreal/docs/SPEC.md](unreal/docs/SPEC.md) |

Rules for agents working here are in [AGENTS.md](AGENTS.md).

## Status

Lode v1 (milestones M0 to M6), its packaging and 0.2 features (M7 to M12), Lode Desk with a
native Slint UI (M13) and the server's admin web UI (M14) are implemented. The first public
release is 0.2.0. Lode for Unreal is planned. Status boards: [lode/docs/tasks/README.md](lode/docs/tasks/README.md),
[unreal/docs/TASKS.md](unreal/docs/TASKS.md).

## How the parts fit

- **Lode** is complete on its own: `loded` serves depots over HTTPS, `lode` and Lode Desk work
  with workspaces, and webhooks and checks connect any build server.
- **Lode for Unreal** runs the `lode` command and reads its JSON output. The contract is
  section 5 of [unreal/docs/SPEC.md](unreal/docs/SPEC.md). Lode never names Unreal.
