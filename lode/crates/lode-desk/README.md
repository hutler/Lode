# Lode Desk

The Lode desktop client for Windows, macOS and Linux: Tauri 2 with a React and TypeScript
window (`ui/`). It runs the same `lode-ops` operations as the `lode` command, and keeps its
sign-ins in the same place, so signing in with either one signs in both.

How to use it: [the Lode Desk guide](../../docs/desk-guide.md).

This folder is its own Cargo workspace, so building the CLI and server never needs the system
web view libraries. It needs Rust 1.90 or newer (Tauri's minimum; the rest of Lode needs 1.88)
and Node.js 22.

## Build and run

Linux needs the WebKitGTK development packages first (Ubuntu and Debian:
`libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libayatana-appindicator3-dev
libsoup-3.0-dev`). Windows uses the WebView2 runtime that ships with Windows 10 and 11.

```
cd ui && npm ci && cd ..
./ui/node_modules/.bin/tauri dev                 # the app with live reload
./ui/node_modules/.bin/tauri build --no-bundle   # an optimized binary in target/release
./ui/node_modules/.bin/tauri build --bundles deb # an installer (see below)
```

On Windows, use `ui\node_modules\.bin\tauri` in the same way.

`cargo build` alone also works once `npm run build` has filled `ui/dist`; add
`--features tauri/custom-protocol` to serve that folder instead of the dev server.

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
refused with the reason and nothing is opened. The rules are in `src/links.rs` and
`ui/src/links.ts`, both tested against `ui/src/linkCases.json`.

Only one copy of the app runs: starting it again with a link hands the link to the running one.
Installers register the `lode` scheme with the system. A build run with `tauri dev` registers it
for the current user when it starts on Windows (under `HKEY_CURRENT_USER\Software\Classes`) and
Linux (a `.desktop` file), and so does an AppImage; on macOS only a bundled app receives links.

## Tests

- `cargo test`: every command the window calls, against a test server, the link rules, and
  update checks (an update signed with another key is refused).
- `node --test packaging/feed.test.mjs`: the update feed.
- `npm test` in `ui/`: the screens, against the mocked backend (`ui/src/mock.ts`).
- `cargo test --test acceptance`: the guide followed end to end by two people against a real
  server; [docs/desk-acceptance.md](../../docs/desk-acceptance.md) is the checklist for what
  needs a person (installers, an update, links from other apps).
- `npm run guide-screens` in `ui/`: the guide's pictures, into `docs/desk-guide/`.
- `npm run screenshots` in `ui/`: renders each screen in light and dark into
  `ui/screenshots/` with Playwright (`SCENES=branches,switched` renders only those). `npm run mock` opens the same mocked app in a browser.

## Installers and updates

`tauri build` makes the installers of the system it runs on: an MSI and an NSIS setup on Windows
(the MSI needs WiX, which the build downloads), a `.app` and `.dmg` on macOS
(`--target universal-apple-darwin` for both chips), and `.deb`, `.rpm` and AppImage on Linux.
Each registers `lode://` links. The release workflow builds them all; see
[docs/release.md](../../docs/release.md).

The app looks for a new version at start and once a day, in `latest.json` of the release
`lode-desk-feed` on GitHub, and offers it with its release notes, "Install and restart" and
"Later". Home has the setting to turn the daily check off and a "Check for updates now" button.
Setting `LODE_DESK_NO_UPDATE` (to anything) turns every check off, for machines whose software
is managed. A package is installed only when its signature matches the public key built into
the app; a build without that key, such as one from source, never offers updates. The feed is
written by `packaging/feed.mjs` and checked against `packaging/feed.schema.json`.
