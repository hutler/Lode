# Releasing Lode

A release is one tag, `lode-v<version>`, which `.github/workflows/lode-release.yml` turns into
a GitHub release holding the `lode` and `loded` archives, the client and server packages, the
server image, and the Lode Desk installers with their update feed. This page is the checklist.

## Once per repository

These are set up once, by someone with admin rights on the repository.

- **Lode Desk update key.** Updates of the app are accepted only when signed with this key.
  Make it on your own computer, never in CI or a shared folder:

  ```
  cargo install cargo-packager --locked
  cargo packager signer generate --path ~/.lode-desk/update.key
  ```

  Choose a password when asked. Then, under the repository's Settings, Secrets and variables,
  Actions:

  | Kind | Name | Value |
  | --- | --- | --- |
  | Secret | `LODE_DESK_UPDATE_KEY` | the content of `~/.lode-desk/update.key` |
  | Secret | `LODE_DESK_UPDATE_KEY_PASSWORD` | its password |
  | Variable | `LODE_DESK_UPDATE_PUBKEY` | the content of `~/.lode-desk/update.key.pub` |

  A key made earlier with `tauri signer generate` has the same format and works as it is.

  Keep the private key and its password in a password manager as well. If the key is lost,
  installed copies can no longer be updated by the app: people must install the next version
  by hand. The private key never goes into the repository.

  Without these the release still builds every installer, but unsigned for updates, and those
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
- [ ] The version in `lode/Cargo.toml` (workspace) is the tag's, and
      `lode/crates/lode-desk/Cargo.toml` agrees. The programs report the workspace version, so
      the workflow stops if it differs from the tag.
- [ ] `docs/UPGRADING.md` says what changes for people upgrading a server, if anything.
- [ ] The user guide and the Lode Desk guide describe what changed; the PDF is regenerated.
- [ ] A manual run of the workflow ("Run workflow" on `lode-release`) passed on the commit to
      be tagged. It builds everything without publishing; its `lode-desk-release` artifact
      holds the installers and, when the update key is set, a `latest.json` checked against
      `lode/crates/lode-desk/packaging/feed.schema.json`.

## Build versions

Every commit builds programs that say which commit they came from, so a build from any push
to `main` can be told apart from the release and from other pushes. `lode --version`,
`loded --version`, `/api/v1/info` (`server_version`), the admin pages and Lode Desk's About
show it; `lode --version --json` has it as `build`. The crate `lode-build` works it out in its
`build.rs`:

| Built from | Build version |
|---|---|
| The commit tagged `lode-v0.2.0` | `0.2.0` |
| Five commits later, at `4a33c89` | `0.2.0+5.g4a33c89` |
| A shallow clone, no tag reachable | `0.2.0+g4a33c89` |
| No Git (a source archive, the Docker build) | `0.2.0` |
| `LODE_BUILD_VERSION` set | that value |

The part after `+` is SemVer build metadata: version comparisons (`lode self-update`, Lode Desk
updates, the Unreal plugin's minimum version) use the release version and ignore it. Nothing
needs bumping by hand for this; the release version in `Cargo.toml` changes only for a release.

## The tag

```
git tag -a lode-v0.2.0 -m "Lode 0.2.0"
git push origin lode-v0.2.0
```

The workflow then:

1. builds the CLI and server for five targets and the server packages, installing each
   package once before going on;
2. creates the release with generated notes;
3. builds the Lode Desk installers with cargo-packager on Linux (`.deb`, AppImage), Windows
   (MSI and NSIS setup) and macOS (universal `.app` and `.dmg`), installs the `.deb` on Ubuntu
   and the MSI on Windows, and checks that the `.deb`, the AppImage and the MSI register
   `lode://` links;
4. attaches the Lode Desk installers to the release, writes the update feed from their
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
