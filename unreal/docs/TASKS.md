# Lode for Unreal: tasks

Each task is sized for one agent session and one branch. Rules: [SPEC.md](SPEC.md) section 9 and [../../AGENTS.md](../../AGENTS.md). Outside this folder, write these ids as `unreal T04`.

Re-read this file just before you edit it, change only your own row, and save at once. Status is one of `todo`, `in progress`, `blocked`, `done`.

| ID | Task | Needs | Status | Owner | Notes |
| --- | --- | --- | --- | --- | --- |
| T01 | [Plugin scaffold](#t01-plugin-scaffold) | none | todo | | |
| T02 | [Lode command runner](#t02-lode-command-runner) | T01 | todo | | |
| T03 | [Provider, connection and settings](#t03-provider-connection-and-settings) | T02 | todo | | |
| T04 | [File states and cache](#t04-file-states-and-cache) | T03 | todo | | |
| T05 | [Add, delete, revert, sync, submit](#t05-add-delete-revert-sync-submit) | T04 | todo | | |
| T06 | [Locks as check out](#t06-locks-as-check-out) | T04 | todo | | |
| T07 | [History and diff](#t07-history-and-diff) | T05 | todo | | |
| T08 | [Rename, copy and resolve](#t08-rename-copy-and-resolve) | T05 | todo | | |
| T09 | [Changelists and shelves](#t09-changelists-and-shelves) | T06, T08 | todo | | |
| T10 | [End-to-end tests with real Lode](#t10-end-to-end-tests-with-real-lode) | T09, and lode M3 | todo | | |
| T11 | [Packaging and guide](#t11-packaging-and-guide) | T10 | todo | | |

## Order

```
T01 -> T02 -> T03 -> T04 -> T05 -> T07
                        \-> T06     T08 (after T05)
                             T09 (after T06 T08) -> T10 -> T11
```

T01 to T03 need no real `lode`: they use the fake from T02. T04 to T09 can be built against the fake but must be checked against the real `lode` once Lode's client milestone M2 and its locks, shelves and changelists milestone M3 are done. T10 needs both.

## T01. Plugin scaffold

**Needs:** none.

**Scope.** `LodeSourceControl.uplugin`, one editor-only module, build files, an empty provider that registers and unregisters cleanly, README, a script that builds the plugin with `RunUAT BuildPlugin`.

**Done when.** The plugin builds for the supported engine versions on the machine, loads in the editor without warnings, and unloads cleanly.

## T02. Lode command runner

**Needs:** T01.

**Scope.** A class that starts `lode` with `--json`, feeds standard input, reads output without deadlock, parses the single JSON document, maps Lode's error codes and exit codes to typed results, supports cancel and a time limit, and splits long path lists into batches. A fake `lode` program for tests that replays recorded JSON by command line. One header that lists every JSON field the plugin reads.

**Done when.** Unit tests cover success, each exit code, malformed output, cancel, time-out and batching. No call runs on the game thread.

## T03. Provider, connection and settings

**Needs:** T02.

**Scope.** Implement the provider: workspace discovery from the project folder, connect and status text, settings object and panel, sign-in dialog using `lode login --token-stdin`, version check, clear messages when `lode` is missing or the project is not in a workspace.

**Done when.** The editor connects to a workspace made by the fake or a real `lode`, shows server, depot, branch and user, and reports each failure case with a message that says what to do.

## T04. File states and cache

**Needs:** T03.

**Scope.** State class with every state in spec section 4, icons and tooltips, the state cache, merged refresh requests, the timer for server information, and measurements against the targets in section 6.

**Done when.** Content browser icons match `lode status` for every state. The performance targets are measured and recorded; if they are missed, the report says where the time goes.

## T05. Add, delete, revert, sync, submit

**Needs:** T04.

**Scope.** Workers for mark for add, delete, revert, sync and submit, including the submit window's description and file selection, "keep locks", and errors such as `out_of_date`, `locked` and `lock_required` shown in plain words.

**Done when.** Each operation works from the editor and leaves the cache correct. A rejected submit names the files and the reason.

## T06. Locks as check out

**Needs:** T04.

**Scope.** Check out runs `lode lock`; the read-only prompt path; lock holder shown for assets locked by others; unlock on revert; refusal to save an asset locked by someone else, with their name.

**Done when.** With two workspaces, a lock taken in one shows in the other within the refresh interval, and editing the locked asset there is refused before any change is made.

## T07. History and diff

**Needs:** T05.

**Scope.** History window from `lode log`; diff against a chosen revision by fetching it to a temporary file and handing both files to the editor's asset diff; clean-up of temporary files.

**Done when.** History shows author and submitter. Diff opens for a blueprint and for a text file. Temporary files are removed when the editor closes.

## T08. Rename, copy and resolve

**Needs:** T05.

**Scope.** Rename and move through `lode mv` with the redirector added, duplicate through `lode cp`, and the resolve flow for conflicted assets.

**Done when.** After a rename, `lode log` on the new path shows the old history. A conflicted binary asset can be resolved to either side from the editor.

## T09. Changelists and shelves

**Needs:** T06, T08.

**Scope.** The editor's changelist interface mapped to Lode changelists; shelve and unshelve from the View Changes window; shelves of other people listed read-only.

**Done when.** Files can be moved between changelists, one changelist can be submitted or shelved alone, and a shelf made by another user can be unshelved.

## T10. End-to-end tests with real Lode

**Needs:** T09, and lode M3.

**Scope.** Automation tests that start a real `loded` on a temporary folder, check out a small Unreal project and drive the plugin through lock, edit, submit, sync, conflict, shelve and unshelve.

**Done when.** The tests pass on Windows three times in a row. The fake from T02 is checked against the real JSON and corrected.

## T11. Packaging and guide

**Needs:** T10.

**Scope.** Built plugin zips per engine version, the `.lodeignore` and `.lodeconfig` templates, an install and daily-use guide with screenshots, and a troubleshooting page.

**Done when.** A person following the guide on a clean machine reaches a working editor connection and submits an asset without other help.
