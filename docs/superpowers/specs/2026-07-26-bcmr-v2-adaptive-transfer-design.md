# BCMR v2 Adaptive Transactional Transfer Design

Status: approved direction, detailed design under review

Date: 2026-07-26

Target: protocol compatibility may break; device and adverse-link compatibility may not

## 1. Decision

BCMR v2 uses an SSH-first, transport-independent transaction engine.

SSH is always able to carry the control plane and the complete data plane. Direct
TLS/TCP and QUIC are optional accelerators. They attach to the same transfer
transaction and share the same verified-chunk state. If an accelerator cannot be
reached, becomes slower, or disconnects, unacknowledged chunks return to the SSH
queue without restarting the transfer.

This is the best fit for the target environments because:

- Cloudflare-published TCP and SSH can be carried over WebSocket, so a direct port
  or UDP path cannot be assumed.
- Tailscale paths can begin relayed, upgrade to direct, or fall back to a relay.
- old kernels, weak CPUs, and small-memory servers must still work;
- high-bandwidth LANs should not remain limited by a single OpenSSH cipher stream;
- reliability and commit semantics must not change when the transport changes.

Protocol v1 compatibility is not a goal. A v1 peer gets a clear version error and
an upgrade/deploy instruction before any mutation. Legacy SCP/SFTP may be selected
explicitly, or used automatically only when no v2 mutation has started.

## 2. Non-negotiable invariants

1. **No final path is modified before commit.**
   Uploads, downloads, local copies, and striped transfers write a unique sibling
   staging object on the destination filesystem.
2. **Acknowledged work is idempotent.**
   Re-sending a chunk with the same transaction, file, offset, length, and hash
   either succeeds without changing the result or fails as a protocol violation.
3. **Verification precedes replacement.**
   The receiver verifies chunk hashes, lengths, the ordered file root, and the
   manifest root before the staging object replaces an existing destination.
4. **A failed transfer preserves the old destination.**
   This includes disconnects, cancellation, decompression errors, disk-full,
   hash mismatch, worker failure, and transport migration.
5. **A move deletes only material proven committed to every requested target.**
   Excluded, unsupported, or failed entries remain at the source.
6. **Untrusted paths never become host paths by string concatenation.**
   Wire paths are validated relative component sequences and are resolved beneath
   an already-open root directory.
7. **Memory is bounded by negotiated byte credits.**
   File size, peer concurrency, or compressed length cannot force unbounded
   allocation.
8. **Every optimization has a scalar and ordinary-I/O fallback.**
   SIMD, QUIC, `copy_file_range`, sparse extent discovery, reflink, and
   `io_uring` are runtime capabilities, never runtime requirements.

## 3. Required v1 safety gate

No v2 performance work is allowed to hide or inherit the current destructive
paths. The first implementation milestone fixes and locks down:

- resume falsely accepting a preallocated but incomplete destination;
- `move -f x x` deleting its only source and same-file aliases;
- remote move with exclusions deleting excluded source data;
- local `--verify` committing before it verifies;
- legacy, pipelined, and striped GET writing the final destination directly;
- striped PUT truncating the final remote destination directly;
- SSH target option injection;
- unvalidated server list entries escaping the download root;
- recursive symlink and special-file omissions followed by source deletion;
- remote overwrite semantics and post-mutation fallback to legacy;
- short reads corrupting checkpoint block boundaries;
- decompression length amplification;
- unsafe job IDs and deploy shell quoting;
- sparse auto mode being defeated by whole-file `fallocate`;
- a changing or shortened source being committed as a successful copy.

Each fix starts with a failing regression test and lands independently where
practical.

## 4. Layered architecture

### 4.1 Control session

The client starts one v2 server over SSH. This authenticated session remains alive
until commit or explicit abort and carries:

- capability and resource negotiation;
- root and path validation results;
- manifests and reconciliation;
- transaction/checkpoint state;
- transport rendezvous credentials;
- commit, abort, and diagnostic messages;
- the fallback data stream.

