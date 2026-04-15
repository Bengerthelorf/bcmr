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
through to the relevant page for the table.

| Decision | Lives on | Measured benefit |
|----------|----------|------------------|
| Always-on BLAKE3 (single hash) | [SCC](/ablation/scc#experiment-1-inline-blake3-hash-overhead) | Free source hash on Linux AVX-512; small cost on macOS NEON |
| Tail-block resume verify | [SCC](/ablation/scc#experiment-3-tail-block-vs-full-prefix-rehash) | 50--145× vs full prefix rehash |
| 64 MB checkpoint interval | [SCC](/ablation/scc#experiment-4-sync-interval-overhead) | $\leq$ 16 % overhead, $\leq$ 64 MB rework |
| `copy_file_range` with offset | [SCC](/ablation/scc#experiment-6-copy-file-range-with-offset-linux) | 8--24 % faster resume on NVMe |
| Opt-in per-file fsync | [Local Perf](/ablation/local-perf#experiment-7-per-file-durability-cost) | 13× faster many-small-files |
| `--jobs` parallel local copy | [Local Perf](/ablation/local-perf#experiment-8-file-level-parallelism) | 1.5--2× on many-medium workloads |
| Skip src hash when unused | [Local Perf](/ablation/local-perf#experiment-10-whole-source-blake3-on-the-i-o-thread) | 28 % off no-verify streaming |
| Single spawn_blocking copy loop | [Local Perf](/ablation/local-perf#experiment-13-one-spawn-blocking-for-the-whole-loop) | 2.3× faster Linux NVMe streaming |
| Per-worker SSH connections | [Wire](/ablation/wire-protocol#parallel-ssh-with-independent-connections) | Up to ~6× parallel throughput |
| Auto-skip wire compression | [Wire](/ablation/wire-protocol#experiment-9-wire-compression-for-data-frames) | 2--5× bandwidth on source text |
| `CAP_DEDUP` repeat PUT | [Wire](/ablation/wire-protocol#experiment-11-content-addressed-dedup-for-repeat-put) | All wire bytes removed for cached blocks |
| `CAP_FAST` GET (splice on Linux) | [Wire](/ablation/wire-protocol) | Skip server hash; zero-copy on Linux |
| CAS LRU cap | [Wire](/ablation/wire-protocol#experiment-11-content-addressed-dedup-for-repeat-put) | Bounded disk usage for the dedup store |
