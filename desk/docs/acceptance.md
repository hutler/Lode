# Lode Desk acceptance checklist

This checklist covers the installers, updates and links opened by another app. The Desk CI job
also tests commands against a real server and drives the screens through UI tests.

## Run packaged acceptance on GitHub

Open **Actions → Desk installer acceptance → Run workflow**. Leave the inputs empty to test
the version in the public update feed. Windows installs the newest older stable Desk MSI,
updates through the app and compares the installed executable with the published MSI. It then
tests sign-in, checkout, lock contention, a two-file commit, revision links and NSIS uninstall.
Ubuntu installs the Debian package and opens two links in the same Desk process.

Before publishing, enter the run ID of a successful **desk-release** build in
`candidate_run`. The job uses its signed packages and serves its update feed on the runner.
For either mode, `previous_tag` selects a particular older release containing a Desk MSI,
and `core_tag` selects the server release for the Windows test. The default server is the
newest stable Lode release. Candidate runs require the unexpired `lode-desk-release` artifact.

Changes to this workflow or its test scripts run the public-feed check on the pull request.
Use the manual checklist below for macOS and for visual checks on each platform.

Run this on each system before a release, with the release's own files, and note the result
and the date. Windows is required for every release; macOS and Linux when someone has them.

You need: a Lode server you can reach, an account that may write a test depot, a second
account (or a colleague) for the lock and review steps, and for the update step either the
previous release of Lode Desk or a test feed (below).

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

1. **Install.** Install from the release: on Windows the `.msi` (and on another run the
   `-setup.exe`), on macOS the `.dmg`, on Linux the `.deb` or the AppImage. The app is in the
   Start menu, Applications or the desktop's app list, with its icon, and starts.
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
   `lode://someone@example.com/game/r1`: refused with a message, nothing opens. (macOS: the app
   starts or comes forward, but does not open the link's target yet; note it and go on.)
5. **Second start hands over the link.** With the app running, open another copy of a link:
   no second window appears; the running one comes to the front and shows the link.
6. **Update.** Install the previous release (or the test build, with the test feed), start it,
   and wait for the update bar (or Home, **Check for updates now**). **What's new** shows the release notes. **Install and
   restart**: the app comes back at the new version (Home shows it), with the same recent
   workspaces and sign-in.
7. **Updates turned off.** Untick **Check for updates automatically** on the previous release
   and restart it: no bar. Then set `LODE_DESK_NO_UPDATE=1` for the app (on Windows, as a user
   environment variable; elsewhere, start it from a terminal with the variable set): **Check
   for updates now** says updates are turned off on this computer.
8. **Uninstall.** Uninstall the app as the system does (Settings, Apps on Windows; drag to the
   Bin on macOS; `apt remove lode-desk` on Linux, or delete the AppImage). The workspace
   folders stay. On Windows and Linux a `lode://` link no longer opens anything.

## A test feed

Until there is a previous release, step 6 uses two builds signed with a test key and a feed
served from the same computer. In `desk/`, with Rust and cargo-packager
installed (`cargo install cargo-packager --locked`); PowerShell on Windows:

1. Make a test key: `cargo packager signer generate -w test.key` (it asks for a password; an
   empty one is fine). `test.key.pub` is the public half. Keep both out of the repository.
2. Build the version to install, with the test key built in, and install its MSI:
   ```
   $env:LODE_DESK_UPDATE_PUBKEY = Get-Content test.key.pub -Raw
   cargo build --release
   cargo packager --release --formats wix
   ```
   The MSI is in `target/release`; install it.
3. Raise `version` in `Cargo.toml` (for example to 0.1.1), build again the same way, and sign
   the new MSI: set `$env:CARGO_PACKAGER_SIGN_PRIVATE_KEY = Get-Content test.key -Raw` (and
   `CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD` if it has one) before `cargo packager`, which
   then writes `….msi.sig` next to it.
4. Put the new MSI and its `.sig` in a folder `feed`, named `lode-desk-0.1.1-windows-x86_64.msi`
   and `.msi.sig`, write the feed with
   `cargo run --example feed -- feed 0.1.1 https://example.invalid > feed\latest.json`, and in
   `feed\latest.json` replace `https://example.invalid` with `http://127.0.0.1:8000`. Serve the
   folder: `python -m http.server 8000 --directory feed`.
5. Start the installed app with `LODE_DESK_UPDATE_FEED=http://127.0.0.1:8000/latest.json` (a
   user environment variable on Windows). It offers 0.1.1; **Install and restart** runs the
   MSI and starts 0.1.1. Put the version in `Cargo.toml` back afterwards.

A feed may only be `http://` on this computer (127.0.0.1, localhost); anywhere else it must be
https. The package must still carry a signature of the key built into the installed copy.
