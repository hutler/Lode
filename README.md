# Lode

[简体中文](README.zh-CN.md)

<p align="center">
  <img src="docs/logo/lode-banner-v2.png" alt="Lode wordmark beside a red-haired pixel-art miner holding a turquoise crystal" width="592">
</p>

Lode is a self-hosted version control system for teams that keep source code and large binary assets together. One server tracks revisions, file locks and reviewable shelves. Developers can work from the command line, artists from Lode Desk or Unreal Editor, and agents through the CLI or MCP.

## Get started

Install the client for your platform from [GitHub Releases](https://github.com/hutler/Lode/releases). If your team already has a server and a depot, sign in and check out a workspace. Replace the example address and depot with yours:

```sh
lode login https://lode.example.com
lode checkout https://lode.example.com/game game
cd game
lode status --remote
```

To host a new server, follow the [Linux deployment guide](lode/docs/deploy-linux-for-agents.md) or the [installation and user guide](lode/docs/user-guide.html). To bring an existing project into Lode, use `lode init` for the files in a directory or the Git and SVN importers for existing history. [Command reference](lode/docs/CLI-JSON.md).

## How teams use it

- **Work on large projects.** A workspace view selects the paths to check out. The server deduplicates content and transfers changed chunks for large files. Teams can require exclusive locks on binary assets.
- **Review changes.** Group pending edits in changelists, propose them as versioned shelves and submit the version that was reviewed. Depot policies can require CI checks and content validation before submission.
- **Integrate branches.** Commits are atomic. Branches share depot-wide revision numbers, and merges record their source. Streams support teams with prescribed integration directions.
- **Automate through stable interfaces.** The CLI returns structured JSON. Local MCP works on a workspace; remote MCP reads depots and proposes shelves without a checkout. Agents have their own identities, scopes and recorded authorship.

## Documentation

| Audience or task | Start here |
| --- | --- |
| Everyday use | [User guide](lode/docs/user-guide.html) · [PDF](lode/docs/Lode-User-Guide.pdf) |
| Agent working in a depot | [Agent guide and task index](docs/agent-guide.md) · [MCP tools](lode/docs/MCP.md) · [CLI JSON](lode/docs/CLI-JSON.md) |
| Writing pre-commit checks | [Local and server validation](docs/agent-validation.md) · [Runnable Python examples](lode/examples/validate/) |
| Running a server or integrating CI | [Operations guide](docs/agent-operations.md) · [Upgrades and backups](lode/docs/UPGRADING.md) |
| Desktop and editor workflows | [Desk guide](desk/docs/guide.md) · [Unreal guide](unreal/docs/guide.md) |
| Building or contributing | [Client and release workflows](docs/agent-clients.md) · [Repository rules](AGENTS.md) · [Architecture](lode/docs/ARCHITECTURE.md) |

## Build from source

The core in [lode/](lode/) requires Rust 1.88 or newer. On Windows it also needs the MSVC C++ build tools.

```sh
cd lode
cargo build --workspace
cargo test --workspace
```

[Lode Desk](desk/) is a separate Rust workspace with its own core pin, CI and release tags. It requires Rust 1.92 or newer. The [Unreal plugin](unreal/) calls the installed CLI and consumes its JSON output. Check [published releases](https://github.com/hutler/Lode/releases) for downloadable builds and the [task index](lode/docs/tasks/README.md) for implementation status.

Lode is licensed under [Apache 2.0](LICENSE).
