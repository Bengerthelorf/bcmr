# Contributing

## Build & test

```
cargo build
cargo build --features test-support --tests
cargo test --features test-support
```

`test-support` gates loopback spawners used only by integration tests and benches (`ServeClient::connect_local`, `connect_direct_local`, etc.). Without the feature these symbols don't compile — that's intentional, they have no production use.

## Lint & format

CI runs both:

```
cargo clippy -- -D warnings
cargo clippy --all-targets --features test-support -- -D warnings
cargo fmt --check
```

The `--all-targets` run also lints test and bench code. Both must be green.

## Comments

Add comments only when a competent reader would form a wrong mental model without them. Delete otherwise. This is strict — the codebase has been swept multiple times. Common anti-patterns:

- `//` describing what the next line already says
- File-level `//!` that restates the filename
- `///` above self-named items (`pub const AUTH_HELLO_TAG`, `pub fn connect`)
- "mirrors X on the server" pointers — grep suffices
- Performance numbers that rot

High-value exceptions: cryptographic invariants, magic errno cross-platform values, hand-rolled wire byte layouts, ordering constraints the type system can't express.

## PR flow

1. Branch from `main`
2. Push and open PR via `gh pr create`
3. Wait for both OS runners (ubuntu + windows) to go green
4. Squash-merge with the PR auto-delete flag

## Running benches and fuzz targets

Benches use the `test-support` feature via `cargo run --example` or `cargo bench`. The `fuzz/` directory requires nightly + `cargo-fuzz`; see `fuzz/README.md`.
