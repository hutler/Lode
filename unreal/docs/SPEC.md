# Lode for Unreal: specification

Status: draft v0.1, 2026-11-12. Licence: Apache-2.0. Language: C++ (Unreal Engine 5).

## 1. Purpose

Lode is built for teams with code plus large binary files, and Unreal projects are the main case. This plugin makes Lode appear in the Unreal Editor as a revision control provider, next to the built-in Perforce, Git and Subversion providers. People who never open a terminal can then lock an asset, get the latest files, submit, and shelve work for review.

It is planned from the start, together with Lode itself, so that Lode's command line gives an editor what it needs before that interface is frozen.

## 2. Goals and non-goals

**Goals**

1. Everything an artist does daily works from the editor: see file state, lock, sync, submit, revert, view history.
2. Locks are visible and reliable. A person sees who holds an asset before they start editing it.
3. The editor stays responsive on projects with 100,000 or more assets.
4. No Lode logic is duplicated. The plugin runs the `lode` command and reads its JSON.
5. Installing is copying one folder into the project's `Plugins/` folder.

**Non-goals for version 1**

- Talking to the Lode server directly, or linking Lode's Rust code into the editor.
- Branch creation, switching and merging from the editor. These stay in the `lode` command.
- Showing build results from UmpaLumpa in the editor. A later version can read Lode's checks.
- Engines before Unreal Engine 5, and any runtime (non-editor) module.

## 3. How it works

```
Unreal Editor
  revision control UI (content browser icons, Submit window, View Changes, History)
        |
        v
  LodeSourceControl module  (this plugin, editor only)
    provider      registers Lode with the editor, holds settings and the state cache
    operations    one worker per editor operation, run on a background thread
    runner        starts `lode <command> --json`, parses the one JSON document it prints
        |
        v
  lode (command line client, from ../lode)  --->  loded (server)
```

- The plugin implements the editor's revision control provider interface (`ISourceControlProvider`, with states, revisions, changelists and worker classes), the same interface the built-in providers use. It is a feature registered under the `SourceControl` name, so it appears in the editor's provider list.
- Each editor operation becomes one or a few `lode` commands with `--json`. Lode guarantees exactly one JSON document on standard output and stable error codes (Lode spec 7.4), which is what makes this dependable.
- The workspace is the Lode workspace that contains the `.uproject` file. The plugin finds it by walking up from the project folder to a `.lode` folder.
- Sign-in is Lode's own: the person runs `lode login` once, or uses the plugin's sign-in dialog, which passes the token on standard input. The plugin never stores a password or token.

**Why the command line and not the server API.** Workspace state, change detection, read-only handling, locks and merges all live in Lode's client. Calling the server directly would mean writing that logic a second time in C++ and keeping two copies in step. The cost is one process start per operation, which batching (section 6) keeps small.

## 4. What the editor can do

| Editor action | Lode command | Notes |
| --- | --- | --- |
| Connect | `lode --version --json`, `lode status --json` | Checks the client version, finds the workspace, shows server, depot, branch and user |
| Refresh state | `lode status --json <paths>`, `lode locks --json` | See section 6 for how often |
| Check out | `lode lock <paths>` | In Lode, checking out means taking the lock. Files that need no lock are simply edited |
| Mark for add | `lode add <paths>` | |
| Delete | `lode rm <paths>` | |
| Rename or move | `lode mv <from> <to>` | The editor leaves a redirector at the old path; the plugin adds it, so history follows the asset |
| Duplicate | `lode cp <from> <to>` | |
| Revert | `lode revert <paths>` | Releases locks held on those files |
| Sync | `lode update` | Assets changed on both sides become conflicts; binaries are never merged |
| Submit | `lode commit -m <text> <paths>` | Optional "keep locks" maps to `--keep-locks` |
| Resolve | `lode resolve <path> --mine` or `--theirs` | For binary assets the choice is one side or the other |
| History | `lode log <path> --json` | Revision, author, submitter, time, message |
| Diff against a revision | `lode cat <path>@<rev>` into a temporary file | The editor's own asset diff then compares the two files |
| Changelists | `lode changelist new, list, move, delete` | Shown in the editor's View Changes window |
| Shelve and unshelve | `lode shelve -c <changelist>`, `lode unshelve <id>` | A shelf is how work goes to review, and how agent work arrives |

