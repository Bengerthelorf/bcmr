use crate::cli::Commands;
use crate::core::checksum;
use crate::core::error::BcmrError;
use crate::core::traversal;
use crate::ui::display::{print_dry_run, ActionType};

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;

use super::file_copy::{copy_file, CopyFileOptions};
use super::overwrite::{check_overwrite, determine_dry_run_action, is_normal_write};
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
    let test_mode = cli.get_test_mode();
    let callback = ProgressCallback {
        callback: progress_callback,
        on_new_file: Arc::new(on_new_file),
        on_reflink: Arc::new(on_reflink),
    };

    for entry in &plan.entries {
        if let PlanEntry::CreateDir { dst, .. } = entry {
            if !dst.exists() {
                fs::create_dir_all(dst).await?;
            }
        }
    }

    use futures::stream::{self, StreamExt};

    let jobs = cli.local_jobs();
    let verbose = cli.is_verbose();

    for entry in &plan.entries {
        if let PlanEntry::Symlink { dst, target, .. } = entry {
            check_symlink_overwrite(dst, cli)?;
            create_symlink_replacing(dst, target).await?;
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
                preserve_attributes(src, dst).await?;
            }
        }
    }

    Ok(())
}

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
    let test_mode = cli.get_test_mode();
    let callback = ProgressCallback {
        callback: progress_callback,
        on_new_file: Arc::new(on_new_file),
        on_reflink: Arc::new(on_reflink),
    };

    if traversal::is_excluded(src, excludes) {
        return Ok(());
    }

    if src.is_file() {
        let dst_path = if dst.is_dir() {
            dst.join(
                src.file_name()
                    .ok_or_else(BcmrError::invalid_source_file_name)?,
            )
        } else {
            dst.to_path_buf()
        };

        if dst_path.exists() && !cli.is_force() && is_normal_write(cli) {
            return Err(BcmrError::TargetExists(dst_path));
        }

        if cli.is_dry_run() {
            let action = determine_dry_run_action(src, &dst_path, cli)?;
            print_dry_run(
                action,
                &src.to_string_lossy(),
                Some(&dst_path.to_string_lossy()),
            );
            return Ok(());
        }

        if dst_path.exists() && cli.is_force() && !is_normal_write(cli) {
            fs::remove_file(&dst_path).await?;
        }

        copy_file(
            src,
            &dst_path,
            CopyFileOptions::from_cli(cli, test_mode),
            &callback,
        )
        .await?;

        if cli.is_verbose() {
            eprintln!("'{}' -> '{}'", src.display(), dst_path.display());
        }
    } else if cli.is_recursive() && src.is_dir() {
        let src_dir_name = src
            .file_name()
            .ok_or_else(BcmrError::invalid_source_dir_name)?;
        let new_dst = if dst.is_dir() {
            dst.join(src_dir_name)
        } else {
            dst.to_path_buf()
        };

        if cli.is_dry_run() && !new_dst.exists() {
            print_dry_run(
                ActionType::Add,
                &src.to_string_lossy(),
                Some(&format!("(DIR) -> {}", new_dst.display())),
            );
        }

        if !new_dst.exists() && !cli.is_dry_run() {
            fs::create_dir_all(&new_dst).await?;
        }

        let mut files_to_copy = Vec::new();
        let mut dir_pairs: Vec<(PathBuf, PathBuf)> = Vec::new();
        for entry in traversal::walk(src, true, false, 1, excludes) {
            let entry = entry?;
            let path = entry.path();

            let relative_path = path.strip_prefix(src)?;
            let target_path = new_dst.join(relative_path);

            if path.is_dir() {
                if !cli.is_dry_run() {
                    if !target_path.exists() {
                        fs::create_dir_all(&target_path).await?;
                    }
                    dir_pairs.push((path.to_path_buf(), target_path));
                } else if !target_path.exists() {
                    print_dry_run(
                        ActionType::Add,
                        &path.to_string_lossy(),
                        Some(&format!("(DIR) -> {}", target_path.display())),
                    );
                }
            } else if path.is_file() {
                files_to_copy.push((path.to_path_buf(), target_path));
            }
        }

        for (src_path, dst_path) in files_to_copy {
            if let Some(parent) = dst_path.parent() {
                if !parent.exists() && !cli.is_dry_run() {
                    fs::create_dir_all(parent).await?;
                }
            }

            if dst_path.exists() && !cli.is_force() && is_normal_write(cli) {
                return Err(BcmrError::TargetExists(dst_path));
            }

            if cli.is_dry_run() {
                let action = determine_dry_run_action(&src_path, &dst_path, cli)?;
                print_dry_run(
                    action,
                    &src_path.to_string_lossy(),
                    Some(&dst_path.to_string_lossy()),
                );
            } else {
                if dst_path.exists() && cli.is_force() && !is_normal_write(cli) {
                    fs::remove_file(&dst_path).await?;
                }

                copy_file(
                    &src_path,
                    &dst_path,
                    CopyFileOptions::from_cli(cli, test_mode.clone()),
                    &callback,
                )
                .await?;

                if cli.is_verbose() {
                    eprintln!("'{}' -> '{}'", src_path.display(), dst_path.display());
                }
            }
        }

        if cli.is_preserve() && !cli.is_dry_run() {
            for (src_dir, dst_dir) in dir_pairs.iter().rev() {
                preserve_attributes(src_dir, dst_dir).await?;
            }
            preserve_attributes(src, &new_dst).await?;
        }
    } else if src.is_dir() {
        return Err(BcmrError::InvalidInput(format!(
            "Source '{}' is a directory. Use -r flag for recursive copy.",
            src.display()
        )));
    } else {
        return Err(BcmrError::SourceNotFound(src.to_path_buf()));
    }

    Ok(())
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

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    super::file_copy::copy_xattrs(src, dst)?;

    Ok(())
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
