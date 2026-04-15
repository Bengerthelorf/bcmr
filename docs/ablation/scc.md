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

![SCC data flow](/images/ablation/flow/scc_dataflow.svg)

The two hashers run in lockstep: `src_hasher` accumulates a single
BLAKE3 over the whole file (used for `-V` and for cross-run resume
detection); `block_hasher` is reset every 4 MiB so the session
file can record per-block hashes. See [Experiment 10](/ablation/local-perf#experiment-10-whole-source-blake3-on-the-i-o-thread)
for when the source hasher is skipped.

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

![Crash recovery state machine](/images/ablation/flow/scc_crash_safety.svg)

The two recovery branches that recopy a block are wasteful but
correct; the only forbidden state ("session says k is durable, but
k isn't on disk") is unreachable.

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

### Page Cache Management

Large copies pollute the page cache, evicting unrelated cached data. On Linux, bcmr calls `posix_fadvise(FADV_DONTNEED)` at each checkpoint interval to evict already-copied pages from both source and destination file descriptors.

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

::: warning
This conclusion is later revisited on the [Local Multi-File Performance](/ablation/local-perf) page (Experiment 10): the "free hash" claim was correct for *single* hashes but the streaming path actually hashed every byte twice (source + per-block), and the second hash isn't needed when nothing reads it.
:::

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

::: warning
This conclusion was for the single-file case. The [Local Multi-File Performance](/ablation/local-perf) page (Experiment 7) shows that *per-file* `F_FULLFSYNC` on the parent directory becomes catastrophic on many-small-files workloads, where the cost scales with file count rather than file size.
:::

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
| `F_FULLFSYNC` | ~0% (single file) | Correct macOS durability |
| `copy_file_range` offset | 0% (saves I/O) | 8--24% faster resume |
