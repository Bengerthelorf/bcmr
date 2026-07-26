use crate::cli::{Commands, SparseMode, TestMode};
use crate::core::error::BcmrError;

use std::path::Path;
use std::time::{Duration, Instant};
use tempfile::TempPath;
use tokio::fs::{self, File};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

use super::exec::ProgressCallback;
use crate::core::cleanup::TempFileGuard;

pub(crate) struct AtomicStaging {
    path: TempPath,
    guard: TempFileGuard,
}

impl AtomicStaging {
    pub(crate) fn path(&self) -> &Path {
        self.path.as_ref()
    }

    pub(crate) fn commit(self, dst: &Path, replace_existing: bool) -> Result<(), BcmrError> {
        let AtomicStaging { path, mut guard } = self;
        let persisted = if replace_existing {
            path.persist(dst)
        } else {
            path.persist_noclobber(dst)
        };
        match persisted {
            Ok(()) => {
                guard.disarm();
                Ok(())
            }
            Err(error) => {
                let is_target_exists = error.error.kind() == std::io::ErrorKind::AlreadyExists;
                drop(error.path);
                if !replace_existing && is_target_exists {
                    Err(BcmrError::TargetExists(dst.to_path_buf()))
                } else {
                    Err(BcmrError::Io(error.error))
                }
            }
        }
    }

    fn relinquish_cleanup(&mut self) {
        self.guard.disarm();
        self.path.disable_cleanup(true);
    }

    fn try_reflink_create_new<F>(
        mut self,
        fail_on_error: bool,
        operation: F,
    ) -> Result<(Self, bool), BcmrError>
    where
        F: FnOnce(&Path) -> std::io::Result<()>,
    {
        std::fs::remove_file(self.path())?;
        // reflink-copy uses create-new on every supported platform. Linux and
        // Windows wrap partial destinations in AutoRemovedFile; macOS clonefile
        // creates the destination as one operation. Therefore an error leaves
        // no file owned by that operation at this path.
        match operation(self.path()) {
            Ok(()) => Ok((self, true)),
            Err(error) if fail_on_error => {
                // The create-new operation owns cleanup of any partial file.
                // Relinquish our path because an EEXIST path may belong to a
                // competing creator and must never be removed by our guards.
                self.relinquish_cleanup();
                Err(BcmrError::Reflink(format!(
                    "Reflink failed (forced): {error}"
                )))
            }
            Err(_) => match create_new_stage_file(self.path()) {
                Ok(file) => {
                    drop(file);
                    Ok((self, false))
                }
                Err(error) => {
                    self.relinquish_cleanup();
                    Err(BcmrError::Io(error))
                }
            },
        }
    }
}

fn create_new_stage_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o666);
    }
    options.open(path)
}

#[cfg(any(target_os = "linux", test))]
fn reset_copy_file_range_stage_for_fallback(file: &std::fs::File) -> std::io::Result<()> {
    file.set_len(0)
}

