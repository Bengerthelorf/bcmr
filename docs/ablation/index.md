# Streaming Checkpoint Copy

bcmr implements a **Streaming Checkpoint Copy (SCC)** algorithm that unifies three capabilities no existing `cp`-class tool provides together: inline integrity hashing at zero extra I/O, crash-safe resumable state, and constant-time resume verification.

::: info Note
SCC applies to the buffered read/write copy path. When reflink (CoW) or `copy_file_range` succeeds, bcmr uses those kernel fast paths instead — these bypass userspace entirely, so inline hashing is not available. Verification with `-V` in those cases falls back to a separate hash pass.
:::

This document describes the algorithm design with formal analysis, presents ablation experiments across macOS and Linux validating each design decision, and compares with prior art.

## Problem Statement

File copying with integrity verification faces a fundamental I/O trade-off. Let $S$ denote the source file of size $n$ bytes and $D$ the destination.

| Operation | I/O Passes | Total Bytes Read/Written |
|-----------|-----------|--------------------------|
| `cp S D` | 1R + 1W | $2n$ |
| `cp S D && sha256sum S D` | 3R + 1W | $4n$ |
| `rsync --checksum S D` | 2R + 1W | $3n$ (both sides hash) |

The verification tax is steep: confirming a copy doubled is a whole-file re-read. For a 100 GB file on a 500 MB/s drive, that is **200 extra seconds** purely for re-reading.

Resume after interruption is worse. Existing tools verify the written prefix by re-hashing it entirely:

$$T_{\text{resume-verify}} = \mathcal{O}(k) \quad \text{where } k = \text{bytes already written}$$

For a 90% complete 100 GB transfer, this means re-reading 90 GB just to confirm the prefix is intact.

## Algorithm Design

### Core Insight

BLAKE3 achieves 1--5 GB/s on modern hardware (NEON / AVX-512), which **exceeds the throughput of most storage devices**. Hashing is no longer the bottleneck --- disk I/O is. This means we can compute the source hash *during* the copy at effectively zero marginal cost.

### Data Flow

```
Source ──read──> [4MB buffer] ──write──> Destination (.bcmr.tmp)
                     │
                 src_hasher.update(buf)     ← streaming hash (free)
                 block_hasher.update(buf)   ← per-block hash
                     │
                 every 64 MB:
                   fdatasync(dst)           ← data durable
                   session.save()           ← atomic write → fsync → rename
```

### Session File

The session file persists the copy state across crashes. It uses a compact binary format:

![Session Layout](/images/ablation/session_layout.png)

For a file of $n$ bytes with block size $b = 4\,\text{MB}$:

$$|\text{session}| = 256 + 32 \cdot \lceil n / b \rceil \quad \text{bytes}$$

| File Size | Blocks | Session Size | Overhead |
|-----------|--------|-------------|----------|
| 1 GB | 256 | 8.2 KB | $7.6 \times 10^{-6}$ |
| 10 GB | 2,560 | 80 KB | $7.6 \times 10^{-6}$ |
| 100 GB | 25,600 | 800 KB | $7.6 \times 10^{-6}$ |
| 1 TB | 262,144 | 8 MB | $7.6 \times 10^{-6}$ |

![Session Overhead](/images/ablation/session_overhead.png)

The overhead converges to $32/b \approx 7.6 \times 10^{-6}$ as metadata becomes negligible.

### Crash Safety Invariant

The write ordering ensures a strict invariant:

![Crash Safety](/images/ablation/crash_safety.png)

Let $S$ be the session and $B_k$ the $k$-th block on disk. After each checkpoint:

$$\forall\, k < S.n: \quad B_k \text{ is durable on disk} \;\wedge\; H(B_k) = S.\text{hashes}[k]$$

The ordering is:

1. `write(dst, block_data)` --- block in page cache
2. `fdatasync(dst)` --- block durable on media
3. `session.save()` via atomic write-fsync-rename --- session updated

If a crash occurs at any point:
- **Before step 2**: Block not on disk. Session unchanged. Block is recopied.
- **Between steps 2 and 3**: Block on disk, session old. Block is recopied (redundant but correct).
- **After step 3**: Both durable. Resume from $B_{k+1}$.

