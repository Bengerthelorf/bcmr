# Internals

These pages document the design decisions inside bcmr that aren't
visible from the CLI surface, with measurements from real workloads.
The structure mirrors the layers of the tool:

- **[Streaming Checkpoint Copy](/ablation/scc)** is the algorithm at
  the heart of every single-file copy: 4 MiB blocks, inline BLAKE3,
  64 MB checkpoint with crash-safe write ordering, $\mathcal{O}(1)$
  resume verification, and the kernel fast paths (`copy_file_range`,
  `clonefile`/reflink) we fall through to when available.

- **[Local Multi-File Performance](/ablation/local-perf)** covers the
  parts above that algorithm: per-file fsync gating, file-level
  `--jobs` concurrency, and skipping the whole-source hash when
  nothing reads it. These are the changes that took bcmr from "13×
  slower than `cp` on a 2100-file repo" to "1.5× faster than `cp`,
  2× faster than `rsync`" without touching the per-file path.

- **[Wire Protocol & Remote Transfers](/ablation/wire-protocol)** is
  the binary frame protocol that drives `bcmr serve`, the
  per-worker SSH connection design, the negotiated wire compression
  (LZ4 + Zstd-3 with auto-skip), and the content-addressed dedup
  for repeat PUTs.

- **[Open Questions](/ablation/open-questions)** lists work that's
  designed but not shipped: `splice(2)` zero-copy, `io_uring` reads,
  CAS LRU eviction, and the failed pipelined-hashing experiment.

## Cross-Cutting Summary

Every row below has its own experiment with measured numbers; click
through to the relevant page for the table. Every number here is
from a **specific workload** — the benefit column notes the
conditions so the row can't be lifted context-free.

| Decision | Lives on | Measured benefit (workload) |
|----------|----------|------------------|
| Always-on BLAKE3 (single hash) | [SCC](/ablation/scc#experiment-1-inline-blake3-hash-overhead) | Free on Linux AVX-512 (~5 GB/s > NVMe); 8--56 % overhead on macOS NEON warm cache |
| Tail-block resume verify | [SCC](/ablation/scc#experiment-3-tail-block-vs-full-prefix-rehash) | 50--145× vs full prefix rehash, 48--768 MiB written, mac/Linux |
| 64 MiB checkpoint interval | [SCC](/ablation/scc#experiment-4-sync-interval-overhead) | ≤ 16 % overhead and ≤ 64 MiB rework on both platforms (single-file, warm cache) |
| `copy_file_range` with offset | [SCC](/ablation/scc#experiment-6-copy-file-range-with-offset-linux) | 8--24 % faster resume on Linux NVMe (64--512 MiB) |
| Opt-in per-file fsync | [Local Perf](/ablation/local-perf#experiment-7-per-file-durability-cost) | 13× faster (9.9 s → 0.72 s) on 2100 × 4 KiB repo, mac APFS warm cache |
| `--jobs` parallel local copy | [Local Perf](/ablation/local-perf#experiment-8-file-level-parallelism) | 1.5--1.67× vs `-j1` on 10 000 × 64 KiB files, mac APFS warm cache |
| Skip src hash when unused | [Local Perf](/ablation/local-perf#experiment-10-whole-source-blake3-on-the-i-o-thread) | 28 % off (285 → 205 ms) on 32 MiB streaming no-verify copy, mac APFS |
| Single spawn_blocking copy loop | [Local Perf](/ablation/local-perf#experiment-13-one-spawn-blocking-for-the-whole-loop) | 2.3× (12.3 → 5.4 s) on 2 GiB streaming copy, Linux NVMe ext4 warm cache |
| Session + checkpoint gated on intent | [Local Perf](/ablation/local-perf#experiment-16-gate-session-block-hash-checkpoint-fsync-on-intent-v0-5-10) | ~2× (3.9 → 1.89 s) on 1 GiB streaming copy, mac APFS; lands within 1.65× of cp |
| Per-worker SSH connections | [Wire](/ablation/wire-protocol#parallel-ssh-with-independent-connections) | Up to ~6× parallel throughput (not re-measured on this tree; mscp's 8-conn 100 Gbps figure) |
| Wire compression (Zstd-3) | [Wire](/ablation/wire-protocol#experiment-12-wire-compression-across-real-hosts) | 2.48--5.59× vs uncompressed on 64 MiB source-text, ~10 MB/s WAN; essentially no cost on incompressible blocks (auto-skip) |
| `CAP_DEDUP` repeat PUT | [Wire](/ablation/wire-protocol#experiment-11-content-addressed-dedup-for-repeat-put) | 32 % faster (18.9 → 12.9 s) on 64 MiB re-upload, ~10 MB/s WAN; savings match the bytes not sent |
| `CAP_FAST` GET | [Wire](/ablation/wire-protocol#experiment-14-cap-fast-real-numbers) | **Mixed.** 1.07× on WAN (network-bound); **0.78× (i.e. slower) on Linux loopback** due to pipe-size + spawn_blocking issues documented in the experiment |
| CAS LRU cap | [Wire](/ablation/wire-protocol#experiment-15-cas-lru-eviction-under-load) | Holds CAS ≤ cap under 3× 24 MiB repeat uploads (unit-test-sized; intended to prove bound, not speedup) |
