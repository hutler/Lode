# Releasing Lode

A release is one tag, `lode-v<version>`, which `.github/workflows/lode-release.yml` turns into
a GitHub release holding the `lode` and `loded` archives, the client and server packages, the
server image, and the Lode Desk bundles with their update feed. This page is the checklist.

## Once per repository

These are set up once, by someone with admin rights on the repository.

- **Lode Desk update key.** Updates of the app are accepted only when signed with this key.
  Make it on your own computer, never in CI or a shared folder:

  ```
  cd lode/crates/lode-desk
  ./ui/node_modules/.bin/tauri signer generate -w ~/.tauri/lode-desk-update.key
  ```

  Choose a password when asked. Then, under the repository's Settings, Secrets and variables,
  Actions:

  | Kind | Name | Value |
  | --- | --- | --- |
  | Secret | `LODE_DESK_UPDATE_KEY` | the content of `~/.tauri/lode-desk-update.key` |
  | Secret | `LODE_DESK_UPDATE_KEY_PASSWORD` | its password |
  | Variable | `LODE_DESK_UPDATE_PUBKEY` | the content of `~/.tauri/lode-desk-update.key.pub` |

  Keep the private key and its password in a password manager as well. If the key is lost,
  installed copies can no longer be updated by the app: people must install the next version
  by hand. The private key never goes into the repository.

  Without these the release still builds every bundle, but unsigned for updates, and those
  copies never offer an update (they say "This build does not update itself").

- **Code signing (optional, off until the project has certificates).** The workflow signs
  when these secrets exist and skips signing otherwise:
  - Windows: `WINDOWS_CERTIFICATE` (the `.pfx`, base64) and `WINDOWS_CERTIFICATE_PASSWORD`.
  - macOS: `APPLE_CERTIFICATE` (the `.p12`, base64), `APPLE_CERTIFICATE_PASSWORD`,
    `APPLE_SIGNING_IDENTITY`, and for notarization `APPLE_ID`, `APPLE_PASSWORD` (an app
    password) and `APPLE_TEAM_ID`.

  Unsigned, Windows SmartScreen warns on the first start of the installer and macOS needs a
  right click, Open, the first time.

## Before the tag

- [ ] `main` is green in `.github/workflows/lode.yml`, including the Lode Desk jobs.
- [ ] The version is the same in `lode/Cargo.toml` (workspace), `lode/crates/lode-desk/Cargo.toml`
      and `lode/crates/lode-desk/tauri.conf.json`. The workflow takes the version from the tag,
      so these only matter for builds without a tag, but they should agree.
- [ ] `docs/UPGRADING.md` says what changes for people upgrading a server, if anything.
- [ ] The user guide and the Lode Desk guide describe what changed; the PDF is regenerated.
- [ ] A manual run of the workflow ("Run workflow" on `lode-release`) passed on the commit to
      be tagged. It builds everything without publishing; its `lode-desk-release` artifact
      holds the bundles and, when the update key is set, a `latest.json` checked against
      `lode/crates/lode-desk/packaging/feed.schema.json`.

## The tag

```
git tag -a lode-v0.2.0 -m "Lode 0.2.0"
git push origin lode-v0.2.0
```

The workflow then:

1. builds the CLI and server for five targets and the server packages, installing each
   package once before going on;
2. creates the release with generated notes;
3. builds Lode Desk on Linux (`.deb`, `.rpm`, AppImage), Windows (MSI and NSIS setup) and macOS
   (universal `.app` and `.dmg`), installs the `.deb` on Ubuntu and the `.rpm` on Fedora,
   and checks that each one registers `lode://` links;
4. attaches the Lode Desk bundles to the release, writes the update feed from the bundles'
   signatures and the release notes, checks it against its schema and uploads it as
   `latest.json` to the fixed release `lode-desk-feed`, where installed copies look;
5. pushes the server image.

## After the tag

- [ ] The release lists the archives, packages, `SHA256SUMS` and the `lode-desk-*` files.
- [ ] `https://github.com/<owner>/Lode/releases/download/lode-desk-feed/latest.json`
      names the new version.
- [ ] On one machine with the previous Lode Desk: Home, "Check for updates now" finds the new
      version; "Install and restart" brings it up at the new version.
- [ ] Open a `lode://` link from a browser or chat on Windows and Linux: it opens in the app.
- [ ] `lode self-update` on a machine with the previous client finds the new version.
- [ ] winget and Homebrew manifests from the `lode-release` artifact are submitted if wanted.

## If something is wrong

Delete the tag and the release, fix, and tag again with the next patch version. Do not reuse
a version: installed copies may already have seen it in the feed. To stop copies from updating
to a bad version, upload the previous release's `latest.json` to `lode-desk-feed` again
(`gh release upload lode-desk-feed latest.json --clobber`); copies already updated stay.
