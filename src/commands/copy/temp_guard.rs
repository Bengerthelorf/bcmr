use crate::core::cleanup::{self, CleanupRegistry};
use std::path::PathBuf;

pub fn cleanup_partial_files() {
    cleanup::global().drain_and_remove();
}

pub(crate) struct TempFileGuard {
    registry: &'static CleanupRegistry,
    path: PathBuf,
    active: bool,
}

impl TempFileGuard {
    pub(crate) fn new(path: PathBuf) -> Self {
        let registry = cleanup::global();
        registry.register(&path);
        Self {
            registry,
            path,
            active: true,
        }
    }

    pub(crate) fn disarm(&mut self) {
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
