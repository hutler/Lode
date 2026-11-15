# Lode for Unreal

An Unreal Editor plugin that makes Lode a revision control provider, so artists and designers lock, sync, submit and shelve from inside the editor. A subproject of SimpleDevOps. Unreal Engine 5.4 and later; Windows first, with Linux and macOS editors supported.

- [docs/guide.md](docs/guide.md): install and daily use.
- [docs/troubleshooting.md](docs/troubleshooting.md): messages and what to do.
- [docs/SPEC.md](docs/SPEC.md): how the plugin works, and what it needs from `lode` (section 5).
- [docs/TASKS.md](docs/TASKS.md): task status.

Layout:

- `LodeSourceControl/`: the plugin (one editor module, `Source/LodeSourceControl/Private`). Every `lode` call goes through `LodeRunner` on a worker thread with `--json`; the JSON fields read are listed in `LodeJsonFields.h`.
- `LodeSourceControl/Tests/FakeLode/`: a fake `lode` for the automation tests, with answers recorded from a real server.
- `LodeSourceControl/Tests/E2E/`: starts a real Lode server for the end-to-end tests.
- `Scripts/`: `BuildPlugin` (zip per engine version) and `RunTests` (automation tests, optionally end-to-end).
