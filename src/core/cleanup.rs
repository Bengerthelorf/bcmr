use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use parking_lot::Mutex;

fn remove_temp_file(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if !metadata.file_type().is_symlink() {
            let mut permissions = metadata.permissions();
            if permissions.readonly() {
                // This branch is Windows-only. On Windows, set_readonly(false)
                // clears FILE_ATTRIBUTE_READONLY rather than widening Unix
                // mode bits as the cross-platform Clippy lint warns about.
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                let _ = std::fs::set_permissions(path, permissions);
            }
        }

        // DeleteFileW refuses directory symlinks. The raw directory attribute
        // describes the link entry itself and lets cleanup choose RemoveDirectoryW
        // without following the link target.
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
        if metadata.file_type().is_symlink()
            && metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0
        {
            return std::fs::remove_dir(path);
        }
    }
    std::fs::remove_file(path)
}

pub fn cleanup_partial_files() {
    global().drain_and_remove();
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

    pub fn disarm(&mut self) {
        self.active = false;
        self.registry.unregister(&self.path);
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = remove_temp_file(&self.path);
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
            let _ = remove_temp_file(&path);
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

    #[cfg(windows)]
    #[test]
    fn drain_removes_readonly_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("readonly-stage");
        std::fs::write(&path, b"private stage").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();

        let registry = CleanupRegistry::new();
        registry.register(&path);
        registry.drain_and_remove();

        assert!(!path.exists());
        assert!(registry.is_empty());
    }
}
