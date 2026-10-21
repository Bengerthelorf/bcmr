use crate::core::move_file::LocalCleanupPlan;
use crate::core::remote::{backend, parse_remote_path, RemoteCleanupPlan, SftpCleanupPlan};
use anyhow::Result;
use std::path::PathBuf;

/// Freeze the eligible namespace before any fan-out target starts copying.
/// Both implementations remove selected entries without following links or
/// recursively rediscovering source contents after the copy has finished.
pub(crate) struct MoveCleanupPlan {
    local: LocalCleanupPlan,
    remote: RemoteCleanupPlan,
    sftp: SftpCleanupPlan,
}

impl MoveCleanupPlan {
    pub(crate) async fn capture(sources: &[PathBuf], excludes: &[regex::Regex]) -> Result<Self> {
        crate::core::interrupt::check()?;
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
        let mut ssh_sources = Vec::new();
        let mut sftp_sources = Vec::new();
        for remote in remote_sources {
            if backend::is_sftp(&remote.ssh_target()).await? {
                sftp_sources.push(remote);
            } else {
                ssh_sources.push(remote);
            }
        }
        let remote = RemoteCleanupPlan::capture(&ssh_sources, excludes).await?;
        let sftp = SftpCleanupPlan::capture(&sftp_sources, excludes).await?;
        Ok(Self {
            local,
            remote,
            sftp,
        })
    }

    pub(crate) async fn remove(&self) -> Result<()> {
        // Validate both remote manifests before either transport removes a leaf.
        crate::core::interrupt::read_only(self.remote.validate()).await?;
        crate::core::interrupt::read_only(self.sftp.validate()).await?;
        crate::core::interrupt::check()?;
        self.sftp.remove().await?;
        crate::core::interrupt::check()?;
        self.remote.remove().await?;
        crate::core::interrupt::check()?;
        self.local.remove().await?;
        crate::core::interrupt::check()?;
        Ok(())
    }
}
