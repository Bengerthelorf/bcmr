use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use parking_lot::Mutex;

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
        r.register(&p);
        assert_eq!(r.len(), 1);
        r.unregister(&p);
        assert_eq!(r.len(), 0);
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
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn drain_tolerates_missing_files() {
        let r = CleanupRegistry::new();
        r.register(Path::new("/tmp/this-does-not-exist-xyz"));
        r.drain_and_remove();
    }
}