No state can be reached where the session claims a block is complete but the block is not on disk.

### Resume: Tail-Block Verification

On resume, we exploit the invariant. All blocks except possibly the last are guaranteed by the checkpoint ordering. We only verify the **tail block** --- the one that was being written when the crash occurred:

$$T_{\text{resume}} = T_{\text{hash}}(b) = \mathcal{O}(1) \quad \text{independent of } k$$

versus full prefix rehash:

$$T_{\text{old-resume}} = T_{\text{hash}}(k) = \mathcal{O}(k)$$

### 2-Pass Verified Copy

Since the source hash is computed inline during the copy, the `-V` verification mode needs only one additional pass (re-read destination to hash it), not two:

| Mode | I/O Passes | Total Bytes |
|------|-----------|-------------|
| Old `-V`: copy, hash src, hash dst | 3R + 1W | $4n$ |
| New `-V`: copy+hash src, hash dst | 2R + 1W | $3n$ |
| Saving | 1 full read eliminated | $n$ bytes, 25% of total |

![I/O Complexity](/images/ablation/io_complexity.png)

### Durable Sync

On macOS, `fsync()` only flushes data from the OS buffer cache to the **drive's write cache** --- it does not issue a cache flush command to the drive controller. Data can be lost on power failure. `F_FULLFSYNC` via `fcntl()` issues a full barrier.

bcmr uses `F_FULLFSYNC` on macOS and `fdatasync()` on Linux (where `data=ordered` mode on ext4/XFS provides sufficient ordering guarantees). After every atomic rename, the parent directory is also fsynced to ensure the directory entry is durable.

### Parallel SSH with Independent Connections

SSH's `ControlMaster` multiplexing serializes all channels through one TCP connection and one encryption context. For $P$ parallel workers, throughput is bounded by a single core's encryption speed regardless of $P$.

bcmr assigns each parallel worker its own `ControlPath`, creating $P$ independent TCP connections:

$$\text{throughput} \approx \min(P \cdot T_{\text{single}},\; T_{\text{link}})$$

