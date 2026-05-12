use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use once_cell::sync::Lazy;
use parking_lot::Mutex;

pub fn cleanup_partial_files() {
    global().drain_and_remove();
}

// pid + atomic counter so concurrent writers (CAS, serve PUT staging)
// can't collide on the same temp path.
pub fn unique_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}.{}", std::process::id(), n)
}

pub struct TempFileGuard {
    registry: &'static CleanupRegistry,
    path: PathBuf,
    active: bool,
}

impl TempFileGuard {
    pub fn new(path: PathBuf) -> Self {
        let registry = global();
        registry.register(&path);
        Self {
            registry,
            path,
            active: true,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn disarm(&mut self) {
        self.active = false;
        self.registry.unregister(&self.path);
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = std::fs::remove_file(&self.path);
            self.registry.unregister(&self.path);
        }
    }
}

pub struct CleanupRegistry {
    paths: Mutex<Vec<PathBuf>>,
}

impl CleanupRegistry {
    pub const fn new() -> Self {
        Self {
            paths: Mutex::new(Vec::new()),
        }
    }

    pub fn register(&self, path: &Path) {
        self.paths.lock().push(path.to_path_buf());
    }

    pub fn unregister(&self, path: &Path) {
        self.paths.lock().retain(|p| p != path);
    }

    pub fn drain_and_remove(&self) {
        let drained: Vec<PathBuf> = self.paths.lock().drain(..).collect();
        for path in drained {
            let _ = std::fs::remove_file(&path);
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.paths.lock().len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.paths.lock().is_empty()
    }
}

impl Default for CleanupRegistry {
    fn default() -> Self {
        Self::new()
    }
}

static GLOBAL: Lazy<CleanupRegistry> = Lazy::new(CleanupRegistry::new);

pub fn global() -> &'static CleanupRegistry {
    &GLOBAL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_unregister_roundtrip() {
        let r = CleanupRegistry::new();
        let p = PathBuf::from("/tmp/does-not-matter-42");
        assert!(r.is_empty());
        r.register(&p);
        assert_eq!(r.len(), 1);
        r.unregister(&p);
        assert!(r.is_empty());
    }

    #[test]
    fn drain_removes_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"hi").unwrap();
        std::fs::write(&b, b"yo").unwrap();
        let r = CleanupRegistry::new();
        r.register(&a);
        r.register(&b);
        r.drain_and_remove();
        assert!(!a.exists());
        assert!(!b.exists());
        assert!(r.is_empty());
    }

    #[test]
    fn drain_tolerates_missing_files() {
        let r = CleanupRegistry::new();
        r.register(Path::new("/tmp/this-does-not-exist-xyz"));
        r.drain_and_remove();
    }
}
