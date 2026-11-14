# Lode Desk

The Lode desktop client for Windows, macOS and Linux: Tauri 2 with a React and TypeScript
window (`ui/`). It runs the same `lode-ops` operations as the `lode` command, and keeps its
sign-ins in the same place, so signing in with either one signs in both.

This folder is its own Cargo workspace, so building the CLI and server never needs the system
web view libraries. It needs Rust 1.90 or newer (Tauri's minimum; the rest of Lode needs 1.88)
and Node.js 22.

## Build and run

Linux needs the WebKitGTK development packages first (Ubuntu and Debian:
`libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libayatana-appindicator3-dev
libsoup-3.0-dev`). Windows uses the WebView2 runtime that ships with Windows 10 and 11.

```
cd ui && npm ci && cd ..
npx --prefix ui tauri dev        # the app with live reload
npx --prefix ui tauri build --no-bundle   # an optimized binary in target/release
```

`cargo build` alone also works once `npm run build` has filled `ui/dist`; add
`--features tauri/custom-protocol` to serve that folder instead of the dev server.

## Tests

- `cargo test`: every command the window calls, against a test server.
- `npm test` in `ui/`: the screens, against the mocked backend (`ui/src/mock.ts`).
- `npm run screenshots` in `ui/`: renders each screen in light and dark into
  `ui/screenshots/` with Playwright. `npm run mock` opens the same mocked app in a browser.

Installers and auto-update come with M11 (T114).
