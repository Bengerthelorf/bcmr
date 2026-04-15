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

## xattr Cross-FS Edge Cases

Today's xattr preservation (see code under `commands/copy.rs`) is
best-effort: ENOTSUP and EPERM are both swallowed silently. That's
the right default for cross-FS copies, but we should track which
attributes were dropped and surface that under `-v` --- silent
dropping of `security.selinux` or Finder tags is a footgun for
power users.