The control session is sufficient by itself. Accelerator failure therefore cannot
make a supported device unusable.

### 4.2 Transfer engine

The transfer engine has no SSH-, TCP-, or QUIC-specific logic. It operates on:

- transactions;
- directory and file manifests;
- content chunks and sparse extents;
- verified-range state;
- bounded work queues;
- staging and commit operations.

### 4.3 Data transports

`DataTransport` implementations expose ordered or unordered chunk delivery plus
byte credits:

- `SshStream`: mandatory, one persistent v2 stream;
- `DirectTlsTcp`: optional, TLS 1.3 with an ephemeral certificate fingerprint
  authenticated over SSH;
- `Quic`: optional, TLS-protected streams over UDP;
- `Legacy`: separate pre-transaction compatibility path, never a mid-transfer
  retry target.

QUIC is not a v2 launch requirement. It is added only after the transactional SSH
and direct-TCP engines pass the same fault suite and QUIC demonstrates a measured
benefit under loss or path migration.

## 5. Transaction state machine

```text
NEW
  -> PLANNED
  -> STAGING
  -> TRANSFERRING
  -> VERIFIED
  -> COMMITTING
  -> COMMITTED

Any pre-commit state -> PAUSED | ABORTED
```

`transfer_id` is a random 128-bit identifier. The receiver persists:

- the negotiated manifest root;
- per-file staging identity;
- verified chunk/range bitmap;
- file length and sparse extent plan;
- a monotonic checkpoint sequence;
- commit state.

The checkpoint journal is append-only with fixed-size records and checksums.
Periodic compaction replaces quadratic re-serialization of all historical block
hashes. A checkpoint only advertises data that can be recovered after a process
crash. When `--sync` is off, a fast received acknowledgment and a durable
checkpoint acknowledgment are distinct internally; reconnect trusts only the
durable or revalidated set.

File commit order is:

1. verify every required range and hole;
2. verify the ordered file root;
3. apply mode, timestamps, xattrs, and supported link metadata to staging;
4. optionally sync staging for `--sync`;
5. atomically replace/no-replace the final path according to overwrite policy;
6. optionally sync the parent directory for `--sync`;
7. persist committed state.

Directory transactions stage a sibling tree where the platform can atomically
rename it. Merge-into-existing-directory operations use a journaled per-entry
commit and rollback/preservation rules rather than claiming whole-tree atomicity.

## 6. Protocol model

Representative messages:

```text
HelloV2 {
  protocol,
  transports,
  codecs,
  integrity_modes,
  path_encoding,
  cpu_budget,
  memory_budget,
  max_frame,
  max_streams,
  fs_capabilities
}

BeginTransfer { transfer_id, direction, overwrite, durability, policy }
TreeRoot { root_hash, entry_count }
ManifestPage { page, entries, page_hash }
ReconcileSymbols { scheme, sequence, symbols }
NeedEntries { ids_or_ranges }
FilePlan { file_id, path_components, kind, metadata, content_root, extents }
ChunkPlan { file_id, chunk_id, offset, raw_len, content_hash }
NeedChunks { file_id, bitmap_or_ranges }
DataChunk { file_id, chunk_id, attempt, offset, raw_len, codec, hash, payload }
Hole { file_id, offset, length }
ChunkAck { file_id, chunk_id, level }
Checkpoint { sequence, verified_ranges }
CommitFile { file_id, content_root }
CommitTransfer { manifest_root }
ResumeState { checkpoint_sequence, verified_ranges }
Abort { reason }
```

Incoming frame size, declared raw size, codec window, collection counts, nesting,
and cumulative manifest bytes are all capped before allocation.

Unix paths are transported as byte components. Windows paths use an explicitly
tagged platform encoding. Absolute paths, prefixes, `.`/`..`, empty components,
separator injection, NUL, and duplicate conflicting entries are rejected.

## 7. Integrity model

The default is `--integrity=transfer`:

- the sender hashes each content chunk while reading it;
- the receiver hashes the same bytes while writing them;
- a chunk is accepted only if length and BLAKE3 match;
- an ordered, domain-separated Merkle root binds chunk index, offset, length,
  sparse extents, and content hash;
- the directory root binds path bytes, type, selected metadata, and file root.

This detects in-flight corruption without a second full read on either endpoint.
`--integrity=storage` additionally re-reads the committed destination after sync
for media/controller verification. `--integrity=none` is never the default and
does not disable authenticated transport.

Content integrity and AEAD are separate:

- chunk hashes provide retry, dedup, resume, and end-to-end content identity;
- TLS/QUIC/SSH provide peer authentication, confidentiality, and frame integrity.

## 8. Content chunking: portfolio, not one universal default

Content chunks are stable identity units. Transport frames are link-specific and
may split or aggregate content chunks. Changing frame size or transport therefore
does not invalidate CAS entries or resume state.

### 8.1 Fixed chunks

Fixed chunks remain the cheapest choice for:

- a new destination with no useful history;
- high-bandwidth links where scanning for delta costs more than transmission;
- exact repeat uploads backed by an existing fixed-chunk CAS;
- memory- or CPU-constrained peers.

Candidate averages: 1 MiB and 4 MiB. The winner is measured, not assumed.

### 8.2 FastCDC

FastCDC is the portable scalar baseline for edit-heavy files where insertions or
deletions would shift fixed boundaries. Initial candidates:

- minimum 256 KiB;
- average 1 MiB;
- maximum 4 MiB.

These are experiment parameters, not final constants.

Chunk identity profiles are stable and discrete rather than continuously retuned
to the current network. Initial profile averages are 256 KiB, 1 MiB, and 4 MiB.
An existing file or manifest keeps its profile unless an explicit rechunk is
profitable enough to pay for losing historical reuse.

The following cost model is an experimental profile selector:

```text
T(c) = H * S / c + q * c / B + T_hash(c)
c*   ~= sqrt(H * S * B / q)
```

`H` is per-chunk metadata/protocol cost, `S` is file size, `q` estimates
independent changed regions, and `B` is expected link goodput. The continuous
answer is mapped to a stable profile with hysteresis. Faster links may therefore
select a larger identity profile for new data, but ordinary link variation only
changes transport framing and concurrency. This bcmr-specific policy is a
falsifiable hypothesis and is compared with fixed profile selection.

### 8.3 SeqCDC and VectorCDC

Recent SeqCDC/VectorCDC work reports substantially higher chunking throughput on
supported SIMD hardware. They are experimental optional engines with:

- a scalar implementation or FastCDC fallback;
- runtime SSE/AVX/NEON detection;
- identical correctness and chunk-distribution tests across implementations;
- no `target-cpu=native` requirement in release binaries.

They can become defaults only if bcmr-scale chunk sizes and representative files
preserve dedup effectiveness while materially reducing end-to-end time.

AE/AE-Max is retained as an additional research baseline because recent broad CDC
comparisons identify it, alongside normalized Gear-style chunking, as a strong
candidate. Rabin remains a historical quality baseline rather than the expected
throughput default.

### 8.4 Proposed: hierarchical content reconciliation

This is a bcmr-specific hypothesis, not a measured result.

A file can first be represented by large content-defined superchunks. Matching
superchunks are reused immediately. Only unmatched superchunks are rescanned or
expanded into smaller content chunks. This can reduce hashing, manifest, and index
work when versions are highly similar, while avoiding a permanently tiny chunk
size.

It loses when most content changed because unmatched areas are processed twice.
It therefore remains behind a similarity/cost gate and is ablated against
single-level FastCDC.

### 8.5 Sparse extents

Holes are structural manifest entries, not zero-filled chunks. On filesystems
supporting `SEEK_DATA`/`SEEK_HOLE`, only data extents enter the chunker. Unsupported
or unreliable filesystems fall back to bounded zero detection. Whole-file
preallocation is disabled for sparse auto/always modes.

## 9. Manifest and chunk reconciliation

### 9.1 Exact paginated baseline

