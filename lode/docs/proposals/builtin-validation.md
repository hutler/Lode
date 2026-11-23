# Draft for review — Built-in content validation and depot migration

**Status:** proposed, awaiting maintainer decisions. **Beads:** `lode-ptr.2`. **Baseline:** core at `33ea654`. The example TOML and rollout states below describe a proposed feature. The shipped contract is [SPEC §§5.6, 6.6 and 6.8](../SPEC.md), with execution and submit gating in [ARCHITECTURE §§9.5 and 9.7](../ARCHITECTURE.md).

A depot should be able to enforce its naming, size and JSON conventions without granting code execution to a contributor. An artist should get the same path, severity and reason locally and on the reviewed shelf. A depot administrator should be able to see exactly which protections will change before switching an existing branch.

## Decisions for maintainers

| Decision | Recommended starting point | Alternative and consequence |
| --- | --- | --- |
| D1. Which JSON inputs pass? | Require UTF-8 and standard JSON syntax. Accept duplicate object keys for this syntax-only check. Expose `NaN` and infinity differences in the migration preview. | Match the Python example's numeric extensions to reduce migration failures. Rejecting duplicate keys would add a separate content policy and require tracking keys within each object. |
| D2. Who controls a protected branch's rules? | Let contributors propose rule data in the depot. A depot administrator separately activates a complete immutable policy revision on a branch. | Store all mandatory rules directly in administrator-managed configuration. This simplifies authority but removes ordinary depot review of proposed parameter changes. |
| D3. What happens when policy changes with open shelves? | At submit, require the current policy revision. Queue a recheck of the same shelf version; artists re-shelve only if the content must change. | Grandfather shelves under the policy used when created. This delays new protections for outstanding work and needs an expiry policy. |
| D4. How should existing Python depots transition? | Make built-ins the default for new setups. Keep existing explicitly enabled legacy enforcement during a reviewed transition; enable shadow comparison and switch atomically per branch. | On upgrade, immediately stop legacy execution and hold affected submissions until an admin chooses replacements. Removes host execution sooner and can interrupt active projects. |

The rest of the draft uses the recommended choices so that they can be reviewed against concrete examples. With D1, review the proposed JSON budgets (16 MiB and 128 levels) against existing depot files before fixing them in format 1. D4 preserves the known host-execution risk during transition. An operator must also be able to suspend legacy execution immediately while keeping affected mandatory submissions on hold.

## Proposed rule file

Rules live in the depot's root `.lodeconfig`. An activated policy captures their effective values. These entries reproduce the scope of the [three shipped examples](../../examples/validate/), subject to the JSON and resource-limit differences below.

```toml
[validate]
rule_format = 1

[[validate.rule]]
name = "asset-names"
kind = "filename"
paths = ["Content/**"]
stem_chars = "ascii-alnum-underscore"
prefixes = [
  { folder = "Content/Textures", prefix = "T_" },
  { folder = "Content/Materials", prefix = "M_" },
  { folder = "Content/Sounds", prefix = "S_" },
]

[[validate.rule]]
name = "max-size"
kind = "size"
paths = ["Content/**/*.png", "Content/**/*.wav"]
warn_above_percent = 80
limits = [
  { suffix = ".png", max_bytes = 8388608 },
  { suffix = ".wav", max_bytes = 33554432 },
]

[[validate.rule]]
name = "json"
kind = "json-syntax"
paths = ["**/*.json"]
```

The format accepts only enumerated kinds and their typed fields. It has no commands, imports, expressions or environment expansion. A capable parser rejects unknown formats, fields, invalid depot paths/globs, empty selectors, conflicting suffix entries and duplicate rule names before activation. Rule names follow the existing validation name grammar. A name maps to the stable check name `validate/<name>`. Ownership of each name is unambiguous within an active policy. An explicit legacy-to-built-in replacement may preserve a name at cutover. Shadow results use a separate comparison record and cannot satisfy a required check.

## Evaluation contract

