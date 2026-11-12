# Implementation tasks

Each task is sized for one agent session and one pull request. Rules for implementers:
[../../AGENTS.md](../../AGENTS.md).

Task format: **Goal**, **Depends on**, **Scope** (crates touched), **Requirements**,
**Acceptance** (must be automated tests unless marked manual), **Out of scope**.

## Milestones

| File | Milestone | Tasks |
| --- | --- | --- |
| [M0.md](M0.md) | Foundations | T001–T005 |
| [M1.md](M1.md) | Server core | T010–T019 |
| [M2.md](M2.md) | Client | T020–T028 |
| [M3.md](M3.md) | Locks, shelves, changelists | T030–T035 |
| [M4.md](M4.md) | Agents and MCP | T040–T043 |
| [M5.md](M5.md) | Branching, merge, hardening, packaging | T050–T056 |
| [M6.md](M6.md) | Integration: webhooks, checks, submit gate, unattended client | T060–T064 |

## Dependency graph

Tasks on the same line have no dependency on each other and can run in parallel.

```
M0   T001
     T002  T005(after T002)
     T003  T004                       (both after T002)

M1   T010                             (after T003 T004 T005)
     T011
     T012  T013                       (after T011)
     T014                             (after T012 T013)
     T015  T016  T017                 (after T014)
     T018  T019                       (after T017)

M2   T020                             (after T005; can start during M1)
     T021                             (after T002; can start during M1)
     T027                             (after T015 T020)
     T022                             (after T021 T027)
     T023  T026                       (after T022)
     T024                             (after T023)
     T025                             (after T024)
     T028                             (after T025 T026)

M3   T030  T032  T034                 (T030, T032 after T017; T034 after T024)
     T031  T033                       (T031 after T030 T025; T033 after T032 T025)
     T035                             (after T033)

M4   T040                             (after T014)
     T041                             (after T040 T015 T030 T032)
     T042                             (after T028 T031 T033)
     T043                             (after T041 T035)

M5   T050                             (after T016 T025)
     T051                             (after T050)
     T052  T053  T054  T055           (T052 after T015 T026; T053 after T019 T022; T054 after T025; T055 after T028)
     T056                             (after everything, including M6)

M6   T060  T061                       (T060 after T017 T022; T061 after T017 T040; can run alongside M5)
     T062  T063                       (T062 after T061 T035; T063 after T061 T028 T041)
     T064                             (after T050 T028)
```

## Status

Keep this table current. One row per task; states: `todo`, `in progress`, `review`, `done`.

| Task | State | Branch / PR | Notes |
| --- | --- | --- | --- |
| T001 | done | `lode/T001-*` | merged into `main` |
| T002 | done | `lode/T002-*` | merged into `main` |
| T003 | done | `lode/T003-*` | merged into `main` |
| T004 | done | `lode/T004-*` | merged into `main` |
| T005 | done | `lode/T005-*` | merged into `main` |
| T010 | review | `lode/T010-server-skeleton` |  |
| T011 | review | `lode/T011-authentication` |  |
| T012 | review | `lode/T012-permissions` |  |
| T013 | review | `lode/T013-blob-api` | plus `lode/path-windows-chars` (paths reject `< > : " \| ? *`) |
| T014 | review | `lode/T014-commit-engine` |  |
| T015 | review | `lode/T015-read-api` |  |
| T016 | review | `lode/T016-branches` |  |
| T017 | review | `lode/T017-module-framework` |  |
| T018 | review | `lode/T018-gc` |  |
| T019 | review | `lode/T019-fsck-backup` |  |
| T020 | review | `lode/T020-client` | reqwest 0.12 (rustls, ring, native roots) |
| T021 | review | `lode/T021-workspace` | status of 200,000 files: 1.8 s (debug build, Linux) |
| T027 | review | `lode/T027-testkit` | adds commit failpoints to `lode-server` |
| T022 | review | `lode/T022-checkout` | `rpassword` for the no-echo prompt; `cache_dir` setting not read yet, only `LODE_CACHE_DIR` |
| T023 | review | `lode/T023-local-changes` | diff and merge crate: `diffy` 0.5 (needs Rust 1.85) |
| T026 | review | `lode/T026-history` | snapshot tests use `insta`; `--depot` is an option of `log`/`cat`/`ls` |
| T024 | review | `lode/T024-commit` | missing files stop a commit (use `rm` or `revert`) |
| T025 | review | `lode/T025-update` | workspace schema 2: `conflict.mine_id` for `resolve --mine` |
| T028 | review | `lode/T028-json-contract` | `docs/CLI-JSON.md`; a failed checkout no longer leaves a workspace behind |
| T030 | review | `lode/T030-locks` | server admins may grant module scopes; `CommittedCtx.created_at`; `UnlockResponse` in lode-proto |
| T032 | review | `lode/T032-shelves` | `Commits::check` in `ModuleCtx` (commit checks against a base revision); `ShelfView.agent_owner` and `diff` |
| T034 | review | `lode/T034-changelists` | status lists changelists under headings; `shelve -c` comes with T033 |