The correctness baseline is a paginated, hash-chained exact manifest. It works for
any overlap and has predictable resource bounds.

### 9.2 Persistent two-level Merkle/Prolly index

Both endpoints may cache:

- a deterministic Prolly-style path tree for a transferred directory;
- ordered content-chunk trees for files;
- file identity, size, nanosecond timestamps, and platform generation metadata;
- storage capability and algorithm version.

The path tree sorts byte-preserving path components and uses content-defined node
boundaries. Inserting one entry therefore does not force every later page to be
regrouped, as an ordinal page tree would. Equal roots skip entire subtrees;
different roots descend only into changed branches. File content roots use stable
chunk profiles independently of directory node boundaries.

External modification invalidates the cache or forces revalidation. This follows
the same broad principle as reusing storage-layer checksum metadata; cached
metadata is an optimization and never the sole proof of new bytes.

### 9.3 Experimental rateless set reconciliation

Rateless IBLT is a candidate when both endpoints already hold very large,
high-overlap sets of file or chunk identifiers and the difference size is unknown.
It can make reconciliation communication scale with the difference instead of the
full set.

It is not the correctness format:

- decoding is resource-capped;
- decoded identifiers are verified against exact hashes;
- failure or excessive symbol cost falls back to the exact paginated manifest;
- ordered file layout is still authenticated by the Merkle manifest;
- multiset/duplicate-chunk semantics are explicitly tested.

It ships only if it reduces total wall time or wire metadata on realistic trees,
not merely synthetic set microbenchmarks.

### 9.4 CAS pack and exact index

The existing one-file-per-chunk layout is the correctness baseline but not the
large-scale design. V2 evaluates immutable 64–256 MiB pack segments with:

- per-record and whole-pack checksums;
- a recoverable footer and exact `hash -> pack, offset, length` index;
- a small mutable recent-object map before a pack is sealed;
- crash-safe background compaction;
- no synchronous access-time update on every hit.

A Binary Fuse filter may reject absent hashes before the exact lookup. Its answer
can only avoid an exact negative lookup; a positive result is always checked
against the exact index. Thus a probabilistic-filter error can cause extra work
but can never suppress required data.

Eviction ablates byte LRU, SIEVE, S3-FIFO, GDSF, and an experimental
`TransferCost-SIEVE`. The latter weights an object by estimated network time
saved plus disk/hash recomputation cost, subject to object size, instead of raw
hit count alone.

### 9.5 Experimental similarity delta

CDC reuses identical chunks but sends a full chunk after a small in-chunk edit.
For large mutable images, checkpoints, and database snapshots, v2 may try a
second-stage delta:

1. use the old object at the same destination path as the default candidate;
2. optionally use Finesse-style super-features to shortlist similar chunks;
3. obtain bounded weak and strong block signatures from the receiver;
4. emit authenticated COPY/LITERAL instructions in a VCDIFF-compatible or
   equivalently self-describing form;
5. verify the reconstructed content chunk against its normal BLAKE3 identity.

The similarity index never proves equality. Delta is enabled only when predicted
scan, signature, CPU, and wire time beats direct chunk transmission by a guarded
margin. It is expected to lose on compressed, encrypted, random, and most media
files and remains an opt-in experiment until the ablation says otherwise.

## 10. Proposed one-pass windowed dedup

The current implementation reads a source once to build all hashes and again to
send missing blocks. V2 pipelines bounded windows:

1. read a window bounded by the memory budget and estimated BDP;
2. identify and hash content chunks;
3. send the window manifest;
4. while the server resolves its missing bitmap, prepare a later window;
5. send only missing payload from retained buffers;
6. release buffers after acknowledged or durably checkpointed.

Candidate window:

```text
clamp(max(32 MiB, 2 * estimated_BDP), 8 MiB, negotiated_memory_share)
```

Multiple manifest windows may be in flight, subject to byte credits. Cold CAS,
warm CAS, partial CAS, low-memory, and high-RTT cases are separate ablations.

## 11. Compression

### 11.1 Cost model

