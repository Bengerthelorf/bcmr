use crate::cli::Commands;
use crate::core::checksum;
use crate::core::error::BcmrError;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;

use super::file_copy::{copy_file, CopyFileOptions};
use super::overwrite::check_overwrite;
use super::plan::{CopyPlan, PlanEntry};
use super::symlinks::{check_symlink_overwrite, create_symlink_replacing};

type OnNewFileFn = Arc<dyn Fn(&str, u64) + Send + Sync>;
type OnReflinkFn = Arc<dyn Fn() + Send + Sync>;

pub(super) struct ProgressCallback<F> {
    pub(super) callback: F,
    pub(super) on_new_file: OnNewFileFn,
    pub(super) on_reflink: OnReflinkFn,
}

impl<F: Clone> Clone for ProgressCallback<F> {
    fn clone(&self) -> Self {
        Self {
            callback: self.callback.clone(),
            on_new_file: Arc::clone(&self.on_new_file),
            on_reflink: Arc::clone(&self.on_reflink),
        }
    }
}

pub async fn execute_plan<F>(
    plan: &CopyPlan,
    cli: &Commands,
    progress_callback: F,
    on_new_file: impl Fn(&str, u64) + Send + Sync + 'static,
    on_reflink: impl Fn() + Send + Sync + 'static,
) -> std::result::Result<(), BcmrError>
where
    F: Fn(u64) + Send + Sync + Clone + 'static,
{
    #[cfg(not(unix))]
    if cli.is_sync() && !cli.is_dry_run() {
        return Err(BcmrError::InvalidInput(
            "--sync cannot guarantee durable local namespace publication on this platform; refusing before transfer".into(),
        ));
    }
    let test_mode = cli.get_test_mode();
    CopyFileOptions::from_cli(cli, test_mode.clone()).validate_reflink_compatibility()?;
    let callback = ProgressCallback {
        callback: progress_callback,
        on_new_file: Arc::new(on_new_file),
        on_reflink: Arc::new(on_reflink),
    };

    for entry in &plan.entries {
        if let PlanEntry::CreateDir { dst, .. } = entry {
            if cli.is_sync() {
                crate::core::io::create_dir_all_durable_async(dst).await?;
            } else {
                fs::create_dir_all(dst).await?;
            }
        }
    }

    use futures::stream::{self, StreamExt};

    let jobs = cli.local_jobs();
    let verbose = cli.is_verbose();

    for entry in &plan.entries {
        if let PlanEntry::Symlink {
            dst, target, kind, ..
        } = entry
        {
            check_symlink_overwrite(dst, *kind, cli)?;
            if let Some(parent) = dst.parent().filter(|parent| !parent.as_os_str().is_empty()) {
                if cli.is_sync() {
                    crate::core::io::create_dir_all_durable_async(parent).await?;
                } else {
                    fs::create_dir_all(parent).await?;
                }
            }
            create_symlink_replacing(
                dst,
                target,
                *kind,
                cli.is_force(),
                cli.is_sync(),
                &test_mode,
            )
            .await?;
            if verbose {
                eprintln!("'{}' -> '{}' (symlink)", target.display(), dst.display());
            }
        }
    }

    let file_entries: Vec<(&PathBuf, &PathBuf)> = plan
        .entries
        .iter()
        .filter_map(|e| match e {
            PlanEntry::CopyFile { src, dst } => Some((src, dst)),
            _ => None,
        })
        .collect();

    let stream = stream::iter(file_entries).map(|(src, dst)| {
        let cb = &callback;
        let opts = CopyFileOptions::from_cli(cli, test_mode.clone());
        async move {
            check_overwrite(dst, cli).await?;
            copy_file(src, dst, opts, cb).await?;
            if verbose {
                eprintln!("'{}' -> '{}'", src.display(), dst.display());
            }
            Ok::<(), BcmrError>(())
        }
    });

    let mut buf = stream.buffer_unordered(jobs);
    while let Some(res) = buf.next().await {
        res?;
    }

    if cli.is_preserve() {
        for entry in plan.entries.iter().rev() {
            if let PlanEntry::CreateDir { src, dst } = entry {
                #[cfg(unix)]
                let handle = if cli.is_sync() {
                    Some(fs::File::open(dst).await?.into_std().await)
                } else {
                    None
                };
                preserve_attributes(src, dst).await?;
                #[cfg(unix)]
                if let Some(handle) = handle {
                    tokio::task::spawn_blocking(move || {
                        crate::core::io::durable_sync_directory_handle_strict(&handle)
                    })
                    .await??;
                }
            }
        }
    }

    Ok(())
}

