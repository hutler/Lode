# Rules for agents in this repository

This repository holds two subprojects: `lode/` and `unreal/`. These rules apply to both. Each
subproject adds its own rules, which you must also read:

| Subproject | Its rules | Its tasks |
| --- | --- | --- |
| `lode/` | `lode/AGENTS.md` | `lode/docs/tasks/` |
| `unreal/` | `unreal/docs/SPEC.md` section 9 | `unreal/docs/TASKS.md` |

## One task, one subproject

- A task belongs to one subproject. Work only inside that subproject's folder.
- If your task needs a change in the other subproject, do not make it. Write what you need in
  your report and in the Notes of your task row, and stop if you cannot continue without it.
- The shared contract is section 5 of `unreal/docs/SPEC.md`. Changing it is its own change,
  agreed by the person, and lands together with the edits to every document it affects.
- Lode never names Unreal, and neither subproject imports the other's code.

## Task ids, branches and commits

The plans number their tasks separately (Lode `T001` onwards, Unreal `T01` to `T11`). Inside a
subproject's own documents the short id is enough. Everywhere else, put the subproject name in
front. Where a subproject's own rules say otherwise, this section wins.

| | Form | Example |
| --- | --- | --- |
| Branch | `<subproject>/T<id>-<slug>` | `lode/T012-auth`, `unreal/T04-file-states` |
| Commit message | `<subproject> T<id>: <summary>` | `lode T012: token auth` |
| Mention in text | `<subproject> T<id>` | "needs lode T062" |

One task per branch. Merge into `main` only when the task's acceptance list is met and that
subproject's checks pass.

## Checks before you finish

Run the checks of the subproject you touched, from its folder:

- `lode/`: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`; for `lode/crates/lode-desk` (its own workspace) the same three
  commands without `--workspace`
- `unreal/`: the build and automation commands in `unreal/docs/SPEC.md` section 9

Report plainly what you built, what you left open, and how you verified it.

## Never

- Commit secrets, tokens or passwords, or write them to logs.
- Add a dependency from one subproject to another.
- Rewrite history on `main`.