`auto` no longer means “prefer Zstd whenever supported.” A small bounded sample
from several positions, initially 64–256 KiB total, estimates codec ratio and
local encode rate. Negotiated calibration supplies receiver decode rate. The
controller compares predicted pipeline bottlenecks:

```text
raw_goodput   = network_goodput
codec_goodput = min(encode_rate, network_goodput / ratio, decode_rate / ratio)
```

Compression activates only with a minimum predicted margin, initially 10–15%,
plus hysteresis. Actual ratios and rates update the estimate, and distribution
changes trigger a bounded re-sample. File extensions are priors, not proof.

Candidates:

- raw;
- LZ4;
- Zstd negative/fast levels;
- Zstd level 1;
- Zstd level 3.

Contexts and buffers are reused in a bounded CPU pool. The decoder enforces raw
length and maximum window before allocation.

### 11.2 Proposed microfile packs

For many small files, v2 may build bounded, restartable packs whose manifest keeps
individual path, metadata, offset, length, and hash records. Deterministic grouping
by content class or extension allows one compression context to exploit cross-file
redundancy and reduces framing/syscall overhead.

Candidate pack sizes are 4, 8, and 16 MiB. Packs are independently retriable and
never weaken per-file verification. Group-by-extension, path order, content sketch,
and no-pack baselines are compared. This remains experimental until it wins on
source trees without harming random or already-compressed trees.

### 11.3 Restart granularity

Stable links can use larger independent compression groups for ratio. Unstable
links use smaller groups so reconnects redo less work. Content chunks and hashes
do not change when compression grouping changes.

## 12. Scheduling and parallel processing

### 12.1 Bounded pipeline

```text
scan -> read -> chunk/hash -> compress -> encrypt -> send
     -> receive -> decrypt -> decompress -> pwrite -> verify -> acknowledge
```

Reusable buffers move between stages. CPU work runs in a bounded worker pool;
blocking filesystem work does not execute on async reactor threads.

### 12.2 Size- and speed-aware three-lane work stealing

Round-robin by file count is replaced with estimated finish time:

```text
worker_cost = queued_bytes / EWMA_worker_goodput + failure_penalty
```

Large files become independently schedulable chunk ranges. Idle workers steal
unassigned chunks. Work is classified into three borrowable lanes:

- control/metadata, which must never wait behind a large payload;
- micro-pack, which amortizes small-file framing while retaining per-file state;
- bulk, which preserves sequential disk locality for large files and extents.

The scheduler starts from longest-processing-time-first assignment by predicted
byte/CPU cost, then corrects estimates from EWMA worker goodput. A single large
file is striped only when storage, CPU, and network telemetry all show usable
headroom.

Baselines:

- round-robin;
- file-size LPT;
- shortest-remaining-processing-time;
- dynamic work stealing;
- dual-lane work stealing;
- three-lane LPT plus work stealing.

### 12.3 Online concurrency governor

Start with one data stream. During bounded observation windows:

- increase streams if marginal goodput improves enough and CPU, memory, disk queue,
  retransmissions, and tail latency stay healthy;
- reduce streams on timeout, retransmission, queue inflation, memory pressure, or
  negligible marginal gain;
- cap by peer-advertised resources and user policy.

A simple guarded hill-climber is preferred to an opaque learned policy. Its
decisions and inputs are logged in benchmark JSON.

This controller changes application concurrency only. TCP and QUIC retain their
standard transport congestion and loss recovery; application FEC over an already
reliable stream is not a default because it can duplicate recovery and worsen
congestion.

### 12.4 Proposed tail rescue

When one chunk is a statistically clear straggler and another path is idle, the
engine may issue a duplicate attempt. The first verified result wins and the other
is canceled. Idempotent chunks make this safe, but bandwidth overhead can be
harmful. Tail rescue is off by default until loss/jitter ablations justify it.

## 13. Transport selection and migration

`--transport=auto`:

