//! Transport layer for `bcmr serve` connections.
//!
//! Currently the only transport is `ssh` — spawning a remote `bcmr
//! serve` via an SSH subprocess and talking to it over stdin/stdout.
//! The module exists to stake out the seam where alternative
//! transports will slot in (see `docs/ablation/path-b-design.md` on
//! the `path-b/direct-tcp` branch for the rendezvous-to-direct-TCP
//! plan).
//!
//! Phase 1 scope (current): extract the SSH spawn call out of
//! `ServeClient` so `ServeClient` doesn't import `tokio::process`
//! directly. No trait abstraction yet — that comes when there's an
//! actual second transport to dispatch to. Introducing a trait with
//! only one implementation is premature structure; introducing it at
//! the same time as the second impl is when the shape is real.

pub mod ssh;
