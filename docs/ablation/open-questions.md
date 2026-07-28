---
title: Open Questions
section: internals
order: 7
---

These are open investigations plus resolved historical notes that
surfaced during and after the v0.5.7 / v0.5.8 work. Open entries
need design before shipping; the notes record the shape we have in
mind so follow-up work does not start from scratch.

## Zero-Copy Serve GET via `splice(2)`

`CAP_FAST` shipped in v0.5.9 with a `splice(2)` Linux path
(see [Wire Experiment 14](/ablation/wire-protocol#experiment-14-cap-fast-real-numbers)),
but the loopback measurement showed it's actually *slower* than
the default buffered path because of two implementation problems:

1. **Pipe buffer**: `fcntl(F_SETPIPE_SZ, 4 MiB)` needs the kernel's
   `pipe-max-size` knob lifted (default 1 MiB on Ubuntu) or root.
   When it silently fails, each 4 MiB chunk takes ~64 splice rounds
   instead of the expected 1.
2. **`spawn_blocking` per chunk**: the splice loop currently
   dispatches one tokio blocking task per chunk --- the exact
   anti-pattern Experiment 13 fixed for the local copy path.

**Fix**: move the entire splice loop into one `spawn_blocking` per
file; either probe `/proc/sys/fs/pipe-max-size` and use the
largest allowed value, or fall back to writing the frame header +
plain `read+write` syscalls inside that one blocking task.

Until that's done, `--fast` still has a use: it skips the server's
BLAKE3 computation, which matters for low-spec servers even over
WAN.

## io_uring Read Path on Linux

Each `read` in the inner copy loop is a plain `read(2)` syscall
since v0.5.9 ([Local Perf Experiment 13](/ablation/local-perf#experiment-13-one-spawn-blocking-for-the-whole-loop)
moved the loop into one spawn_blocking). Batching via `io_uring`
would replace the read+write pair per chunk with a single
submission queue entry, saving the round trip through the kernel.

**Decision needed first**: `tokio-uring` vs raw `io-uring` --- the
former still requires its own runtime
(`tokio_uring::start()`) that doesn't drive standard tokio futures,
which is a structural problem for bcmr; the latter requires running
outside tokio for the read loop and reattaching futures around it.

Estimated win after Experiment 13: single-digit-percent on
sustained NVMe reads. The big multipler that motivated this entry
in v0.5.7's open list (10x slower than cp) was actually the
spawn_blocking-per-chunk overhead, which Experiment 13 closed
without io_uring.

## Resolved: CAS LRU / Cap

The CAS now has a configurable byte cap and LRU-by-mtime eviction.
Reads and writes touch the block, eviction is concurrency-tolerant,
and integration tests prove the store stays at or below the cap
under repeated PUT load. See
[Wire Experiment 15](/ablation/wire-protocol#experiment-15-cas-lru-eviction-under-load).

## Pipelined Hashing for the Streaming-Copy Hot Path

Tried in v0.5.8 (see [Experiment 10](/ablation/local-perf#experiment-10-whole-source-blake3-on-the-i-o-thread))
and the obvious version was *slower* than the synchronous double
hash --- the per-chunk `Vec<u8>` clone for the channel send and the
channel sync overhead together cost more than the parallelism
saved. A useful version would need:

- A buffer pool (e.g. `bytes::Bytes` with refcount) so the channel
  send is zero-copy.
- Probably also `update_rayon` so the hash itself parallelises
  across cores instead of running on one.

The wins on the existing serial path are small enough that "skip
the hash entirely when it's not needed" was the better lever
(Experiment 10 did this, gating on `verify || session.is_some()`).
The leftover gap to `cp` on the streaming path comes from per-block
hash, per-checkpoint `posix_fadvise`, and tokio I/O scheduling
overhead --- those are separate experiments.

## Recursive Tree Dedup

Dedup currently fires only on individual file PUTs. Extending it to
directory copies (where the client first sends a manifest of all
files + per-file block hashes, server probes the CAS in one
round-trip, client streams only what's missing across the whole
tree) would be the natural follow-up to Experiment 11. Saves $N - 1$
extra round-trips for $N$ files.

## Content-Defined Chunking for CAS (FastCDC)

`CAP_DEDUP` today chunks at fixed 4 MiB boundaries + BLAKE3 per
block (see
[Experiment 11](/ablation/wire-protocol#experiment-11-content-addressed-dedup-for-repeat-put)
and `src/core/serve_client/ops.rs:319`). That's optimal for
"re-upload the exact same artifact" — the measured 32 % win on
that experiment — but a single-byte insertion partway through a
file shifts every subsequent block boundary and evicts the whole
tail from the CAS hit set.

FastCDC-style chunking replaces the fixed boundary with a
lightweight gear rolling hash that picks boundaries based on
content, targeting the same ~4 MiB average. A mid-file insert
only displaces the chunks around it; everything after the next
CDC boundary still matches the previous upload. This is **not**
rsync-style byte-precise delta-sync (see
[Non-Goal: Rolling-Checksum Delta-Sync](/ablation/no-rolling-checksum))
— the wire unit stays a whole chunk of ~4 MiB, matched whole,
hashed with BLAKE3. Only the boundary rule changes.

**Protocol change required.** The "wire format unchanged"
intuition this proposal sometimes comes with is wrong:

- `Message::HaveBlocks { block_size: u32, hashes: Vec<[u8; 32]> }`
  today carries a single block size for all hashes; CDC needs
  per-chunk length (and offset) on the wire. Either extend the
  message or introduce a variant under a new cap bit
  (`CAP_DEDUP_CDC`) so the fixed-block path stays interoperable
  with older peers.
- Server reconstruction currently derives each block's file
  offset from `idx * block_size`; with variable chunks the offset
  has to come from the wire.
- Post-upload CAS lookup by hash is unchanged (CAS is keyed by
  hash alone, not offset).

**Measure before shipping.** CAS's one measured workload is
identical 64 MiB re-upload. CDC's *additional* win over fixed
blocks materialises only on re-uploads where content was modified
in place — SQL dumps patched mid-file, ML checkpoints with
rewritten weights, VM images after an in-place patch. We have no
data that these dominate bcmr users' actual traffic. Reasonable
experiment shape:

1. Feature-flag a gear-hash chunker (e.g. the `fastcdc` crate)
   behind an env var; emit the variable-chunk message when set.
2. On a workload with real mid-file deltas (a repeated SQL dump
   across schema migrations, or a synthetic "insert 64 KiB at
   byte offset 512 MiB in a 2 GiB file"), compare CAS hit rate
   and wire bytes saved vs fixed blocks.
3. If hit-rate gain is material on a representative workload,
   plan a protocol bump; otherwise park indefinitely.

Conservative implementation estimate: 1–2 weeks (chunker
integration, protocol variant, server offset handling, test
matrix for small files / all-zero inputs / boundary-aligned
edits). Add a few days up front for the measurement gate.

## Closing the 1 GiB Single-File Gap to scp

After [Experiment 18](/ablation/wire-protocol#experiment-18-client-side-request-pipelining)
closed many-files to 1.18× scp, the single-1-GiB workload still
runs at ~2× scp (`--fast` 4.61 s vs scp 2.41 s). The remaining
overhead is structural:

1. **Per-block BLAKE3** (~1 GB/s NEON / AVX-512). For 1 GiB
   that's ~1 second of pure hash CPU. Scp computes nothing.
2. **Frame overhead**. Each Data frame has a 9-byte header per
   4 MiB chunk. Negligible bytes but each `write_message`
   crosses the tokio scheduler. ~256 frames per GiB.
3. **Tokio I/O scheduling**. Per Experiment 13, `tokio::fs`
   reads do `spawn_blocking` per call; we partially fixed this
   for the local copy path but the serve send-loop still goes
   through `protocol::write_message().await` per frame.

Possible directions, ordered by cleanness:
- A `--no-hash` mode that drops integrity entirely (stronger
  than `--fast` which only skips server-side; this also skips
  client-side per-block hashing). Would close most of the 1×
  CPU-second gap.
- A "bulk" wire mode that sends the body raw after a single
  Put header — no per-chunk framing during streaming. Server
  reads `declared_size` bytes; client writes them. Loses the
  ability to interleave `Error` mid-stream but matches scp's
  shape exactly.

Single-file isn't the most common workload for bcmr serve
(WAN deployments are wire-bound; many-files dominates LAN),
so this is parked behind any user complaint that actually
identifies single-file as their bottleneck.

## Resolved: Visible Fallback When Path Escapes Server Root

Fallback is now typed and allowed only before transfer mutation.
The client prints the serve failure reason plus a visible
legacy-SCP warning. Once serve-side processing may have mutated
state, errors are returned instead of trying a second transport.

## Resolved: Path B Direct TCP After SSH Rendezvous

The direct data plane shipped with SSH-authenticated rendezvous,
challenge-response possession proof, mandatory AES-256-GCM framing,
bounded squatter handling, and SSH fallback for unreachable direct
addresses. See [Wire Experiment 20](/ablation/wire-protocol).

## Transactional Single-File Striped PUT

The historical striped upload first truncated the visible remote
destination, then let independent sessions write chunks into it.
Any disconnect exposed a partial file and destroyed an existing
good version. Production v2 therefore routes single-file uploads
through one handle-bound atomic transaction; `-P N` remains active
for multi-file batches.

Re-enabling single-file striping safely requires:

1. `BeginPut` creates one server-owned transaction and returns an
   unguessable transfer token;
2. every worker authenticates into that transaction and writes a
   bounded, non-overlapping range;
3. the coordinator verifies complete interval coverage, length,
   hash, metadata and durability;
4. exactly one commit atomically publishes or no-replaces;
5. worker loss cancels the transaction without touching the final
   path.

This stays off until fault-injection covers missing, duplicate,
overlapping and reordered chunks plus coordinator death. The
performance ablation must then show a material win over direct TCP
or a single compressed stream; otherwise the extra protocol state
is not justified.

## xattr Cross-FS Edge Cases

Today's xattr preservation (see code under `commands/copy.rs`) is
best-effort: ENOTSUP and EPERM are both swallowed silently. That's
the right default for cross-FS copies, but we should track which
attributes were dropped and surface that under `-v` --- silent
dropping of `security.selinux` or Finder tags is a footgun for
power users.
