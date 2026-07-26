# Task 3 Report: Bound Decompression Before Allocation

## Implementation

- Defined `MAX_CONTENT_BLOCK_SIZE` as the protocol's 4 MiB uncompressed content
  block limit. This is intentionally separate from the existing 16 MiB encoded
  frame limit.
- `decode_block` now converts `original_size` with `usize::try_from`, rejects a
  size above the content limit (and invalid empty compressed blocks) before
  invoking LZ4 or Zstd, rejects `algo=None`, and requires the decoded length to
  equal the declaration.
- Added `decode_data_block(Message)` so raw and compressed data frames share one
  content-block validation boundary. All production data-frame consumers now use
  that helper.

## TDD Evidence

- RED: `cargo test --locked --features test-support --test serve_protocol_tests data_block_rejects_hostile_declared_original_size_before_decompression -- --exact`
  failed safely at compilation because `decode_data_block` and
  `MAX_CONTENT_BLOCK_SIZE` did not yet exist. The hostile frame contains only a
  one-byte payload and declares `u32::MAX`, so no test allocated the declared
  size or called the vulnerable decompressor.
- GREEN: focused hostile-size, oversized-raw, `algo=None`, LZ4 length-mismatch,
  and Zstd length-mismatch tests all passed after the minimal implementation.

## Verification

- `cargo fmt --all -- --check` passed.
- `git diff --check` passed.
- `cargo test --locked --features test-support --test serve_protocol_tests --quiet`: 38 passed.
- `cargo test --locked --features test-support --test proptest_codec --quiet`: 4 passed.
- `cargo test --locked --features test-support --test e2e_serve_basic --quiet`: 22 passed.
- `cargo test --all --locked --features test-support --quiet` passed (all listed
  unit, integration, property, and end-to-end groups).

All commands used the required external KIOXIA Cargo, target, temporary,
XDG-cache/data/config, and BCMR CAS locations.

## Self-review and concerns

- `rg` confirms production call sites no longer invoke `decode_block` directly;
  `decode_data_block` is the single raw/compressed message-to-bytes boundary.
- The wire `Data` variant has no separate declared original-size field; it is
  therefore checked against the same 4 MiB content limit. A `DataCompressed`
  frame with `algo=None` is rejected rather than being treated as raw bytes.
- The pre-existing 16 MiB frame limit still bounds wire-frame allocation before
  protocol parsing. This task adds the stricter 4 MiB bound specifically before
  decompressor allocation/window use. No remaining Task 3 concern identified.

## Review Follow-up (2026-07-26)

### Additional implementation

- Moved the 4 MiB content-block maximum and validation into `protocol`, so
  plain framing, codec parsing, compression, and AEAD share one boundary.
- Plain `read_message` now reads the type and the raw `Data` inner length first;
  it rejects an empty frame, malformed raw header, oversized raw length, or a
  mismatched outer/inner length before allocating the raw payload. Raw `Data`
  is returned directly rather than being re-framed and cloned.
- Codec parsing independently validates raw content length and compressed
  algorithm/decompressed size before cloning data. Thus AEAD may retain its
  authenticated, type-oblivious 16 MiB ciphertext cap, but post-auth decoding
  cannot clone an oversized raw `Data` Vec. AEAD output validates data messages
  before encryption as well.
- `encode_block` is now fallible, rejects blocks above 4 MiB before compression,
  and uses `u32::try_from` for the protocol declaration. All six producers now
  propagate that result.
- Added a shared checked transfer-total helper and migrated the reviewed
  server/client/pipelined accounting and write paths, eliminating unchecked
  `written + incoming` / `received + incoming` arithmetic.

### Follow-up TDD evidence

- RED 1: focused protocol test initially failed because
  `protocol::checked_transfer_total` did not exist.
- RED 2: a 9-byte plain raw-frame header declaring 4 MiB + 1 returned
  `UnexpectedEof` instead of `InvalidData`, proving the old reader tried to
  read the declared payload before rejection.
- RED 3: an empty declared frame returned `UnexpectedEof` instead of
  `InvalidData`, proving it read a type byte before checking the outer length.
- GREEN: raw wire, empty-frame, exact-boundary/overflow accounting, encoder
  bounds, zero/unknown algorithms, and AEAD outbound-bound regressions passed.

### Follow-up verification

- `cargo fmt --all -- --check` passed.
- `git diff --check` passed.
- Protocol 42/42, property codec 4/4, basic serve 22/22, and pipelined serve
  8/8 passed.
- `cargo test --all --locked --features test-support --quiet` passed after the
  follow-up (98 library tests, 212 binary tests, and all integration groups).

### Follow-up self-review

- `rg` confirms every `encode_block` producer now handles its `Result`; no
  reviewed aggregate `written + incoming` or `received + incoming` expression
  remains.
- Encrypted records necessarily allocate ciphertext up to the existing 16 MiB
  record cap before authentication reveals their type. The shared codec check
  prevents a second oversized raw payload clone after authentication; this is
  the deliberate finite authenticated-frame strategy requested by review.
