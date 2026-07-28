# BCMR P0 Safety Implementation Plan

Date: 2026-07-26

Design source:
`docs/superpowers/specs/2026-07-26-bcmr-v2-adaptive-transfer-design.md`

## Global Constraints

- Protocol compatibility may break, but old CPUs, old kernels, ordinary
  filesystems, low-memory servers, SSH-only paths, Cloudflare relays, Tailscale
  relays, and adverse links must retain a correct fallback.
- No final path may be modified before verification and commit.
- A failed operation must preserve the old destination and all uncommitted source
  material.
- Move deletes only material proven committed to every requested target.
- Incoming paths are byte-preserving relative components resolved beneath an
  already-open root; absolute paths, `.`/`..`, prefixes, separator injection, NUL,
  and conflicting duplicates are rejected.
- Peer-controlled counts, lengths, codec windows, and aggregate bytes are bounded
  before allocation.
- Every production behavior change follows RED, GREEN, REFACTOR. Focused tests use
  real filesystem or protocol behavior rather than mocks.
- Each task is a separate commit and is independently reviewed before the next
  task begins.
- All builds, temporary files, test data, caches, CAS data, and reports remain on
  validated external volumes. Use:

  ```text
  CARGO_HOME=/Volumes/KIOXIA/Developments/cargo-home
  CARGO_TARGET_DIR=/Volumes/KIOXIA/Developments/bcmr/p0-safety/target
  TMPDIR=/Volumes/KIOXIA/Developments/bcmr/p0-safety/tmp
  XDG_CACHE_HOME=/Volumes/KIOXIA/Developments/bcmr/p0-safety/xdg-cache
  XDG_DATA_HOME=/Volumes/KIOXIA/Developments/bcmr/p0-safety/xdg-data
  XDG_CONFIG_HOME=/Volumes/KIOXIA/Developments/bcmr/p0-safety/xdg-config
  BCMR_CAS_DIR=/Volumes/KIOXIA/Developments/bcmr/p0-safety/cas
  ```

- Do not touch the dirty `main` checkout or its untracked benchmark fixtures.

## Task 1: Prevent Same-Object File Move Data Loss

Add real e2e regressions in `tests/e2e_move_tests.rs` proving that each of the
following returns an error without deleting or modifying data:

- `bcmr move -f -y FILE FILE`;
- moving `FILE` into its own parent directory, which resolves to the same target;
- on platforms supporting hard links, moving `FILE` with force onto another hard
  link to the same inode;
- on platforms supporting symlinks, moving a symlink or file onto an alias that
  resolves to the same underlying object.

The test must first fail against the current implementation for the destructive
same-path case. Fix at the source by resolving the effective destination and
checking same-object identity before any overwrite removal. The check must be
cross-platform and must not convert a missing destination into an error.

Acceptance:

- every alias remains readable with the original bytes after refusal;
- no overwrite removal occurs before the identity guard;
- ordinary forced overwrite of a genuinely different file still passes;
- focused move tests and the full suite pass.

## Task 2: Reject Zero Concurrency and Unsafe Job IDs

Add CLI/config tests proving:

- `copy -j0`, remote `copy -P0`, and equivalent long forms fail during parsing;
- configured `scp.parallel_transfers = 0` fails validation rather than entering a
  semaphore or unordered-buffer hang;
- job IDs containing a separator, parent component, absolute path, NUL, or an
  empty string cannot be used to read or remove files outside the jobs directory.

Positive integer values and generated job IDs keep working. Validation belongs at
the input/config boundary, not as scattered `.max(1)` symptom suppression.

## Task 3: Bound Decompression Before Allocation

Add protocol/codec tests using tiny encoded frames with hostile declared
`original_size` values. Decoding must reject:

- a raw size greater than the negotiated or protocol maximum;
- a raw size inconsistent with an uncompressed payload;
- a decompressed result whose actual length differs from the declaration.

No test may allocate the hostile declared size. Validate the integer and codec
window before allocation, use checked conversions, and retain normal LZ4/Zstd/raw
round trips.

