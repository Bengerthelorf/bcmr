# Open Questions

These are investigations that surfaced during the v0.5.7 / v0.5.8
work but were intentionally deferred. Each needs design before
shipping; the notes here record the shape we have in mind so the
follow-up doesn't start from scratch.

## Zero-Copy Serve GET via `splice(2)`

`splice(2)` from the source fd into a pipe and then into stdout
would bypass two userspace memcpys per 4 MiB block on Linux. The
blocker is that `splice` doesn't expose the bytes to userspace, so
the inline BLAKE3 we use to populate the `Ok { hash }` response
can't run.

**Likely shape**: a `CAP_FAST` cap that trades server-side hashing
for splice. Negotiated like the other caps. When active:

- Server uses splice for the file → pipe → stdout path.
- Server's `Ok` carries `hash: None` instead of the BLAKE3.
- Clients that asked for `-V` re-hash the dst on the receiving side
  (which they were already doing for `--verify` semantics), so
  integrity isn't lost --- it just moves to the client.

Estimated win: dominant on $\geq$ 10 Gbps LANs where the userspace
memcpy is the actual bottleneck; modest on the more common WAN
case where network throughput is well below memcpy rate.

## io_uring Read Path on Linux

Each `read.await` in `streaming_copy` trips a syscall; batching via
`io_uring` would help when sequential throughput is at the syscall
ceiling rather than the device.

**Decision needed first**: `tokio-uring` vs raw `io-uring` --- the
former still gates everything through tokio's reactor and may not
unlock the win, while the latter requires running outside tokio for
the read loop and reattaching futures around it.

Estimated win: single-digit-percent on NVMe sustained reads; bigger
when combined with the existing `--jobs` since each worker would
get its own ring.

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
