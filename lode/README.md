# Lode

A centralized, open-source version control system for small teams that work with source code
and large binary files. Part of SimpleDevOps.

- What it does: [docs/SPEC.md](docs/SPEC.md)
- How it is built: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- Tasks and status: [docs/tasks/README.md](docs/tasks/README.md)
- Rules for contributors and agents: [AGENTS.md](AGENTS.md)
- Using and running it: [docs/user-guide.html](docs/user-guide.html)
  ([PDF](docs/Lode-User-Guide.pdf))
- The desktop app, Lode Desk: [docs/desk-guide.md](docs/desk-guide.md)
- Robustness tests and their findings: [docs/HARDENING.md](docs/HARDENING.md)
- Performance against the SPEC targets: [docs/BENCHMARKS.md](docs/BENCHMARKS.md)

## Installing

Releases are published on the repository's GitHub Releases page under tags `lode-v<version>`.
Each holds archives for Linux (static, x86_64 and aarch64), Windows (x86_64) and macOS (Intel
and Apple silicon) with `lode`, `loded` and the service files in `packaging/`, a `SHA256SUMS`
list, and the server image `ghcr.io/<owner>/loded`. Unpack, put the binaries on your `PATH`,
and follow the user guide: Part I for the client, Part II for running a server (systemd,
Windows service, container).

The same release holds Lode Desk, the desktop app: an MSI and an NSIS setup for Windows, a
universal `.dmg` for macOS, and `.deb`, `.rpm` and AppImage for Linux. Installed, it keeps
itself up to date (see [crates/lode-desk/README.md](crates/lode-desk/README.md)).

A release is made by pushing a tag `lode-v<version>`; `.github/workflows/lode-release.yml`
builds and publishes it. The checklist, including the keys it needs, is in
[docs/release.md](docs/release.md).

```
docker build -t loded .                 # the server image, from this folder
packaging/docker/smoke.sh loded         # start, commit, restart, check
```

## Building

Needs Rust 1.88 or newer (`rust-toolchain.toml` selects the current stable) and, on Windows,
the MSVC C++ build tools.

```
cargo build --workspace
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

The binaries are `lode` (client) and `loded` (server).