- **Selection.** Use Lode's existing case-sensitive, depot-relative [glob matcher](../../crates/lode-core/src/glob.rs). A rule matches any listed glob. As with [current validation](../../crates/lode-validate/src/lib.rs), a move selects the rule if either endpoint matches. Evaluate the destination once with its new name, folder, extension and bytes. A JSON file moved out of scope still receives a syntax check on that move. A copy evaluates its added destination. A deleted file needs no read. With no applicable regular files, record a server-computed `skipped` reason. Never follow a workspace symlink to read its target.
- **Filename.** Strip the final extension using the example's split-extension convention (leading dots alone do not form an extension). Require a nonempty stem of ASCII letters, digits or underscores. Apply the first listed folder equal to, or a complete directory ancestor of, the destination folder. Declaration order resolves overlaps. With no matching prefix, only the stem check applies. Report one actionable name error per file. Thus `Content/Art/BigRock.uasset` passes the example rules and `Content/Textures/rock.png` fails.
- **Size.** Use the verified logical byte length of the resulting file, including chunked files. The client uses its local file length. The server uses trusted object metadata or counts streamed bytes. Match the ASCII-lowercased final extension to a configured suffix after glob selection. A suffix without a limit has no size finding. A positive byte ceiling and an optional integer warning percentage from 1 to 99 are valid. Error if size exceeds the ceiling, otherwise warn if size exceeds the exact percentage. Use overflow-safe integer arithmetic. At exactly 8 MiB the example PNG warns and passes; at 8 MiB plus one byte it fails.
- **JSON.** Parse exactly one UTF-8 JSON value including scalars, followed only by permitted whitespace. Reject invalid UTF-8, an initial BOM, trailing values and malformed syntax. Report the path and a one-based line for syntax errors. Under D1's recommendation, accept duplicate member names without assigning any last-value semantics and reject NaN, Infinity and -Infinity. Treat a syntactically valid number as a token irrespective of machine numeric range. Stream input and validate without building a document tree. Proposed first-release budgets are **16 MiB per JSON file and 128 levels of container nesting** (outermost container at level 1). Exactly the limit is allowed. Exceeding either budget produces a visible check `error` naming the limit; the file has not been validated. Include configured limits and their effect in migration previews.
- **Results.** Preserve the existing result vocabulary and path/line diagnostics. Warnings alone pass. Content violations fail. Missing inputs, unavailable evaluators and exhausted resources produce an error and block required submission. Stable order is rule declaration, depot path, then diagnostic position. Limit retained diagnostics and report omitted counts; truncation must preserve the worst outcome. Client preflight uses the candidate workspace configuration and identifies it as local. The server independently evaluates the immutable shelf inputs against the active policy.

## Authority and submit

Under D2, the active policy identifies the depot and protected branch, rule format and evaluator semantics version, complete ordered effective rules and budgets, required check names and their authorized reporters, and the requirement to submit through shelves. Store its activation revision, source revision and actor for audit. Ordinary commits to `.lodeconfig`, including changes to globs, thresholds, `shelf_only` or the required list, can propose a new policy. Only an authorized activation changes the protected gate. Other external checks continue to be required. Local trust in a script conveys no server approval.

Bind authoritative results to **depot, target branch, shelf ID, shelf version, activated policy revision, check name and reporter**. Only the server evaluator may issue authoritative built-in outcomes. A report from another principal bearing the same check name cannot qualify. For a protected branch, direct commit is refused, and local `--no-verify` only skips local preflight. Saving a shelf remains possible while server checks are unavailable.

At submit, verify the reviewed shelf version, the effective policy revision and all mandatory results in the same commit decision. Accept a server-computed inapplicable rule or a pass under that policy. A missing, pending, failed, errored, cancelled, superseded or wrong-reporter result holds submission and identifies the reason. Concurrent activation either precedes this decision or follows it. Under D3 a late completion for the old policy remains historical. Retries and duplicate completions cannot publish success for a newer run. Record results and terminal run state atomically as in the current validation module.

## Rollout and rollback

