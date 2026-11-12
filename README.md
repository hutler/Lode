# SimpleDevOps

Open source tools for small teams that build software with large binary files, such as games. Three subprojects share this repository. Each one builds, tests and releases alone, and each is useful without the others.

| Folder | What it is | Language | Start reading at |
| --- | --- | --- | --- |
| [lode/](lode/) | **Lode**, a centralized version control system for code plus large binaries: locks, shelves, changelists, AI agents as users | Rust | [lode/docs/SPEC.md](lode/docs/SPEC.md) |
| [umpalumpa/](umpalumpa/) | **UmpaLumpa**, a self-hosted CI/CD server with plugins and a built-in MCP endpoint | Go | [umpalumpa/plan/README.md](umpalumpa/plan/README.md) |
| [unreal/](unreal/) | **Lode for Unreal**, an Unreal Editor plugin that makes Lode a revision control provider | C++ | [unreal/docs/SPEC.md](unreal/docs/SPEC.md) |

Shared documents are in [docs/](docs/). Rules for agents working here are in [AGENTS.md](AGENTS.md). Everything is licensed under Apache-2.0 ([LICENSE](LICENSE)).

## Status

Planning is done for all three; no code exists yet. Status boards: [lode/docs/tasks/README.md](lode/docs/tasks/README.md), [umpalumpa/plan/TASKS.md](umpalumpa/plan/TASKS.md), [unreal/docs/TASKS.md](unreal/docs/TASKS.md).

## How the parts fit

```
            Unreal Editor                      AI agents (MCP clients)
                 |                                 |            |
        unreal/ plugin runs                        |            |
        the `lode` command                         v            v
                 |                          Lode /mcp     UmpaLumpa /mcp
                 v                                 |            |
   +------------------------+   webhooks, checks   +------------------------+
   | Lode                   | <------------------> | UmpaLumpa              |
   | server `loded`,        |   HTTP API, `lode`   | server, build agents,  |
   | client `lode`          |   on build agents    | plugin `plugins/lode`  |
   +------------------------+                      +------------------------+
```

- **Lode and UmpaLumpa** meet through one UmpaLumpa plugin and two optional Lode modules. The contract is [docs/integration/lode-umpalumpa.md](docs/integration/lode-umpalumpa.md).
- **Lode and Unreal** meet through the `lode` command's JSON output. The contract is section 5 of [unreal/docs/SPEC.md](unreal/docs/SPEC.md).
- **UmpaLumpa and Unreal** do not meet in version 1. Building Unreal projects in UmpaLumpa is ordinary pipeline work.

## Boundaries that keep each part independent

1. Lode never names UmpaLumpa or Unreal. Its webhooks, checks and JSON output are generic.
2. The UmpaLumpa core never names Lode. Everything about Lode is inside `umpalumpa/plugins/lode`.
3. UmpaLumpa works with Git alone, and Lode works with no build server.
4. No subproject imports another's code. They talk through HTTP, the command line and files.
5. A change to a shared contract in `docs/` lands in the same commit as the subproject documents it affects.

## Order of work across subprojects

| Step | Lode | UmpaLumpa | Unreal |
| --- | --- | --- | --- |
| 1 | M0 to M2: foundations, server, client | Waves 0 to 2: contracts and core | T01 to T03 against a recorded `lode` |
| 2 | M3: locks, shelves, changelists | Wave 3: Git, GitHub, MCP, web UI | T04 to T08 against the real `lode` |
| 3 | M4 to M6: agents, MCP, hardening, webhooks and checks | Wave 4: release, plus T22 Lode plugin | T09 to T11: changelists, shelves, packaging |
| 4 | | T23: shelf loop end to end | |

The three can start at once. The points where one waits for another are listed in each plan.
