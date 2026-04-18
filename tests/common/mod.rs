//! Shared helpers for the e2e_serve_* integration test files.
//!
//! Each integration test is its own binary, so `common/mod.rs` gets
//! compiled per-file — helpers unused by a particular file still
//! appear "unused" to that file's compilation. `#![allow(dead_code)]`
//! keeps the warning out of `cargo test` output.
#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::path::Path;

/// Write deterministic pseudo-random bytes to `path`.
/// Uses a simple LCG so the data is never all-zeros, making hash
/// collisions detectable.
pub fn create_file(path: &Path, size: usize) {
    let mut file = fs::File::create(path).unwrap();
    let mut state: u64 = 0xdeadbeef_cafebabe;
    let mut buf = Vec::with_capacity(size.min(65536));
    let mut remaining = size;
    while remaining > 0 {
        let chunk = remaining.min(65536);
        buf.clear();
        for _ in 0..chunk {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            buf.push((state >> 33) as u8);
        }
        file.write_all(&buf).unwrap();
        remaining -= chunk;
    }
    file.flush().unwrap();
}

pub fn bytes_to_hex(hash: &[u8; 32]) -> String {
    hash.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Serialise the CAS-eviction test against any concurrent PUTs on
/// the shared `BCMR_CAS_CAP_MB` env var — unrelated tests running in
/// the same process would otherwise see capped-or-uncapped behaviour
/// inherited at spawn time.
pub fn cas_test_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}
