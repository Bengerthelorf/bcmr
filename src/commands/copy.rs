use crate::cli::{Commands, TestMode, SparseMode};
use crate::core::traversal;
use crate::core::checksum;
use crate::core::error::BcmrError;
use crate::ui::display::{print_dry_run, ActionType};

use once_cell::sync::Lazy;
use parking_lot::Mutex as ParkingMutex;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::fs::{self, File};
use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncSeekExt, SeekFrom};

// --- Global cleanup registry for partial/temp files ---

static CLEANUP_PATHS: Lazy<ParkingMutex<Vec<PathBuf>>> = Lazy::new(|| ParkingMutex::new(Vec::new()));

fn register_cleanup(path: &Path) {
    CLEANUP_PATHS.lock().push(path.to_path_buf());
}

fn unregister_cleanup(path: &Path) {
    CLEANUP_PATHS.lock().retain(|p| p != path);
}

/// Remove all registered partial/temp files. Called on Ctrl+C.
pub fn cleanup_partial_files() {
    let paths: Vec<PathBuf> = CLEANUP_PATHS.lock().drain(..).collect();
    for path in paths {
        let _ = std::fs::remove_file(&path);
    }
}

/// RAII guard that removes a temp file on drop unless disarmed.
struct TempFileGuard {
    path: PathBuf,
    active: bool,
}

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        register_cleanup(&path);
        Self { path, active: true }
    }

    fn disarm(&mut self) {
        self.active = false;
        unregister_cleanup(&self.path);
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = std::fs::remove_file(&self.path);
            unregister_cleanup(&self.path);
        }
    }
}

/// Generate a temp file path in the same directory as dst.
fn temp_path_for(dst: &Path) -> PathBuf {
    let name = dst.file_name().unwrap_or_default().to_string_lossy();
    dst.with_file_name(format!(".{}.bcmr.tmp", name))
}

// --- Platform-native fast copy (Linux) ---

#[cfg(target_os = "linux")]
async fn try_copy_file_range(
    src: &Path,
    dst: &Path,
    file_size: u64,
    callback: &impl Fn(u64),
    sync: bool,
) -> Option<Result<(), BcmrError>> {
    use std::os::unix::io::AsRawFd;

    let src_file = std::fs::File::open(src).ok()?;
    let dst_file = std::fs::OpenOptions::new()
        .write(true).create(true).truncate(true)
        .open(dst).ok()?;

    let src_fd = src_file.as_raw_fd();
    let dst_fd = dst_file.as_raw_fd();

    // Pre-allocate (best-effort, ignore errors)
    if file_size > 0 {
        unsafe { libc::fallocate(dst_fd, 0, 0, file_size as libc::off_t); }
    }

    const CHUNK: usize = 4 * 1024 * 1024;
    let mut remaining = file_size;

    while remaining > 0 {
        let to_copy = (remaining as usize).min(CHUNK);
        let sfd = src_fd;
        let dfd = dst_fd;

        // Capture errno inside the blocking thread (errno is thread-local)
        let result = tokio::task::spawn_blocking(move || {
            let ret = unsafe {
                libc::copy_file_range(
                    sfd, std::ptr::null_mut(),
                    dfd, std::ptr::null_mut(),
                    to_copy, 0,
                )
            };
            if ret < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(ret)
            }
        }).await.ok()?;

        match result {
            Err(err) => {
                let errno = err.raw_os_error().unwrap_or(0);
                // Not available or not supported — fall back to buffer copy
                if errno == libc::ENOSYS || errno == libc::EXDEV
                    || errno == libc::EINVAL || errno == libc::EOPNOTSUPP
                {
                    drop(dst_file);
                    let _ = std::fs::remove_file(dst);
                    return None;
                }
                return Some(Err(BcmrError::Io(err)));
            }
            Ok(0) => break,
            Ok(n) => {
                let n = n as u64;
                remaining -= n;
                callback(n);
            }
        }
    }

    if sync {
        if let Err(e) = dst_file.sync_data() {
            return Some(Err(BcmrError::Io(e)));
        }
    }

    Some(Ok(()))
}

