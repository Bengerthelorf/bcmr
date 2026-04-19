use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};
use std::time::SystemTime;

static CACHED_ROOT: OnceLock<PathBuf> = OnceLock::new();
static ROOT_OVERRIDE: RwLock<Option<PathBuf>> = RwLock::new(None);

pub fn cas_root() -> PathBuf {
    if let Some(p) = ROOT_OVERRIDE.read().unwrap().as_ref() {
        return p.clone();
    }
    CACHED_ROOT.get_or_init(compute_root).clone()
}

fn compute_root() -> PathBuf {
    if let Ok(custom) = std::env::var("BCMR_CAS_DIR") {
        return PathBuf::from(custom);
    }
    directories::BaseDirs::new()
        .map(|d| d.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("bcmr")
        .join("cas")
}

#[cfg(test)]
pub fn set_root_override(p: Option<PathBuf>) {
    *ROOT_OVERRIDE.write().unwrap() = p;
}

fn path_for(hash: &[u8; 32]) -> PathBuf {
    let hex = blake3::Hash::from_bytes(*hash).to_hex();
    cas_root()
        .join(&hex[0..2])
        .join(&hex[2..4])
        .join(format!("{}.blk", &hex[4..]))
}

pub fn has(hash: &[u8; 32]) -> bool {
    path_for(hash).exists()
}

pub fn read(hash: &[u8; 32]) -> io::Result<Vec<u8>> {
    let p = path_for(hash);
    let data = std::fs::read(&p)?;
    let now = filetime::FileTime::from_system_time(SystemTime::now());
    let _ = filetime::set_file_mtime(&p, now);
    Ok(data)
}

pub fn write(hash: &[u8; 32], data: &[u8]) -> io::Result<()> {
    let dst = path_for(hash);
    if dst.exists() {
        let now = filetime::FileTime::from_system_time(SystemTime::now());
        let _ = filetime::set_file_mtime(&dst, now);
        return Ok(());
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = unique_tmp_path(&dst);
    std::fs::write(&tmp, data)?;
    match std::fs::rename(&tmp, &dst) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            if dst.exists() {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

fn unique_tmp_path(dst: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    dst.with_extension(format!("blk.tmp.{}.{}", std::process::id(), n))
}

pub fn cap_bytes() -> Option<u64> {
    let raw = std::env::var("BCMR_CAS_CAP_MB").ok();
    let mb: u64 = match raw.as_deref().and_then(|s| s.parse().ok()) {
        Some(0) => return None,
        Some(n) => n,
        None => 1024,
    };
    Some(mb * 1024 * 1024)
}

pub fn evict_to_cap(cap: u64) -> io::Result<u64> {
    let root = cas_root();
    if !root.exists() {
        return Ok(0);
    }

    let mut entries: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    let mut total: u64 = 0;
    walk_blobs(&root, &mut |path, size, mtime| {
        total += size;
        entries.push((mtime, size, path));
    })?;

    if total <= cap {
        return Ok(0);
    }

    entries.sort_by_key(|(m, _, _)| *m);

    let mut freed: u64 = 0;
    for (_, size, path) in entries {
        if total - freed <= cap {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            freed += size;
        }
    }
    Ok(freed)
}

fn walk_blobs<F>(dir: &std::path::Path, sink: &mut F) -> io::Result<()>
where
    F: FnMut(PathBuf, u64, SystemTime),
{
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in read.flatten() {
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk_blobs(&entry.path(), sink)?;
        } else if ft.is_file() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("blk") {
                continue;
            }
            let md = entry.metadata()?;
            let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            sink(path, md.len(), mtime);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_hash() -> [u8; 32] {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let pid = std::process::id() as u128;
        let mut data = Vec::with_capacity(40);
        data.extend_from_slice(&now.to_le_bytes());
        data.extend_from_slice(&pid.to_le_bytes());
        data.extend_from_slice(&n.to_le_bytes());
        let h = blake3::hash(&data);
        let mut out = [0u8; 32];
        out.copy_from_slice(h.as_bytes());
        out
    }

    #[test]
    fn write_then_read_roundtrip() {
        let _g = lock_cas();
        let _tmp = isolated_cas();
        let h = unique_hash();
        let payload = b"the quick brown fox".repeat(100);
        write(&h, &payload).unwrap();
        assert!(has(&h));
        assert_eq!(read(&h).unwrap(), payload);
        clear_root();
    }

    #[test]
    fn double_write_is_idempotent() {
        let _g = lock_cas();
        let _tmp = isolated_cas();
        let h = unique_hash();
        write(&h, b"hello").unwrap();
        write(&h, b"hello").unwrap();
        clear_root();
    }

    #[test]
    fn missing_hash_returns_false() {
        let _g = lock_cas();
        let _tmp = isolated_cas();
        let h = [0xab; 32];
        assert!(!has(&h));
        clear_root();
    }

    fn isolated_cas() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        set_root_override(Some(tmp.path().to_path_buf()));
        tmp
    }

    static CAS_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_cas() -> std::sync::MutexGuard<'static, ()> {
        CAS_DIR_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn clear_root() {
        set_root_override(None);
    }

    #[test]
    fn evict_drops_oldest_first_until_under_cap() {
        let _g = lock_cas();
        let _tmp = isolated_cas();

        let h_old = unique_hash();
        let h_mid = unique_hash();
        let h_new = unique_hash();
        let payload = vec![0xa1u8; 1024];
        write(&h_old, &payload).unwrap();
        write(&h_mid, &payload).unwrap();
        write(&h_new, &payload).unwrap();

        let now = std::time::SystemTime::now();
        let t_old = filetime::FileTime::from_system_time(now - std::time::Duration::from_secs(300));
        let t_mid = filetime::FileTime::from_system_time(now - std::time::Duration::from_secs(100));
        let t_new = filetime::FileTime::from_system_time(now);
        filetime::set_file_mtime(path_for(&h_old), t_old).unwrap();
        filetime::set_file_mtime(path_for(&h_mid), t_mid).unwrap();
        filetime::set_file_mtime(path_for(&h_new), t_new).unwrap();

        let freed = evict_to_cap(1536).unwrap();
        assert!(freed >= 2 * 1024, "freed={}", freed);
        assert!(!has(&h_old));
        assert!(!has(&h_mid));
        assert!(has(&h_new), "newest blob should survive eviction");

        clear_root();
    }

    #[test]
    fn evict_under_cap_is_noop() {
        let _g = lock_cas();
        let _tmp = isolated_cas();

        let h = unique_hash();
        write(&h, b"tiny").unwrap();
        let freed = evict_to_cap(u64::MAX).unwrap();
        assert_eq!(freed, 0);
        assert!(has(&h));

        clear_root();
    }

    #[test]
    fn cap_env_zero_disables() {
        let _g = lock_cas();
        let prev = std::env::var("BCMR_CAS_CAP_MB").ok();
        std::env::set_var("BCMR_CAS_CAP_MB", "0");
        assert!(cap_bytes().is_none());
        std::env::set_var("BCMR_CAS_CAP_MB", "10");
        assert_eq!(cap_bytes(), Some(10 * 1024 * 1024));
        match prev {
            Some(v) => std::env::set_var("BCMR_CAS_CAP_MB", v),
            None => std::env::remove_var("BCMR_CAS_CAP_MB"),
        }
    }

    #[test]
    fn concurrent_put_same_hash_all_succeed() {
        let _g = lock_cas();
        let _tmp = isolated_cas();

        let h = unique_hash();
        let payload = vec![0x7cu8; 8 * 1024];
        let payload = std::sync::Arc::new(payload);

        let mut handles = Vec::new();
        for _ in 0..8 {
            let p = payload.clone();
            handles.push(std::thread::spawn(move || write(&h, &p)));
        }
        for h in handles {
            h.join().unwrap().unwrap();
        }
        assert!(has(&h));
        assert_eq!(&*read(&h).unwrap(), &*payload);

        let parent = path_for(&h).parent().unwrap().to_path_buf();
        let orphans: Vec<_> = std::fs::read_dir(&parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                name.to_string_lossy().contains("blk.tmp")
            })
            .collect();
        assert!(
            orphans.is_empty(),
            "orphan tmp files left behind: {:?}",
            orphans
        );

        clear_root();
    }

    #[test]
    fn read_touches_mtime_for_lru() {
        let _g = lock_cas();
        let _tmp = isolated_cas();

        let h_a = unique_hash();
        let h_b = unique_hash();
        let payload = vec![0xa1u8; 1024];
        write(&h_a, &payload).unwrap();
        write(&h_b, &payload).unwrap();

        let old = filetime::FileTime::from_system_time(
            std::time::SystemTime::now() - std::time::Duration::from_secs(300),
        );
        filetime::set_file_mtime(path_for(&h_a), old).unwrap();
        filetime::set_file_mtime(path_for(&h_b), old).unwrap();

        let _ = read(&h_a).unwrap();

        let _ = evict_to_cap(1024).unwrap();
        assert!(has(&h_a), "recently-read blob should win the LRU");
        assert!(!has(&h_b));

        clear_root();
    }
}