**File states shown in the editor**

| State | Meaning | Source |
| --- | --- | --- |
| Not controlled | Untracked file | `status` |
| Ignored | Matches `.lodeignore` | `status` |
| Added, deleted, modified | Pending local change | `status` |
| Conflicted | Needs resolve | `status` |
| Locked by me | The lock is held in this workspace | `status` |
| Locked by someone else | Shows the holder's name; the editor refuses the edit | `locks` |
| Lock required | Matches `[locks].required` in `.lodeconfig` and is read-only on disk | `status` |
| Not at head | A newer revision exists on the branch | `status` with server information |

**Fit with Lode's lock rule.** Lode keeps files that match `[locks].required` read-only on disk until they are locked. The editor already asks to check out a read-only asset when a person starts editing it, so the two behaviours join without extra code: the prompt appears, the plugin runs `lode lock`, the file becomes writable.

## 5. What the plugin needs from Lode

This section is the contract between the two subprojects. Lode stays generic: nothing here mentions Unreal, and each point is useful to any editor or tool. Items marked "to confirm" are not yet stated in Lode's documents and need a decision on the Lode side before the plugin task that uses them.

| # | Need | Why | State in Lode's documents |
| --- | --- | --- | --- |
| 1 | Every command prints exactly one JSON document with `--json`, with stable error codes and exit codes | The whole plugin depends on it | Present (spec 7.4, task T028) |
| 2 | A written JSON schema for each command, `docs/CLI-JSON.md` | The plugin is built and tested against it | Present (task T028) |
| 3 | `status --json <paths>` limited to the paths given, reporting per file: state, changelist, lock held, lock required | Refreshing a few assets must not scan the whole workspace | Present: `status --unchanged <paths>` lists every named file with `status`, `changelist`, `locked` and `lock_required` (task T071) |
| 4 | Server information in status on request: whether a newer revision of the file exists, and who holds its lock | "Not at head" and "locked by someone else" without two extra calls per refresh | Present: `status --remote` (task T071) |
| 5 | `locks --json` with a path prefix | Lock holders for the folder being viewed | Present (task T031) |
| 6 | `log <path> --json` with revision, author, submitter, time, message, action and size | History window | Present in outline; fields to confirm |
| 7 | `cat <path>@<rev> -o <file>` writing to a file | Diff against a revision for binary assets, which cannot go through standard output safely on Windows | Present (task T071) |
| 8 | A way to pass many paths without the command line, such as `--paths-from <file>` | Windows limits a command line to about 32,000 characters, and a submit can name thousands of assets | Present: `--paths-from <file>` or `-` for stdin (task T071) |
| 9 | Paths accepted as absolute Windows paths, with either slash | The editor hands the plugin absolute paths | Present; on Windows the root is matched without regard to case (task T071) |
| 10 | Readers (`status`, `locks`, `log`) may run while another `lode` process is working in the same workspace | The editor refreshes state in the background | Present (task T021: readers may run in parallel) |
| 11 | Changes to a file's read-only attribute happen only through `lock`, `unlock`, `revert`, `commit` and `update` | The editor caches writability | Present (task T031) |
| 12 | `lode login --token-stdin` and `LODE_TOKEN_FILE` | Sign-in dialog without storing a token in the plugin | Present |

**Lode task.** Items 3, 4, 7, 8 and 9 were done in Lode task T071 ("Command-line support for editor integrations", milestone M7). The JSON shapes are in `lode/docs/CLI-JSON.md`.

**Version check.** On connect the plugin reads `lode --version --json` and refuses a client whose `api` value is older than the plugin supports, with a message naming the version needed.

## 6. Performance

The editor asks for file state very often, so the plugin must never start a process per file.

