# Client workflows, development and releases

Use this guide when a task crosses the boundary between a workspace, a user interface and a build. Start with the exact client build, core revision, server, branch and workspace. For a shelf, also record its ID and version. [Agent workflow](agent-guide.md).

## Hand a shelf to a Desk reviewer

1. In the CLI or local MCP, inspect the changes and create the shelf. Retain the returned ID, version and `link`. If the link is unavailable, provide the server, depot, branch and shelf ID.
2. In Desk, open the matching workspace and inspect the shelf's changes, text or image diff, and checks. Compare the current version with the one the author handed over. Use file history and blame to understand earlier changes. For a binary, open the appropriate viewer when a textual diff is unavailable.
3. If more work is needed, return the paths and findings to the author. Have them update the shelf and review the new version. When accepting, identify the version that was actually reviewed.
4. When Desk refuses commit or shelve, reproduce the operation through the CLI in the same workspace. Compare identity, selected paths, changelist and validation status. For `validation_untrusted`, the owner can inspect the scripts and establish trust using the CLI. [Validation diagnostics](agent-validation.md).

Desk shares sign-ins with the CLI and calls the public operations in its pinned `lode-ops`. To reproduce a client defect, record both Desk's build and its pinned core SHA. Compare those with the standalone CLI build. Reproduce a mutating operation in a test workspace before retrying it on a user's pending changes. [Desk user guide](../desk/docs/guide.md) · [Desk build and core pin](../desk/README.md).

## Handle an Unreal asset change

1. Verify that the directory containing the `.uproject` belongs to the intended Lode workspace and that the plugin matches the engine version. The plugin documentation targets Unreal Engine 5.8. In the editor's revision control settings choose Lode and inspect the executable path, server, depot, branch and user.
2. Use the same CLI executable from that directory to run `lode --json status --remote`. If it fails, first resolve the workspace, login or connection issue. If it works, compare the editor's detected executable and settings and inspect its Revision Control Message Log.
3. Check out or lock the asset before editing when the depot requires a lock. In an Unreal-template depot, this typically covers `.uasset` and `.umap`. Rename or move assets in the Content Browser. Include resulting redirectors when submitting the move so references and file history remain usable.
4. Review changes in the editor's asset diff. Group related assets, shelve or submit the intended files, then verify the server result and remaining locks. Report the asset path, branch and shelf version or revision. [Unreal guide](../unreal/docs/guide.md) · [Troubleshooting](../unreal/docs/troubleshooting.md).

## Import an existing project

For an existing directory, run `lode init --help`, choose the destination depot and template, and start with the dry run. Review generated paths, large generated artifacts, ignore patterns and lock requirements before creating the initial revision. For a project with history, inspect `lode import git --help` or `lode import svn --help` and choose the branch or source explicitly. Do the first trial in a test depot. Compare a known old revision, the current file tree and a path with a rename before directing collaborators to the imported depot. [Initialization and import output](../lode/docs/CLI-JSON.md).

## Change the product source

The Git repository contains the product implementation. Depot changelists and shelves apply to work carried out in Lode itself. When editing this repository, find the task, its dependencies, acceptance criteria and the files owned by the subproject. Read the [repository rules](../AGENTS.md), [core rules](../lode/AGENTS.md), [architecture](../lode/docs/ARCHITECTURE.md) and [task index](../lode/docs/tasks/README.md).

| Area | Boundary | Verification |
| --- | --- | --- |
| Core, CLI, server and MCP | Modules use public extension points. CLI and MCP share operation semantics. Preserve documented JSON fields and stable error codes | In `lode/`, run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` |
| Desk | Update all six git dependency pins together, regenerate the lockfile and adapt calls through the Desk operations layer. Add regression coverage for changed behavior | In `desk/`, run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings` and `cargo test --locked` |
| Unreal | The plugin runs the CLI and consumes its JSON contract. Compare editor mappings with captured real responses | Use the engine build and automation commands in [the plugin specification, section 9](../unreal/docs/SPEC.md) |

For a workspace bug, start with a fixture that takes the normal user action. Check both the returned result and the files left on disk. For a failed proposal, also verify server state. For a change to backup or storage, exercise the restored snapshot through the regular reader. When a platform or external backend is unavailable locally, record the missing check and obtain its result from CI.

## Identify what GitHub builds

| Trigger | Result |
| --- | --- |
| PR or push to main changing Desk files covered by its workflow | Windows and Linux build and test Desk against the pinned core. Linux uploads Slint screenshots |
| Weekly or manually invoked Desk compatibility job | Tests Desk against the core in the checkout |
| Manual Desk release workflow | Builds installer artifacts for Windows, macOS and Linux |
| Matching `desk-v<version>` tag | Builds and publishes the Desk release and its update feed |
| Matching `lode-v<version>` tag | Builds the core client/server packages and server image |

When asked whether a build is available, distinguish the tested source SHA, an Actions run with downloadable artifacts and a published release. Provide the relevant run or release link and platform. Before publishing, follow the package-version, signing, feed and installation checks in the [release procedure](../lode/docs/release.md). [Desk CI](../.github/workflows/desk.yml) · [Desk packaging](../.github/workflows/desk-release.yml) · [Core packaging](../.github/workflows/lode-release.yml).
