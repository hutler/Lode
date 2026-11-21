# Lode issue tracking

Lode uses Beads for new work. The issue database and its history are stored in this GitHub
repository at `refs/dolt/data`. The files in this directory configure the local database.
The milestone specifications and existing task IDs remain in [the task plan](../lode/docs/tasks/README.md).

## Using a clone

Install the [Beads CLI](https://github.com/gastownhall/beads/releases). This repository was
initialized with bd 1.3.4 in embedded mode. Run these commands from the repository root.

```sh
bd bootstrap
bd dolt pull
bd ready
bd show lode-vle
bd children lode-vle
```

The epic `lode-vle` tracks the five findings from the 0.5.0 field test. Its children contain
the proposed changes and regression tests. A `needs-approval` label means the maintainer
has requested analysis first. Start implementation after they approve the corresponding task.

Pull before editing issues. Use `bd create` for new issues, `bd update` to record progress
and `bd close` after verification. Run `bd dolt push` to share issue changes. Source changes
continue through ordinary branches and pull requests. A fresh clone can verify the remote
database with `git ls-remote origin refs/dolt/data` and retrieve it with `bd bootstrap`.
