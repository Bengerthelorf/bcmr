# Wire Protocol & Remote Transfers

For remote transfers, bcmr implements a binary frame protocol
(`bcmr serve`) that replaces per-file SSH process spawning with a
persistent connection over stdin/stdout. The protocol uses
length-prefixed frames (`[4B length][1B type][payload]`) and supports:
`Stat`, `List`, `Hash`, `Get`, `Put`, `Mkdir`, `Resume`, plus the
extensions covered on this page.

Key properties of the base design:
- **Single connection**: all operations multiplexed over one SSH
  session, eliminating $\mathcal{O}(n)$ process spawns for $n$ files.
- **Server-side hashing**: the remote bcmr computes BLAKE3 hashes
  locally, avoiding data round-trips for verification.
- **Automatic fallback**: if the remote does not have bcmr
  installed, transfers silently fall back to legacy SCP.
- **Frame size limit**: `read_message` rejects frames $> 16$ MiB to
  prevent memory exhaustion from malicious peers.

The Hello / Welcome handshake carries an optional trailing
**capabilities byte** (LZ4 = `0x01`, Zstd = `0x02`, Dedup = `0x04`).
Old decoders read `version` and stop; new decoders read `caps` too.
Talking to a peer that doesn't advertise a bit just means the
feature stays off, so no protocol version bump is needed for
backward-compatible additions.

### Parallel SSH with Independent Connections

SSH's `ControlMaster` multiplexing serializes all channels through
one TCP connection and one encryption context. For $P$ parallel
workers, throughput is bounded by a single core's encryption speed
regardless of $P$.

bcmr assigns each parallel worker its own `ControlPath`, creating
$P$ independent TCP connections:

$$\text{throughput} \approx \min(P \cdot T_{\text{single}},\; T_{\text{link}})$$

The [mscp project](https://github.com/upa/mscp) measured 5.98x
speedup with 8 independent connections on a 100 Gbps link.

See the [Remote Copy guide](/guide/remote-copy#serve-protocol-accelerated-transfers)
for end-user configuration.

---

## Experiment 9: Wire Compression for Data Frames

**Hypothesis**: Per-block LZ4/Zstd encoding pays for itself whenever
the network is slower than the codec. On modern CPUs LZ4 decodes at
multiple GB/s, so the receiver is never compute-bound; the only
question is ratio.

**Method (Part A --- codec probe)**: encode then decode a single 4 MiB
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
does, and to raw Data frames otherwise. Zstd level fixed at 3 ---
the library's own default, and our measurement agrees.

**Method (Part B --- auto-skip in vivo)**: a unit test encodes a 4 MiB
pseudo-random block through `encode_block(Lz4, ...)` and asserts the
emitted message type is `Data` (raw), not `DataCompressed`. Covers
the happy path where the codec's frame header + payload overshoots
the 0.95 × original threshold and the encoder falls back.

## Experiment 11: Content-Addressed Dedup for Repeat PUT

**Hypothesis**: For dev workflows where the same artifact is
uploaded to a remote host repeatedly, the second-and-onward upload
can avoid the wire entirely if the receiver remembers what it has
seen. BLAKE3 is already computed per 4 MiB block, so a tiny
pre-flight that exchanges hashes lets the server short-circuit to a
local CAS read.

**Design**: Negotiate `CAP_DEDUP` in the Hello/Welcome caps byte.
When active and the file is at least 16 MiB:

1. Client hashes the source in 4 MiB blocks (re-reads the file ---
   we don't keep the bytes around).
2. Client sends `HaveBlocks { block_size, hashes }`.
3. Server checks each hash against
   `~/.local/share/bcmr/cas/<aa>/<bb>/<rest>.blk`, replies
   `MissingBlocks { bits }` (1 = needed on the wire).
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

::: info CAS Eviction
Today the CAS grows monotonically. Manual cleanup with
`rm -rf ~/.local/share/bcmr/cas` works but is easy to forget. A
size-capped LRU is on the [Open Questions](/ablation/open-questions)
list.
:::

## Experiment 12: Wire Compression Across Real Hosts

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

## Summary

| Decision | Measured Cost | Measured Benefit |
|----------|-------------|-----------------|
| Per-worker SSH | 0% (additive) | Up to ~6x parallel throughput |
| Serve protocol | 0% (replaces SSH spawns) | Eliminates per-file process overhead |
| Auto-skip wire compression | Negligible (LZ4 ~4 GB/s encode on random) | 2--5x bandwidth on source text |
| `CAP_DEDUP` repeat-PUT | One file re-read for hash | All wire bytes removed for cached blocks |
