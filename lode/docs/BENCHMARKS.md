# Benchmarks (T056)

The three numeric targets of SPEC §10, measured with the ignored tests in
`crates/lode-cli/tests/perf.rs`. All three are met with a wide margin.

| Target (SPEC §10) | Result | Margin |
| --- | --- | --- |
| A commit of 10,000 small files completes in under 30 s on a local network | **7.1 s** | 4× |
| `lode status` on a 200,000-file workspace with no changes completes in under 5 s warm | **1.1 s** (first run 1.1 s) | 4.5× |
| Server memory use is independent of file size | peak RSS **22, 24, 23 MiB** for 16 MiB, 256 MiB and 1 GiB files | flat |

## Hardware and setup

- Cloud VM, 4 vCPU Intel Xeon at 2.10 GHz, 15 GiB RAM, virtual disk; Linux 6.18.
- Rust 1.97 stable, release profile of the workspace (`opt-level = 3`, thin LTO).
- Client and server on the same machine over loopback (the "local network" case). For the
  first two targets the server runs inside the test process (`lode-testkit`), for the third it
  is the real `loded` binary in its own process, so its memory can be read alone.
- Every measurement runs the real `lode` binary as a user would.

## Datasets and results

**Commit of small files.** 10,000 new files of 100 to 1,100 bytes with distinct contents,
100 per directory, committed with `lode commit -A` (scan, hash, upload of every blob, commit,
local bookkeeping): 7.14 s. Modifying all 10,000 and committing again: 6.77 s.

**Status of a large workspace.** 200,000 files of the same kind in 2,000 directories, committed
first (setup, 128 s for the 200,000-file commit). Then `lode status` with nothing changed: the
first run 1.06 s, the median of the next three 1.12 s. The scan compares size and mtime with
the workspace database and hashes nothing.

**Server memory.** For each size a fresh `loded` stores one file of incompressible bytes
(`lode commit`) and serves it back (`lode checkout`). Peak resident memory (`VmHWM`) of the
server process:

| File | At start | After upload | After download | Upload | Download |
| --- | --- | --- | --- | --- | --- |
| 16 MiB | 13 MiB | 22 MiB | 22 MiB | 0.1 s | 0.1 s |
| 256 MiB | 13 MiB | 24 MiB | 24 MiB | 1.0 s | 0.7 s |
| 1 GiB | 13 MiB | 23 MiB | 23 MiB | 3.5 s | 2.4 s |

Content is streamed in both directions, so the peak does not follow the file size. The test
fails if the 1 GiB peak exceeds the 16 MiB peak by more than 64 MiB.

## Running them

```sh
cargo build --release -p loded
cargo test --release -p lode-cli --test perf -- --ignored --nocapture --test-threads 1
```

`LODE_PERF_COMMIT_FILES` and `LODE_PERF_STATUS_FILES` change the sizes. The memory test reads
`/proc` and skips itself elsewhere. No target was missed, so no profiling or fixes were needed
for this pass. Windows and macOS were not benchmarked (see SPEC §10.1).
