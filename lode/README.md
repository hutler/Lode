# Lode

A centralized, open-source version control system for small teams that work with source code
and large binary files. Part of SimpleDevOps.

- What it does: [docs/SPEC.md](docs/SPEC.md)
- How it is built: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- Tasks and status: [docs/tasks/README.md](docs/tasks/README.md)
- Rules for contributors and agents: [AGENTS.md](AGENTS.md)
- Using and running it: [docs/user-guide.html](docs/user-guide.html)
  ([PDF](docs/Lode-User-Guide.pdf))
- Robustness tests and their findings: [docs/HARDENING.md](docs/HARDENING.md)

## Installing

Release archives for Linux (static, x86_64 and aarch64), Windows (x86_64) and macOS (Intel
and Apple silicon) hold `lode`, `loded` and the service files in `packaging/`, with a
`SHA256SUMS` list. A release is made by pushing a tag `lode-v<version>`; the workflow
`.github/workflows/lode-release.yml` builds the archives, publishes them, and pushes the
server image `ghcr.io/<owner>/loded`. The user guide covers installation, systemd, Windows
(WinSW) and the container.

```
docker build -t loded .                 # the server image, from this folder
packaging/docker/smoke.sh loded         # start, commit, restart, check
```

## Building

Needs a stable Rust toolchain (`rust-toolchain.toml` selects it) and, on Windows, the MSVC
C++ build tools.

```
cargo build --workspace
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

The binaries are `lode` (client) and `loded` (server).
