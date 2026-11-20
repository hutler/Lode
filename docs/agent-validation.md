# Pre-commit and shelf validation

Lode runs depot-defined Python checks before `commit` and `shelve`, or on demand with `validate`. The server can independently run checks on each shelf version and gate submission. This guide applies to builds containing M19. Check `lode --version` and `lode validate --help` on the machine running the task. Git hooks are a separate mechanism. [Agent guide](agent-guide.md) · [Specification, section 6.8](../lode/docs/SPEC.md).

## Register a check in the depot

To validate changed JSON files, copy `tools/validate/json_valid.py` and `tools/validate/report.py` from the [working example](../lode/examples/validate/) into the same paths in the depot. Add the following rule at its root. Merge the settings into existing TOML tables. Include the script and its dependencies in the revision that introduces the rule.

```toml
# .lodeconfig
[validate]
python = "3.11"
support = ["tools/validate/report.py"]

[[validate.script]]
name = "json"
run = "tools/validate/json_valid.py"
paths = ["**/*.json"]
timeout = "60s"
server = true
```

- The depot's `python` value specifies a minimum interpreter version. The `run` path is relative to the depot root. A commit referring to an absent script can fail with `invalid_request`.
- `paths` filters the changes supplied to this script. No match produces a skipped result. Schedule a newly created file with `lode add PATH` before testing it.
- List imported modules, rule files and other inputs under `support`. The server builds a temporary tree with selected changed files and declared dependencies. An undeclared file present in a full local checkout might be absent there.
- The timeout defaults to 60 seconds, up to ten minutes. By default `server = true` makes a matching script eligible when the server module is enabled. This rule's server check name is `validate/json`.

The complete example also validates asset names against a TOML table and limits texture and audio sizes. Include the full support list when using those checks. [Example depot configuration](../lode/examples/validate/.lodeconfig).

## Implement the Python contract

Lode invokes `python -X utf8 <run>` from the workspace root or the server's temporary tree. It supplies a restricted environment with `LODE_VALIDATE=1`. Scripts run with the operating-system privileges of the account launching them. Limit diagnostics to information the reviewer needs.

**Input:** one JSON object on stdin. Its `phase` is `manual`, `commit`, `shelve` or `server`. Each file has a depot-relative `path` and an `action` of `add`, `edit`, `delete` or `move`. A move can include `from`. For content checks, skip deleted files and open the destination of a move.

```json
{"version":1,"phase":"manual","depot":"game","branch":"main","files":[{"path":"Data/items.json","action":"edit"}]}
```

**Output:** emit one JSON object per problem on stdout. Include `level` and `message`, plus `path` and `line` when known. Other output lines are captured as text. Exit 0 to pass, including when only warnings were found. Exit 1 for invalid content. Other nonzero exits, timeouts and uncaught exceptions indicate a broken check.

```json
{"path":"Data/items.json","line":2,"level":"error","message":"Expecting property name enclosed in double quotes"}
```

[json_valid.py](../lode/examples/validate/tools/validate/json_valid.py) handles decoding and syntax errors. [report.py](../lode/examples/validate/tools/validate/report.py) writes diagnostics and chooses the exit code. Exercise a valid file, invalid file, deletion and a nonmatching path. If the rule concerns naming, test a move between matching and nonmatching paths.

## Trust the scripts and select Python

Review the depot configuration, **all listed scripts**, imports and support files before authorizing execution. The local trust record binds the server, depot, script path and script content hash. A helper's contents do not have their own entry under the importing script's fingerprint. Review changes to helpers too. After review, the machine's owner can run:

```sh
lode validate --trust
```

This records trust for all current available scripts in that depot and runs the matching ones. Changing an entry script's content requires renewed trust. In noninteractive JSON mode, a refusal returns `validation_untrusted` with `details.scripts`. Pass those paths to the owner. Local MCP exposes validation but cannot grant trust or skip the checks.