1. establish SSH control and begin v2 planning;
2. probe direct TLS/TCP and QUIC within a short, bounded budget;
3. begin sending over SSH rather than waiting indefinitely;
4. move future chunks to a faster healthy accelerator;
5. return unacknowledged chunks to another transport on failure;
6. keep SSH alive through commit.

Suggested environment behavior:

| Environment | Initial policy |
|---|---|
| Cloudflare published TCP/SSH or ProxyJump | SSH data, conservative streams, heartbeat |
| Tailscale DERP/peer relay | measured low concurrency, adaptive compression, aggressive resume |
| Tailscale/direct LAN | race direct TCP and QUIC, then follow measured goodput |
| UDP blocked | fail QUIC quickly, continue TCP/SSH |
| high RTT/loss with UDP available | bounded QUIC streams, BDP-sized credits |
| old CPU/low memory | one stream, raw/LZ4, scalar chunker, small windows |
| old kernel/filesystem | ordinary read/write, no mandatory sparse/reflink/uring feature |

Frame payload candidates are 64 KiB, 256 KiB, 1 MiB, and 4 MiB. Flow-control
credit must cover the measured bandwidth-delay product without exceeding memory.
Tailscale's documented 1280-byte MTU and QUIC's UDP requirements mean path MTU
must be conservative and transport-managed rather than inferred from filesystem
chunk sizes.

## 14. Device compatibility

Release artifacts continue to include:

- static musl Linux x86_64 and aarch64;
- macOS x86_64 and arm64;
- Windows x86_64 and arm64;
- FreeBSD x86_64 where dependencies support it.

Generic binaries use runtime CPU feature detection. Mandatory protocol code avoids
new-kernel-only syscalls. Feature probes are cached per session, and unsupported
operations fall back without changing correctness.

Memory negotiation is explicit. The server may advertise a small budget and one
CPU worker; the client must honor it. No benchmark or default should cause swap on
a small server merely to chase peak throughput.

## 15. CLI direction

```text
--transport auto|quic|tcp|ssh|legacy
--transport-strict
--parallel auto|N
--compress adaptive|none|lz4|zstd-fast|zstd-1|zstd-3
--dedup adaptive|off|fixed|cdc
--integrity transfer|storage
--memory-limit SIZE
--bwlimit RATE
```

Invalid zero concurrency and unknown codec/mode values fail during argument or
configuration parsing.

## 16. Ablation and comparison plan

### 16.1 Reproducibility

Every run records:

- exact command and environment;
- git commit and dirty diff hash;
- binary checksum and build profile;
- OS, kernel, filesystem, CPU, memory, and storage device;
- transport path and relevant proxy/overlay state;
- dataset manifest and checksum;
- raw per-run JSON.

Run order is randomized. Report median, p95, and bootstrap 95% confidence
intervals. Use at least 10 paired repetitions per profile and continue when the
predeclared confidence-width target is not met. Never select best-of-three.

Cold-cache, warm-cache, cold-CAS, warm-CAS, and partially-warm-CAS results are
separate.

### 16.2 Baselines

- current pinned v1 main;
- safety-patched v1;
- v2 raw, one SSH stream, no dedup;
- `scp`/SFTP;
- `rsync -a` for delta workloads;
- local `cp` and platform-native copy;
- optional representative modern tools when installed and version-pinned.

### 16.3 Workloads

- 10,000 files at 4 KiB and 64 KiB;
- Zipf-distributed mixed trees;
- 1, 16, and 100 GiB large files where resources allow;
- text/source, database dump, VM/checkpoint, media/archive, random, and mixed;
- 90% and 99% sparse files;
- exact repeat;
- append/prepend;
- a 64 KiB middle insertion;
- 1% scattered edits;
- directory rename, delete, and metadata-only changes;
- symlink, hardlink, dangling link, xattr, nanosecond timestamp, and special-file
  correctness corpus.

### 16.4 Network matrix

