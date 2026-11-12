# Lode

A centralized, open-source version control system for small teams that work with source code
and large binary files. Part of SimpleDevOps.

- What it does: [docs/SPEC.md](docs/SPEC.md)
- How it is built: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- Tasks and status: [docs/tasks/README.md](docs/tasks/README.md)
- Rules for contributors and agents: [AGENTS.md](AGENTS.md)

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