#[allow(dead_code)] // Compatibility entry point; CLI operations execute a captured plan.
pub async fn copy_path<F>(
    src: &Path,
    dst: &Path,
    cli: &Commands,
    excludes: &[regex::Regex],
    progress_callback: F,
    on_new_file: impl Fn(&str, u64) + Send + Sync + 'static,
    on_reflink: impl Fn() + Send + Sync + 'static,
) -> std::result::Result<(), BcmrError>
where
    F: Fn(u64) + Send + Sync + Clone + 'static,
{
    let no_deref = cli.is_no_deref() || matches!(cli, Commands::Move { .. });
    let plan = super::plan::plan_copy(
        &[src.to_path_buf()],
        dst,
        cli.is_recursive(),
        no_deref,
        excludes,
    )
    .await?;
    if cli.is_dry_run() {
        return super::plan::dry_run_plan(&plan, cli);
    }
    execute_plan(&plan, cli, progress_callback, on_new_file, on_reflink).await
}

pub(crate) async fn preserve_attributes(
    src: &Path,
    dst: &Path,
) -> std::result::Result<(), BcmrError> {
    let src_metadata = src.metadata()?;
    let permissions = src_metadata.permissions();
    tokio::fs::set_permissions(dst, permissions).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let atime = filetime::FileTime::from_unix_time(src_metadata.atime(), 0);
        let mtime = filetime::FileTime::from_unix_time(src_metadata.mtime(), 0);
        filetime::set_file_times(dst, atime, mtime)?;
    }

    #[cfg(windows)]
    {
        let atime = filetime::FileTime::from_last_access_time(&src_metadata);
        let mtime = filetime::FileTime::from_last_modification_time(&src_metadata);
        filetime::set_file_times(dst, atime, mtime)?;
    }

    #[cfg(unix)]
    super::file_copy::copy_xattrs(src, dst)?;

    Ok(())
}

pub(crate) async fn preserve_staging_attributes(
    src: &Path,
    dst: &Path,
) -> std::result::Result<Option<bool>, BcmrError> {
    #[cfg(not(windows))]
    {
        preserve_attributes(src, dst).await?;
        Ok(None)
    }

    #[cfg(windows)]
    {
        // A preserved READONLY bit makes a private stage undeletable on
        // Windows. Preserve fallible timestamps while it is still writable;
        // AtomicStaging applies READONLY only after precommit validation and
        // immediately before MoveFileExW.
        let src_metadata = src.metadata()?;
        let atime = filetime::FileTime::from_last_access_time(&src_metadata);
        let mtime = filetime::FileTime::from_last_modification_time(&src_metadata);
        filetime::set_file_times(dst, atime, mtime)?;
        Ok(Some(src_metadata.permissions().readonly()))
    }
}

pub(crate) async fn verify_copy(
    src: &Path,
    dst: &Path,
    inline_src_hash: Option<blake3::Hash>,
) -> std::result::Result<(), BcmrError> {
    let src_hash_str = if let Some(h) = inline_src_hash {
        h.to_hex().to_string()
    } else {
        let src_path = src.to_path_buf();
        tokio::task::spawn_blocking(move || checksum::calculate_hash(&src_path)).await??
    };

    let dst_path = dst.to_path_buf();
    let dst_hash_str =
        tokio::task::spawn_blocking(move || checksum::calculate_hash(&dst_path)).await??;

    if src_hash_str != dst_hash_str {
        let _ = fs::remove_file(dst).await;
        return Err(BcmrError::VerificationError(dst.to_path_buf()));
    }
    Ok(())
}
