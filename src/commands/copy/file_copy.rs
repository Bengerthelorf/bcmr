use crate::cli::{Commands, SparseMode, TestMode};
use crate::core::error::BcmrError;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::fs::{self, File};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

use super::{ProgressCallback, TempFileGuard};

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
    sync: bool,
    reflink_arg: Option<String>,
    sparse_arg: Option<String>,
    test_mode: TestMode,
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
            },
            sync: cli.is_sync(),
            reflink_arg: cli.get_reflink_mode(),
            sparse_arg: cli.get_sparse_mode(),
            test_mode,
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
        sync,
        ref reflink_arg,
        ref sparse_arg,
        test_mode,
    } = opts;
    let crate::core::remote::TransferOptions {
        preserve,
        verify,
        resume,
        strict,
        append,
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

    if super::super::copy_strategies::try_reflink(
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
        let ctx = FinalizeCtx {
            write_target: &write_target,
            dst,
            src,
            use_atomic,
            guard: &mut guard,
            sync,
            preserve,
            verify,
            inline_src_hash: None,
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
                    guard: &mut guard,
                    sync,
                    preserve,
                    verify,
                    inline_src_hash: None,
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
        TestMode::None => {
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
        guard: &mut guard,
        sync,
        preserve,
        verify,
        inline_src_hash,
    };
    run_finalize(ctx, dst_file).await
}