- 5, 20, and 100 Mbps, plus 1, 10, and 25 Gbps where hardware permits;
- RTT 1, 30, 100, and 250 ms;
- independent and correlated/burst loss at 0, 0.1, 1, 3, and 5%;
- jitter up to 250 +/- 50 ms, reordering, duplication, and asymmetric bandwidth;
- complete path loss for randomized 30–120 second intervals, followed by recovery;
- Tailscale direct, peer relay, and DERP;
- Cloudflare Client-to-Tunnel/published SSH;
- ProxyJump/SSH-only.

Synthetic impairment runs inside an isolated Linux namespace or dedicated remote
test host. The current Mac network configuration is not modified for a benchmark.

Device profiles include one generic core with 128, 256, and 512 MiB negotiated
memory limits; SIMD-disabled portable builds; old-kernel ordinary read/write; and
modern multicore/SIMD hosts. Every functional profile runs with the same protocol
semantics even when its fast paths are unavailable.

### 16.5 Single-factor ablations

1. SSH vs direct TCP vs QUIC;
2. streams 1/2/4/8;
3. frame 64 KiB/256 KiB/1 MiB/4 MiB;
4. in-flight credit 0.5/1/2/4 times measured BDP;
5. raw/LZ4/Zstd-fast/Zstd-1/Zstd-3/adaptive;
6. fixed/FastCDC/AE/SeqCDC/VectorCDC/hierarchical/stable-cost-profile;
7. exact manifest/Merkle-Prolly descent/Rateless IBLT;
8. dedup off/current two-pass/windowed one-pass;
9. loose CAS/pack+exact/pack+Binary Fuse;
10. CAS cold/warm/partial and LRU/SIEVE/S3-FIFO/GDSF/TransferCost-SIEVE;
11. round-robin/LPT/work stealing/dual lane/three lane;
12. delta off/same-path signature/Finesse candidate;
13. current hashing/inline chunk tree/storage re-read;
14. zero scan/extent map/wire holes;
15. no micro-pack/path-order pack/type pack/content-sketch pack;
16. tail rescue off/on.

Only the largest single-factor effects enter a small factorial interaction study.
This prevents an all-at-once “optimized” result with no causal attribution.

### 16.6 Metrics

- end-to-end wall time and goodput;
- time to first completed file and p95 file latency;
- CPU time by endpoint and stage;
- peak RSS and buffer-pool high-water mark;
- source/destination physical bytes read and written;
- wire payload and metadata bytes;
- syscalls/context switches where available;
- retransmitted/duplicate chunks and reconnect recovery bytes;
- compression ratio and codec throughput;
- chunking throughput, chunk count/distribution, and dedup byte hit rate;
- CAS object/byte hit rate and GC cost;
- commit latency and checkpoint amplification.

Every performance run performs an independent final content check. Any correctness
failure invalidates the performance result.

### 16.7 Initial promotion gates

An optimization is not a default unless it:

- improves end-to-end throughput by at least 10%, or reduces a constrained resource
  by at least 20%;
- does not worsen p95 latency by more than 5% outside its declared target workload;
- does not increase failure recovery bytes or lose old-device support;
- passes all fault, security, and cross-platform correctness tests.

The thresholds may be revised before experiments, never after seeing results.

## 17. Fault and security campaign

Inject failures:

- disconnect before first chunk, mid-chunk, after chunk ACK, before checkpoint,
  during commit, and after rename before commit record;
- kill client/server and reboot-style recovery at every transaction state;
- one striped worker stalls, corrupts, duplicates, or reorders a chunk;
- disk full, permission change, read-only remount, and short/changing source;
- decompression error and hostile declared lengths/windows;
- symlink swap at every path component;
- malicious absolute/parent/duplicate manifest paths;
- replayed transaction/chunk attempts;
- stale or poisoned CAS objects;
- truncated pack/index recovery and probabilistic-filter false positives;
- delta COPY outside the declared base object or reconstructed length;
- legacy fallback offered after mutation;
- direct/QUIC accelerator loss while SSH remains alive.

Success means either a committed, independently verified result or a clear error
with the old destination and all uncommitted source material intact.

