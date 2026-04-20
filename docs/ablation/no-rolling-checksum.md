# Non-Goal: Rolling-Checksum Delta-Sync

rsync's signature feature is rolling-checksum byte-precise delta-sync:
Adler-32 as a weak sliding-window hash, MD5 as the strong match, and a
sender-side rolling window that finds the byte ranges already present
on the receiver. bcmr does not implement this and will not. The
decision rests on three pieces, which matter together — cherry-picking
any one leaves the argument incomplete.

## 1. The 1996 trade-off has flipped

rsync's algorithm was designed for 28.8 Kbps dial-up, where
transmitting 1 MB less *mattered* against computing 100 MB of hashes.
At that ratio, the CPU was effectively free.

On a 100 Mbps+ link (every deployment bcmr targets) the ratio
inverts. Even SIMD-optimized Adler-32 on the receiver, plus MD5 on
matches, plus the sender-side rolling window, approaches the wall
time of just shipping the bytes over the wire. On 1 Gbps LAN the
delta wins shrink to marginal for most workloads — and for the
specific workload where they don't (very large files with small
in-place edits), the next point applies.

## 2. The byte-precise-delta workloads already moved elsewhere

The use cases that genuinely need byte-level delta didn't stay on
rsync — they migrated to tools built around **content-defined
chunking** (CDC), which solves the same problem more cheaply on
modern hardware:

| Workload                           | Tool of choice             | Why not rsync           |
|------------------------------------|----------------------------|-------------------------|
| Incremental filesystem backup      | restic / borg / kopia      | CDC + dedup, not rolling |
| Container image distribution       | OCI registry (layer dedup) | Layer-level, not byte-level |
| Cloud object storage incrementals  | S3 multipart + ETag        | Offset-bounded, not rolling |

The residual rsync-delta-dependent workloads are mostly (a)
unmigrated legacy backup scripts, (b) the minority of VM-image
distribution that hasn't switched to OCI. Neither is the demographic
bcmr is competing for. See also: our own
[`docs/guide/remote-copy.md`](/guide/remote-copy) already points
users to rsync for these cases as reader triage.

## 3. The engineering cost is not small, and the ceiling is low

A conservative sketch of what implementing it entails:

- New wire protocol message types (signature block, match result,
  literal span), versioned on both sides
- Receiver: compute block hashes for the existing destination file,
  hash-table keyed on weak-hash for candidate lookup
- Sender: rolling-window computation (performance-sensitive;
  off-by-one here silently corrupts in ways tests miss)
- Sender-side reconstruction of the transmission plan (match spans
  + literal spans)
- Receiver-side reconstruction with correct fsync ordering
- Edge cases: sparse files, files smaller than the block size,
  boundary alignment, concurrent modification, crash safety across
  the reconstruction write path

Conservative: 2–4 weeks of focused engineering, plus a permanent
maintenance tax. After all of it, bcmr still would not be an rsync
replacement — `--link-dest`, `--delete`, `--files-from`, ACLs, BSD
flags, and the hardlink-graph preservation in `rsync -a` are each
their own separate engineering effort, none cheaper than this one.

## What bcmr ships instead

bcmr ships [content-addressed 4 MiB block dedup](/ablation/wire-protocol#content-addressed-dedup)
for repeat uploads. It's a different point in the design space —
whole-block matches, no sliding window, no rolling hash — and it
happens to be cheap enough in 2026 to turn on unconditionally for
files ≥ 16 MiB. It pays off exactly where users would otherwise
reach for rsync: re-uploading the same artifact (container image,
dataset snapshot, built binary) to the same server.

## Reopen-this-decision triggers

This is a closed question today. It would be worth revisiting if:

- Link speeds drop back below modem-era levels in a way bcmr should
  care about (unlikely)
- A workload emerges that genuinely needs byte-precise delta and
  can't be served by CDC-based tools or OCI layer dedup (unlikely)
- rsync's implementation becomes unmaintained upstream and a
  community asks bcmr to become the migration target (possible, but
  that conversation is a long way from starting)

Until one of those: pointing users at rsync for delta-sync is the
correct answer.
