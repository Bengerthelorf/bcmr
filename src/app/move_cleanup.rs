use crate::core::move_file::LocalCleanupPlan;
use crate::core::remote::{parse_remote_path, RemoteCleanupPlan};
use anyhow::Result;
use std::path::PathBuf;

/// Freeze the eligible namespace before any fan-out target starts copying.
/// Both implementations remove selected entries without following links or
/// recursively rediscovering source contents after the copy has finished.
pub(crate) struct MoveCleanupPlan {
    local: LocalCleanupPlan,
    remote: RemoteCleanupPlan,
}

impl MoveCleanupPlan {
    pub(crate) async fn capture(sources: &[PathBuf], excludes: &[regex::Regex]) -> Result<Self> {
        let mut local_sources = Vec::new();
        let mut remote_sources = Vec::new();
        for source in sources {
            if let Some(remote) = parse_remote_path(&source.to_string_lossy()) {
                remote_sources.push(remote);
            } else {
                local_sources.push(source.clone());
            }
        }
        // Capture local paths before the first SSH command; a remote connection
        // may overlap with another process creating files in a local source.
        let local = LocalCleanupPlan::capture(&local_sources, excludes)?;
        let remote = RemoteCleanupPlan::capture(&remote_sources, excludes).await?;
        Ok(Self { local, remote })
    }

    pub(crate) async fn remove(&self) -> Result<()> {
        self.remote.remove().await?;
        self.local.remove().await?;
        Ok(())
    }
}
