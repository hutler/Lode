# Rules for agents in this repository

This repository holds three subprojects: the core `lode/`, and `desk/` and `unreal/` built on
it. These rules apply to all of them. Each subproject adds its own rules, which you must also
read:

| Subproject | Its rules | Its tasks |
| --- | --- | --- |
| `lode/` | `lode/AGENTS.md` | `lode/docs/tasks/` |
| `desk/` | `lode/AGENTS.md` (code rules), `desk/README.md` ("The core version") | `lode/docs/tasks/` (the Desk tasks, `T09x` onwards) |
| `unreal/` | `unreal/docs/SPEC.md` section 9 | `unreal/docs/TASKS.md` |

Use English for project documentation and agent instructions. Keep translations linked from
the English original. For tasks carried out in a Lode depot, start with the
[agent workflow guide](docs/agent-guide.md).

Track new Lode tasks in [Beads](.beads/README.md). Run `bd bootstrap` in a fresh clone,
`bd dolt pull` before planning and `bd dolt push` after updating issues. Check the issue's
acceptance criteria and approval status before implementing it. Existing milestone task IDs
and specifications remain in `lode/docs/tasks/`.

## One task, one subproject

- A task belongs to one subproject. Work only inside that subproject's folder.
- If your task needs a change in the other subproject, do not make it. Write what you need in
  your report and in the Notes of your task row, and stop if you cannot continue without it.
- The core comes first. A core change does not touch `desk/` or `unreal/`; they move to the new
  core afterwards, each in its own change.
- `unreal/` talks to the core only through the `lode` command and its JSON. The contract is
  section 5 of `unreal/docs/SPEC.md`. Changing it is its own change, agreed by the person, and
  lands together with the edits to every document it affects.
- `desk/` uses the core's `lode-ops` library at the version pinned in `desk/Cargo.toml` (a
  `git` dependency on this repository at a fixed `rev` or `lode-v*` tag), never by path. Moving
  the pin is its own change, `desk: move to core <version>`.
- The core never depends on or names Desk or Unreal in code, and Desk and Unreal never import
  each other.

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
  `cargo test --workspace`
- `desk/`: `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`,
  `cargo test --locked`
- `unreal/`: the build and automation commands in `unreal/docs/SPEC.md` section 9

Report plainly what you built, what you left open, and how you verified it.

## Never

- Commit secrets, tokens or passwords, or write them to logs.
- Add a dependency from one subproject to another, other than Desk's pinned dependency on the
  core.
- Rewrite history on `main`.