// --- Public API ---

pub struct FileToOverwrite {
    pub path: PathBuf,
    pub is_dir: bool,
}

pub async fn check_overwrites(
    sources: &[PathBuf],
    dst: &Path,
    recursive: bool,
    _cli: &Commands,
    excludes: &[regex::Regex],
) -> std::result::Result<Vec<FileToOverwrite>, BcmrError> {
    let mut files_to_overwrite = Vec::new();

    let dst_is_dir = dst.exists() && dst.is_dir();

    for src in sources {
        if traversal::is_excluded(src, excludes) {
            continue;
        }

        if src.is_file() {
            let dst_path = if dst_is_dir {
                dst.join(
                    src.file_name()
                        .ok_or_else(|| BcmrError::InvalidInput("Invalid source file name".to_string()))?,
                )
            } else {
                dst.to_path_buf()
            };

            if dst_path.exists() && !traversal::is_excluded(&dst_path, excludes) {
                files_to_overwrite.push(FileToOverwrite {
                    path: dst_path,
                    is_dir: false,
                });
            }
        } else if recursive && src.is_dir() {
            let src_name = src
                .file_name()
                .ok_or_else(|| BcmrError::InvalidInput("Invalid source directory name".to_string()))?;
            let new_dst = if dst_is_dir {
                dst.join(src_name)
            } else {
                dst.to_path_buf()
            };

            if new_dst.exists() {
                for entry in traversal::walk(src, true, false, 1, excludes) {
                    let entry = entry?;
                    let path = entry.path();

                    let relative_path = path.strip_prefix(src)?;
                    let target_path = new_dst.join(relative_path);

                    if target_path.exists() {
                        files_to_overwrite.push(FileToOverwrite {
                            path: target_path,
                            is_dir: path.is_dir(),
                        });
                    }
                }
            }
        }
    }

    Ok(files_to_overwrite)
}

// Helper function to run blocking directory traversal
fn get_total_size_sync(
    sources: Vec<PathBuf>,
    recursive: bool,
    excludes: Vec<regex::Regex>,
) -> std::result::Result<u64, BcmrError> {
    let mut total_size = 0;

    for src in sources {
        if traversal::is_excluded(&src, &excludes) {
            continue;
        }

        if src.is_file() {
            total_size += src.metadata()?.len();
        } else if src.is_dir() {
            if recursive {
                for entry in traversal::walk(&src, true, false, 1, &excludes) {
                    let entry = entry?;
                    let path = entry.path();
                    if path.is_file() {
                        total_size += entry.metadata()?.len();
                    }
                }
            } else {
                return Err(BcmrError::InvalidInput(format!(
                    "Source '{}' is a directory. Use -r flag for recursive copy.",
                    src.display()
                )));
            }
        } else {
            return Err(BcmrError::SourceNotFound(src));
        }
    }

    Ok(total_size)
}

pub async fn get_total_size(
    sources: &[PathBuf],
    recursive: bool,
    _cli: &Commands,
    excludes: &[regex::Regex],
) -> std::result::Result<u64, BcmrError> {
    let sources = sources.to_vec();
    let excludes = excludes.to_vec();

    tokio::task::spawn_blocking(move || get_total_size_sync(sources, recursive, excludes)).await?
}

/// Check if the destination should be treated as a normal write (no resume/append/strict).
fn is_normal_write(cli: &Commands) -> bool {
    !cli.is_resume() && !cli.is_append() && !cli.is_strict()
}

fn determine_dry_run_action(
    src: &Path,
    dst: &Path,
    cli: &Commands,
) -> std::result::Result<ActionType, BcmrError> {
    if !dst.exists() {
        return Ok(ActionType::Add);
    }
    let src_meta = src.metadata()?;
    let dst_meta = dst.metadata()?;
    let src_len = src_meta.len();
    let dst_len = dst_meta.len();

    if cli.is_strict() || cli.is_append() {
        if dst_len == src_len {
            return Ok(ActionType::Skip);
        } else if dst_len < src_len {
            return Ok(ActionType::Append);
        }
        return Ok(ActionType::Overwrite);
    }

    if cli.is_resume() {
        let src_mtime = src_meta.modified()?;
        let dst_mtime = dst_meta.modified()?;
        if src_mtime != dst_mtime {
            return Ok(ActionType::Overwrite);
        }
        if dst_len == src_len {
            return Ok(ActionType::Skip);
        } else if dst_len < src_len {
            return Ok(ActionType::Append);
        }
        return Ok(ActionType::Overwrite);
    }

    Ok(ActionType::Overwrite)
}

