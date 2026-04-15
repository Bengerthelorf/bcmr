# Local Multi-File Performance

This page covers performance investigations of the *local* hot path
--- the in-process logic that runs when bcmr is copying or moving
files on a single host. The common thread is that the v0.5.4 design
had correctness-first defaults (fsync after every rename, serial
file-at-a-time copy, hash every byte twice) that were the right
baseline but paid for guarantees the user hadn't asked for, and
left cores and queues idle on typical workloads.

The Streaming Checkpoint Copy ablation on the [SCC page](/ablation/scc)
covered the *single-file* case where these choices were measured to
be free. The experiments here cover the regimes that single-file
benchmarks didn't surface: many-files, fast-disk, and unused-hash
paths.

## Experiment 7: Per-File Durability Cost

**Hypothesis**: Calling `F_FULLFSYNC` on the parent directory after
every atomic rename is correct for single-file copies where rework
cost is irrelevant, but dominates wall-clock time when the operation
is copying thousands of small files.

**Method**: `bcmr copy -r` of a 2100-file, 9 MiB directory tree
(resembling a small source repo); compared against `cp -R` and
`rsync -a` on the same input. Five runs, median reported. macOS
Apple Silicon, APFS SSD, warm cache.

| Command | Before gate | After gate |
|---------|------------:|-----------:|
| `bcmr copy -r` (default) | 9.90 s | 0.72 s |
| `cp -R` | 1.00 s | 1.00 s |
| `rsync -a` | 0.75 s | 0.75 s |
| `bcmr copy -r --sync` | 9.90 s | 9.90 s |

**Interpretation**: The pre-gate path issued two `F_FULLFSYNC` calls
per file (one on the file descriptor, one on the parent directory
after `rename`). `F_FULLFSYNC` is a full drive-cache flush command
and costs roughly 4 ms of barrier latency per call on the test
hardware. At 2100 files that is 8.4 s of pure barrier time, matching
the observed 9.9 s almost exactly.

Neither `cp` nor `rsync` fsyncs by default --- they rely on the OS
page cache and background flush. bcmr now does the same: the default
path does zero per-file fsync calls, and `--sync` restores the old
durability-strong behaviour (still 9.9 s, but deterministic).

**Decision**: Gate per-file durable sync on `--sync`. No final
directory fsync is issued at the end of the operation either, again
matching `cp` and `rsync`. Users who need transactional guarantees
opt in explicitly.

**Why the original design was wrong**: the v0.5.4 ablation argued
`F_FULLFSYNC` was free ([SCC Experiment 5](/ablation/scc#experiment-5-f-fullfsync-vs-fsync-on-macos)). The measurement was
done on *single large files* where the fsync cost is amortised over
hundreds of megabytes of copy time. For many-small-files the cost
becomes `fsync_count × fsync_latency`, which scales with file count,
not file size. The single-file benchmark didn't exercise the regime
where the pathology lives.

## Experiment 8: File-Level Parallelism

**Hypothesis**: `execute_plan` iterates the plan serially, awaiting
each `copy_file` before starting the next. For many-file workloads
on an NVMe/APFS device with $> 1$ disk queue, this leaves most of
the queue idle.

**Method**: 10 000 × 64 KiB files (~640 MiB), sweep `--jobs N` from
1 to 32. Five runs per N.

| `--jobs` | Mean (s) | Relative |
|---------:|---------:|---------:|
| 1  | 6.52 | 1.67x |
| 2  | 5.23 | 1.34x |
| 4  | 5.05 | 1.29x |
| **8**  | **4.16** | **1.06x** |
| 16 | 4.39 | 1.12x |
| 32 | 3.91 | 1.00x |

**Interpretation**: Throughput improves monotonically up to the
physical core count (8 on the test box) and plateaus beyond it. The
platform has 8 performance cores and each copy task is mostly
I/O-bound, so the scheduler can happily overlap 8 tasks worth of
`read` / `write` / `fdatasync` without the kernel becoming the
bottleneck. Beyond 16, adding concurrency just churns the tokio
runtime without unlocking additional disk parallelism.

**Decision**: Default `--jobs = min(num_cpus, 8)`. Users with faster
storage or different profiles can override.

**Implementation note**: directory creation stays serial so a parent
always exists before its children try to open files inside it.
`walkdir` yields parents before contents, so a single pre-pass over
`plan.entries` picking out `CreateDir` nodes is enough. The file
stream then runs through `futures::stream::buffer_unordered(N)`.

## Experiment 10: Whole-Source BLAKE3 on the I/O Thread

**Hypothesis**: `streaming_copy` updates two BLAKE3 hashers per byte
--- the per-block hasher (needed at the next checkpoint to populate
the session) and the whole-source hasher. On macOS NEON BLAKE3 runs
at ~1 GB/s, so the doubled hash work effectively serialises 2 GB/s
worth of CPU against ~2 GB/s of APFS write throughput. The whole-
source hash is only consumed when `--verify` is set or when a
session is being persisted across runs; for a one-shot copy of a
small file with neither flag, it's pure overhead.

**Method**: Streaming-path 32 MiB file copy on macOS APFS. Five runs
each, hyperfine.

| Mode | Mean (ms) | Δ vs cp |
|------|----------:|--------:|
| `cp` (no hash) | 18 | 1.00x |
| `bcmr stream` (block hash only, after fix) | 205 | 11.4x slower |
| `bcmr stream -V` (block + source hash, after fix) | 285 | 15.9x slower |
| `bcmr stream` (block + source hash, before fix) | ~285 | 15.9x slower |

The skip is gated on `verify || session.is_some()`. For files >= 64
MiB or with `--resume`/`--strict`/`--append` set, the source hash is
needed and computed. The 28 % saving (285 ms → 205 ms) on the no-
hash path is the upper bound for the no-verify case --- the rest of
the gap to `cp` is the per-block hash itself, the per-checkpoint
`posix_fadvise`, and tokio I/O scheduling overhead, which a future
revision can pick at separately.

**Why the obvious fix didn't work**: the first attempt was the
textbook one --- move the source hash to a `tokio::task::spawn_blocking`
and feed it 4 MiB chunks via an `mpsc::channel`. The expected win
was the source hash overlapping with the next read+write+block-hash.
Measured result: *slower* than the synchronous double-hash. The
per-chunk `Vec<u8>` clone for the channel send and the channel sync
overhead together cost more than the parallelism saved. Skipping
the hash entirely when nothing reads it was the better lever.

A useful pipelined version would need a buffer pool (e.g. `Bytes`
with refcount) plus probably `update_rayon` to parallelise the hash
itself across cores. That's deferred; see the
[Open Questions](/ablation/open-questions) page.

---

## Summary

| Decision | Measured Cost | Measured Benefit |
|----------|-------------|-----------------|
| Opt-in per-file fsync | ~0% (default skip) | 13x faster many-small-files |
| `--jobs` parallel local copy | 0% (configurable) | 1.5--2x on many-medium workloads |
| Skip src hash when unused | 0% (saves CPU) | 28% off no-verify streaming |