## Task 4: Verify Local Copies Before Atomic Replacement

Add regressions proving that a forced verified copy with injected corruption or a
hash mismatch leaves the pre-existing destination byte-for-byte intact and removes
only its unique staging object. Two concurrent copies to the same destination must
not share the same temporary path.

Refactor local finalization so it:

1. writes a unique sibling staging file on the destination filesystem;
2. verifies staging against the source before replacement;
3. applies requested metadata and durability to staging;
4. atomically replaces or no-replaces according to policy;
5. never deletes a pre-existing destination on verification failure.

Windows replacement semantics must have an explicit implementation or a clearly
tested safe fallback.

## Task 5: Make Resume Proof-Based, Not Length-Based

Add focused unit/e2e tests for a session-backed destination whose file length
equals the source because of preallocation but whose unverified tail is zero or
corrupt. Resume must not return `AlreadyComplete`; it must resume from the last
verified block or restart safely.

Remove whole-file preallocation from resume/sparse-auto paths. Persist or
revalidate only block ranges that were actually written and hashed. A checkpoint
acknowledgment must never cover bytes that a crash can lose.

## Task 6: Handle Short Reads and Exact Checkpoint Boundaries

Add deterministic tests that feed the streaming copier reads crossing a 4 MiB
checkpoint boundary at non-aligned sizes, such as 2 MiB followed by 4 MiB. Stored
block hashes must match independently computed hashes for exact block ranges.

Add a changing/truncated-source regression. If `copy_file_range` or streaming I/O
returns EOF while expected bytes remain, the copy must fail before commit and
preserve the old destination. One blocking task should own the kernel-copy loop
rather than spawning once per chunk when refactoring is needed to make this safe.

## Task 7: Reject SSH Option Injection and Download Path Escapes

Add parser/command tests proving remote targets that would become SSH options are
rejected before process launch. Cover a host beginning with `-`, injected option
tokens, and valid IPv4/IPv6/user hosts.

Add serve and legacy download tests with malicious `ListEntry` values: absolute
paths, parent components, mixed separators, empty/conflicting components, and
non-UTF-8 Unix names where supported. No joined path may escape the selected local
root. Fix the validation at the wire-path conversion boundary and reuse it in all
download modes.

## Task 8: Stage Every Remote GET and PUT

Add failure-injection tests for legacy, pipelined, pooled, and striped GET/PUT
paths. Disconnect, worker error, short payload, decompression failure, or hash
mismatch must preserve the old final path.

All modes write a transaction-unique sibling staging object, validate expected
length and integrity, then atomically commit. Striped workers write disjoint
offsets in the same staging object; no worker opens the final path with truncate.
Remote force/no-force behavior must be consistent across fast and legacy paths.

## Task 9: Make Remote Fallback Pre-Mutation Only

Add tests proving an operational error after any remote mutation does not trigger
legacy replay. Fallback decisions use typed capability/version outcomes rather than
matching arbitrary error strings. Legacy may be selected only before the
transaction creates or truncates remote state.

## Task 10: Preserve Excluded and Unsupported Source Entries During Move

Add local and remote move regressions with exclusions, symlinks, dangling links,
hard links, and special files supported by the platform. A source entry may be
deleted only after the matching destination entry is committed and independently
accounted for.

Replace whole-source recursive deletion with a committed-entry deletion journal.
Excluded, skipped, unsupported, failed, or unverified entries remain at source;
parent directories are removed only when empty.

## Task 11: Full P0 Verification and Review

Run, with the external-volume environment:

```text
cargo fmt --all -- --check
cargo test --locked --features test-support
cargo clippy --locked --all-targets --features test-support -- -D warnings
```

Run the destructive reproductions against fresh external-volume fixtures and
verify filesystem state after each command. Review the complete branch against the
global constraints and the P0 list in the design. Do not start v2 performance work
until this gate is clean or every residual issue is explicitly recorded as a
blocker.