1. **Inventory before writing.** For each depot and target branch, record effective script IDs, entry points, support files and dependencies, selectors, client/server settings, shelf-only protection, required checks and reporters, and the last observed results. Include custom and client-only scripts. An operator explicitly marks each item as replace, retain or retire, with a reason. File or check names alone establish no equivalence. A depot without validation needs no conversion.
2. **Upgrade server capability first.** The proposed server advertises supported rule format and enforcement capability before an operator can activate. Keep the legacy configuration readable by current clients during the transition. A new client connected to an old server can offer local preview and must identify it as local-only. Unknown capability, or an outdated server in a serving cluster, blocks activation. An older client may omit local built-in preflight. Its requests still face the same server gate and receive the existing shelf/validation refusal with a readable summary.
3. **Compare before cutover.** Evaluate proposed rules on representative historical or open shelf versions with known inputs, recording both outcomes and diagnostics. Include known failures, warnings, non-finite numbers and oversized/deep JSON. The existing gate remains authoritative. Use recorded legacy results when available; running an unapproved script for comparison requires separate authorization. Resolve or explicitly accept differences and name the owner of each retained custom check. An unmapped mandatory check holds cutover.
4. **Activate per branch.** Atomically replace approved legacy bindings, protect direct writes, retain other required checks and enqueue rechecks for open shelf versions. Record the administrator, old/new policy revisions and mapping. A built-in-only depot needs no Python interpreter. Replaced scripts cease to run on the server. Old workspaces may still run locally trusted scripts until updated; tell their owners when to sync. Retire legacy files once the supported clients and retained checks no longer depend on them.
5. **Rollback.** Restore a reviewed policy as a fresh activation and enqueue rechecks. Keep shelf contents, previous diagnostics and audit history. Resuming legacy execution requires an explicit operator decision; if unavailable, leave submissions pending or blocked. Before downgrading the server binary, verify that the older release can enforce equivalent protections, or suspend writes to affected branches. A failed or interrupted migration leaves either the old complete gate or the new complete gate active.

### Example cutover on main

Suppose the inventory finds the shipped `asset-names`, `max-size` and `json` scripts plus a required studio check called `art-review`. The operator maps the three scripts to the TOML above, pins the existing reporter for `art-review`, and records any approved JSON differences. In shadow mode a previously accepted JSON file containing `NaN` yields a visible difference. The team can fix the file or postpone activation. At cutover, the active bindings for the three stable names switch together. An already-open shelf can be submitted after its current version passes the new gate, including `art-review`. An update to the shelf starts a new run.

## Acceptance examples for implementation

| Case | Expected observation |
| --- | --- |
| Nested prefix matches both parent and child declarations | Reordering the declarations changes which prefix is required and creates a new policy revision on activation. |
| Move into and out of scope; copy; delete | Check the moved destination once, check a matching copy, and never open the deleted file. |
| 32 MiB WAV, then one extra byte | The first can pass with a warning; the second fails. Test the integer boundary immediately below and above 80% for both shipped suffixes. |
| UTF-8 error, syntax error on line 2, duplicate key, non-finite number, byte/depth limit | Distinguish content errors from resource errors. Ordinary syntax errors have a line; duplicate and number behavior follows D1. Client and server agree on identical inputs and policy. |
| A contributor removes or weakens a required rule in a shelf | The active policy still evaluates that shelf. Only an authorized activation can change subsequent enforcement. |
| Shelf 12@3 passed under policy A; 12@4 arrives, or B activates as 12@3 is submitted | Neither the old success nor a delayed completion satisfies the new version/policy. A recheck of unchanged 12@3 under B can permit submission. |
| An old client skips preflight, or a depot retains a custom check | The server gate still applies. An unavailable mandatory result holds submit and the saved shelf stays reviewable. |
| Restart during cutover; repeat activation request; attempt rollback | No partial gate, duplicate activation or silent re-enable of host Python. Inspect the active revision and audit record after recovery. |

Implementation of the evaluator, protected policy and rollout belongs to the dependent Beads issues after design approval. Studio-specific execution and engine integration have separate investigation tracks.