The adverse-link recovery gate is 100 successful completions out of 100 after
connectivity returns. A reconnect may waste no more than one checkpoint window
per active stream. Probabilistic structures may cause extra transfer but never
missing transfer, and peak RSS may not exceed the negotiated budget.

## 18. Storage constraint for development and benchmarks

All build artifacts, temporary files, generated datasets, CAS data, raw results,
and benchmark destinations must reside on an explicitly validated external volume.

Before running, benchmark tooling must require:

```text
BCMR_BENCH_ROOT
BCMR_REMOTE_BENCH_ROOT
CARGO_TARGET_DIR
TMPDIR
XDG_CACHE_HOME
XDG_DATA_HOME
XDG_CONFIG_HOME
BCMR_CAS_DIR
```

It rejects `/`, `/tmp`, `/private/tmp`, the user's home, the workspace root, and
the macOS internal data volume. On macOS it verifies the selected local benchmark
volume with `diskutil`. Memory budgets are capped and swap usage is monitored so a
large external-disk benchmark does not indirectly create large internal-disk swap.

Existing benchmark scripts that hard-code `/tmp` must be fixed before use.

## 19. Implementation sequence

1. Land P0 regression tests and safety fixes.
2. Remove redundant hash/read passes and quadratic session checkpoints.
3. Introduce v2 transaction, exact manifests, staging, resume, and SSH data.
4. Add persistent Merkle/Prolly indexes and windowed one-pass dedup.
5. Add direct TLS/TCP with seamless return to SSH.
6. Add adaptive compression, lazy concurrency, and size-aware scheduling.
7. Run the external-volume ablation suite.
8. Add QUIC only if loss/path experiments justify it.
9. Evaluate pack CAS, FastCDC, AE, SeqCDC/VectorCDC, hierarchical reconciliation,
   Rateless IBLT, microfile packs, similarity delta, and alternative CAS policies
   independently.

No historical benchmark number is treated as current evidence. The existing
ablation documents are useful hypotheses, but several describe code paths that
have since changed and do not retain complete raw artifacts.

## 20. Primary references

- [RFC 9000: QUIC](https://www.rfc-editor.org/rfc/rfc9000.html)
- [Cloudflare Tunnel routing](https://developers.cloudflare.com/tunnel/routing/)
- [Tailscale connection types](https://tailscale.com/docs/reference/connection-types)
- [FastCDC, USENIX ATC 2016](https://www.usenix.org/conference/atc16/technical-sessions/presentation/xia)
- [CDC algorithm comparison, 2024](https://arxiv.org/abs/2409.06066)
- [VectorCDC/SeqCDC project and papers](https://wasl.uwaterloo.ca/projects/deduplication/)
- [Dolt Prolly Tree architecture](https://www.dolthub.com/docs/architecture/storage-engine/prolly-tree/)
- [Practical Rateless Set Reconciliation, SIGCOMM 2024](https://doi.org/10.1145/3651890.3672219)
- [Binary Fuse Filters](https://arxiv.org/abs/2201.01174)
- [Finesse, FAST 2019](https://www.usenix.org/conference/fast19/presentation/zhang)
- [RFC 3284: VCDIFF](https://www.rfc-editor.org/rfc/rfc3284)
- [SkySync, FAST 2026](https://www.usenix.org/conference/fast26/presentation/zhang-zhihao)
- [BLAKE3 and Bao verified streaming](https://github.com/BLAKE3-team/BLAKE3)
- [Zstandard API manual](https://facebook.github.io/zstd/doc/api_manual_latest.html)
- [`copy_file_range(2)`](https://man7.org/linux/man-pages/man2/copy_file_range.2.html)
- [`lseek(2)` sparse extents](https://man7.org/linux/man-pages/man2/lseek.2.html)
- [SIEVE, NSDI 2024](https://www.usenix.org/conference/nsdi24/presentation/zhang-yazhuo)
- [S3-FIFO, SOSP 2023](https://www.pdl.cmu.edu/PDL-FTP/Storage/FIFOqueues-SOSP23_abs.shtml)
