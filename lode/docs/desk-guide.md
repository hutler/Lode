# Lode Desk guide

Lode Desk is the desktop app for Lode. It shows a workspace's changes, history, locks, shelves,
branches and view, and runs the same operations as the `lode` command, so the two can be used
side by side on the same workspace. This guide follows a working day from installing the app
to merging a branch. Each part says what to click and what you should see.

The pictures come from `npm run guide-screens` in `crates/lode-desk/ui`, against the app's
demo data (a depot called `skyfall` on `lode.example.com`).

Contents: [Install](#install) · [Sign in](#sign-in) · [Check out](#check-out-a-depot) ·
[The daily loop](#the-daily-loop) · [History, blame and diffs](#history-blame-and-diffs) ·
[Shelves and review](#shelves-and-review) · [Branches and merges](#branches-and-merges) ·
[Conflicts](#conflicts) · [The workspace view](#the-workspace-view) · [Links](#links) ·
[Updates](#updates) · [Errors](#what-errors-mean)

## Install

Download the file for your system from the project's release page (tags `lode-v<version>`):

| System | File | Notes |
| --- | --- | --- |
| Windows 10 and 11 | `lode-desk-<version>-windows-x86_64-setup.exe`, or the `.msi` | The setup installs for you alone and needs no administrator; the MSI installs for everyone. |
| macOS 11 or newer | `lode-desk-<version>-macos-universal.dmg` | Open it and drag Lode Desk to Applications. |
| Debian, Ubuntu | `lode-desk-<version>-linux-x86_64.deb` | `sudo apt install ./lode-desk-….deb` |
| Fedora, RHEL | `lode-desk-<version>-linux-x86_64.rpm` | `sudo dnf install ./lode-desk-….rpm` |
| Any Linux | `lode-desk-<version>-linux-x86_64.AppImage` | Make it executable and run it. |

Until the project signs its installers, Windows may show "Windows protected your PC": choose
More info, then Run anyway. On macOS, right-click the app and choose Open the first time.

Every installer registers `lode://` links, so a link to a revision, file or shelf opens in the
app (see [Links](#links)). Lode Desk keeps its sign-ins in the same place as the `lode`
command: signing in with either one signs in both.

![Home: recent workspaces and ways to start](desk-guide/welcome-light.png)

Home lists the workspaces you opened recently. The × beside one forgets it (the folder stays).

## Sign in

Home, **Sign in to a server…**. Give the server address (such as `https://lode.example.com`),
your name and your password. A wrong password says so and nothing is saved.

![Signing in](desk-guide/sign-in-light.png)

## Check out a depot

Home, **Check out a depot…**. The depot is the server address followed by the depot name, such
as `https://lode.example.com/skyfall`. Pick a branch (usually `main`) and an empty folder. The
progress shows the files arriving; then the workspace opens in a tab of its own.

![Checking out](desk-guide/check-out-light.png)

A workspace you already have opens from Home's list or with **Open a workspace folder…**. Each
open workspace has a tab; the sidebar shows its depot, server, branch and revision.

![A workspace](desk-guide/workspace-light.png)

## The daily loop

**Changes** lists what differs from the revision you have: modified (M), added (A), deleted
(D), files not added yet (?), and files in conflict (C). The list follows the disk: save a
file in your editor and it shows up within a moment.

![Picking files to commit](desk-guide/changes-picked-light.png)

- **Pick** files with their check boxes (or a whole group with the group's box), write a
  message and **Commit**. Only the picked files go in; the rest stay as they are. Files not
  added yet are added when picked.
- Click a file to see its details on the right: **Show diff**, **Revert…**, **Lock…**,
  **History** and **Blame**. Right-click a file for the same actions.
- If someone committed the same file first, the commit is refused and names the files. Update
  (the branch button, then **Update to r…**; see [Branches](#branches-and-merges)) and commit
  again.

![A refused commit names the files](desk-guide/commit-refused-light.png)

### Locks

Files that cannot be merged, such as art and maps, are marked **needs lock**. Lock one before
you change it: **Lock…** on the file, with a comment others will see. **Locks** in the sidebar
lists every lock on the branch, yours first, with Unlock for yours. A commit releases the locks of the files it
commits.

![Locks](desk-guide/locks-light.png)

When someone else holds the lock, the lock is refused and says who. Only a depot admin can
break another person's lock.

![A lock held by someone else](desk-guide/lock-refused-light.png)

### Changelists

**New changelist…** groups changes you want to commit separately, such as a fix you are not
ready to send. Right-click files and choose a changelist under **Move to**. A changelist has
its own **Commit…**, which uses its description as the message, plus **Shelve…** and
**Delete…** (deleting a changelist sends its files back to Changes; no change is lost).

![Changelists](desk-guide/changelists-light.png)

## History, blame and diffs

**History** shows the branch's revisions, newest first, with a graph of merges, a badge with
the checks of each revision (passed, failed or still running) and badges for agents and
imports. Scroll for older
ones; they load as you go. Click a revision for its message, author, checks and changed files.
**Copy link** copies a link to the revision, and **Link** beside a file copies a link to that
file at that revision.

![History](desk-guide/history-light.png)

**History** on a file shows only the revisions that changed it, following renames.

![A file's history](desk-guide/file-history-light.png)

**Blame** shows, for each line, the revision that last changed it. Click a revision number to
open that revision.

![Blame](desk-guide/blame-light.png)

**Show diff** compares your file with the revision you have, or a file in a revision with the
one before. Switch between unified and side by side; the choice is remembered.

![A diff](desk-guide/diff-light.png)

Images are shown side by side, as a swipe or as an onion skin.

![Comparing two versions of an image](desk-guide/image-swipe-light.png)

## Shelves and review

A shelf keeps changes on the server without committing them: to ask for a review, to move
work to another computer, or to put something aside. Pick files in Changes and choose
**Shelve…**. By default the shelved files are reverted here; tick **Keep my changes in the
workspace** to keep them.

![Shelving](desk-guide/shelve-light.png)

**Shelves** lists open, submitted and deleted shelves, yours or everyone's. A shelf shows its
description, files, diff and checks.

![Shelves](desk-guide/shelves-light.png)

- **Unshelve** brings a shelf's changes into your workspace, to try them or to continue them.
- **Submit…** commits the shelf as it is. If the owner updated it after you looked, the submit
  is refused so that you review what changed first.
- When the depot requires checks, a shelf whose checks have not passed cannot be submitted;
  the refusal lists the checks still missing.
- **Delete…** removes your own shelf. **Copy link** copies a link to it, for a review request.

![A shelf with its checks](desk-guide/shelf-checks-light.png)

## Branches and merges

The branch button at the top of the sidebar opens **Branches**: every branch with its head and
where it started. From there:

- With your own branch selected, **Update to r…** brings the workspace to the head of its
  branch. Do this before you start, and when a commit is refused as out of date.
- With another branch selected, **Switch** moves the workspace to it (to its head, or to the
  revision you type). Your local changes come along; files are only fetched where they differ.
  Locks you hold stay on the old branch, and the summary lists them.
- **New branch…** starts a branch from your revision, or from any branch at any revision
  (`release-1.2@r1400`), and can switch to it.
- **Merge…** merges the selected branch into yours.
- **Archive…** hides a finished branch (depot admins only); **Show archived** lists them.

![Branches](desk-guide/branches-light.png)

![A new branch](desk-guide/new-branch-light.png)

After a switch or update, a summary says what came and what stayed. If it stopped half way,
for example because the connection dropped, an error offers **Finish the update**.

![After a switch](desk-guide/switched-light.png)

A merge applies the other branch's changes to your files and tells you what happened to each:
applied, merged, conflicted or unchanged. Changes then shows the merge in progress. **Commit
merge** commits everything together; **Abandon merge…** forgets the merge and keeps the files.

![The result of a merge](desk-guide/merged-light.png)

## Conflicts

A file changed on both sides that did not merge is in conflict (C). Commit stays off until no
conflict is left. **Resolve…** on the file opens the three-way view: the base, yours and
theirs above, and the result below. Each conflict has **Use mine**, **Use theirs** and **Use
both**, and the result can also be edited by hand; **Take mine** and **Take theirs** settle the
whole file at once. **Save and mark resolved** saves the result once no conflict is left.

![The three-way conflict view](desk-guide/three-way-light.png)

For a file that is not text, such as an image, choose **Keep mine** or **Take theirs**. Theirs
is also saved beside the file (`name.theirs-r<rev>`) so you can open it in its own tool first.

## The workspace view

A depot can be larger than you need. **Workspace** in the sidebar says how much of the depot
is in your view and on disk, and lets you choose which folders come to disk: tick a folder to
bring it, untick it to leave it out. The ticks become include and exclude patterns, which you
can also edit as text (`*` stays within a folder, `**` goes into subfolders; excludes win).

**Preview change** says how many files would be fetched and removed before anything happens;
**Apply** does it, and you can keep working while it runs. A file with local changes that would
leave the view stops the change; commit, shelve or revert it, or drop the changes with a
second confirmation.

![Choosing what comes to disk](desk-guide/view-editor-light.png)

## Links

**Copy link** on a revision, on a file in a revision, and on a shelf copies a `lode://` link
you can paste into chat, a ticket or a review:

```
lode://lode.example.com/skyfall/r1482
lode://lode.example.com/skyfall/Source/Skyfall/Boat.cpp@1477
lode://lode.example.com/skyfall/shelf/44
```

Clicking one opens it in Lode Desk (starting the app if needed). The app picks a workspace of
the same server and depot from your open tabs and Home's list. When several fit, it asks which
one; when none does, it offers to check the depot out and then shows the link.

![Choosing a workspace for a link](desk-guide/link-choose-light.png)

![No workspace for a link yet](desk-guide/link-none-light.png)

A link that is not exactly one of these forms is refused with the reason, and nothing is
opened. Links never carry user names or passwords.

![A refused link](desk-guide/link-refused-light.png)

## Updates

The app looks for a new version when it starts and once a day. When there is one, a bar at the
top shows it, with **What's new**, **Install and restart** and **Later**. An update is
installed only when it is signed with the project's key; anything else is refused.

![A new version](desk-guide/update-light.png)

Home has **Check for updates automatically** and **Check for updates now**. On machines whose
software is managed, setting the environment variable `LODE_DESK_NO_UPDATE` (to anything)
turns every check off.

## What errors mean

Errors show the message, a next step where one helps, and a code. The codes:

| Code | What happened | What to do |
| --- | --- | --- |
| `unauthenticated` | You are not signed in to that server, or the sign-in expired. | Sign in from Home. |
| `permission_denied` | Your account may not do this here. | Ask a depot admin. |
| `not_found` | The depot, branch, revision, file or shelf does not exist (any more). | Check the name; Refresh. |
| `not_a_workspace` | The folder is not a Lode workspace. | Pick the folder you checked out into. |
| `already_a_workspace` | The checkout folder already holds a workspace. | Open it, or pick an empty folder. |
| `workspace_in_use` | A `lode` command is using the workspace right now. | Try again when it has finished. |
| `checkout_incomplete` | A checkout stopped before it finished. | Check out into the same folder again. |
| `update_incomplete` | An update or switch stopped before it finished. | **Finish the update**. |
| `network` | The server cannot be reached. | Check the address and your connection. |
| `insecure_url` | The address is plain `http://` to another computer. | Use `https://`. |
| `out_of_date` | Someone committed these files first. | Update, then commit again. |
| `locked` | Someone else holds the lock. | Ask them, or a depot admin. |
| `lock_required` | The file must be locked before it is committed. | Lock it, then commit. |
| `conflicted` | Files are still in conflict. | Resolve them first. |
| `conflict` | The shelf changed since you looked, or the name is taken. | Look again, then retry. |
| `checks_required` | The shelf's required checks have not passed. | Wait for them or fix the change. |
| `module_disabled` | The server does not have that feature (locks, shelves, checks). | Ask the server's admin. |
| `usage` | The request itself was wrong, such as a bad branch name. | Read the message. |
| `invalid_path`, `invalid_pattern` | A path or view pattern cannot be used. | Fix it. |
| `bad_signature` | An update was not signed with the project's key, so it was not installed. | Report it; install by hand from the release page. |
| `update_failed` | The update could not be downloaded or installed. | Try later or install by hand. |
| `io` | A file could not be read or written. | Check disk space and permissions. |

![An error with its next step](desk-guide/open-error-light.png)

---

How this guide is checked: `crates/lode-desk/tests/acceptance.rs` follows it section by section
against a real server with two people, and [desk-acceptance.md](desk-acceptance.md) is the
checklist for what needs a person.
