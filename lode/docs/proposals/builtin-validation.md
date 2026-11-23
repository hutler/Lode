# Draft for review — Built-in content validation for new depots

**Status:** proposed, awaiting maintainer decisions. **Beads:** `lode-ptr.2`. **Baseline:** core at `33ea654`. The example TOML and activation flow below describe a proposed feature. The shipped contract is [SPEC §§5.6, 6.6 and 6.8](../SPEC.md), with execution and submit gating in [ARCHITECTURE §§9.5 and 9.7](../ARCHITECTURE.md).

A depot should be able to enforce its naming, size and JSON conventions without granting code execution to a contributor. An artist should get the same path, severity and reason locally and on the reviewed shelf. A depot administrator should be able to see which rules and reporters will protect a branch before activating its first policy.

## Deployment decision (D4, settled)

The maintainers have no existing depots to migrate. The proposed release starts with built-in server validation for new depots. This removes the need for a legacy execution window, dual evaluation and per-depot conversion. The released 0.5 behavior is documented in the baseline SPEC. The change concerns execution on the server; local-only checks and checks reported by an external service have distinct trust boundaries.

## Open decisions for maintainers

| Decision | Recommended starting point | Alternative and consequence |
| --- | --- | --- |
| D1. Which JSON inputs pass? | Require UTF-8 and standard JSON syntax. Accept duplicate object keys for this syntax-only check. Report non-finite numbers as syntax errors. | Accept the numeric extensions recognized by the Python example. Rejecting duplicate keys would add a separate content policy and require tracking keys within each object. |
| D2. Who controls a protected branch's rules? | Let contributors propose rule data in the depot. A depot administrator separately activates a complete immutable policy revision on a branch. | Store all mandatory rules directly in administrator-managed configuration. This simplifies authority but removes ordinary depot review of proposed parameter changes. |
| D3. What happens when policy changes with open shelves? | At submit, require the current policy revision. Queue a recheck of the same shelf version; artists re-shelve only if the content must change. | Grandfather shelves under the policy used when created. This delays new protections for outstanding work and needs an expiry policy. |

The rest of the draft uses the recommended D1–D3 choices for review. D1 also proposes JSON budgets of 16 MiB and 128 levels; check those limits against representative fixtures before fixing them in format 1.

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

The format accepts only enumerated kinds and their typed fields. It has no commands, imports, expressions or environment expansion. A capable parser rejects unknown formats, fields, invalid depot paths/globs, empty selectors, conflicting suffix entries and duplicate rule names before activation. Rule names follow the existing validation name grammar. A name maps to the stable check name `validate/<name>`. Ownership of each name is unambiguous within an active policy. A built-in and an external check cannot claim the same required name in one policy.

## Evaluation contract

- **Selection.** Use Lode's existing case-sensitive, depot-relative [glob matcher](../../crates/lode-core/src/glob.rs). A rule matches any listed glob. As with [current validation](../../crates/lode-validate/src/lib.rs), a move selects the rule if either endpoint matches. Evaluate the destination once with its new name, folder, extension and bytes. A JSON file moved out of scope still receives a syntax check on that move. A copy evaluates its added destination. A deleted file needs no read. With no applicable regular files, record a server-computed `skipped` reason. Never follow a workspace symlink to read its target.
- **Filename.** Strip the final extension using the example's split-extension convention (leading dots alone do not form an extension). Require a nonempty stem of ASCII letters, digits or underscores. Apply the first listed folder equal to, or a complete directory ancestor of, the destination folder. Declaration order resolves overlaps. With no matching prefix, only the stem check applies. Report one actionable name error per file. Thus `Content/Art/BigRock.uasset` passes the example rules and `Content/Textures/rock.png` fails.
- **Size.** Use the verified logical byte length of the resulting file, including chunked files. The client uses its local file length. The server uses trusted object metadata or counts streamed bytes. Match the ASCII-lowercased final extension to a configured suffix after glob selection. A suffix without a limit has no size finding. A positive byte ceiling and an optional integer warning percentage from 1 to 99 are valid. Error if size exceeds the ceiling, otherwise warn if size exceeds the exact percentage. Use overflow-safe integer arithmetic. At exactly 8 MiB the example PNG warns and passes; at 8 MiB plus one byte it fails.
- **JSON.** Parse exactly one UTF-8 JSON value including scalars, followed only by permitted whitespace. Reject invalid UTF-8, an initial BOM, trailing values and malformed syntax. Report the path and a one-based line for syntax errors. Under D1's recommendation, accept duplicate member names without assigning any last-value semantics and reject NaN, Infinity and -Infinity. Treat a syntactically valid number as a token irrespective of machine numeric range. Stream input and validate without building a document tree. Proposed first-release budgets are **16 MiB per JSON file and 128 levels of container nesting** (outermost container at level 1). Exactly the limit is allowed. Exceeding either budget produces a visible check `error` naming the limit; the file has not been validated. Include the effective limit in the diagnostic.
- **Results.** Preserve the existing result vocabulary and path/line diagnostics. Warnings alone pass. Content violations fail. Missing inputs, unavailable evaluators and exhausted resources produce an error and block required submission. Stable order is rule declaration, depot path, then diagnostic position. Limit retained diagnostics and report omitted counts; truncation must preserve the worst outcome. Client preflight uses the candidate workspace configuration and identifies it as local. The server independently evaluates the immutable shelf inputs against the active policy.