// --- Single-pass copy planning ---

#[allow(dead_code)]
pub enum PlanEntry {
    CreateDir { src: PathBuf, dst: PathBuf },
    CopyFile { src: PathBuf, dst: PathBuf, size: u64 },
}

pub struct CopyPlan {
    pub entries: Vec<PlanEntry>,
    pub total_size: u64,
    pub overwrites: Vec<FileToOverwrite>,
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
    let dst_is_dir = dst.exists() && dst.is_dir();

    for src in &sources {
        if traversal::is_excluded(src, &excludes) {
            continue;
        }

        if src.is_file() {
            let dst_path = if dst_is_dir {
                dst.join(src.file_name().ok_or_else(||
                    BcmrError::InvalidInput("Invalid source file name".into()))?)
            } else {
                dst.clone()
            };

            let size = src.metadata()?.len();
            total_size += size;

            if dst_path.exists() && !traversal::is_excluded(&dst_path, &excludes) {
                overwrites.push(FileToOverwrite { path: dst_path.clone(), is_dir: false });
            }

            entries.push(PlanEntry::CopyFile { src: src.clone(), dst: dst_path, size });
        } else if recursive && src.is_dir() {
            let src_name = src.file_name().ok_or_else(||
                BcmrError::InvalidInput("Invalid source directory name".into()))?;
            let new_dst = if dst_is_dir { dst.join(src_name) } else { dst.clone() };

            entries.push(PlanEntry::CreateDir { src: src.clone(), dst: new_dst.clone() });

            for entry in traversal::walk(src, true, false, 1, &excludes) {
                let entry = entry?;
                let path = entry.path();
                let relative = path.strip_prefix(src)?;
                let target = new_dst.join(relative);

                if path.is_dir() {
                    if target.exists() && !traversal::is_excluded(&target, &excludes) {
                        overwrites.push(FileToOverwrite { path: target.clone(), is_dir: true });
                    }
                    entries.push(PlanEntry::CreateDir { src: path.to_path_buf(), dst: target });
                } else if path.is_file() {
                    let size = entry.metadata()?.len();
                    total_size += size;
                    if target.exists() && !traversal::is_excluded(&target, &excludes) {
                        overwrites.push(FileToOverwrite { path: target.clone(), is_dir: false });
                    }
                    entries.push(PlanEntry::CopyFile { src: path.to_path_buf(), dst: target, size });
                }
            }
        } else if src.is_dir() {
            return Err(BcmrError::InvalidInput(format!(
                "Source '{}' is a directory. Use -r flag for recursive copy.", src.display()
            )));
        } else {
            return Err(BcmrError::SourceNotFound(src.clone()));
        }
    }

