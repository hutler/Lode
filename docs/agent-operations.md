# Server and automation operations

Use this guide for tasks on a running Lode installation and for CI integrations. Examples refer to depot `game`. Use the actual server address, service account, configuration path and depot from the deployment. For workspace changes, start with the [agent workflow](agent-guide.md).

## Bring a server online and establish what is running

1. For a new Linux host, follow the [deployment runbook](../lode/docs/deploy-linux-for-agents.md). It covers DNS, inbound TCP 443, packages, built-in TLS, an existing reverse proxy, the service and an external connectivity test. On Windows use the server installer. For containers preserve the data volume when recreating the service.
2. On the host, identify the active configuration and run `loded doctor --config FILE --json`. Review the configuration, data directory, disk, certificate, backups, database and object store results. Preserve the first failing check and its detail.
3. From the client network run `lode doctor https://lode.example.com`. An external client tests a different path from localhost. A working TCP connection alone does not establish API compatibility or identity. Record the client build and server version separately.
4. In a disposable checkout of a readable depot, run `lode --json status --remote` and read a known historical file. For an authorized write test, prefer a proposal in the test depot and record the shelf ID and version.

The web surfaces serve different audiences. Signed-in users can browse a depot at `/web/` when available. Server administrators use `/admin/` for users, depots, access diagnostics, backup status and maintenance reports. Use the authenticated service's actual capabilities when diagnosing older installations. [User and administration guide](../lode/docs/user-guide.html).

## Give an agent access and diagnose refusals

An administrator can provision an owned agent. The following example creates the identity, adds a depot read rule and issues a token once. **Run token issuance in a private session** and deliver the value through the deployment's secret store. Keep it out of terminal transcripts, project files and task reports.

```sh
lode admin agent create fixer --owner ana --grant game=read --with-token
```

Check the resulting identity from the agent's session with `lode --json whoami`. Its owner can issue or revoke the agent's tokens. Default agent scopes include read, shelve and lock. Direct commit requires the write scope and matching ACL permission. Locks also require write access to the relevant path. For a CI reporter request the `checks` scope and add read access separately if the runner needs to fetch content.

To investigate an access refusal, capture the exact principal, depot, branch, operation and path. From an administrative session inspect the matching rules:

```sh
lode --json admin acl check fixer game main Data/items.json
```

A matching deny at a level also denies higher levels. Check group and `agents` rules, as well as the specific user. A deny-write rule can intentionally allow an agent to propose a shelf on a protected branch while disallowing a direct commit. Missing scopes, missing depot access and disabled modules have different remedies. For SSO, check the user's linked provider identity before changing the login policy. [Identity, ACL and module rules](../lode/docs/SPEC.md).

## Connect CI to an exact shelf version

1. Enable the checks module for result storage. Enable webhooks for push delivery, or consume the event feed with a persisted cursor. A webhook receiver verifies the HMAC-SHA256 `X-Lode-Signature` against the original body. Events contain IDs. Fetch details using the runner's own token. Handle repeated delivery without starting duplicate jobs for the same target.
2. Capture the depot, target kind, ID and version before starting a build. Inspect or materialize that exact input. If it changed while preparing the job, start the build against the intended version again. Run project checks outside Lode.
3. Report results to `POST /api/v1/depots/{depot}/checks` with the runner's token. First report `pending`, then the final state for that same target and check name. Example final body:

```json
{"target":{"kind":"shelf","id":17,"version":2},"name":"ci/build","state":"success","summary":"Build and tests passed"}
```

4. Define the accepted check name and reporter in the branch configuration. For normal changes to reach `main` through shelves, set the shelf-only policy too:

```toml
# .lodeconfig
[checks]
branches = ["main"]
required = [{ name = "ci/build", reporter = "ci" }]

[branches]
shelf_only = ["main"]
```

5. Read `lode --json checks 17@2` as a consumer. Confirm the reporter, name, state and version. A successful result on version 1 cannot satisfy version 2. If the service reports `checks_required`, compare those fields with `details.missing`. Python content checks have their own configuration and reporter. [Pre-commit and server validation](agent-validation.md).

For unattended readers, persist the event cursor only after successfully processing the batch. For webhook delivery trouble, check the registered depot, URL, kind filter, branch filter and delivery history. Reconcile from history after interruptions. [Event and webhook semantics](../lode/docs/SPEC.md).

## Plan and verify recovery

Identify both backend selections in the active `loded.toml` before deciding what counts as a backup. Metadata and file content can live in separate services.

| Component | Backup procedure to account for |
| --- | --- |
| SQLite metadata and filesystem objects | Use `loded backup --config FILE DEST` or a scheduled Lode backup. Use a new destination and retain its configuration |
| Postgres metadata | Take and verify a database backup such as `pg_dump`. Lode's directory backup omits the external database |
| S3 object storage | Configure bucket versioning or replication for content. The directory backup omits external objects |
| Remote copies of Lode backups | The scheduled backup copy can use an S3-compatible bucket. Run `loded restore --help` for listing and restoring saved copies |

For a restore drill, use a separate destination and isolated external backends. Inspect the copied configuration before starting the restored service, particularly external database and bucket endpoints. Restore metadata and objects from a consistent recovery point. Read a known revision, fetch a known file and run `lode admin fsck DEPOT` against the restored deployment. Record what was recovered, the revision, the backend snapshots and any data outside the recovery point. A successful backup command alone tests creation only. [Backend-specific backup and upgrade notes](../lode/docs/UPGRADING.md).

## Upgrade, inspect and maintain a deployment

Before upgrading, record the current binaries and configuration, read the release's migration notes, take a recoverable backup and identify the rollback point. Replace the programs using the installation's existing package, service or container procedure. Restart, check the migration log, run the doctor commands, perform a depot integrity check and update a test workspace. A rollback across a schema change can require the old binary **and** the pre-upgrade database. [Upgrade procedure](../lode/docs/UPGRADING.md).

For suspected corruption, start with `lode --json admin fsck game`. It reports `fsck_failed` with details when it finds problems. To estimate reclamation, run `lode --json admin gc game --dry-run` and report candidate objects before collection. A depot moved to trash can be inspected with `lode admin depot trash` and restored within its retention period. Obliteration removes selected content from the server and does not recall existing copies in workspaces, backups or external systems. Use its dry run to identify exactly which paths and revisions a requested purge would affect.

For a remote office with repeated reads of large content, investigate the caching proxy and client cache before changing the primary store. `lode proxy show` reports the client's selected proxy. For slow transfers, compare fetched/downloaded counts with reused bytes, file sizes and whether a repeat checkout shares a cache. For deployments that expose metrics, correlate transfer and error rates with the failing operation. [Operational hardening](../lode/docs/HARDENING.md) · [Performance measurements](../lode/docs/BENCHMARKS.md).
