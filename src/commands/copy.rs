use crate::cli::Commands;
use crate::core::checksum;
use crate::core::cleanup::{self, CleanupRegistry};
use crate::core::error::BcmrError;
use crate::core::traversal;
use crate::ui::display::{print_dry_run, ActionType};

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;

mod file_copy;
mod overwrite;
mod pipeline_batch;

pub use overwrite::{check_overwrites, get_total_size, FileToOverwrite};
pub use pipeline_batch::{pipeline_copy, PipelineCallbacks};

use file_copy::{copy_file, CopyFileOptions};
use overwrite::{check_overwrite, determine_dry_run_action, is_normal_write};

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

pub enum PlanEntry {
    CreateDir { src: PathBuf, dst: PathBuf },
    CopyFile { src: PathBuf, dst: PathBuf },
}

pub struct CopyPlan {
    pub entries: Vec<PlanEntry>,
    pub total_size: u64,
    pub overwrites: Vec<FileToOverwrite>,
}

pub(super) fn scan_sources(
    sources: &[PathBuf],
    dst: &Path,
    recursive: bool,
    excludes: &[regex::Regex],
    mut on_entry: impl FnMut(PlanEntry, u64) -> std::result::Result<(), BcmrError>,
) -> std::result::Result<(), BcmrError> {
    let dst_is_dir = dst.exists() && dst.is_dir();

    for src in sources {
        if traversal::is_excluded(src, excludes) {
            continue;
        }

        if src.is_file() {
            let dst_path =
                if dst_is_dir {
                    dst.join(src.file_name().ok_or_else(|| {
                        BcmrError::InvalidInput("Invalid source file name".into())
                    })?)
                } else {
                    dst.to_path_buf()
                };

            let size = src.metadata()?.len();
            on_entry(
                PlanEntry::CopyFile {
                    src: src.clone(),
                    dst: dst_path,
                },
                size,
            )?;
        } else if recursive && src.is_dir() {
            let src_name = src
                .file_name()
                .ok_or_else(|| BcmrError::InvalidInput("Invalid source directory name".into()))?;
            let new_dst = if dst_is_dir {
                dst.join(src_name)
            } else {
                dst.to_path_buf()
            };

            on_entry(
                PlanEntry::CreateDir {
                    src: src.clone(),
                    dst: new_dst.clone(),
                },
                0,
            )?;

            for entry in traversal::walk(src, true, false, 1, excludes) {
                let entry = entry?;
                let path = entry.path();
                let relative = path.strip_prefix(src)?;
                let target = new_dst.join(relative);

                if path.is_dir() {
                    on_entry(
                        PlanEntry::CreateDir {
                            src: path.to_path_buf(),
                            dst: target,
                        },
                        0,
                    )?;
                } else if path.is_file() {
                    let size = entry.metadata()?.len();
                    on_entry(
                        PlanEntry::CopyFile {
                            src: path.to_path_buf(),
                            dst: target,
                        },
                        size,
                    )?;
                }
            }
        } else if src.is_dir() {
            return Err(BcmrError::InvalidInput(format!(
                "Source '{}' is a directory. Use -r flag for recursive copy.",
                src.display()
            )));
        } else {
            return Err(BcmrError::SourceNotFound(src.clone()));
        }
    }

    Ok(())
}

fn plan_copy_sync(
    sources: Vec<PathBuf>,
    dst: PathBuf,
    recursive: bool,
    excludes: Vec<regex::Regex>,
) -> std::result::Result<CopyPlan, BcmrError> {
    let mut entries = Vec::new();
    let mut total_size = 0u64;
    let mut overwrites = Vec::new();

    scan_sources(&sources, &dst, recursive, &excludes, |entry, size| {
        total_size += size;

        let target = match &entry {
            PlanEntry::CopyFile { dst, .. } => Some((dst.clone(), false)),
            PlanEntry::CreateDir { dst, .. } => {
                if dst.exists() {
                    Some((dst.clone(), true))
                } else {
                    None
                }
            }
        };
        if let Some((path, is_dir)) = target {
            if path.exists() && !traversal::is_excluded(&path, &excludes) {
                overwrites.push(FileToOverwrite { path, is_dir });
            }
        }

        entries.push(entry);
        Ok(())
    })?;

    Ok(CopyPlan {
        entries,
        total_size,
        overwrites,
    })
}

pub async fn plan_copy(
    sources: &[PathBuf],
    dst: &Path,
    recursive: bool,
    excludes: &[regex::Regex],
) -> std::result::Result<CopyPlan, BcmrError> {
    let sources = sources.to_vec();
    let dst = dst.to_path_buf();
    let excludes = excludes.to_vec();
    tokio::task::spawn_blocking(move || plan_copy_sync(sources, dst, recursive, excludes)).await?
}

pub fn dry_run_plan(plan: &CopyPlan, cli: &Commands) -> std::result::Result<(), BcmrError> {
    for entry in &plan.entries {
        match entry {
            PlanEntry::CreateDir { src, dst } => {
                if !dst.exists() {
                    print_dry_run(
                        ActionType::Add,
                        &src.to_string_lossy(),
                        Some(&format!("(DIR) -> {}", dst.display())),
                    );
                }
            }
            PlanEntry::CopyFile { src, dst } => {
                let action = determine_dry_run_action(src, dst, cli)?;
                print_dry_run(action, &src.to_string_lossy(), Some(&dst.to_string_lossy()));
            }
        }
    }
    Ok(())
}

pub async fn execute_plan<F>(
    plan: &CopyPlan,
    cli: &Commands,
    progress_callback: F,
    on_new_file: impl Fn(&str, u64) + Send + Sync + 'static,
) -> std::result::Result<(), BcmrError>
where
    F: Fn(u64) + Send + Sync + Clone + 'static,
{
    let test_mode = cli.get_test_mode();
    let callback = ProgressCallback {
        callback: progress_callback,
        on_new_file: Arc::new(on_new_file),
    };

    // Directories must exist before concurrent file copies into them.
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

type OnNewFileFn = Arc<dyn Fn(&str, u64) + Send + Sync>;

pub struct ProgressCallback<F> {
    pub(super) callback: F,
    pub(super) on_new_file: OnNewFileFn,
}

impl<F: Clone> Clone for ProgressCallback<F> {
    fn clone(&self) -> Self {
        Self {
            callback: self.callback.clone(),
            on_new_file: Arc::clone(&self.on_new_file),
        }
    }
}

pub async fn copy_path<F>(
    src: &Path,
    dst: &Path,
    cli: &Commands,
    excludes: &[regex::Regex],
    progress_callback: F,
    on_new_file: impl Fn(&str, u64) + Send + Sync + 'static,
) -> std::result::Result<(), BcmrError>
where
    F: Fn(u64) + Send + Sync + Clone + 'static,
{
    let test_mode = cli.get_test_mode();
    let callback = ProgressCallback {
        callback: progress_callback,
        on_new_file: Arc::new(on_new_file),
    };

    if traversal::is_excluded(src, excludes) {
        return Ok(());
    }

    if src.is_file() {
        let dst_path =
            if dst.is_dir() {
                dst.join(src.file_name().ok_or_else(|| {
                    BcmrError::InvalidInput("Invalid source file name".to_string())
                })?)
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
            .ok_or_else(|| BcmrError::InvalidInput("Invalid source directory name".to_string()))?;
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
    file_copy::copy_xattrs(src, dst)?;

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
