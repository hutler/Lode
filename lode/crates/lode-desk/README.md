# Lode Desk

The Lode desktop client for Windows, macOS and Linux. It runs the same `lode-ops` operations as
the `lode` command, and keeps its sign-ins in the same place, so signing in with either one
signs in both.

How to use it: [the Lode Desk guide](../../docs/desk-guide.md).

The window is native Rust, made with [Slint](https://slint.dev) (milestone M13, which replaced
the earlier Tauri and React window): Home with recent workspaces, sign-in (password and single
sign-on), check out and workspace tabs (T130), each workspace's pending changes with
changelists (T131), history, blame and diffs (T132), locks, shelves and check results (T133),
branches, merges, conflicts and the workspace view (T134), and `lode://` links, installers and
updates (T135). Nothing in it needs Node.js or a web view.

This folder is its own Cargo workspace, so building the CLI and server never needs the GUI
libraries. It needs Rust 1.92 or newer (Slint's minimum; the rest of Lode needs 1.88).

## Build and run

```
cargo run                      # the Slint window
cargo build --release          # an optimized binary in target/release
```

The screens are `.slint` files in `ui/slint/`, compiled by `build.rs` with the `fluent` style
(Windows 11 controls) on Windows and Linux and `cupertino` on macOS; light or dark follows the
system. Linux needs the fontconfig and xkbcommon development packages (Ubuntu and Debian:
`libfontconfig1-dev libxkbcommon-dev`). Folder pickers are the system's, through `rfd`.

Slint is used under the Slint Royalty-free License 2.0; the About dialog carries the notice it
asks for. See [THIRD-PARTY.md](THIRD-PARTY.md).

## `lode://` links

"Copy link" on a revision, on a file in a revision or in a file's history, and on a shelf copies
a link another person can open in their Lode Desk:

```
lode://HOST[:PORT]/DEPOT/rREV                 a revision
lode://HOST[:PORT]/DEPOT/PATH@REV             a file's history at that revision
lode://HOST[:PORT]/DEPOT/PATH?branch=NAME     a file's history on a branch
lode://HOST[:PORT]/DEPOT/shelf/ID             a shelf
```

The server is `https://HOST[:PORT]`; `?http=1` makes it `http://` for a local server. Path parts
and the branch are percent-encoded. A link opens in an open or recent workspace of the same
server and depot (the app asks when there are several, putting ones on the link's branch first)
and offers a checkout when there is none. Anything that is not exactly one of these forms is
refused with the reason and nothing is opened. A link that arrives while a dialog is open waits
until it closes. The rules are in `lode-ops` (`src/links.rs`, shared with `lode link`), tested against its `tests/link_cases.json`.

Only one copy of the app runs per user (`src/instance.rs`): the first holds a lock file in the
app's data folder and listens on a loopback port it writes there with a random token; starting
the app again hands its link (or, without one, a request to come forward) to the running copy
and exits. Only someone who can read the user's data folder can send it links, and they are
parsed as strictly as any other.

The installers register the `lode` scheme with the system (Windows: under
`Software\Classes\lode`; Linux: the `.desktop` file's `x-scheme-handler/lode`; macOS: the app's
`Info.plist`), and Windows and Linux start the app with the link as its argument. macOS hands
links to a running app as an Apple event, which the window does not take yet: there a link
starts or brings forward Lode Desk without opening its target.

## Tests

- `cargo test`: every operation the window calls, against a test server; the link rules
  (`tests/links.rs`); one copy per user and the link hand-over between two starts
  (`tests/instance.rs`); updates (`tests/update.rs`: the feed, a package signed with the key,
  with another key, and damaged, the settings and `LODE_DESK_NO_UPDATE`); and `tests/ui.rs`, the
  window driven headless through Slint's testing backend against test servers, from sign-in to
  links handed to the app and the update banner. Controls are found by their accessible labels.
- `cargo run --example screenshots -- DIR`: renders the screens in light and dark into PNG
  files with the software renderer (default `target/screenshots`). The guide's pictures in
  `docs/desk-guide/` are the light ones.
- `cargo test --test acceptance`: the guide followed end to end by two people against a real
  server; [docs/desk-acceptance.md](../../docs/desk-acceptance.md) is the checklist for what
  needs a person (installers, an update, links from other apps).

## Installers and updates

[cargo-packager](https://github.com/crabnebula-dev/cargo-packager) makes the installers of the
system it runs on, from `[package.metadata.packager]` in `Cargo.toml`:

```
cargo install cargo-packager --locked
cargo build --release
cargo packager --release --formats wix,nsis     # Windows: an MSI (WiX) and a setup program (NSIS)
cargo packager --release --formats deb,appimage # Linux
cargo packager --release --formats app,dmg      # macOS
```

The MSI installs for all users into `Program Files\Lode Desk`, the setup program for the current
user into `%LOCALAPPDATA%\Lode Desk`: the names, identifier and folders of the Tauri installers
before them (none was released). On Windows the program carries its icon and manifest
(`packaging/windows/`, embedded by `build.rs`). The release workflow builds every installer; see
[docs/release.md](../../docs/release.md).

The app looks for a new version a moment after start and once a day, in `latest.json` of the
release `lode-desk-feed` on GitHub, and offers it with its release notes, "Install and restart"
and "Later". Home has the setting to turn the daily check off and a "Check for updates now"
button. Setting `LODE_DESK_NO_UPDATE` (to anything) turns every check off, for machines whose
software is managed. A package is installed only when its minisign signature (checked with
`minisign-verify`) matches the public key built into the app from `LODE_DESK_UPDATE_PUBKEY` at
build time; a build without that key, such as one from source, never offers updates.
`LODE_DESK_UPDATE_FEED` points a copy at another feed (a test feed; its packages still need the
built-in key). Installing runs the MSI or setup program passively, replaces the AppImage or the
`.app`, or installs the `.deb` through `pkexec`, then starts the new version. The feed is
written by `cargo run --example feed` and checked against `packaging/feed.schema.json`, in the
format the Tauri updater read, with the same key.