The [mscp project](https://github.com/upa/mscp) measured 5.98x speedup with 8 independent connections on a 100 Gbps link.

### Page Cache Management

Large copies pollute the page cache, evicting unrelated cached data. On Linux, bcmr calls `posix_fadvise(FADV_DONTNEED)` at each checkpoint interval to evict already-copied pages from both source and destination file descriptors.

### Serve Protocol (Remote Transfers)

For remote transfers, bcmr implements a binary frame protocol (`bcmr serve`) that replaces per-file SSH process spawning with a persistent connection over stdin/stdout. The protocol uses length-prefixed frames (`[4B length][1B type][payload]`) and supports: `Stat`, `List`, `Hash`, `Get`, `Put`, `Mkdir`, `Resume`.

Key properties:
- **Single connection**: all operations multiplexed over one SSH session, eliminating $\mathcal{O}(n)$ process spawns for $n$ files
- **Server-side hashing**: the remote bcmr computes BLAKE3 hashes locally, avoiding data round-trips for verification
- **Automatic fallback**: if the remote does not have bcmr installed, transfers silently fall back to legacy SCP
- **Frame size limit**: `read_message` rejects frames $> 16\,\text{MiB}$ to prevent memory exhaustion from malicious peers

See the [Remote Copy guide](/guide/remote-copy#serve-protocol-accelerated-transfers) for usage details.

---

## Ablation Experiments

All experiments use median of 3--5 runs. File data is pseudo-random (`(i*7+13) mod 256`) to prevent compression and deduplication artifacts.

**Test environments:**
- **macOS**: Apple Silicon, APFS SSD
- **Linux**: Intel Xeon Gold 6238R (AVX-512), NVMe SSD (Samsung), ext4

### Experiment 1: Inline BLAKE3 Hash Overhead

**Hypothesis**: BLAKE3 throughput exceeds storage I/O, so inline hashing adds negligible wall-clock time.

**Method**: Copy files of size $n \in \{16, 64, 256, 512, 1024\}$ MB in three modes: (A) copy only, (B) copy + `hasher.update()`, (C) copy + hash + `hasher.clone()` per block.

![BLAKE3 Throughput](/images/ablation/blake3_throughput.png)

| Platform | BLAKE3 Throughput | Bottleneck |
|----------|------------------|-----------|
| macOS (NEON) | ~1.0 GB/s | CPU-bound for fast SSD |
| Linux (AVX-512) | ~5.4 GB/s | Always I/O-bound |

On Linux, BLAKE3 at 5.4 GB/s exceeds NVMe peak (~3.5 GB/s). Inline hashing is truly free --- the CPU finishes hashing before the next disk read completes.

On macOS, BLAKE3 at ~1 GB/s is comparable to SSD speed, so warm-cache tests show 8--56% overhead. In cold-cache (real-world) scenarios, disk latency dominates and the overhead shrinks toward the Linux numbers.

### Experiment 2: 2-Pass vs 3-Pass Verification

**Hypothesis**: Eliminating one full-file read from verification saves $n / (3n) \approx 33\%$ of total I/O.

| File Size | 3-pass | 2-pass | Speedup | Theoretical |
|-----------|--------|--------|---------|------------|
| 64 MB | 171 ms | 163 ms | 1.05x | 1.33x |
| 256 MB | 654 ms | 623 ms | 1.05x | 1.33x |
| 512 MB | 1426 ms | 1251 ms | 1.14x | 1.33x |

Warm-cache results show 5--14% savings (page cache masks the eliminated read). With cold cache, the savings converge toward the theoretical 33%.

### Experiment 3: Tail-Block vs Full Prefix Rehash

**Hypothesis**: $T_{\text{tail}} = \mathcal{O}(1)$ while $T_{\text{full}} = \mathcal{O}(k)$.

![Resume Verification](/images/ablation/resume_verification.png)

| Written | Full Rehash (macOS / Linux) | Tail-Block | Speedup |
|---------|---------------------------|------------|---------|
| 48 MB | 50.7 / 15.3 ms | 4.4 / 1.6 ms | **11x / 10x** |
| 192 MB | 198.2 / 62.5 ms | 4.5 / 1.8 ms | **44x / 34x** |
| 384 MB | 396.2 / 122.2 ms | 5.1 / 1.8 ms | **78x / 66x** |
| 768 MB | 817.3 / 240.0 ms | 5.6 / 1.8 ms | **145x / 131x** |

Tail-block verification is constant at ~5 ms (macOS) / ~1.8 ms (Linux) regardless of file size. The speedup grows linearly with $k$ as predicted.

### Experiment 4: Sync Interval Overhead

**Hypothesis**: There exists an interval $I$ where the fsync overhead is acceptable ($<20\%$) and worst-case rework on crash is bounded.

![Sync Interval](/images/ablation/sync_interval.png)

| Interval | macOS Overhead | Linux Overhead | Max Rework |
|----------|---------------|----------------|------------|
| 4 MB | +225% | +37% | 4 MB |
| 16 MB | +58% | +12.5% | 16 MB |
| **64 MB** | **+16%** | **+3.9%** | **64 MB** |
| 256 MB | +9% | +0.8% | 256 MB |

64 MB (16 blocks) is the chosen default: $\leq 16\%$ overhead on both platforms, at most 64 MB of rework (~0.1s on NVMe).

### Experiment 5: F_FULLFSYNC vs fsync on macOS

**Hypothesis**: `F_FULLFSYNC` costs negligibly more than `fsync` on Apple Silicon.

![F_FULLFSYNC Comparison](/images/ablation/fsync_comparison.png)

| File Size | fsync | F_FULLFSYNC | Difference |
|-----------|-------|-------------|-----------|
| 4 MB | 7.0 ms | 6.0 ms | -14% |
| 16 MB | 12.0 ms | 13.9 ms | +16% |
| 64 MB | 33.0 ms | 34.1 ms | +3% |
| 256 MB | 143.5 ms | 125.0 ms | -13% |

Differences are within noise. `F_FULLFSYNC` provides **correct durability guarantees** at no measurable performance cost. SQLite, RocksDB, and PostgreSQL all use `F_FULLFSYNC` on macOS.

### Experiment 6: `copy_file_range` with Offset (Linux)

**Hypothesis**: The kernel fast path supports non-zero offsets for resume, avoiding userspace buffer copies.

![copy_file_range Resume](/images/ablation/cfr_resume.png)

| File Size | read/write | copy_file_range | Speedup |
|-----------|-----------|-----------------|---------|
| 64 MB | 52 ms | 42 ms | 1.24x |
| 256 MB | 185 ms | 171 ms | 1.08x |
| 512 MB | 356 ms | 323 ms | 1.10x |

8--24% faster on NVMe. The benefit would be larger on slower media or network filesystems where zero-copy matters more.

---

## Comparison with Prior Art

![Tool Comparison](/images/ablation/tool_comparison.png)

| | cp | rsync | curl -C | aria2 | bcmr (SCC) |
|---|---|---|---|---|---|
| Resume granularity | None | Block rolling | Byte offset | 16 KiB bitmap | 4 MB blocks |
| Resume verification | N/A | $\mathcal{O}(n)$ rolling | None | Piece hash (if available) | $\mathcal{O}(1)$ tail-block |
| State persistence | None | None | None | `.aria2` control file | Binary session file |
| Crash safety | None | Partial file left | Partial file left | Good (bitmap) | fdatasync ordering invariant |
| Source change detection | None | mtime+size | None | If-Modified-Since | mtime+size+inode (session) |
| Inline hash | No | No | No | No | Always-on BLAKE3 |
| Verify I/O cost | N/A | $3n$ | N/A | $2n$ (with piece hashes) | $2n$ (inline src hash) |

Key differentiators:
- **Constant-time resume verification** --- no other `cp`-class tool achieves $\mathcal{O}(1)$.
- **Always-on source hash** --- verification is a byproduct of copying, not a separate pass.
- **Formal crash safety** --- write ordering invariant with `F_FULLFSYNC` / `fdatasync` + directory fsync.

---

## Summary

| Decision | Measured Cost | Measured Benefit |
|----------|-------------|-----------------|
| Always-on BLAKE3 | 0--15% CPU (hidden by I/O) | Free source hash |
| Session file | $< 8 \times 10^{-6}$ of file size | Crash-safe resume |
| 64 MB checkpoint | 4--16% overhead | $\leq$ 64 MB rework |
| Tail-block verify | 1.8--5.6 ms constant | 50--145x vs full rehash |
| 2-pass `-V` | 0% (saves I/O) | 25% less total I/O |
| `F_FULLFSYNC` | ~0% | Correct macOS durability |
| `copy_file_range` offset | 0% (saves I/O) | 8--24% faster resume |
| Per-worker SSH | 0% (additive) | Up to ~6x parallel throughput |
| Serve protocol | 0% (replaces SSH spawns) | Eliminates per-file process overhead |
| Opt-in per-file fsync | ~0% (default skip) | 13x faster many-small-files |
| `--jobs` parallel local copy | 0% (configurable) | 1.5--2x on many-medium workloads |
| Auto-skip wire compression | Negligible (LZ4 ~4 GB/s encode on random) | 2--5x bandwidth on source text |

---

## Experiments 7--9 (v0.5.7)

This section covers three performance investigations added after the
initial v0.5.4 release. The common thread is that the original design
had correctness-first defaults --- fsync after every rename, serial
file-at-a-time copy, raw bytes on the wire --- that were the right
baseline but paid for durability even when the user hadn't asked for
it, and left cores and bandwidth idle on typical workloads.

### Experiment 7: Per-File Durability Cost

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
`F_FULLFSYNC` was free (Experiment 5 above). The measurement was
done on *single large files* where the fsync cost is amortised over
hundreds of megabytes of copy time. For many-small-files the cost
becomes `fsync_count × fsync_latency`, which scales with file count,
not file size. The single-file benchmark didn't exercise the regime
where the pathology lives.

### Experiment 8: File-Level Parallelism

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

### Experiment 9: Wire Compression for Remote Transfers

**Hypothesis**: Per-block LZ4/Zstd encoding pays for itself whenever
the network is slower than the codec. On modern CPUs LZ4 decodes at
multiple GB/s, so the receiver is never compute-bound; the only
question is ratio.

**Method (Part A — codec probe)**: encode then decode a single 4 MiB
block three times (random, text-like, mixed) for each algorithm.
Ratios and throughputs measured on Apple Silicon:

| Workload | Algo     | Ratio | Enc MB/s | Dec MB/s |
|----------|----------|------:|---------:|---------:|
| random   | LZ4      | 1.004 |   3578.2 |  17330.5 |
| random   | Zstd-1   | 1.000 |   4769.9 |  33692.0 |
| random   | Zstd-3   | 1.000 |   4655.3 |  33635.0 |
| random   | Zstd-9   | 1.000 |   2442.8 |  31432.3 |
| text     | LZ4      | 0.390 |    472.8 |   1526.7 |
| text     | Zstd-1   | 0.210 |    301.2 |    871.9 |
| text     | **Zstd-3** | **0.198** | **320.5** |   1012.1 |
| text     | Zstd-9   | 0.180 |     47.2 |   1130.9 |
| mixed    | LZ4      | 0.697 |    863.4 |   2875.6 |
| mixed    | Zstd-3   | 0.599 |    457.2 |   1971.9 |

**Interpretation**:

1. **Random data**. All three codecs return ratios indistinguishable
   from 1.0. Sending compressed is pure CPU waste, so the wire path
   must auto-skip when the encode output is within 5 % of the input.
2. **Text-like data**. Zstd-3 reaches 5x reduction at 320 MB/s encode.
   For anything under ~2.5 Gbps of effective network throughput,
   compression is the bandwidth bottleneck, not the CPU.
3. **Zstd-9** is consistently worse than -3 for file content: encode
   drops by 7x (to 47 MB/s) for only a ~2 % ratio gain. Skip it.

**Decision**: Default to auto-negotiation advertising both LZ4 and
Zstd. The handshake picks Zstd when both sides speak it (better
ratio at acceptable encode cost), falls back to LZ4 when only one
does, and to raw Data frames otherwise. Zstd level fixed at 3 --- the
library's own default, and our measurement agrees.

**Method (Part B — auto-skip in vivo)**: a unit test encodes a 4 MiB
pseudo-random block through `encode_block(Lz4, ...)` and asserts the
emitted message type is `Data` (raw), not `DataCompressed`. Covers
the happy path where the codec's frame header + payload overshoots
the 0.95 × original threshold and the encoder falls back.

**Backward compatibility**: `Hello` / `Welcome` carry an optional
trailing caps byte. Old decoders read `version` and stop; new
decoders read `caps` too. `caps.unwrap_or(0)` means talking to an
old peer automatically negotiates to `CompressionAlgo::None`, so no
protocol version bump is needed.

---

---

## Experiments 10--12 (v0.5.8)

### Experiment 10: Whole-Source BLAKE3 on the I/O Thread

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

### Experiment 11: Content-Addressed Dedup for Repeat PUT

**Hypothesis**: For dev workflows where the same artifact is uploaded
to a remote host repeatedly, the second-and-onward upload can avoid
the wire entirely if the receiver remembers what it has seen.
BLAKE3 is already computed per 4 MiB block, so a tiny pre-flight
that exchanges hashes lets the server short-circuit to a local CAS
read.

**Design**: Negotiate `CAP_DEDUP` in the Hello/Welcome caps byte.
When active and the file is at least 16 MiB:

1. Client hashes the source in 4 MiB blocks (re-reads the file ---
   we don't keep the bytes around).
2. Client sends `HaveBlocks { block_size, hashes }`.
3. Server checks each hash against `~/.local/share/bcmr/cas/<aa>/<bb>/<rest>.blk`,
   replies `MissingBlocks { bits }` (1 = needed on the wire).
4. Client streams only the missing blocks via the existing Data /
   DataCompressed path.
5. For each block the server receives, it both writes it to the dst
   file *and* deposits it in the CAS for future reuse. For each
   block the server already had, it reads the cached copy at the
   right point in the stream.

The composite hash returned in `Ok` covers the full file regardless
of which blocks took which path. The 16 MiB threshold protects
small uploads from the round-trip cost of HaveBlocks/MissingBlocks
itself.

**Method**: 64 MiB pseudo-random file uploaded twice from macOS to a
Linux host (`4090_J`, ~30 ms RTT, ~10 MB/s effective WAN bandwidth).
Cold cache via `rm -rf ~/.local/share/bcmr/cas` between runs.

| Run | Wall (s) | Notes |
|-----|---------:|-------|
| 1 (cold cache) | 18.96 | full 64 MiB on the wire |
| 2 (warm cache) | 12.93 | every block a CAS hit; ~6 s saved |

The savings track the eliminated wire bytes: 64 MiB at ~10 MB/s ≈
6 s, which matches the observed delta. The remaining 13 s is local
hash + CAS read + dst write + protocol round trips, all on either
side of the network. For higher-bandwidth links the relative win
shrinks; for slower / metered ones (cellular tethering, transoceanic
SSH) it grows.

**Correctness check**: SHA-256 of source matches both destinations
across the two runs.

### Experiment 12: Wire Compression Across Real Hosts

The earlier Experiment 9 measured codec ratios in isolation; this
one re-runs the protocol over real SSH connections to confirm the
prediction. 64 MiB of source-text-like content from this MacBook to
three peers, three runs each.

| Peer | None (s) | LZ4 (s) | Zstd (s) | Zstd vs None |
|------|---------:|--------:|---------:|-------------:|
| 4090_J (WAN, ~10 MB/s) | 18.18 | 10.22 | 3.25 | **5.59x** |
| A100_J (WAN, ~10 MB/s) | 8.14 | 4.36 | 3.28 | 2.48x |
| mini_m2 (LAN, gigabit) | 9.58 | 3.26 | 1.82 | 5.28x |

Zstd-3 wins on every link. LZ4 wins over None but loses to Zstd
because the bandwidth saving from Zstd's extra ratio more than pays
for the lower encode throughput. The A100_J peer's smaller relative
win comes from the path's variance dominating the small absolute
duration --- the absolute saving is similar to the others.

---

## Open Questions (still)

After Experiments 10--12 the items below are still on the list,
intentionally deferred because each needs design work that didn't
fit this release.

- **Zero-copy serve path**. `splice(2)` from the source fd into a
  pipe and then into stdout would bypass two userspace memcpys per
  4 MiB block. The blocker is that `splice` doesn't expose the bytes
  to userspace, so the inline BLAKE3 we use to populate the `Ok
  { hash }` response can't run. Likely shape: a `CAP_FAST` cap that
  trades server-side hashing for splice. Users who care about
  integrity stay on the current path or pass `-V` (which re-hashes
  the dst on the client anyway).
- **io_uring read path on Linux**. Each `read.await` trips a
  syscall; batching via `io_uring` would help when sequential
  throughput is at the syscall ceiling rather than the device. Need
  to evaluate `tokio-uring` vs raw `io-uring` --- the former still
  gates everything through tokio's reactor. Likely modest single-
  digit-percent win on NVMe; bigger on parallel-many-files when
  combined with the existing `--jobs`.
- **CAS eviction / cap**. Today the dedup CAS grows monotonically
  under `~/.local/share/bcmr/cas`. The cleanest design is an LRU
  with a configurable byte cap (default ~1 GiB?), garbage-collected
  on the next dedup-enabled PUT.
- **Pipelined hashing for the streaming-copy hot path**. We tried
  the obvious move-to-channel approach in v0.5.8 and the channel
  send + Vec allocation per 4 MiB block actually slowed things
  down. A useful version would need a buffer pool (e.g. `Bytes`)
  and probably also `update_rayon` --- the wins on the existing
  serial path are small enough that "skip the hash entirely when
  it's not needed" (Experiment 10) was the better lever.
