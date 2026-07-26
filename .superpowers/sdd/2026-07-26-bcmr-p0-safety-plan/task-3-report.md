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
