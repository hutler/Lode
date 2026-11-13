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
| T031 | review | `lode/T031-locks-client` | `module_disabled` error code (exit 1); `revert` also restores the read-only bit; read-only bits checked on Linux only so far |
| T033 | review | `lode/T033-shelves-client` | base_rev is the branch head; `shelve -c` (T034 follow-up) done; `merged_from` added in T051; error codes `nothing_to_shelve`, `update_incomplete` |
| T035 | review | `lode/T035-shelf-submit` | `Provenance.submitted_shelf` in lode-proto (engine drops it from direct commits); submit answers `CommitResponse`; MCP `submit_shelf` tool left for the MCP task |
| T040 | review | `lode/T040-agents` | `PrincipalRef.owner`; `AgentProvenance` is a string map; `GET`/`DELETE …/admin/principals/{name}/tokens[/{id}]`; an empty scope list on an agent token means the defaults |
| T041 | review | `lode/T041-mcp-endpoint` | stateless `rmcp` streamable HTTP; `Caller.via`; `ModuleCtx` gains depot and branch lists, `list_dir`, `open_file`, `changed_files`; the module is registered in `loded` and the test kit; `rmcp` needs Rust 1.88 |
| T042 | review | `lode/T042-local-mcp` | commands run on a worker thread (workspace handles are not `Send`); `Globals.agent` feeds commit and shelve provenance; `outside_workspace` code; `TestWorkspace::std_cmd`; `docs/MCP.md` |
| T043 | review | `lode/T043-mcp-shelves` | `create_shelf`/`update_shelf`/`submit_shelf` in the shelves module; `[limits] mcp_max_edits`, `mcp_max_inline_bytes`; `ModuleCtx` gains `limits()` and `Depots::entry`; shelves record `via`; workflow test lives in `lode-mod-mcp/tests` |
| T050 | review | `lode/T050-branch-switch` | `switch` is `update` to another branch with a full-manifest comparison (a delta misses files committed since the last sync); `switch_branch` config key makes it resumable; `create` defaults to the workspace branch at its revision; locks are cleared locally and reported, they stay on the old branch; `status` gains `branch` and `rev` |
| T051 | review | `lode/T051-merge` | `merge-info` endpoint computes branch point, recorded ranges and base on the server; the CLI applies `base → head` with the unshelve rules (shared in `cmd/apply.rs`), keyed `pending_merge` (a list); a merge must be committed or shelved whole; `revert --merge`; binary conflicts use `.theirs-r<head>`; default message `Merge feat r5..r9` |
| T052 | review | `lode/T052-blame` | Myers line diff from `similar` (already in the lock file via insta) with a 2 s deadline per pair; cache key is (depot, path, revision of the newest version) rather than blob id, since that fixes the walk; caps 1000 versions and 1 MiB per version; history the caller cannot read ends the walk (`truncated`, not cached); the placeholder `Blame` wire type replaced by `BlameView`; MCP `blame` returns up to 2000 lines per call |
| T053 | review | `lode/T053-admin-cli` | `lode admin` covers depot, user, agent, group, token (`--for`), acl, gc, fsck; new `GET /admin/acl/check` evaluates the rules for a named principal (as with every scope) and backs `acl add --check` and `acl check`; client gains `tokens_for`, `delete_token_for`, `acl_check`; `fsck` with problems exits 1 (`fsck_failed`); MCP.md now uses `lode admin` instead of curl |
| T054 | review | `lode/T054-hardening` | server failpoints in GC and `TestServer::restart`; client crash points (`LODE_TEST_CRASH_AT`, exit 86); fixed a killed checkout that could not resume; client read timeout, size caps on downloads and JSON; server `[limits] body_idle_seconds`; soak, property fuzz and hostile-peer tests; report in `docs/HARDENING.md` |
| T055 | review | `lode/T055-packaging` | release workflow on tag `lode-v*` (cross for static musl, crt-static on Windows, SHA256SUMS, ghcr image); `Dockerfile` without apt (CA bundle copied from the build stage), entrypoint runs `loded init` on first start and listens on 0.0.0.0:7480; CI job runs `packaging/docker/smoke.sh`; systemd unit; WinSW config as the Windows service wrapper; webhooks and checks marked planned in the guide (scaffolds until M6) |
| T056 | review | `lode/T056-acceptance` | scenario suite `lode-cli/tests/scenarios.rs` (one test per SPEC §5.1–7.4; §6.5 and §6.6 not built); benchmarks in `lode-cli/tests/perf.rs` (ignored) and `docs/BENCHMARKS.md`, all three targets met; module surface reviewed (ARCHITECTURE §8); SPEC §10.1 lists deviations; SPEC MCP tool list and provenance of submitted shelves corrected |
| T060 | review | `lode/T060-webhooks` | module trait gains `worker` (background task with a `Stop` signal, started by `spawn_workers` in `loded serve` and the testkit) and `on_event` gets the `ModuleCtx`; `ModuleCtx::config()`, `AppState::module_ctx`; `[webhooks] allow_http` in `loded.toml`; delivery table gains `first_attempt_at` (the 24 h limit counts from the first attempt) and `outbox_id` is NULL for pings; admin is depot admin; events without a branch are not filtered by the pattern; CLI takes the depot on every subcommand (`remove <depot> <id>` and so on); `hmac` added; the testkit server now runs the event dispatcher |
| T061 | review | `lode/T061-checks-server` | `POST` answers 200 with the check (an upsert); `GET` needs `target_kind` and `target_id`, plus `target_version` for a shelf; reporting needs the depot to be visible to the caller and a readable revision; shelf checks are visible to anyone who sees the depot; client gains `report_check` and `checks`; module registered in `loded` and the testkit (off by default) |
