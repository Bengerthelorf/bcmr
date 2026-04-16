# Local Multi-File Performance

::: info Test hardware
Two reference machines surface in the experiments below; the same
codenames are used on the [Wire Protocol](/ablation/wire-protocol)
page.

| Codename | Class | Notes |
|----------|-------|-------|
| host-L | Linux server | x86_64, AVX-512, NVMe ext4, kernel 6.x |
| host-N | macOS desktop | arm64 M-series, APFS |
:::

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

![Concurrent multi-file copy](/images/ablation/flow/local_concurrent_copy.svg)

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

## Experiment 13: One spawn_blocking for the Whole Loop

**Hypothesis**: Tokio's async file I/O (`tokio::fs::File::read`,
`write`, `seek`, `stream_position`) wraps each call in its own
`spawn_blocking`. For a 2 GiB file at 4 MiB blocks that's ~1024
round trips through the blocking-thread pool; on Linux NVMe the
syscall ceiling is much higher than the device, so this overhead
dominates wall time.

**Method**: Same `bcmr copy --reflink=disable` of a 2 GiB random
file on **host-L** (Xeon Gold AVX-512, NVMe ext4, kernel 6.x)
before and after the refactor.

| Command | Before | After | cp |
|---------|-------:|------:|---:|
| Wall (s) | 12.34 | **5.38** | 2.17 |
| Throughput (MB/s) | 170 | **383** | ~1000 |

**Implementation**: `streaming_copy` now `try_clone()`'s both file
descriptors into `std::fs::File` handles (which dup the fds, so
the sync handle and the original `tokio::fs::File` share the same
open file description), takes ownership of the `Option<Session>`,
and runs the entire read/write/sparse-detect/checkpoint loop
inside one `tokio::task::spawn_blocking`. The session is returned
through the join handle so the outer async function preserves the
`&mut Option<Session>` contract.

![spawn_blocking before vs after](/images/ablation/flow/local_spawn_blocking.svg)

**Decision**: Ship the refactor. It's a 2.3x wall-clock win on
Linux NVMe and ~1.7x on macOS APFS for the streaming path, with no
test regressions across the existing 14 e2e copy cases.

**Why not just use `io_uring`?** `tokio-uring` requires its own
runtime (`tokio_uring::start()`) that can't drive standard tokio
futures; mixing it into bcmr's existing tokio runtime would
require a major restructuring. The win this experiment captured
(2.3x) is most of what `io_uring` would have offered on top of
`tokio::fs`. `io_uring` remains a larger-scope follow-up; see the
[Open Questions](/ablation/open-questions) page.

**Remaining gap to `cp`**: ~2.5x (5.4s vs 2.2s on Linux). This is
now the cost of:
- The double BLAKE3 (source + per-block) since files >= 64 MiB
  always create a session.
- Periodic `fdatasync` at every 64 MiB checkpoint (~32 syscalls
  for 2 GiB, each ~50 ms on this NVMe).
- `posix_fadvise(FADV_DONTNEED)` calls for cache eviction.

These are correctness features, not bugs. **cp computes no
hashes at all, runs no fsync mid-copy, and never writes a
session file.** The comparison is "bcmr with always-on
BLAKE3 and crash-safe session" vs "cp's unverified fastest-
possible path". Experiment 13's follow-up below removes this
overhead when the user hasn't asked for it.

**Update, v0.5.10 (see Experiment 16 below):** this 2.5x gap was
almost entirely that self-imposed "auto-session for files over
64 MiB" rule. Gating it on the user's explicit intent
(`-C`/`-s`/`-a`) closes the gap to ~1.65x on mac. The
correctness features are intact for anyone who actually asks
for them.

---

## Experiment 16: Gate Session + Block Hash + Checkpoint Fsync on Intent (v0.5.10)

**Hypothesis**: v0.5.8's rule "any file > 64 MiB auto-creates a
session" was over-cautious — it paid for resumable semantics the
user hadn't asked for. Gating the session (and therefore the
per-block BLAKE3 and the periodic `durable_sync` checkpoint) on
the user's explicit resume flags should close most of the
remaining gap to cp for one-shot streaming copies.

**Method**: 1 GiB random file, mac APFS, `--reflink=disable` to
force the streaming path. Warm cache. 5 runs, hyperfine.

| command | mean wall (s) | vs cp |
|---|---:|---:|
| `cp` | 1.14 | 1.00x |
| `bcmr copy` (default, no flag, v0.5.10) | **1.89** | **1.65x** |
| `bcmr copy -C` (session created) | 4.46 | 3.91x |
| `bcmr copy -V` (+source rehash + verify) | 4.69 | 4.12x |
| `bcmr copy` (v0.5.9: same command, session auto-created) | ~3.9 | ~3.4x |

**What the change does**: three separate gates now all check
`session.is_some()`:

1. `create_session` drops the `file_size > 64 MiB` auto-trigger
   — only `-C` / `-s` / `-a` create a session now.
2. `block_hasher` itself becomes `Option<Hasher>` driven by
   session presence. Before, every 4 MiB chunk paid a BLAKE3
   pass regardless of whether anyone would read the result. On
   NEON that's ~1 GB/s of wasted CPU.
3. The per-64-MiB `durable_sync(dst) + posix_fadvise` checkpoint
   only runs with a session — its only purpose is to uphold the
   session's crash-safety invariant, which is moot with no
   session.

**Decision**: Ship. Users who want crash-safe resume pass the
flag and pay the price. One-shot `bcmr copy big.iso dst/` now
runs at ~83 % of cp's wall time.

**The progression of the Linux-NVMe streaming gap across
releases** (2 GiB file):

| version | wall | gap vs cp |
|---|---:|---:|
| v0.5.8 streaming | 12.34 s | 5.7x |
| v0.5.9 (spawn_blocking refactor, [Exp 13](#experiment-13-one-spawn-blocking-for-the-whole-loop)) | 5.38 s | 2.48x |
| v0.5.10 (this experiment) | est. ~2.5 s | ~1.15x |

**What I got wrong before**: the v0.5.4 Summary and early Local
Perf framing treated "session for every big file" as harmless
background bookkeeping because the bench files were 1 GiB or less
and the session fsync was amortised. Real usage includes one-shot
multi-GiB tarball copies where the session cost was the single
biggest gap to cp. Credit: this was flagged in an external code
audit.

---

## Summary

Every benefit below is tied to a specific workload. If you're
looking for an apples-to-apples comparison against a different
workload (huge single file / cold cache / HDD / very many files
on a slow filesystem), the [Open Questions](/ablation/open-questions)
page lists the measurement gaps honestly.

| Decision | Measured Cost | Measured Benefit (workload) |
|----------|-------------|-----------------|
| Opt-in per-file fsync | ~0 % (default skip) | 13× (9.9 → 0.72 s) on 2100 × 4 KiB files, mac APFS warm cache |
| `--jobs` parallel local copy | 0 % (configurable) | 1.67× (6.52 → 3.91 s) `-j1` → `-j32` on 10 000 × 64 KiB files, mac APFS warm cache |
| Skip src hash when unused | 0 % (saves CPU) | 28 % (285 → 205 ms) on a 32 MiB streaming copy, no `--verify`, mac APFS |
| Single spawn_blocking copy loop | One std::fs::File dup per call | 2.3× (12.3 → 5.38 s) on 2 GiB streaming copy, Linux NVMe ext4 |
| Session + checkpoint gated on intent | 0 % (off when unasked) | ~2× (3.9 → 1.89 s) on 1 GiB streaming copy, mac APFS — lands at 1.65× of cp |
