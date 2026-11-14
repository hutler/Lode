# Lode Desk acceptance checklist

What [the Lode Desk guide](desk-guide.md) promises that only a person can check: installers, an
update, and a link clicked in another app. Everything else in the guide is driven through the
app's commands against a real server by `crates/lode-desk/tests/acceptance.rs` (run by CI in
the Lode Desk job), and the screens by the UI tests.

Run this on each system before a release, with the release's own files, and note the result
and the date. Windows is required for every release; macOS and Linux when someone has them.

You need: a Lode server you can reach, an account that may write a test depot, a second
account (or a colleague) for the lock and review steps, and the previous release of Lode Desk
for the update step.

## Results

| Step | Windows | macOS | Linux |
| --- | --- | --- | --- |
| 1. Install | | | |
| 2. First start and sign in | | | |
| 3. Check out and daily loop | | | |
| 4. Link from another app | | | |
| 5. Second start hands over the link | | | |
| 6. Update | | | |
| 7. Updates turned off | | | |
| 8. Uninstall | | | |

Write ✓ with the date, ✕ with what went wrong, or – when not run. Tester and version:

## Steps

1. **Install.** Install from the release: on Windows the `-setup.exe` (and on another run the
   `.msi`), on macOS the `.dmg`, on Linux the `.deb` or `.rpm`, or the AppImage. The app is in
   the Start menu, Applications or the desktop's app list, with its icon, and starts.
2. **First start and sign in.** Home opens with no recent workspaces and the version under the
   name. Sign in with a wrong password (refused, nothing saved), then the right one. After
   step 3, `lode whoami` run in the workspace folder shows the same account.
3. **Check out and daily loop.** Check out the test depot into an empty folder; the progress
   shows. Edit a text file in an editor: it appears in Changes within a few seconds. Lock a
   binary file; the second account sees the lock and is refused it. Commit two picked files;
   History shows the revision at the top with your name.
4. **Link from another app.** In History, **Copy link** on that revision. Quit Lode Desk.
   Paste the link into a browser's address bar, a chat or a text document and open it. The
   system asks (or not) to open Lode Desk, which starts and shows that revision. Then a link to
   a depot you have no workspace of: the app offers a checkout with the depot filled in. Then
   `lode://someone@example.com/game/r1`: refused with a message, nothing opens.
5. **Second start hands over the link.** With the app running, open another copy of a link:
   no second window appears; the running one comes to the front and shows the link.
6. **Update.** Install the previous release, start it, and wait for the update bar (or
   Home, **Check for updates now**). **What's new** shows the release notes. **Install and
   restart**: the app comes back at the new version (Home shows it), with the same recent
   workspaces and sign-in.
7. **Updates turned off.** Untick **Check for updates automatically** on the previous release
   and restart it: no bar. Then set `LODE_DESK_NO_UPDATE=1` for the app (on Windows, as a user
   environment variable; elsewhere, start it from a terminal with the variable set): **Check
   for updates now** says updates are turned off on this computer.
8. **Uninstall.** Uninstall the app as the system does (Settings, Apps on Windows; drag to the
   Bin on macOS; `apt remove lode-desk` or `dnf remove lode-desk` on Linux). The workspace
   folders stay. On Windows and Linux a `lode://` link no longer opens anything.