## Authority and submit

Under D2, the active policy identifies the depot and protected branch, rule format and evaluator semantics version, complete ordered effective rules and budgets, required check names and their authorized reporters, and the requirement to submit through shelves. Store its activation revision, source revision and actor for audit. Ordinary commits to `.lodeconfig`, including changes to globs, thresholds, `shelf_only` or the required list, can propose a new policy. Only an authorized activation changes the protected gate. Other external checks continue to be required. Local trust in a script conveys no server approval.

Bind authoritative results to **depot, target branch, shelf ID, shelf version, activated policy revision, check name and reporter**. Only the server evaluator may issue authoritative built-in outcomes. A report from another principal bearing the same check name cannot qualify. For a protected branch, direct commit is refused, and local `--no-verify` only skips local preflight. Saving a shelf remains possible while server checks are unavailable.

At submit, verify the reviewed shelf version, the effective policy revision and all mandatory results in the same commit decision. Accept a server-computed inapplicable rule or a pass under that policy. A missing, pending, failed, errored, cancelled, superseded or wrong-reporter result holds submission and identifies the reason. Concurrent activation either precedes this decision or follows it. Under D3 a late completion for the old policy remains historical. Retries and duplicate completions cannot publish success for a newer run. Record results and terminal run state atomically as in the current validation module.

## First activation

1. Create the depot on a server that advertises rule format 1 and protected validation. The three shipped example checks use built-ins and need no Python interpreter. A plain depot can still be created without mandatory validation.
2. Add the declarative configuration and review its names, selectors, thresholds, and any required external reporters. If configuration requests a server Python script (including a script with the old implicit server default), reject that configuration for activation, identify the entry and keep protected submissions on hold until a valid policy is active. Explicitly client-only scripts cannot fulfill a mandatory server check. A plain script file stored as depot content is inert on the server.
3. Under D2, the administrator activates the reviewed policy for the branch. Enabling protection and publishing the first complete policy is atomic. A branch designated for protection with no valid policy can accept shelves but refuses direct commits and submission. Failed updates to an already active policy keep the previous revision effective.
4. Submit a test shelf containing a valid asset and one breaking a required rule. Verify the reported path, check name and shelf version. Correct the asset, update the shelf and submit after all required results pass. Later edits to active policies follow D3. Restoring an earlier policy is a fresh activation with an audit record.

A client with older validation support may omit local preflight. A capable server still enforces its active policy at submit. A new client connected to a server without the capability may display local findings as advisory only. Refuse protected-policy activation when serving nodes cannot enforce format 1.

## Acceptance examples for implementation

| Case | Expected observation |
| --- | --- |
| Nested prefix matches both parent and child declarations | Reordering the declarations changes which prefix is required and creates a new policy revision on activation. |
| Move into and out of scope; copy; delete | Check the moved destination once, check a matching copy, and never open the deleted file. |
| 32 MiB WAV, then one extra byte | The first can pass with a warning; the second fails. Test the integer boundary immediately below and above 80% for both shipped suffixes. |
| UTF-8 error, syntax error on line 2, duplicate key, non-finite number, byte/depth limit | Distinguish content errors from resource errors. Ordinary syntax errors have a line; duplicate and number behavior follows D1. Client and server agree on identical inputs and policy. |
| A contributor removes or weakens a required rule in a shelf | The active policy still evaluates that shelf. Only an authorized activation can change subsequent enforcement. |
| Shelf 12@3 passed under policy A; 12@4 arrives, or B activates as 12@3 is submitted | Neither the old success nor a delayed completion satisfies the new version/policy. A recheck of unchanged 12@3 under B can permit submission. |
| An old client skips preflight, or a branch requires an external studio check | The server gate still applies. An unavailable mandatory result holds submit and the saved shelf stays reviewable. |
| A new depot proposes an explicit or implicit server Python script | Activation identifies the unsupported entry; no process starts and no required result is marked passed. A depot storing the same file as ordinary content can still use built-in rules. |
| Restart during first activation; repeat the request; restore an earlier policy | A designated branch always has either a complete gate or a visible hold. Retries preserve a single activation record. Restoring a policy starts fresh checks. |

Implementation of the evaluator, protected policy and rollout belongs to the dependent Beads issues after design approval. Studio-specific execution and engine integration have separate investigation tracks.