- **State cache.** The provider keeps the last known state of every file it was asked about. The editor reads the cache; only refresh operations run `lode`.
- **Batching.** One refresh request for many files is one `lode status` call. Requests that arrive while a call is running are merged into the next one.
- **Scope.** A refresh covers the files asked for, never the whole project, except the explicit "Refresh all" action.
- **Server information is slower.** Lock holders and "not at head" need the server. They are refreshed when a folder is opened, before a check out, and on a timer (default 60 seconds for visible assets), not on every local refresh.
- **Targets.** Refreshing 1,000 assets locally in under 1 second, and opening a folder of 500 assets with server information in under 2 seconds on a local network. Lode's own target is a full status of 200,000 files in under 5 seconds.

If measurements in task T04 show that process start time dominates, the fallback is a long-running `lode mcp` process (Lode's local endpoint over standard input and output), which offers the same operations as tools. This is an option, not part of version 1.

## 7. Settings and setup

- **Project setup.** Install `lode`, check out the depot so the `.uproject` is inside the workspace, copy the plugin to `Plugins/LodeSourceControl`, choose Lode in the editor's revision control dialog.
- **Settings.** Path to the `lode` program (default: found on `PATH`), refresh interval for server information, "keep locks after submit" default. Server, depot, branch and user are shown read-only, taken from the workspace.
- **Templates.** The plugin ships a `.lodeignore` for Unreal projects (`Binaries/`, `Intermediate/`, `Saved/`, `DerivedDataCache/`) and a `.lodeconfig` that requires locks for `Content/**/*.uasset` and `Content/**/*.umap`.

## 8. Engine versions and platforms

- **Engine.** Unreal Engine 5.4 and later, one source tree, with version checks only where the engine's interface differs. The revision control interface changes between engine releases, so every interface name in this document must be checked against the headers of each supported engine (`Engine/Source/Developer/SourceControl/Public/`) before it is used.
- **Platforms.** Windows first, since that is where most Unreal work happens. The code must also compile for the Linux and macOS editors.
- **Distribution.** A zip per engine version with the built plugin, attached to SimpleDevOps releases. A listing on Fab can follow.

## 9. Rules for implementing agents

- Read this document, `../../AGENTS.md`, and the Lode spec (`../../lode/docs/SPEC.md`) sections 5 to 7 and 9.
- Work only inside `unreal/`. If the plugin needs something from the `lode` command, add it to the table in section 5 and report it; do not change `lode/`.
- Editor-only module. No engine source changes. No third-party libraries beyond the engine's own.
- Never block the game thread on a `lode` call. All calls go through the worker pattern.
- Never write a token or password to disk, settings or logs.
- Until `lode` exists, build and test against the fake described in task T02. Do not invent JSON fields: use `lode/docs/CLI-JSON.md` once Lode's task T028 has written it, and before that mark every assumed field in one header file so it is easy to correct.
- Checks before finishing: the plugin builds with the engine's `RunUAT BuildPlugin` for each supported engine version available on the machine, and the plugin's automation tests pass (`Automation RunTests Lode`). Say in your report which engine versions you could not test.

## 10. Milestones

| Milestone | Tasks | Done when |
| --- | --- | --- |
| U1. Connects | T01 to T03 | The editor lists Lode as a provider, connects to a workspace and shows who is signed in |
| U2. Daily work | T04 to T08 | State icons, lock on check out, add, delete, rename, revert, sync, submit, history and diff all work on a real project |
| U3. Review and release | T09 to T11 | Changelists and shelves work in the View Changes window, and a packaged plugin with a guide is published |

## 11. Open questions

- [ ] **Which engine versions?** Proposed: 5.4 and later. Supporting older 5.x releases costs extra version checks.
- [ ] **Accept the proposed Lode task** for items 4, 7, 8 and 9 of section 5?
- [ ] **Who may break a lock from the editor?** Proposed: nobody in version 1; admins use `lode unlock --force`.
- [ ] **Show checks on shelves in the editor?** Proposed for a later version, once the UmpaLumpa integration is running.
