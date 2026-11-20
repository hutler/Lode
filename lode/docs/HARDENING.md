# Hardening report (T054)

What was tested to back SPEC §10 ("a killed server or client at any point must not leave a
depot or workspace inconsistent"), what it found, and what is still open. Every test below
runs in CI in a short bounded form; the long forms are under [Running longer](#running-longer).

## What is covered

| Area | Test | What it checks |
| --- | --- | --- |
| Server crash, commit path | `lode-testkit/tests/crash.rs` `commit_path_crashes_recover` | The server is stopped at `commit.before_trees`, `commit.in_transaction` and `commit.after_transaction`, then restarted on the same data. `fsck` is clean. A retry with the same request id commits once if the server never finished, and is replayed if it did. |
| Server crash, GC | same file, `gc_crashes_recover` | Stopped at `gc.after_mark` and `gc.in_batch`. `fsck` is clean after each restart, the next GC finishes the work, committed files are intact, and commits still go through. |
| Client crash, named points | `lode-cli/tests/crash.rs` `named_crash_points_converge` | `lode` exits abruptly (`LODE_TEST_CRASH_AT`) at `checkout.created`, `checkout.planned`, `checkout.fetched`, `commit.uploaded`, `commit.stored`, `commit.sent`, `update.planned`, `update.deleted` and `update.fetched`. Rerunning the command converges: the workspace matches the server, `status` is clean, and a commit lands exactly once. |
| Client crash, random kills | same file, `random_kills_converge` | `update`, `commit` and `checkout` are killed after a random delay (seeded), then rerun. Same convergence checks, plus "one new revision per commit". |
| Concurrency soak | `lode-cli/tests/soak.rs` | Several users, each in their own workspace, commit a shared counter, lock and unlock a shared file, and shelve and unshelve, for a bounded time. Revisions are linear; the counter equals the number of successful increments and each value was committed once (no lost update); no two clients held the lock at once; every shelf exists once with its owner and description. |
| Fuzz: tree decoding | `lode-core/tests/fuzz.rs` | Arbitrary bytes and damaged real trees: never a panic, and anything accepted is the canonical encoding of what was decoded, with valid names. |
| Fuzz: object headers | same file | Arbitrary bytes: anything accepted re-encodes to the same 16 bytes. |
| Fuzz: path validation | same file | Strings built from `.`, `/`, `\`, `:`, drive letters, control and reserved characters: anything accepted round-trips and has no empty, `.` or `..` segment and no reserved character. |
| Fuzz: `.lodeconfig` | same file | Arbitrary bytes and TOML-shaped text: never a panic. |
| Fuzz: client manifest lines | `lode-client/src/manifest.rs` tests | Valid NDJSON split at arbitrary points gives the same entries; arbitrary bytes end in entries or an error; hostile paths never come out as entries; an endless line is refused. |
| Hostile server | `lode-client/tests/hostile.rs` | A body that stalls and a server that never answers hit the read timeout; a blob larger than expected is refused and leaves no file; a zstd bomb stops at the expected size; an endless JSON answer is cut off at 64 MiB. Earlier tests already covered wrong hashes (`mock.rs`) and paths that leave the workspace (`lode-cli/tests/checkout.rs`). |
| Slow and many connections (T080) | `lode-server/tests/connections.rs` | Headers sent one byte at a time, a connection that never sends a request (HTTP/1.1, and HTTP/2 over TLS) and an idle kept-alive connection are closed after `[connections] header_timeout_seconds`. Connections past `max_per_address` from one address are closed at once while other addresses still connect, and closing one frees its slot. |
| Hostile client | `lode-testkit/tests/hostile.rs` | An upload that stalls is dropped after `body_idle_seconds` and stores nothing; a zstd bomb is `too_large`; commit paths such as `../x`, `/etc/x`, `a\..\x` and `C:x` are `invalid_request`; an oversized JSON body and an oversized declared blob length are `too_large`. Earlier tests covered hash mismatches and aborted uploads (`lode-server/tests/blobs.rs`). |

## Defects found and fixed

1. **A killed checkout could not be resumed.** If `lode checkout` was stopped after the
   workspace database was created but before the checkout revision was recorded, a rerun said
   "already a workspace" and the only way out was to delete the directory. Found by the random
   kill test. Now a workspace that has never finished a checkout (no synced revision) is
   resumed, including one whose database has no configuration yet, and a `.lode` directory
   left without a database does not count as "not empty". Regression: the `checkout.created`
   crash point and the empty-database case in `named_crash_points_converge`.
2. **The client trusted the server's sizes.** Downloads now stop as soon as more bytes arrive
   than the manifest promised (`download_blob_max`, `get_blob_to_max`); symlink targets
   stop at 64 KiB as they arrive; JSON answers at 64 MiB; and `Bases::read` refuses to hold more than
   256 MiB in memory. Regression: `lode-client/tests/hostile.rs`.
3. **The client had no read timeout.** A server that stopped sending held `lode` forever. Now
   every response read times out after 300 s without data (`ClientBuilder::read_timeout`).
   Regression: `a_stalled_body_or_answer_times_out`.
4. **The server had no idle timeout on request bodies.** A client could hold an upload, its
   temporary file and a connection open indefinitely by trickling bytes. Now each body fails
   after `[limits] body_idle_seconds` (default 60) without data. Regression:
   `a_stalled_upload_is_dropped`.
5. **GC had no crash coverage.** Failpoints `gc.after_mark` and `gc.in_batch` were added; no
   defect showed up behind them.

## Known limits

- **Slow request headers** were a known limit until T080: `loded` now runs its own
  connection loop, closes a connection whose request headers take longer than
  `[connections] header_timeout_seconds` (30), and holds at most `max_per_address` (64)
  connections per client address (an IPv6 /64 network). A reverse proxy is no longer needed
  for this. Rate limiting by account is not done (login is rate-limited by name and address).
- **HTTP/2 connections that stay idle** are not closed while the peer answers keep-alive
  pings; the per-address limit bounds how many one client can hold.
- **Upload decompression.** A zstd upload is bounded by `max_blob_size` after decompression,
  so a bomb costs the server at most that many bytes of disk in a temporary file before it is
  refused. Lower `max_blob_size` if that is too much.
- **Random kills land where they land.** The random test runs a few rounds in CI; the named
  crash points are what guarantees each durable step is covered every run.
- **No third-party audit** (out of scope for T054).

- **Validation scripts run code.** With the `validate` module on, anyone who can land a
  script on a branch head (by commit or submitted shelf) runs code on the server as the
  `loded` user, in a fresh directory with a cleared environment, a timeout and capped output.
  That is not a sandbox. Run `loded` as its own user (the packages do) or in the container,
  and gate the branches that hold the scripts with `[branches].shelf_only` and required
  checks. Clients run a script only after its owner trusted that exact content.

## Running longer

```sh
# Random client kills: more rounds, or replay a failing seed.
LODE_CRASH_ROUNDS=200 LODE_CRASH_SEED=12345 cargo test -p lode-cli --test crash random

# Soak: longer and wider.
LODE_SOAK_SECS=600 LODE_SOAK_CLIENTS=16 cargo test -p lode-cli --test soak -- --nocapture

# Fuzz: more cases.
PROPTEST_CASES=100000 cargo test -p lode-core --test fuzz
PROPTEST_CASES=100000 cargo test -p lode-client --lib manifest
```

Runs made for this report: 6 seeds of 25 random-kill rounds each; a 40-second soak with 8
clients (about 180 commits, 520 out-of-date retries, 250 lock holds and 350 shelves); 50,000
cases of each core fuzz target. All passed after the fixes above.
