# Rules for implementing agents

These rules apply to every task in [docs/tasks/](docs/tasks/README.md).

## Before you start

1. Read [docs/SPEC.md](docs/SPEC.md) and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) in full.
2. Read your task and the tasks it depends on. Check that their deliverables exist in the tree.
   If a dependency is missing, stop and report; do not implement it yourself.
3. Work only inside the crates and files your task names. If you need a change elsewhere
   (a new field in `lode-proto`, a new `MetaStore` method), make the smallest change that works
   and list it in your report.

## Design authority

- The documents are the contract. Do not change behaviour, API shapes, schema or object formats
  on your own. If the documents are wrong, ambiguous or unimplementable, stop and report the
  problem with a proposed wording change. A doc change and the code that needs it land together.
- Modules (`lode-mod-*`) may use only what `ModuleCtx` and the `Module` trait expose. Never
  reach into `lode-server` internals or core tables from a module.

## Code rules

- Rust stable, edition 2021. `#![forbid(unsafe_code)]` in every crate except where a task
  explicitly permits a platform call (then isolate it in one small module with a safety comment).
- No `unwrap`, `expect` or `panic!` outside tests, except for invariants proven on the line above.
- Library crates use typed errors (`thiserror`). `anyhow` only in the two binaries.
- Content is streamed. Never read a whole blob into memory. Tree objects and text being merged
  are the only exceptions.
- Never log tokens, passwords or file content.
- All paths from the network or from disk go through `DepotPath` validation before use.
- Public items have doc comments. Comments explain why, not what.
- Time and randomness are injected (a `Clock` trait, a seedable RNG) wherever tests need them.

## Dependencies

Allowed without asking: `tokio`, `axum`, `tower`, `tower-http`, `hyper`, `rustls`,
`tokio-rustls`, `reqwest` (rustls backend), `serde`, `serde_json`, `toml`, `rusqlite`
(feature `bundled`), `blake3`, `zstd`, `clap`, `thiserror`, `anyhow`, `tracing`,
`tracing-subscriber`, `argon2`, `sha2`, `hmac`, `rand`, `uuid`, `globset`, `ignore`, `walkdir`, `time`,
`bytes`, `futures`, `async-trait`, `unicode-normalization`, `base64`, `hex`, `tempfile`,
`directories`, `rmcp` (official Rust MCP SDK), and for tests `proptest`, `assert_cmd`,
`predicates`, `insta`.

For three-way text merge and diff, evaluate `diffy` and `imara-diff` in the task that needs
them and record the choice.

Anything else: justify it in your report. Prefer the standard library. Check the current
documentation of every crate you use; do not rely on remembered APIs, especially for `axum`,
`rusqlite` and `rmcp`.

## Tests

- Every task lists acceptance criteria. Each one must be covered by an automated test unless it
  says "manual".
- Integration tests use `lode-testkit` (after task T027). Tests must not depend on the network,
  on wall-clock timing, or on each other, and must pass on Linux, Windows and macOS.
- A bug fixed is a test added.

## Definition of done

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo test --workspace` pass.
- Acceptance criteria are met and tested.
- Documents updated if behaviour or interfaces changed.
- Your report states: what was built, deviations from the task, changes outside the task's
  scope, anything left open, and how you verified it. Report failures and gaps plainly.

## Workflow

- One task per branch and per pull request, named `t<id>-<slug>`.
- Commit messages: `T<id>: <summary>`.
- Do not start a task whose dependencies are not merged.
