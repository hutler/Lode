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
| [M7.md](M7.md) | Release v1 | T070–T072 |
| [M8.md](M8.md) | Easy setup | T080–T087 |
| [M9.md](M9.md) | GUI core | T090–T094 |
| [M10.md](M10.md) | GUI team work | T100–T104 |
| [M11.md](M11.md) | GUI branches, merges, setup and release | T110–T115 |

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

M7   T070  T071                       (T070 after T064; T071 after T028 T031)
     T072                             (after T070 T071 and the GitHub repository)

M8   T080  T084  T085                 (T080 after T054; T084 after T072; T085 after T053)
     T081  T086                       (T081 after T080; T086 after T085)
     T082                             (after T081)
     T083                             (after T082)
     T087                             (after T080–T086)

M9   T090                             (after T087)
     T091                             (after T090)
     T092  T093                       (after T091)
     T094                             (after T092 T093)

M10  T100  T101  T104                 (after T094)
     T102                             (after T101)
     T103                             (after T093 T102)

M11  T110  T112                       (after T093, T091)
     T111                             (after T110)
     T113                             (after T093 T102)
     T114                             (after T113)
     T115                             (after T110-T114)
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
| T062 | review | `lode/T062-submit-gate` | `[checks]` parsing was already in `lode-core` (T014); the gate is `pre_commit` in `lode-mod-checks`, reading `origin` and its own table only; `details.missing` gives `reason` per check (`absent`, `not_success`, `wrong_reporter`); a `shelves` origin without the shelf attributes is refused rather than let through |
| T063 | review | `lode/T063-checks-client` | `lode checks <rN | ID[@V]> [--depot D]` reuses `--server`; a shelf without a version asks the shelves module for the current one; `module_disabled` when the server lacks the module; `shelf submit` lists unmet checks one per line (`checks_required`, exit 1); MCP `list_checks` takes `target_kind`, `target_id`, `target_version` (required for a shelf, since the module cannot look a shelf up); webhook commands added to CLI-JSON.md |
| T064 | review | `lode/T064-unattended` | `switch -r` already existed (T050); T064 adds the tests. Shared cache cap is 20 GiB, set by `cache_max_gib` in `settings.toml`; eviction runs once per sync instead of after every insert; `BlobCache` gains `copy_out` (copy and verify in one read) and `store_file`; merge and revert bases also use the shared cache; an unreadable token file is exit 3 like a missing one; `--version --json` adds `api` |
| T070 | review | `lode/T070-release-prep` | `main` fast-forwarded to the M6 tip; MSRV 1.88 with a CI job building on exactly 1.88; `uuid` held at 1.26 (1.27+ needs Rust 1.89) and `.cargo/config.toml` makes `cargo update` respect the MSRV; M7 and M8 task files written from the post-v1 plan; pushing and the macOS CI run wait for the GitHub repository |
| T071 | review | `lode/T071-editor-support` | `--paths-from` is a global option expanded into arguments after `--` before parsing, allowed only for commands that take path lists, an empty list is `usage`; `status --remote` reports only files with a newer version or a lock, one branch, at most one manifest-delta and one locks request; `lock_required` on every status entry; `cat -o` via a temporary file; Windows root matched without case (component-wise) |
| T080 | review | `lode/T080-timeouts` | `loded` runs its own hyper connection loop instead of `axum::serve`: HTTP/1.1 header timeout (also closes idle kept-alive connections, so no separate idle setting), a first-request deadline for HTTP/2, HTTP/2 only over TLS via ALPN, per-address limit (IPv6 by /64) under `[connections]`; `hyper-util` added as a direct dependency (hyper's own companion crate, already in the tree through axum); the client's idle pool timeout is 20 s, below the server's 30 s |
| T081 | review | `lode/T081-acme` | `[tls]` takes `cert`/`key` or `acme_domain` (+ `acme_email`, `acme_directory`, `acme_ca`), exclusive; TLS-ALPN-01 through a rustls certificate resolver, so no extra port; `instant-acme` (ring, rcgen) is the ACME client and `rcgen` becomes a normal dependency of `lode-server` (both justified: the allow-list has no ACME client); the certificate's validity is read with a small DER reader instead of an X.509 crate; Pebble test runs in CI on Linux (Go installs it) and is skipped where `PEBBLE` is not set |
| T082 | review | `lode/T082-setup` | `loded setup` (prompts or flags, `--yes` or no terminal takes defaults; password from a file or standard input, never a flag value) writes the default config with the answers filled in, parses it before writing, then `init` and an offline `set_admin_password`; `loded service` is native on Windows (`windows-service` crate, justified: no std way to talk to the service manager; its dispatcher macro holds the only `unsafe`, which the crate-level `forbid` does not see in external macros) and writes a systemd unit on Linux (`--dry-run` prints it); scheduled backups via `[backup] every/keep/dir`, `.partial` until complete, schedule taken from the newest backup's name; WinSW config removed; `docs/UPGRADING.md`; Windows code not compiled in the cloud, checked on the PC |
| T085 | review | `lode/T085-init` | `lode init` runs in the current folder (`-C` for another); an existing empty depot is accepted, one with revisions is refused; the workspace keeps an `init_template` key until the import commit lands, so a rerun resumes through the normal pending-commit path; existing `.lodeignore`/`.lodeconfig` are kept; templates are constants in `lode-cli/src/templates.rs`, every template also ignores OS and editor leftovers; `--dry-run` walks the folder with `IgnoreStack::with_text` (new) instead of writing rules |
| T083 | review | `lode/T083-packages` | packages built with nfpm (deb, rpm) and WiX 5 (MSI); the `lode-server` package holds only `loded` and recommends a `lode` client package, built now (T084 adds winget, Homebrew and self-update); the packaged unit waits for `loded.toml` and may bind port 443; `loded setup` points at `systemctl enable` when the packaged unit is there; the MSI does not create the service itself but runs `lode-server-setup.cmd` (elevated `loded setup` and `loded service install`) from its last page and the Start menu, stops the service for upgrades and removes it on uninstall; the release workflow also installs, starts and removes the MSI on its Windows runner (the task asked for a manual check only); the image's entrypoint runs `loded setup` from `LODED_DOMAIN`/`LODED_EMAIL`/`LODED_ADMIN_PASSWORD_FILE`, which `packaging/compose.yaml` sets; checked here in ubuntu:24.04 and fedora:41 containers and with a local image; the MSI is not built or run yet (needs Windows) |
| T084 | review | `lode/T084-client-packages` | done before T072 at the person's request (work locally until the GitHub repo exists), so nothing is published yet; the `lode` .deb/.rpm came with T083; client MSI (WiX 5) shares `lode.exe` and the PATH entry with the server MSI as one component; winget manifests and a Homebrew formula are templates that `packaging/render-manifests.sh` fills from `SHA256SUMS` in the release job (submitting them to winget-pkgs and a tap repo is manual); releases now also carry the bare executables, which `self-update` downloads, so no archive crates are needed; `self-update` recognizes installs by path (Program Files\\Lode, /usr/bin, Homebrew Cellar); new direct deps of lode-cli: `reqwest`, `sha2`, `hex` (all on the allow-list); MSI builds and the client MSI test run only on the release workflow's Windows runner |
| T086 | review | `lode/T086-import-git` | new crate `lode-import` runs the `git` command (no Git library); provenance gains `imported {system,id,author,date}` (SPEC §7.3), and the resume point is read from the branch head's provenance, so a depot with later non-imported revisions is refused; request ids are `import-git-<commit>`, so a commit whose answer was lost is not repeated; Git renames become Lode renames (`copy_from`); empty and submodule-only commits are skipped (counted in `skipped`); always imports into `main`; `lode log` shows the original author and date |
| T087 | review | `lode/T087-quickstart` | quick start in the guide's introduction (Linux server, Windows, Docker, then `init` or `import git`, then the team); the scenario test `loded/tests/quickstart.rs` runs the real `loded` and `lode` binaries through setup with flags, `init --template unreal`, a second user and a checkout; the manual walk-through on Windows and on a Linux VDS needs a 0.1.0 release (T072) and is open; T084 (client packages) is not done yet, so the quick start points at the client archives on Windows and macOS |
| T090 | review | `lode/T090-ops` | new crate `lode-ops` holds the context, workspace helpers and every command but `mcp`, `self-update` and external subcommands, moved unchanged; operations report through a `Ui` trait (`line`, `warn`, `note`, `progress`, `raw`, `stream`, plus `interactive`/`prompt`/`confirm`/`edit`/`read_all`/`read_line` with "nobody to ask" defaults), and `lode-cli/src/term.rs` is the terminal implementation (progress throttling, `rpassword`, `$EDITOR`); `lode mcp` gets its own UI that only warns, so it can never prompt on the protocol stream; `lode cat` without `-o` or `--json` writes through `Ui::stream` and fails as a usage error where there is none; `CliError` is now `OpError`; argument structs keep clap's derive (one definition of options and help) and gain `Default` (by hand where clap has defaults); `Ctx::in_dir` and `Ctx::with_config_dir` let a program run operations for any folder; lode-ops tests drive status, commit, log and cat without the binary and grep its sources for terminal use; no new third-party crates |
| T091 | review | `lode/T091-desk-shell` | `crates/lode-desk` is its own Cargo workspace (excluded from the main one) with `rust-version` 1.90, which Tauri 2.12 needs; new dependencies there: `tauri`, `tauri-build`, `tauri-plugin-dialog` (justified: the GUI framework chosen in the plan, and its folder picker), npm packages React 19, Vite, Vitest, Testing Library; each command runs its operation on a thread of its own with a single-threaded runtime because operations hold the workspace database across awaits; `lode-ops` gains `cmd::info::info` (what a workspace points at) and `LoginArgs.password` (`#[arg(skip)]`, for programs); the app keeps its recent list in its own data folder and its sign-ins in lode's config folder; error banner adds a next step for common codes; screenshot run uses the mocked backend, and the real app was also driven once on Linux (Xvfb, WebKitGTK) against a real `loded`; Windows build and start not tried yet (PC) |
| T092 | review | `lode/T092-desk-changes` | pending changes grouped as `lode status` lists them (plain changes, then changelists by name, then a separate "Not added yet" group for untracked files), with checkboxes per file and per group, a details panel for the selected file, and a right-click menu (Add, Revert…, Pick); committing picked untracked files adds them first; a refused commit keeps the pick and message and lists the refused paths from `details.paths`; revert asks first; the folder watcher uses the `notify` crate (justified: no std file watching; CC0) with a 300 ms quiet period, ignores `.lode/` and read-only access events, and is started per open tab; the view also refreshes on window focus; the real app was driven on Linux against a real `loded`: lock refusal, partial commit and auto-refresh all work; no new `lode-ops` operations were needed |
| T093 | review | `lode/T093-desk-history` | `lode log` gains `--before REV` for paging (CLI-JSON documents it; MCP passes none); `lode-desk` adds `log`, `revision` (one revision with its changed files) and `blame` commands; the History section lists the branch newest first in a virtual list (fixed 40 px rows) that loads 100 revisions per page as the end comes near; the graph is one lane for the branch (the parent's history below the branch point drawn in the muted color) plus a merge edge from a second lane on each merging revision, since Lode branches are linear and merges are recorded as ranges; badges for agent provenance, "imported from git" and merges; the revision panel lists changed files with History and Blame; file history follows renames; blame groups runs of lines by revision and opens a revision in History; the Changes details panel gains History and Blame; the Changes view stays mounted while hidden so the pick and message are kept |
| T094 | review | `lode/T094-desk-diff` | `lode-ops` gains `diff_versions` (two stored versions of a file, each `path@rev`, as one entry of `lode diff --json`; no CLI command yet), sharing `lode diff`'s rendering, binary detection and 16 MB text limit; `lode-desk` adds `diff` (a working file against its base, or null) and `diff_revision` (a file as a revision changed it: against the parent, or the source of a copy); the UI parses the unified patch, pairs each deleted line with the most alike added line for word highlights and the side-by-side rows, and keeps the chosen layout per viewer; binary files show sizes and extension, files over 16 MB say Lode compares them by size, patches over 512 KB are shown as plain text with a note; the diff opens from "Show diff" or a double-click in Changes and from a changed file in a revision |
| T100 | review | `lode/T100-desk-locks` | `lode-desk` adds `lock`, `unlock` and `locks` commands (no `lode-ops` changes); a Locks section lists the branch's locks with a path filter and "only mine", unlocks mine and breaks others' after asking (admin only; `permission_denied` shown otherwise); the Changes view locks one file or all picked files from the details panel or the right-click menu, with a comment, offering Lock first for files that need one; a `locked` refusal lists each holder, workspace, time and comment from `lode locks`; `module_disabled` shows the section as unavailable; the Changes view refreshes when it is shown again; also fixed on Windows: the watcher counted `.lode/` writes as changes (0f604da) |
| T101 | review | `lode/T101-desk-changelists` | `lode-desk` adds `changelists`, `changelist_new`, `changelist_move`, `changelist_delete` and `commit_changelist` (no `lode-ops` changes); the Changes view groups files by changelist with each description, keeps empty changelists, creates one from the header (taking the picked files), moves files by drag and drop or the right-click "Move to" menu, commits exactly one changelist with its description as the suggested message, and deletes one after asking (its files go back to Changes) |
| T102 | review | `lode/T102-desk-shelves` | `lode-desk` adds `shelve`, `shelves`, `shelf`, `shelf_submit`, `shelf_delete` and `unshelve` (no `lode-ops` changes); the Changes view shelves the picked files (adding untracked ones first) or a changelist, with a description, "keep my changes", or as the next version of an open shelf of mine; a Shelves section lists mine or everyone's, open, submitted or deleted, and shows a shelf's description, owner (with the agent's user), base, version, files and each file's diff in either layout; unshelve shows each file's outcome; submit names the version shown, a newer version is refused with a button to review it, and a `checks_required` refusal lists each missing check and why; my shelves can be deleted after asking; `module_disabled` shows the section as unavailable; also fixed: checkbox labels in dialogs were laid out as columns |
| T103 | review | `lode/T103-desk-checks` | `lode-desk` adds `checks` (several targets per call; `available: false` without the checks module) and `open_link` (http and https only, handed to the system's opener; no new dependency); History rows and shelves show one badge with the worst state and a count; revision and shelf details list each check with state, summary, reporter, time and its link; checks load only for the rendered rows, 50 targets per call, and load again when a row comes back into view after a minute |
| T104 | review | `lode/T104-desk-image-diff` | `lode-ops` adds `diff::Content`, `base_content`, `working_content` and `stored_content` (one version of a file in memory, up to a limit); `lode-desk` adds `image_side`, sent as raw bytes (a tag, the size, the content) instead of JSON; PNG, JPEG, GIF, WebP and BMP open an image diff for a working file and for a revision's change: side by side, swipe and onion skin (the mode is remembered), with each side's dimensions and size; a new or deleted image shows its one side; images over 64 MB, or ones the web view cannot decode, fall back to the size note and say why; the text layout switch is hidden for binary files |
