//! Content-addressed store for the dedup wire path.
//!
//! Stores 4 MiB blocks at `~/.local/share/bcmr/cas/<aa>/<bb>/<rest>.blk`
//! where `aabb...` is the lowercase hex of the block's BLAKE3 hash.
//! The two-level prefix keeps any single directory from accumulating
//! more than ~65k entries on the workloads we care about.
//!
//! No eviction logic — the store grows monotonically. Callers needing to
//! cap disk usage should `rm -rf` the directory periodically. A future
//! revision should add an LRU bound, but that's a separate design.

use std::io;
use std::path::PathBuf;

pub fn cas_root() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("bcmr")
        .join("cas")
}

fn hex32(h: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in h {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn path_for(hash: &[u8; 32]) -> PathBuf {
    let hex = hex32(hash);
    cas_root()
        .join(&hex[0..2])
        .join(&hex[2..4])
        .join(format!("{}.blk", &hex[4..]))
}

pub fn has(hash: &[u8; 32]) -> bool {
    path_for(hash).exists()
}

pub fn read(hash: &[u8; 32]) -> io::Result<Vec<u8>> {
    std::fs::read(path_for(hash))
}

/// Atomically write `data` to the CAS at the slot derived from `hash`.
/// No-op when the slot already exists. The hash itself is not verified
/// here — callers are expected to have just computed it from `data`.
pub fn write(hash: &[u8; 32], data: &[u8]) -> io::Result<()> {
    let dst = path_for(hash);
    if dst.exists() {
        return Ok(());
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = dst.with_extension("blk.tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, &dst)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_hash() -> [u8; 32] {
        // Random nonce so concurrent test runs don't collide in the
        // shared CAS directory.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let pid = std::process::id() as u128;
        let mut data = Vec::with_capacity(32);
        data.extend_from_slice(&now.to_le_bytes());
        data.extend_from_slice(&pid.to_le_bytes());
        let h = blake3::hash(&data);
        let mut out = [0u8; 32];
        out.copy_from_slice(h.as_bytes());
        out
    }

    #[test]
    fn write_then_read_roundtrip() {
        let h = unique_hash();
        let payload = b"the quick brown fox".repeat(100);
        write(&h, &payload).unwrap();
        assert!(has(&h));
        assert_eq!(read(&h).unwrap(), payload);

        // Cleanup so the test doesn't leak into local dev CAS.
        let _ = std::fs::remove_file(path_for(&h));
    }

    #[test]
    fn double_write_is_idempotent() {
        let h = unique_hash();
        write(&h, b"hello").unwrap();
        write(&h, b"hello").unwrap();
        let _ = std::fs::remove_file(path_for(&h));
    }

    #[test]
    fn missing_hash_returns_false() {
        let h = [0xab; 32];
        // Only true if no other run wrote this exact constant — the path
        // is namespaced under "ab/ab/..." so collisions are rare.
        let _ = std::fs::remove_file(path_for(&h));
        assert!(!has(&h));
    }
}
