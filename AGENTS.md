# Rules for agents in SimpleDevOps

This repository holds three subprojects: `lode/`, `umpalumpa/` and `unreal/`. These rules apply to all of them. Each subproject adds its own rules, which you must also read:

| Subproject | Its rules | Its tasks |
| --- | --- | --- |
| `lode/` | `lode/AGENTS.md` | `lode/docs/tasks/` |
| `umpalumpa/` | `umpalumpa/plan/README.md` and the rules in each task file | `umpalumpa/plan/TASKS.md` |
| `unreal/` | `unreal/docs/SPEC.md` section 9 | `unreal/docs/TASKS.md` |

## One task, one subproject

- A task belongs to one subproject. Work only inside that subproject's folder.
- If your task needs a change in another subproject, do not make it. Write what you need in your report and in the Notes of your task row, and stop if you cannot continue without it.
- The shared contracts are `docs/integration/lode-umpalumpa.md` and section 5 of `unreal/docs/SPEC.md`. Changing one is its own change, agreed by the person, and lands together with the edits to every subproject document it affects.
- Keep the boundaries in `README.md`: Lode names neither of the others, the UmpaLumpa core does not name Lode, and no subproject imports another's code.

## Task ids, branches and commits

The three plans number their tasks separately (Lode `T001` to `T064`, UmpaLumpa `T01` to `T23`, Unreal `T01` to `T11`). Inside a subproject's own documents the short id is enough. Everywhere else, put the subproject name in front. Where a subproject's own rules say otherwise, this section wins.

| | Form | Example |
| --- | --- | --- |
| Branch | `<subproject>/T<id>-<slug>` | `lode/T012-auth`, `umpalumpa/T07-scheduler`, `unreal/T04-file-states` |
| Commit message | `<subproject> T<id>: <summary>` | `lode T012: token auth` |
| Mention in text | `<subproject> T<id>` | "needs lode T062" |

One task per branch. Merge into `main` only when the task's acceptance list is met and that subproject's checks pass.

## Checks before you finish

Run the checks of the subproject you touched, from its folder:

- `lode/`: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
- `umpalumpa/`: `make build test lint`
- `unreal/`: the build and automation commands in `unreal/docs/SPEC.md` section 9

Report plainly what you built, what you left open, and how you verified it.

## Never

- Commit secrets, tokens or passwords, or write them to logs.
- Add a dependency from one subproject to another.
- Rewrite history on `main`.
