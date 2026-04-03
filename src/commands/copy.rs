use crate::cli::{Commands, SparseMode, TestMode};
use crate::core::checksum;
use crate::core::error::BcmrError;
use crate::core::traversal;
use crate::ui::display::{print_dry_run, ActionType};

use once_cell::sync::Lazy;
use parking_lot::Mutex as ParkingMutex;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::fs::{self, File};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

static CLEANUP_PATHS: Lazy<ParkingMutex<Vec<PathBuf>>> =
    Lazy::new(|| ParkingMutex::new(Vec::new()));

fn register_cleanup(path: &Path) {
    CLEANUP_PATHS.lock().push(path.to_path_buf());
}

fn unregister_cleanup(path: &Path) {
    CLEANUP_PATHS.lock().retain(|p| p != path);
}

pub fn cleanup_partial_files() {
    let paths: Vec<PathBuf> = CLEANUP_PATHS.lock().drain(..).collect();
    for path in paths {
        let _ = std::fs::remove_file(&path);
    }
}

pub(crate) struct TempFileGuard {
    path: PathBuf,
    active: bool,
}

impl TempFileGuard {
    pub(crate) fn new(path: PathBuf) -> Self {
        register_cleanup(&path);
        Self { path, active: true }
    }

    pub(crate) fn disarm(&mut self) {
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

fn temp_path_for(dst: &Path) -> PathBuf {
    let name = dst.file_name().unwrap_or_default().to_string_lossy();
    dst.with_file_name(format!(".{}.bcmr.tmp", name))
}

#[cfg(target_os = "linux")]
async fn try_copy_file_range(
    src: &Path,
    dst: &Path,
    file_size: u64,
    callback: &impl Fn(u64),
) -> Option<Result<(), BcmrError>> {
    use std::os::unix::io::AsRawFd;

    let src_file = std::fs::File::open(src).ok()?;
    let dst_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dst)
        .ok()?;

    let src_fd = src_file.as_raw_fd();
    let dst_fd = dst_file.as_raw_fd();

    if file_size > 0 {
        // Best-effort preallocation; ignore ENOTSUP on filesystems that don't support it
        unsafe {
            let _ = libc::fallocate(dst_fd, 0, 0, file_size as libc::off_t);
        }
    }

    const CHUNK: usize = 4 * 1024 * 1024;
    let mut remaining = file_size;

    while remaining > 0 {
        let to_copy = (remaining as usize).min(CHUNK);
        let sfd = src_fd;
        let dfd = dst_fd;
        let result = tokio::task::spawn_blocking(move || {
            let ret = unsafe {
                libc::copy_file_range(
                    sfd,
                    std::ptr::null_mut(),
                    dfd,
                    std::ptr::null_mut(),
                    to_copy,
                    0,
                )
            };
            if ret < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(ret)
            }
        })
        .await
        .ok()?;

        match result {
            Err(err) => {
                let errno = err.raw_os_error().unwrap_or(0);
                if errno == libc::ENOSYS
                    || errno == libc::EXDEV
                    || errno == libc::EINVAL
                    || errno == libc::EOPNOTSUPP
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

    Some(Ok(()))
}

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
                dst.join(src.file_name().ok_or_else(|| {
                    BcmrError::InvalidInput("Invalid source file name".to_string())
                })?)
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
            let src_name = src.file_name().ok_or_else(|| {
                BcmrError::InvalidInput("Invalid source directory name".to_string())
            })?;
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

pub enum PlanEntry {
    CreateDir {
        src: PathBuf,
        dst: PathBuf,
    },
    CopyFile { src: PathBuf, dst: PathBuf },
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
            let dst_path =
                if dst_is_dir {
                    dst.join(src.file_name().ok_or_else(|| {
                        BcmrError::InvalidInput("Invalid source file name".into())
                    })?)
                } else {
                    dst.clone()
                };

            let size = src.metadata()?.len();
            total_size += size;

            if dst_path.exists() && !traversal::is_excluded(&dst_path, &excludes) {
                overwrites.push(FileToOverwrite {
                    path: dst_path.clone(),
                    is_dir: false,
                });
            }

            entries.push(PlanEntry::CopyFile {
                src: src.clone(),
                dst: dst_path,
            });
        } else if recursive && src.is_dir() {
            let src_name = src
                .file_name()
                .ok_or_else(|| BcmrError::InvalidInput("Invalid source directory name".into()))?;
            let new_dst = if dst_is_dir {
                dst.join(src_name)
            } else {
                dst.clone()
            };

            entries.push(PlanEntry::CreateDir {
                src: src.clone(),
                dst: new_dst.clone(),
            });

            for entry in traversal::walk(src, true, false, 1, &excludes) {
                let entry = entry?;
                let path = entry.path();
                let relative = path.strip_prefix(src)?;
                let target = new_dst.join(relative);

                if path.is_dir() {
                    if target.exists() && !traversal::is_excluded(&target, &excludes) {
                        overwrites.push(FileToOverwrite {
                            path: target.clone(),
                            is_dir: true,
                        });
                    }
                    entries.push(PlanEntry::CreateDir {
                        src: path.to_path_buf(),
                        dst: target,
                    });
                } else if path.is_file() {
                    let size = entry.metadata()?.len();
                    total_size += size;
                    if target.exists() && !traversal::is_excluded(&target, &excludes) {
                        overwrites.push(FileToOverwrite {
                            path: target.clone(),
                            is_dir: false,
                        });
                    }
                    entries.push(PlanEntry::CopyFile {
                        src: path.to_path_buf(),
                        dst: target,
                    });
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
    F: Fn(u64) + Send + Sync,
{
    let test_mode = cli.get_test_mode();
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
            PlanEntry::CopyFile { src, dst } => {
                if dst.exists() && !cli.is_force() && is_normal_write(cli) {
                    return Err(BcmrError::TargetExists(dst.clone()));
                }

                if dst.exists() && cli.is_force() && !is_normal_write(cli) {
                    fs::remove_file(dst).await?;
                }

                copy_file(
                    src,
                    dst,
                    CopyFileOptions::from_cli(cli, test_mode.clone()),
                    &callback,
                )
                .await?;

                if cli.is_verbose() {
                    eprintln!("'{}' -> '{}'", src.display(), dst.display());
                }
            }
        }
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

#[allow(clippy::type_complexity)]
pub struct ProgressCallback<F> {
    callback: F,
    on_new_file: Box<dyn Fn(&str, u64) + Send + Sync>,
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
    F: Fn(u64) + Send + Sync,
{
    let test_mode = cli.get_test_mode();
    let callback = ProgressCallback {
        callback: progress_callback,
        on_new_file: Box::new(on_new_file),
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

impl CopyFileOptions {
    fn from_cli(cli: &Commands, test_mode: TestMode) -> Self {
        Self {
            preserve: cli.is_preserve(),
            verify: cli.is_verify(),
            resume: cli.is_resume(),
            strict: cli.is_strict(),
            append: cli.is_append(),
            sync: cli.is_sync(),
            reflink_arg: cli.get_reflink_mode(),
            sparse_arg: cli.get_sparse_mode(),
            test_mode,
        }
    }
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
        preserve,
        verify,
        resume,
        strict,
        append,
        sync,
        reflink_arg,
        sparse_arg,
        test_mode,
    } = opts;

    let file_size = src.metadata()?.len();
    let file_name = src
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    (callback.on_new_file)(&file_name, file_size);

    let config_reflink = &crate::config::CONFIG.copy.reflink;
    let (try_reflink, fail_on_error) = if let Some(mode) = reflink_arg {
        match mode.to_lowercase().as_str() {
            "force" => (true, true),
            "disable" => (false, false),
            _ => (true, false),
        }
    } else {
        match config_reflink.to_lowercase().as_str() {
            "disable" | "never" => (false, false),
            "force" => (true, true),
            _ => (true, false),
        }
    };

    let config_sparse = &crate::config::CONFIG.copy.sparse;
    let sparse_mode = if let Some(mode) = sparse_arg {
        match mode.to_lowercase().as_str() {
            "force" => SparseMode::Always,
            "disable" => SparseMode::Never,
            _ => SparseMode::Auto,
        }
    } else {
        match config_sparse.to_lowercase().as_str() {
            "force" => SparseMode::Always,
            "disable" | "never" => SparseMode::Never,
            _ => SparseMode::Auto,
        }
    };

    if let Some(parent) = dst.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).await?;
        }
    }

    let use_atomic = !resume && !append && !strict;
    let write_target;
    let mut guard: Option<TempFileGuard> = None;

    if use_atomic {
        let temp = temp_path_for(dst);
        if temp.exists() {
            let _ = fs::remove_file(&temp).await;
        }
        guard = Some(TempFileGuard::new(temp.clone()));
        write_target = temp;
    } else {
        write_target = dst.to_path_buf();
    }
    if super::copy_strategies::try_reflink(
        src,
        &write_target,
        file_size,
        try_reflink,
        fail_on_error,
        &sparse_mode,
        &callback.callback,
    )
    .await?
    {
        return super::copy_strategies::finalize(
            fs::File::open(&write_target).await?,
            &write_target,
            dst,
            src,
            use_atomic,
            &mut guard,
            sync,
            preserve,
            verify,
            None,
        )
        .await;
    }
    #[cfg(target_os = "linux")]
    if use_atomic && matches!(test_mode, TestMode::None) && matches!(sparse_mode, SparseMode::Never)
    {
        match try_copy_file_range(src, &write_target, file_size, &callback.callback).await {
            Some(Ok(())) => {
                return super::copy_strategies::finalize(
                    fs::File::open(&write_target).await?,
                    &write_target,
                    dst,
                    src,
                    use_atomic,
                    &mut guard,
                    sync,
                    preserve,
                    verify,
                    None,
                )
                .await;
            }
            Some(Err(e)) => return Err(e),
            None => {}
        }
    }
    let resume_state = crate::core::resume::resolve(
        src,
        dst,
        file_size,
        resume,
        strict,
        append,
        &callback.callback,
    )
    .await?;

    if resume_state.already_complete {
        return Ok(());
    }

    let start_offset = resume_state.start_offset;
    let loaded_session = resume_state.loaded_session;

    let mut file_flags = fs::OpenOptions::new();
    file_flags.write(true);
    if start_offset > 0 {
        file_flags.create(true);
    } else {
        file_flags.create(true).truncate(true);
    }

    let mut src_file = File::open(src).await?;
    let mut dst_file = file_flags.open(&write_target).await?;

    if start_offset > 0 {
        src_file.seek(SeekFrom::Start(start_offset)).await?;
        dst_file.seek(SeekFrom::Start(start_offset)).await?;
    }

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        let remaining = file_size.saturating_sub(start_offset);
        if remaining > 0 {
            let fd = dst_file.as_raw_fd();
            unsafe {
                let _ =
                    libc::fallocate(fd, 0, start_offset as libc::off_t, remaining as libc::off_t);
            }
        }
    }
    let mut session = super::copy_strategies::create_session(
        src,
        dst,
        file_size,
        start_offset,
        resume,
        append,
        strict,
        &loaded_session,
    );

    let inline_src_hash = match test_mode {
        TestMode::Delay(ms) => {
            let mut buffer = vec![0u8; crate::core::session::COPY_BLOCK_SIZE as usize];
            loop {
                let n = src_file.read(&mut buffer).await?;
                if n == 0 {
                    break;
                }
                dst_file.write_all(&buffer[..n]).await?;
                (callback.callback)(n as u64);
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
            None
        }
        TestMode::SpeedLimit(bps) => {
            let mut buffer = vec![0u8; crate::core::session::COPY_BLOCK_SIZE as usize];
            let chunk_size = bps.min(buffer.len() as u64);
            let mut start_time = Instant::now();
            loop {
                let n = src_file.read(&mut buffer[..chunk_size as usize]).await?;
                if n == 0 {
                    break;
                }
                dst_file.write_all(&buffer[..n]).await?;
                let elapsed = start_time.elapsed();
                let target = Duration::from_secs_f64(n as f64 / bps as f64);
                if elapsed < target {
                    tokio::time::sleep(target - elapsed).await;
                    start_time = Instant::now();
                }
                (callback.callback)(n as u64);
            }
            None
        }
        TestMode::None => {
            super::copy_strategies::streaming_copy(
                &mut src_file,
                &mut dst_file,
                &mut session,
                &sparse_mode,
                start_offset,
                &callback.callback,
            )
            .await?
        }
    };

    super::copy_strategies::finalize(
        dst_file,
        &write_target,
        dst,
        src,
        use_atomic,
        &mut guard,
        sync,
        preserve,
        verify,
        inline_src_hash,
    )
    .await
}

enum ScanMessage {
    Entry(PlanEntry),
    Done,
}

pub async fn pipeline_copy<F>(
    sources: &[PathBuf],
    dst: &Path,
    cli: &Commands,
    excludes: &[regex::Regex],
    progress_callback: F,
    on_new_file: impl Fn(&str, u64) + Send + Sync + 'static,
    on_total_update: impl Fn(u64) + Send + Sync + 'static,
    on_scan_complete: impl Fn() + Send + Sync + 'static,
    on_file_found: impl Fn(u64) + Send + Sync + 'static,
) -> std::result::Result<(), BcmrError>
where
    F: Fn(u64) + Send + Sync,
{
    let test_mode = cli.get_test_mode();
    let recursive = cli.is_recursive();
    let callback = ProgressCallback {
        callback: progress_callback,
        on_new_file: Box::new(on_new_file),
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel::<ScanMessage>(256);

    let sources = sources.to_vec();
    let dst = dst.to_path_buf();
    let excludes = excludes.to_vec();
    let scanner = tokio::task::spawn_blocking(move || {
        let dst_is_dir = dst.exists() && dst.is_dir();
        let mut total_size = 0u64;
        let mut files_found = 0u64;

        for src in &sources {
            if traversal::is_excluded(src, &excludes) {
                continue;
            }

            if src.is_file() {
                let dst_path = if dst_is_dir {
                    dst.join(match src.file_name() {
                        Some(n) => n,
                        None => continue,
                    })
                } else {
                    dst.clone()
                };

                let size = match src.metadata() {
                    Ok(m) => m.len(),
                    Err(e) => {
                        let _ = tx.blocking_send(ScanMessage::Done);
                        return Err(BcmrError::Io(e));
                    }
                };
                total_size += size;
                files_found += 1;
                on_total_update(total_size);
                on_file_found(files_found);

                if tx
                    .blocking_send(ScanMessage::Entry(PlanEntry::CopyFile {
                        src: src.clone(),
                        dst: dst_path,
                    }))
                    .is_err()
                {
                    return Ok(()); // receiver dropped, abort
                }
            } else if recursive && src.is_dir() {
                let src_name = match src.file_name() {
                    Some(n) => n,
                    None => continue,
                };
                let new_dst = if dst_is_dir {
                    dst.join(src_name)
                } else {
                    dst.clone()
                };

                if tx
                    .blocking_send(ScanMessage::Entry(PlanEntry::CreateDir {
                        src: src.clone(),
                        dst: new_dst.clone(),
                    }))
                    .is_err()
                {
                    return Ok(());
                }

                for entry in traversal::walk(src, true, false, 1, &excludes) {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(e) => {
                            let _ = tx.blocking_send(ScanMessage::Done);
                            return Err(BcmrError::WalkDir(e));
                        }
                    };
                    let path = entry.path();
                    let relative = match path.strip_prefix(src) {
                        Ok(r) => r,
                        Err(e) => {
                            let _ = tx.blocking_send(ScanMessage::Done);
                            return Err(BcmrError::StripPrefix(e));
                        }
                    };
                    let target = new_dst.join(relative);

                    if path.is_dir() {
                        if tx
                            .blocking_send(ScanMessage::Entry(PlanEntry::CreateDir {
                                src: path.to_path_buf(),
                                dst: target,
                            }))
                            .is_err()
                        {
                            return Ok(());
                        }
                    } else if path.is_file() {
                        let size = match entry.metadata() {
                            Ok(m) => m.len(),
                            Err(e) => {
                                let _ = tx.blocking_send(ScanMessage::Done);
                                return Err(BcmrError::WalkDir(e));
                            }
                        };
                        total_size += size;
                        files_found += 1;
                        on_total_update(total_size);
                        on_file_found(files_found);

                        if tx
                            .blocking_send(ScanMessage::Entry(PlanEntry::CopyFile {
                                src: path.to_path_buf(),
                                dst: target,
                            }))
                            .is_err()
                        {
                            return Ok(());
                        }
                    }
                }
            } else if src.is_dir() {
                let _ = tx.blocking_send(ScanMessage::Done);
                return Err(BcmrError::InvalidInput(format!(
                    "Source '{}' is a directory. Use -r flag for recursive copy.",
                    src.display()
                )));
            } else {
                let _ = tx.blocking_send(ScanMessage::Done);
                return Err(BcmrError::SourceNotFound(src.clone()));
            }
        }

        let _ = tx.blocking_send(ScanMessage::Done);
        Ok(())
    });

    let mut dir_entries: Vec<(PathBuf, PathBuf)> = Vec::new(); // (src, dst) for preserve

    while let Some(msg) = rx.recv().await {
        match msg {
            ScanMessage::Entry(entry) => match entry {
                PlanEntry::CreateDir { ref src, ref dst } => {
                    if !dst.exists() {
                        fs::create_dir_all(dst).await?;
                    }
                    dir_entries.push((src.clone(), dst.clone()));
                }
                PlanEntry::CopyFile {
                    ref src, ref dst
                } => {
                    if dst.exists() && !cli.is_force() && is_normal_write(cli) {
                        return Err(BcmrError::TargetExists(dst.clone()));
                    }

                    if dst.exists() && cli.is_force() && !is_normal_write(cli) {
                        fs::remove_file(dst).await?;
                    }

                    copy_file(
                        src,
                        dst,
                        CopyFileOptions::from_cli(cli, test_mode.clone()),
                        &callback,
                    )
                    .await?;

                    if cli.is_verbose() {
                        eprintln!("'{}' -> '{}'", src.display(), dst.display());
                    }
                }
            },
            ScanMessage::Done => {
                on_scan_complete();
                break;
            }
        }
    }

    scanner.await??;

    if cli.is_preserve() {
        for (src, dst) in dir_entries.iter().rev() {
            preserve_attributes(src, dst).await?;
        }
    }

    Ok(())
}

pub(crate) async fn verify_copy(
    src: &Path,
    dst: &Path,
    inline_src_hash: Option<blake3::Hash>,
) -> std::result::Result<(), BcmrError> {
    // 2-pass verification: if we have an inline source hash (computed during copy),
    // we only need to re-read the destination — saving one full file read.
    let src_hash_str = if let Some(h) = inline_src_hash {
        h.to_hex().to_string()
    } else {
        // Fallback: no inline hash available (reflink/copy_file_range path)
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
