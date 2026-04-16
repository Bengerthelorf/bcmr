# Open Questions

These are investigations that surfaced during the v0.5.7 / v0.5.8
work but were intentionally deferred. Each needs design before
shipping; the notes here record the shape we have in mind so the
follow-up doesn't start from scratch.

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

## CAS LRU / Cap

The dedup CAS at `~/.local/share/bcmr/cas` grows monotonically.
Cleanest design is an LRU with a configurable byte cap (default
~1 GiB), garbage-collected on the next dedup-enabled PUT.

**Layout sketch**:
- Sidecar `index` file mapping `hash -> (size, last_access_unix)`.
- Before each PUT that uses dedup, sum the index sizes. If over
  cap, drop oldest entries until under.
- `last_access_unix` updated whenever a block is read for a CAS hit.

**Edge case**: concurrent PUTs on the same machine. Could share an
advisory lock on the index, or just accept eventual consistency
(the worst that happens is a recently-evicted block gets re-fetched
from the wire on the next request).

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

## Silent Fallback When Path Escapes Server Root

The `--root` jail (default `$HOME`) is correct security
behavior, but when the server rejects a path the client falls
back silently to a slower transport (legacy per-file SSH).
Symptom from a user's perspective: "bcmr copy of /tmp/foo
takes 30 s where scp takes 2 s". Root cause is invisible
unless you `strace` the server or know to look for the
`path /... escapes server root` line on stderr. Found while
benchmarking [Experiment 17](/ablation/wire-protocol#experiment-17-per-file-fsync-as-the-many-files-tax).

Fix shape: when the server returns `Error` for a path-escape
reason during the initial Stat/List, the client should print a
clear stderr warning ("falling back to legacy SSH transport
because $remote rejected $path") and either continue with the
fallback (current behavior) or exit non-zero (opinionated;
breaks scripts that didn't realize they were inside a jail).

## xattr Cross-FS Edge Cases

Today's xattr preservation (see code under `commands/copy.rs`) is
best-effort: ENOTSUP and EPERM are both swallowed silently. That's
the right default for cross-FS copies, but we should track which
attributes were dropped and surface that under `-v` --- silent
dropping of `security.selinux` or Finder tags is a footgun for
power users.