    Ok(CopyPlan { entries, total_size, overwrites })
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
            PlanEntry::CopyFile { src, dst, .. } => {
                let action = determine_dry_run_action(src, dst, cli)?;
                print_dry_run(
                    action,
                    &src.to_string_lossy(),
                    Some(&dst.to_string_lossy()),
                );
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn execute_plan<F>(
    plan: &CopyPlan,
    preserve: bool,
    test_mode: TestMode,
    cli: &Commands,
    progress_callback: F,
    on_new_file: impl Fn(&str, u64) + Send + Sync + 'static,
) -> std::result::Result<(), BcmrError>
where
    F: Fn(u64) + Send + Sync,
{
    let callback = ProgressCallback {
        callback: progress_callback,
        on_new_file: Box::new(on_new_file),
    };

    for entry in &plan.entries {
        match entry {
            PlanEntry::CreateDir { dst, .. } => {
                if !dst.exists() {
                    fs::create_dir_all(dst).await?;
                }
            }
            PlanEntry::CopyFile { src, dst, .. } => {
                if dst.exists() && !cli.is_force() && is_normal_write(cli) {
                    return Err(BcmrError::TargetExists(dst.clone()));
                }

                if dst.exists() && cli.is_force() && !is_normal_write(cli) {
                    fs::remove_file(dst).await?;
                }

                copy_file(src, dst, CopyFileOptions {
                    preserve,
                    verify: cli.is_verify(),
                    resume: cli.is_resume(),
                    strict: cli.is_strict(),
                    append: cli.is_append(),
                    sync: cli.is_sync(),
                    reflink_arg: cli.get_reflink_mode(),
                    sparse_arg: cli.get_sparse_mode(),
                    test_mode: test_mode.clone(),
                }, &callback).await?;

                if cli.is_verbose() {
                    eprintln!("'{}' -> '{}'", src.display(), dst.display());
                }
            }
        }
    }

    // Preserve directory attributes after all files are copied (deepest first)
    // so that file copies don't alter directory mtimes
    if preserve {
        for entry in plan.entries.iter().rev() {
            if let PlanEntry::CreateDir { src, dst } = entry {
                preserve_attributes(src, dst).await?;
            }
        }
    }

    Ok(())
}

#[allow(clippy::type_complexity)]
pub struct ProgressCallback<F> {
    callback: F,
    on_new_file: Box<dyn Fn(&str, u64) + Send + Sync>,
}

#[allow(clippy::too_many_arguments)]
pub async fn copy_path<F>(
    src: &Path,
    dst: &Path,
    recursive: bool,
    preserve: bool,
    test_mode: TestMode,
    cli: &Commands,
    excludes: &[regex::Regex],
    progress_callback: F,
    on_new_file: impl Fn(&str, u64) + Send + Sync + 'static,
) -> std::result::Result<(), BcmrError>
where
    F: Fn(u64) + Send + Sync,
{
    let callback = ProgressCallback {
        callback: progress_callback,
        on_new_file: Box::new(on_new_file),
    };

    // Main exclusion check for the root item
    if traversal::is_excluded(src, excludes) {
        return Ok(());
    }

    if src.is_file() {
        let dst_path = if dst.is_dir() {
            dst.join(
                src.file_name()
                    .ok_or_else(|| BcmrError::InvalidInput("Invalid source file name".to_string()))?,
            )
        } else {
            dst.to_path_buf()
        };

        // For files, only check when the target file exists
        if dst_path.exists() && !cli.is_force() && is_normal_write(cli) {
            return Err(BcmrError::TargetExists(dst_path));
        }

        if cli.is_dry_run() {
            let action = determine_dry_run_action(src, &dst_path, cli)?;
            print_dry_run(
                action,
                &src.to_string_lossy(),
                Some(&dst_path.to_string_lossy())
            );
            return Ok(());
        }

        // For resume/append modes with force, remove existing file before overwrite
        if dst_path.exists() && cli.is_force() && !is_normal_write(cli) {
            fs::remove_file(&dst_path).await?;
        }
        // For normal writes: atomic rename handles overwrite, no pre-deletion needed

        copy_file(src, &dst_path, CopyFileOptions {
            preserve, verify: cli.is_verify(), resume: cli.is_resume(),
            strict: cli.is_strict(), append: cli.is_append(),
            sync: cli.is_sync(),
            reflink_arg: cli.get_reflink_mode(), sparse_arg: cli.get_sparse_mode(),
            test_mode,
        }, &callback).await?;

        if cli.is_verbose() {
            eprintln!("'{}' -> '{}'", src.display(), dst_path.display());
        }
    } else if recursive && src.is_dir() {
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
                Some(&format!("(DIR) -> {}", new_dst.display()))
            );
        }

        // Create the target directory (if it does not exist)
        if !new_dst.exists() && !cli.is_dry_run() {
            fs::create_dir_all(&new_dst).await?;
        }

        let mut files_to_copy = Vec::new();
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
                    if preserve {
                        preserve_attributes(path, &target_path).await?;
                    }
                } else if !target_path.exists() {
                    print_dry_run(
                        ActionType::Add,
                        &path.to_string_lossy(),
                        Some(&format!("(DIR) -> {}", target_path.display()))
                    );
                }
            } else if path.is_file() {
                files_to_copy.push((path.to_path_buf(), target_path));
            }
        }

        // Copy files
        for (src_path, dst_path) in files_to_copy {
            if let Some(parent) = dst_path.parent() {
                if !parent.exists() && !cli.is_dry_run() {
                    fs::create_dir_all(parent).await?;
                }
            }

            // Check each file to see if it needs to be overwritten
            if dst_path.exists() && !cli.is_force() && is_normal_write(cli) {
                return Err(BcmrError::TargetExists(dst_path));
            }

            if cli.is_dry_run() {
                let action = determine_dry_run_action(&src_path, &dst_path, cli)?;
                print_dry_run(
                    action,
                    &src_path.to_string_lossy(),
                    Some(&dst_path.to_string_lossy())
                );
            } else {
                // For resume/append modes with force, remove existing file
                if dst_path.exists() && cli.is_force() && !is_normal_write(cli) {
                    fs::remove_file(&dst_path).await?;
                }

                copy_file(
                    &src_path,
                    &dst_path,
                    CopyFileOptions {
                        preserve,
                        verify: cli.is_verify(),
                        resume: cli.is_resume(),
                        strict: cli.is_strict(),
                        append: cli.is_append(),
                        sync: cli.is_sync(),
                        reflink_arg: cli.get_reflink_mode(),
                        sparse_arg: cli.get_sparse_mode(),
                        test_mode: test_mode.clone(),
                    },
                    &callback,
                )
                .await?;

                if cli.is_verbose() {
                    eprintln!("'{}' -> '{}'", src_path.display(), dst_path.display());
                }
            }
        }

        // Set the attributes of the target directory (if needed)
        if preserve && !cli.is_dry_run() {
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

async fn preserve_attributes(src: &Path, dst: &Path) -> std::result::Result<(), BcmrError> {
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
    Ok(())
}

struct CopyFileOptions {
    preserve: bool,
    verify: bool,
    resume: bool,
    strict: bool,
    append: bool,
    sync: bool,
    reflink_arg: Option<String>,
    sparse_arg: Option<String>,
    test_mode: TestMode,
}

async fn copy_file<F>(
    src: &Path,
    dst: &Path,
    opts: CopyFileOptions,
    callback: &ProgressCallback<F>,
) -> std::result::Result<(), BcmrError>
where
    F: Fn(u64),
{
    let CopyFileOptions {
        preserve, verify, resume, strict, append, sync,
        reflink_arg, sparse_arg, test_mode,
    } = opts;

    let file_size = src.metadata()?.len();
    let file_name = src
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    (callback.on_new_file)(&file_name, file_size);

    // Determine reflink mode
    let config_reflink = &crate::config::CONFIG.copy.reflink;
    let (try_reflink, fail_on_error) = if let Some(mode) = reflink_arg {
         match mode.to_lowercase().as_str() {
             "force" => (true, true),
             "disable" => (false, false),
             _ => (true, false),
         }
    } else {
        match config_reflink.to_lowercase().as_str() {
             "never" => (false, false),
             _ => (true, false),
        }
    };

    // Determine sparse mode
    let config_sparse = &crate::config::CONFIG.copy.sparse;
    let sparse_mode = if let Some(mode) = sparse_arg {
         match mode.to_lowercase().as_str() {
             "force" => SparseMode::Always,
             "disable" => SparseMode::Never,
             _ => SparseMode::Auto,
         }
    } else {
         match config_sparse.to_lowercase().as_str() {
             "auto" => SparseMode::Auto,
             _ => SparseMode::Never,
         }
    };

    // Ensure parent directory exists (needed for both temp file and direct write)
    if let Some(parent) = dst.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).await?;
        }
    }

    // Determine if we can use atomic writes (temp file + rename)
    let use_atomic = !resume && !append && !strict;
    let write_target;
    let mut guard: Option<TempFileGuard> = None;

    if use_atomic {
        let temp = temp_path_for(dst);
        // Clean up stale temp file from previous crash
        if temp.exists() {
            let _ = fs::remove_file(&temp).await;
        }
        guard = Some(TempFileGuard::new(temp.clone()));
        write_target = temp;
    } else {
        write_target = dst.to_path_buf();
    }

    // Try reflink (writes to write_target)
    if try_reflink && !matches!(sparse_mode, SparseMode::Always) {
        let src_path = src.to_path_buf();
        let target_path = write_target.clone();
        let result = tokio::task::spawn_blocking(move || reflink_copy::reflink(&src_path, &target_path))
            .await?;

        match result {
            Ok(_) => {
                // Reflink successful — atomic rename if needed
                if use_atomic {
                    fs::rename(&write_target, dst).await?;
                    if let Some(ref mut g) = guard { g.disarm(); }
                }
                (callback.callback)(file_size);
                if preserve { preserve_attributes(src, dst).await?; }
                if verify { verify_copy(src, dst).await?; }
                return Ok(());
            }
            Err(e) => {
                if fail_on_error {
                   return Err(BcmrError::Reflink(format!("Reflink failed (forced): {}", e)));
                }
            }
        }
    }

    // Try copy_file_range on Linux (only for fresh writes, no sparse, no test mode)
    #[cfg(target_os = "linux")]
    if use_atomic && matches!(test_mode, TestMode::None) && matches!(sparse_mode, SparseMode::Never) {
        match try_copy_file_range(src, &write_target, file_size, &callback.callback, sync).await {
            Some(Ok(())) => {
                fs::rename(&write_target, dst).await?;
                if let Some(ref mut g) = guard { g.disarm(); }
                if preserve { preserve_attributes(src, dst).await?; }
                if verify { verify_copy(src, dst).await?; }
                return Ok(());
            }
            Some(Err(e)) => {
                return Err(e);
            }
            None => {
                // copy_file_range not available, fall through to buffer copy
            }
        }
    }

    // --- Buffer copy path ---

    let mut start_offset = 0;
    let mut file_flags = fs::OpenOptions::new();
    file_flags.write(true);

    if (resume || append || strict) && dst.exists() {
        let dst_len = dst.metadata()?.len();

        let should_resume = if strict {
            if dst_len == file_size {
                 let src_path = src.to_path_buf();
                 let dst_path = dst.to_path_buf();
                 let (src_hash, dst_hash) = tokio::join!(
                     tokio::task::spawn_blocking(move || checksum::calculate_hash(&src_path)),
                     tokio::task::spawn_blocking(move || checksum::calculate_hash(&dst_path)),
                 );
                 let src_hash = src_hash??;
                 let dst_hash = dst_hash??;

                 if src_hash == dst_hash {
                     (callback.callback)(file_size);
                     return Ok(());
                 }
                 false
            } else if dst_len < file_size {
                 let src_path = src.to_path_buf();
                 let dst_path = dst.to_path_buf();
                 let limit = dst_len;
                 let (dst_hash, src_partial_hash) = tokio::join!(
                     tokio::task::spawn_blocking(move || checksum::calculate_hash(&dst_path)),
                     tokio::task::spawn_blocking(move || checksum::calculate_partial_hash(&src_path, limit)),
                 );
                 let dst_hash = dst_hash??;
                 let src_partial_hash = src_partial_hash??;

                 dst_hash == src_partial_hash
            } else {
                 false
            }
        } else if append {
            if dst_len == file_size {
                (callback.callback)(file_size);
                return Ok(());
            } else if dst_len < file_size {
                true
            } else {
                false
            }
        } else {
            let src_mtime = src.metadata()?.modified()?;
            let dst_mtime = dst.metadata()?.modified()?;

            if src_mtime != dst_mtime {
                false
            } else if dst_len == file_size {
                (callback.callback)(file_size);
                return Ok(());
            } else {
                dst_len < file_size
            }
        };

        if should_resume {
             start_offset = dst_len;
             file_flags.append(true);
             (callback.callback)(start_offset);
        } else {
             start_offset = 0;
             file_flags.create(true).truncate(true);
        }

    } else {
        file_flags.create(true).truncate(true);
    }

    let mut src_file = File::open(src).await?;
    let mut dst_file = file_flags.open(&write_target).await?;

    if start_offset > 0 {
        src_file.seek(SeekFrom::Start(start_offset)).await?;
    }

    // Pre-allocate on Linux (best-effort)
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        let remaining = file_size.saturating_sub(start_offset);
        if remaining > 0 {
            let fd = dst_file.as_raw_fd();
            unsafe {
                libc::fallocate(fd, 0, start_offset as libc::off_t, remaining as libc::off_t);
            }
        }
    }

    let mut buffer = vec![0; 4 * 1024 * 1024]; // 4MB buffer

    match test_mode {
        TestMode::Delay(ms) => loop {
            let n = src_file.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            dst_file.write_all(&buffer[..n]).await?;
            (callback.callback)(n as u64);
            tokio::time::sleep(Duration::from_millis(ms)).await;
        },
        TestMode::SpeedLimit(bps) => {
            let chunk_size = bps.min(buffer.len() as u64);
            let mut start_time = Instant::now();

            loop {
                let n = src_file.read(&mut buffer[..chunk_size as usize]).await?;
                if n == 0 {
                    break;
                }

                dst_file.write_all(&buffer[..n]).await?;

                let elapsed = start_time.elapsed();
                let target_duration = Duration::from_secs_f64(n as f64 / bps as f64);
                if elapsed < target_duration {
                    tokio::time::sleep(target_duration - elapsed).await;
                    start_time = Instant::now();
                }

                (callback.callback)(n as u64);
            }
        }
        TestMode::None => {
            const BLOCK_SIZE: usize = 4096;
            let mut pending_hole = 0u64;

            loop {
                let n = src_file.read(&mut buffer).await?;
                if n == 0 {
                    break;
                }

                match sparse_mode {
                    SparseMode::Never => {
                        dst_file.write_all(&buffer[..n]).await?;
                    }
                    SparseMode::Always | SparseMode::Auto => {
                        let min_block = if matches!(sparse_mode, SparseMode::Always) { 1 } else { BLOCK_SIZE };
                        let mut offset = 0;
                        while offset < n {
                            let end = (offset + BLOCK_SIZE).min(n);
                            let chunk = &buffer[offset..end];
                            let chunk_len = chunk.len();

                            if chunk_len >= min_block && chunk.iter().all(|&b| b == 0) {
                                pending_hole += chunk_len as u64;
                            } else {
                                if pending_hole > 0 {
                                    dst_file.seek(SeekFrom::Current(pending_hole as i64)).await?;
                                    pending_hole = 0;
                                }
                                dst_file.write_all(chunk).await?;
                            }
                            offset = end;
                        }
                    }
                }
                (callback.callback)(n as u64);
            }

            if pending_hole > 0 {
                let current_pos = dst_file.stream_position().await?;
                dst_file.set_len(current_pos + pending_hole).await?;
            }
        },
    }

    // Sync data to disk if requested
    if sync {
        dst_file.sync_data().await?;
    }

    // Close file before rename
    drop(dst_file);

    // Atomic rename: temp → final destination
    if use_atomic {
        fs::rename(&write_target, dst).await?;
        if let Some(ref mut g) = guard { g.disarm(); }
    }

    if preserve {
        preserve_attributes(src, dst).await?;
    }

    if verify {
        verify_copy(src, dst).await?;
    }

    Ok(())
}

async fn verify_copy(src: &Path, dst: &Path) -> std::result::Result<(), BcmrError> {
    let src_path = src.to_path_buf();
    let dst_path = dst.to_path_buf();
    let (src_hash, dst_hash) = tokio::join!(
        tokio::task::spawn_blocking(move || checksum::calculate_hash(&src_path)),
        tokio::task::spawn_blocking(move || checksum::calculate_hash(&dst_path)),
    );
    let src_hash = src_hash??;
    let dst_hash = dst_hash??;

    if src_hash != dst_hash {
        return Err(BcmrError::VerificationError(dst.to_path_buf()));
    }
    Ok(())
}
