# Fuzz targets

Targets for `cargo-fuzz`. **Requires nightly** because `libFuzzer` uses
sanitizer coverage instrumentation. CI runs stable only; this workspace
is an opt-in local tool.

## Setup

    rustup toolchain install nightly
    cargo install cargo-fuzz

## Run

    cargo +nightly fuzz run decode_message

A corpus is populated under `fuzz/corpus/decode_message/`; crashes land
in `fuzz/artifacts/decode_message/`.

## Invariants under test

- `decode_message`: arbitrary bytes must never panic or UB. Return
  `None` for malformed frames; `Some(Message)` is optional.

The property-based version (`tests/proptest_codec.rs`) runs on stable
and covers the same "no panic" invariant at lower depth but in-tree.