On the client, `LODE_PYTHON` names an executable. Next in precedence is `[validate].python` in the user's `settings.toml`, followed by system candidates. Windows tries `py -3`, `python`, then `python3`. Linux and macOS try `python3` then `python`. For example, to make a missing interpreter block checks on a Windows agent:

```toml
# settings.toml
[validate]
python = ["py", "-3.11"]
missing_python = "refuse"
```

By default an unsuitable or absent interpreter produces a warning and matching checks are skipped locally. Read `results[].outcome` and `reason` even when the command succeeds. An empty result list establishes no script execution.

## Run locally and diagnose a refusal

```sh
lode --json status
lode --json validate --script json Data/items.json
lode --json validate -c content-fix
```

The last command assumes the edits belong to changelist `content-fix`. Commit and shelve repeat validation for the changes they actually select. The local MCP tool `validate` accepts `paths`, `changelist` and `script`. A single-file input looks like this:

```json
{"paths":["Data/items.json"],"script":"json"}
```

| Result | What to inspect |
| --- | --- |
| `ok: true`, outcome `passed` | Read warnings in `results[].problems` |
| `ok: true`, outcome `skipped` | Read `reason`. Check tracking, glob, changelist and interpreter |
| `validation_untrusted` | Give the owner the script paths in `details.scripts` |
| `validation_failed`, outcome `failed` | Read `error.details.results[].problems`, fix the indicated file and rerun |
| `validation_failed`, outcome `error` | Read the result's `reason` and `text`. Check imports, files, exceptions and timeout |
| `python_missing` | Compare the depot minimum with the interpreter available to the account launching Lode |

When local validation refuses, commit or shelve stops before creating a revision or shelf. The CLI's `--no-verify` skips local checks. Record a skipped run as skipped. Server gates still apply to submitted shelves.

## Enable server validation and gate a branch

An administrator enables the modules and chooses an interpreter available to the **server** service account. Merge these settings into the active `loded.toml`. This example assumes a Linux host with Python 3.11 at the given path.

```toml
# loded.toml
[modules]
shelves = true
checks = true
validate = true

[validate]
python = "/usr/bin/python3.11"
```

Use both branch settings below to route normal changes to `main` through validated shelves:

```toml
# .lodeconfig
[branches]
shelf_only = ["main"]

[checks]
branches = ["main"]
```

For each new shelf version the server takes **rules, scripts and support files from the target branch head**. The changes being evaluated come from the shelf. A shelf proposing an updated validator is checked with the existing branch rule. Test the proposed implementation locally with positive and negative examples. After it lands, create a fresh test shelf to verify that the new rule ran on the server.

The service principal `lode-validate` reports matching checks as `validate/<name>`. On gated branches, successful matching server checks and any required external checks are needed for submission. A successful local run alone does not establish a server result.

## Verify the full path

1. In a test depot where the JSON rule is already active, leave a bracket unclosed in tracked `Data/items.json`. Run `lode --json validate --script json Data/items.json`. Expect exit 1, `validation_failed` and a file diagnostic. Verify with `status` that the edit remains.
2. Fix the JSON and repeat. Expect `passed: true` and outcome `passed` for `json`. Run `lode --json shelve Data/items.json --keep -m "Check JSON validation"` and record the returned shelf ID and version.
3. Run `lode --json checks ID@VERSION`. Wait for `validate/json` to reach `success`. With read scope, detailed server diagnostics are available at `GET /api/v1/depots/{depot}/validate/{shelf}/{version}`.
4. Report local and server outcomes separately. If a shelf changes, inspect the new version. If submission returns `checks_required`, inspect `details.missing`.

For a source change, run `cargo test -p lode-cli --test validate_examples` from `lode/` with Python 3.11+ available. The suite exercises bad content on the client and the server. Separate tests cover script substitution, timeouts and interrupted runs. [Example integration test](../lode/crates/lode-cli/tests/validate_examples.rs) · [Server validation tests](../lode/crates/lode-cli/tests/server_validate.rs).