pub(crate) fn create_staging(dst: &Path) -> Result<AtomicStaging, BcmrError> {
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    let mut builder = tempfile::Builder::new();
    // Keep this prefix short: a legal 255-byte final name still needs room
    // for tempfile's random suffix on filesystems with NAME_MAX=255.
    builder.prefix(".bcmr.stage.").suffix(".tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // tempfile defaults to 0600.  Local copy historically creates files as
        // 0666 filtered by umask, so preserve that ordinary-copy behavior.
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    let file = builder.tempfile_in(parent)?;
    let path = file.into_temp_path();
    let guard = TempFileGuard::new(path.to_path_buf());
    Ok(AtomicStaging { path, guard })
}

async fn try_atomic_reflink(
    src: &Path,
    staging: AtomicStaging,
    file_size: u64,
    try_reflink: bool,
    fail_on_error: bool,
    sparse_mode: &SparseMode,
    callback: &impl Fn(u64),
) -> Result<(AtomicStaging, bool), BcmrError> {
    if !try_reflink || matches!(sparse_mode, SparseMode::Always) {
        return Ok((staging, false));
    }

    let src = src.to_path_buf();
    let (staging, reflinked) = tokio::task::spawn_blocking(move || {
        staging.try_reflink_create_new(fail_on_error, |dst| reflink_copy::reflink(&src, dst))
    })
    .await??;

    if reflinked {
        callback(file_size);
    }
    Ok((staging, reflinked))
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
                    if let Err(reset_error) = reset_copy_file_range_stage_for_fallback(&dst_file) {
                        return Some(Err(BcmrError::Io(reset_error)));
                    }
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn copy_xattrs(src: &Path, dst: &Path) -> std::result::Result<(), BcmrError> {
    let names = match xattr::list(src) {
        Ok(n) => n,
        Err(e) if is_unsupported(&e) => return Ok(()),
        Err(e) => return Err(BcmrError::Io(e)),
    };
    for name in names {
        let value = match xattr::get(src, &name) {
            Ok(Some(v)) => v,
            Ok(None) => continue,
            Err(e) if is_unsupported(&e) => continue,
            Err(_) => continue,
        };
        let _ = xattr::set(dst, &name, &value);
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn is_unsupported(e: &std::io::Error) -> bool {
    // 95 = ENOTSUP on Linux, 45 = ENOTSUP on macOS.
    matches!(e.raw_os_error(), Some(95) | Some(45))
}

pub(super) struct CopyFileOptions {
    transfer: crate::core::remote::TransferOptions,
    reflink_arg: Option<String>,
    sparse_arg: Option<String>,
    test_mode: TestMode,
    replace_existing: bool,
}

impl CopyFileOptions {
    pub(super) fn from_cli(cli: &Commands, test_mode: TestMode) -> Self {
        Self {
            transfer: crate::core::remote::TransferOptions {
                preserve: cli.is_preserve(),
                verify: cli.is_verify(),
                resume: cli.is_resume(),
                strict: cli.is_strict(),
                append: cli.is_append(),
                sync: cli.is_sync(),
            },
            reflink_arg: cli.get_reflink_mode(),
            sparse_arg: cli.get_sparse_mode(),
            test_mode,
            replace_existing: cli.is_force(),
        }
    }
}

fn resolve_reflink_mode(arg: &Option<String>) -> (bool, bool) {
    let mode_str = arg
        .as_deref()
        .unwrap_or(&crate::config::CONFIG.copy.reflink);
    match mode_str.to_lowercase().as_str() {
        "force" => (true, true),
        "disable" | "never" => (false, false),
        _ => (true, false),
    }
}

fn resolve_sparse_mode(arg: &Option<String>) -> SparseMode {
    let mode_str = arg.as_deref().unwrap_or(&crate::config::CONFIG.copy.sparse);
    match mode_str.to_lowercase().as_str() {
        "force" => SparseMode::Always,
        "disable" | "never" => SparseMode::Never,
        _ => SparseMode::Auto,
    }
}

type FinalizeCtx<'a> = super::super::copy_strategies::FinalizeParams<'a>;

async fn run_finalize(
    ctx: FinalizeCtx<'_>,
    dst_file: fs::File,
) -> std::result::Result<(), BcmrError> {
    super::super::copy_strategies::finalize(dst_file, ctx).await
}

pub(super) async fn copy_file<F>(
    src: &Path,
    dst: &Path,
    opts: CopyFileOptions,
    callback: &ProgressCallback<F>,
) -> std::result::Result<(), BcmrError>
where
    F: Fn(u64) + Send + Sync + Clone + 'static,
{
    let CopyFileOptions {
        transfer,
        ref reflink_arg,
        ref sparse_arg,
        test_mode,
        replace_existing,
    } = opts;
    let crate::core::remote::TransferOptions {
        preserve,
        verify,
        resume,
        strict,
        append,
        sync,
    } = transfer;

    let file_size = src.metadata()?.len();
    let file_name = src
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    (*callback.on_new_file)(&file_name, file_size);

    let (try_reflink, fail_on_error) = resolve_reflink_mode(reflink_arg);
    let sparse_mode = resolve_sparse_mode(sparse_arg);

    if let Some(parent) = dst.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).await?;
        }
    }

    let use_atomic = !resume && !append && !strict;
    let corrupt_before_verify = matches!(test_mode, TestMode::CorruptBeforeFinalize);
    let write_target;
    let mut staging = None;

    if use_atomic {
        let stage = create_staging(dst)?;
        write_target = stage.path().to_path_buf();
        staging = Some(stage);
    } else {
        write_target = dst.to_path_buf();
    }

    let reflinked = if let Some(stage) = staging.take() {
        let (stage, reflinked) = try_atomic_reflink(
            src,
            stage,
            file_size,
            try_reflink,
            fail_on_error,
            &sparse_mode,
            &callback.callback,
        )
        .await?;
        staging = Some(stage);
        reflinked
    } else {
        super::super::copy_strategies::try_reflink(
            src,
            &write_target,
            file_size,
            try_reflink,
            fail_on_error,
            &sparse_mode,
            &callback.callback,
        )
        .await?
    };

    if reflinked {
        (callback.on_reflink)();
        let ctx = FinalizeCtx {
            write_target: &write_target,
            dst,
            src,
            use_atomic,
            staging: staging.take(),
            replace_existing,
            sync,
            preserve,
            verify,
            inline_src_hash: None,
            corrupt_before_verify,
        };
        return run_finalize(ctx, fs::File::open(&write_target).await?).await;
    }

    #[cfg(target_os = "linux")]
    if use_atomic && matches!(test_mode, TestMode::None) && matches!(sparse_mode, SparseMode::Never)
    {
        match try_copy_file_range(src, &write_target, file_size, &callback.callback).await {
            Some(Ok(())) => {
                let ctx = FinalizeCtx {
                    write_target: &write_target,
                    dst,
                    src,
                    use_atomic,
                    staging: staging.take(),
                    replace_existing,
                    sync,
                    preserve,
                    verify,
                    inline_src_hash: None,
                    corrupt_before_verify,
                };
                return run_finalize(ctx, fs::File::open(&write_target).await?).await;
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

    let mut session = super::super::copy_strategies::create_session(
        src,
        dst,
        file_size,
        start_offset,
        super::super::copy_strategies::SessionIntent {
            resume,
            append,
            strict,
        },
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
        TestMode::None | TestMode::CorruptBeforeFinalize => {
            let need_src_hash = verify || session.is_some();
            super::super::copy_strategies::streaming_copy(
                &mut src_file,
                &mut dst_file,
                &mut session,
                &sparse_mode,
                start_offset,
                need_src_hash,
                &callback.callback,
            )
            .await?
        }
    };

    let ctx = FinalizeCtx {
        write_target: &write_target,
        dst,
        src,
        use_atomic,
        staging: staging.take(),
        replace_existing,
        sync,
        preserve,
        verify,
        inline_src_hash,
        corrupt_before_verify,
    };
    run_finalize(ctx, dst_file).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::{Arc, Barrier};

    #[test]
    fn unique_sibling_staging_isolated_from_other_staging_files() {
        let dir = tempfile::tempdir().unwrap();
        let dst = Arc::new(dir.path().join("destination.bin"));
        let preoccupied = dir.path().join(".bcmr.stage.preoccupied.tmp");

        #[cfg(unix)]
        std::os::unix::fs::symlink("nowhere", &preoccupied).unwrap();
        #[cfg(not(unix))]
        std::fs::write(&preoccupied, b"occupied").unwrap();

        let barrier = Arc::new(Barrier::new(16));
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let dst = Arc::clone(&dst);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    create_staging(&dst).unwrap()
                })
            })
            .collect();
        let mut stages: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let paths: HashSet<_> = stages
            .iter()
            .map(|stage| stage.path().to_path_buf())
            .collect();
        assert_eq!(paths.len(), 16, "every concurrent copy needs its own stage");
        for path in &paths {
            assert_eq!(path.parent(), Some(dir.path()));
            assert!(
                path.is_file(),
                "stage must be a regular file: {}",
                path.display()
            );
            assert!(!path.is_symlink(), "stage must not follow a symlink");
        }
        assert!(
            preoccupied.symlink_metadata().is_ok(),
            "preoccupied path changed"
        );

        let removed = stages.pop().unwrap();
        let removed_path = removed.path().to_path_buf();
        drop(removed);
        assert!(!removed_path.exists());
        for stage in stages {
            assert!(
                stage.path().is_file(),
                "one cleanup must not remove another stage"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn staging_uses_the_normal_create_mode_before_umask() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let control = dir.path().join("ordinary-create.bin");
        std::fs::File::create(&control).unwrap();
        let stage = create_staging(&dir.path().join("destination.bin")).unwrap();
        let mode = std::fs::metadata(stage.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let control_mode = std::fs::metadata(&control).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, control_mode,
            "staging must honor the active umask like File::create"
        );
    }

    #[test]
    fn staging_allows_a_maximum_length_final_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("d".repeat(255));
        let stage = create_staging(&dst).unwrap();
        assert_eq!(stage.path().parent(), Some(dir.path()));
    }

    #[test]
    fn no_replace_commit_rejects_a_target_created_after_preflight() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("destination.bin");
        let stage = create_staging(&dst).unwrap();
        std::fs::write(stage.path(), b"new bytes").unwrap();

        // This models a concurrent creator winning after the UI preflight but
        // before the actual commit syscall.
        let old_bytes = b"racing writer wins";
        std::fs::write(&dst, old_bytes).unwrap();
        let error = stage.commit(&dst, false).unwrap_err();

        assert!(matches!(error, BcmrError::TargetExists(path) if path == dst));
        assert_eq!(std::fs::read(&dst).unwrap(), old_bytes);
        let remaining_stages: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".bcmr.stage.")
            })
            .collect();
        assert!(remaining_stages.is_empty());
    }

    #[test]
    fn reflink_operation_gets_unreserved_path_and_stage_still_commits() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();

        let (stage, reflinked) = stage
            .try_reflink_create_new(false, |path| {
                assert!(!path.exists(), "reflink must receive an absent path");
                let mut cloned = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)?;
                cloned.write_all(b"reflink result")
            })
            .unwrap();

        assert!(reflinked);
        assert_eq!(stage.path(), stage_path);
        assert_eq!(std::fs::read(stage.path()).unwrap(), b"reflink result");
        stage.commit(&final_path, true).unwrap();
        assert_eq!(std::fs::read(final_path).unwrap(), b"reflink result");
    }

    #[test]
    fn automatic_reflink_failure_recreates_the_same_exclusive_reservation() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();

        let (stage, reflinked) = stage
            .try_reflink_create_new(false, |path| {
                assert_eq!(path, stage_path);
                assert!(!path.exists(), "reflink must receive an absent path");
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "simulated unsupported reflink",
                ))
            })
            .unwrap();

        assert!(!reflinked);
        assert_eq!(stage.path(), stage_path);
        assert!(stage.path().is_file(), "fallback must own a regular file");
        let error = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stage.path())
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn copy_file_range_fallback_keeps_the_exclusive_stage_reservation() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();
        std::fs::write(&stage_path, b"partial copy_file_range bytes").unwrap();
        let stage_file = std::fs::OpenOptions::new()
            .write(true)
            .open(&stage_path)
            .unwrap();

        reset_copy_file_range_stage_for_fallback(&stage_file).unwrap();
        drop(stage_file);

        assert!(stage_path.is_file());
        assert_eq!(std::fs::metadata(&stage_path).unwrap().len(), 0);
        let error = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage_path)
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn automatic_reflink_reservation_race_does_not_delete_competing_file() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        std::fs::write(&final_path, b"old final bytes").unwrap();
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();
        let sentinel = b"competing writer's bytes";

        let result = stage.try_reflink_create_new(false, |path| {
            assert!(!path.exists(), "reflink must receive an absent path");
            std::fs::write(path, sentinel)?;
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "simulated failed reflink after a competing create",
            ))
        });

        assert!(
            matches!(result, Err(BcmrError::Io(ref error)) if error.kind() == std::io::ErrorKind::AlreadyExists)
        );
        assert_eq!(std::fs::read(&stage_path).unwrap(), sentinel);
        assert_eq!(std::fs::read(&final_path).unwrap(), b"old final bytes");
    }

    #[test]
    fn forced_reflink_failure_keeps_contract_without_deleting_competing_file() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        std::fs::write(&final_path, b"old final bytes").unwrap();
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();
        let sentinel = b"competing writer's bytes";

        let result = stage.try_reflink_create_new(true, |path| {
            assert!(!path.exists(), "reflink must receive an absent path");
            std::fs::write(path, sentinel)?;
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "simulated forced reflink failure",
            ))
        });

        assert!(matches!(result, Err(BcmrError::Reflink(_))));
        assert_eq!(std::fs::read(&stage_path).unwrap(), sentinel);
        assert_eq!(std::fs::read(&final_path).unwrap(), b"old final bytes");
    }

    #[cfg(unix)]
    #[test]
    fn automatic_reflink_reservation_race_does_not_follow_or_delete_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        std::fs::write(&final_path, b"old final bytes").unwrap();
        let sentinel_target = dir.path().join("sentinel-target.bin");
        std::fs::write(&sentinel_target, b"sentinel target bytes").unwrap();
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();

        let result = stage.try_reflink_create_new(false, |path| {
            assert!(!path.exists(), "reflink must receive an absent path");
            std::os::unix::fs::symlink(&sentinel_target, path)?;
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "simulated failed reflink after a competing symlink",
            ))
        });

        assert!(
            matches!(result, Err(BcmrError::Io(ref error)) if error.kind() == std::io::ErrorKind::AlreadyExists)
        );
        assert!(stage_path
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read(&sentinel_target).unwrap(),
            b"sentinel target bytes"
        );
        assert_eq!(std::fs::read(&final_path).unwrap(), b"old final bytes");
    }
}
