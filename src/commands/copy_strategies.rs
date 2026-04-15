use crate::cli::SparseMode;
use crate::core::error::BcmrError;
use crate::core::io as durable_io;
use crate::core::session::{Session, CHECKPOINT_INTERVAL_BLOCKS, COPY_BLOCK_SIZE};
use std::path::Path;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

use super::copy::TempFileGuard;

/// Post-copy: sync, rename, preserve, verify, session cleanup.
#[allow(clippy::too_many_arguments)]
pub async fn finalize(
    dst_file: tokio::fs::File,
    write_target: &Path,
    dst: &Path,
    src: &Path,
    use_atomic: bool,
    guard: &mut Option<TempFileGuard>,
    sync: bool,
    preserve: bool,
    verify: bool,
    inline_src_hash: Option<blake3::Hash>,
) -> Result<(), BcmrError> {
    if sync {
        durable_io::durable_sync_async(&dst_file).await?;
    }
    drop(dst_file);

    if use_atomic {
        fs::rename(write_target, dst).await?;
        if sync {
            if let Some(parent) = dst.parent() {
                durable_io::fsync_dir_async(parent).await;
            }
        }
        if let Some(ref mut g) = guard {
            g.disarm();
        }
    }

    if preserve {
        super::copy::preserve_attributes(src, dst).await?;
    }

    if verify {
        super::copy::verify_copy(src, dst, inline_src_hash).await?;
    }

    Session::remove(src, dst);
    Ok(())
}

/// Returns Ok(true) if reflink succeeded.
pub async fn try_reflink(
    src: &Path,
    write_target: &Path,
    file_size: u64,
    try_reflink: bool,
    fail_on_error: bool,
    sparse_mode: &SparseMode,
    callback: &impl Fn(u64),
) -> Result<bool, BcmrError> {
    if !try_reflink || matches!(sparse_mode, SparseMode::Always) {
        return Ok(false);
    }

    let src_path = src.to_path_buf();
    let target_path = write_target.to_path_buf();
    let result =
        tokio::task::spawn_blocking(move || reflink_copy::reflink(&src_path, &target_path)).await?;

    match result {
        Ok(_) => {
            callback(file_size);
            Ok(true)
        }
        Err(e) => {
            if fail_on_error {
                return Err(BcmrError::Reflink(format!(
                    "Reflink failed (forced): {}",
                    e
                )));
            }
            Ok(false)
        }
    }
}

/// Create a session, carrying forward block hashes from a prior session if resuming.
#[allow(clippy::too_many_arguments)]
pub fn create_session(
    src: &Path,
    dst: &Path,
    file_size: u64,
    start_offset: u64,
    resume: bool,
    append: bool,
    strict: bool,
    loaded_session: &Option<Session>,
) -> Option<Session> {
    if !(resume || append || strict || file_size > 64 * 1024 * 1024) {
        return None;
    }

    let src_meta = src.metadata().ok()?;
    let src_mtime = src_meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let src_inode = durable_io::get_inode(src).unwrap_or(0);
    let mut s = Session::new(src, dst, file_size, src_mtime, src_inode);

    if start_offset > 0 {
        if let Some(ref loaded) = loaded_session {
            let keep = (start_offset / COPY_BLOCK_SIZE) as usize;
            let keep = keep.min(loaded.block_hashes.len());
            s.block_hashes = loaded.block_hashes[..keep].to_vec();
            s.bytes_written = keep as u64 * COPY_BLOCK_SIZE;
        }
    }

    Some(s)
}

/// Returns the inline source hash if the full file was copied (start_offset == 0).
///
/// `need_src_hash` controls whether we compute the whole-source BLAKE3 at
/// all. Verify (-V) needs it, and so do file-level resumes (the session
/// stores it to detect source-changed-between-runs). Otherwise it's pure
/// overhead --- on macOS NEON BLAKE3 runs at ~1 GB/s, so the unused source
/// hash doubled wall time on streaming copies of large files.
pub async fn streaming_copy(
    src_file: &mut tokio::fs::File,
    dst_file: &mut tokio::fs::File,
    session: &mut Option<Session>,
    sparse_mode: &SparseMode,
    start_offset: u64,
    need_src_hash: bool,
    callback: &impl Fn(u64),
) -> Result<Option<blake3::Hash>, BcmrError> {
    const SPARSE_DETECT_SIZE: usize = 4096;

    let mut buffer = vec![0u8; COPY_BLOCK_SIZE as usize];
    let mut pending_hole = 0u64;
    let mut src_hasher = need_src_hash.then(blake3::Hasher::new);
    let mut block_hasher = blake3::Hasher::new();
    let mut bytes_in_block = 0u64;
    let mut blocks_since_checkpoint = 0u32;

    loop {
        let n = src_file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }

        if let Some(h) = src_hasher.as_mut() {
            h.update(&buffer[..n]);
        }
        block_hasher.update(&buffer[..n]);
        bytes_in_block += n as u64;

        match sparse_mode {
            SparseMode::Never => {
                dst_file.write_all(&buffer[..n]).await?;
            }
            SparseMode::Always | SparseMode::Auto => {
                let min_block = if matches!(sparse_mode, SparseMode::Always) {
                    1
                } else {
                    SPARSE_DETECT_SIZE
                };
                let mut offset = 0;
                while offset < n {
                    let end = (offset + SPARSE_DETECT_SIZE).min(n);
                    let chunk = &buffer[offset..end];
                    if chunk.len() >= min_block && chunk.iter().all(|&b| b == 0) {
                        pending_hole += chunk.len() as u64;
                    } else {
                        if pending_hole > 0 {
                            dst_file
                                .seek(SeekFrom::Current(pending_hole as i64))
                                .await?;
                            pending_hole = 0;
                        }
                        dst_file.write_all(chunk).await?;
                    }
                    offset = end;
                }
            }
        }

        callback(n as u64);

        if bytes_in_block >= COPY_BLOCK_SIZE {
            let block_hash = block_hasher.finalize();
            if let Some(ref mut s) = session {
                s.add_block(*block_hash.as_bytes(), COPY_BLOCK_SIZE);
            }
            block_hasher = blake3::Hasher::new();
            bytes_in_block -= COPY_BLOCK_SIZE;
            blocks_since_checkpoint += 1;

            if blocks_since_checkpoint >= CHECKPOINT_INTERVAL_BLOCKS {
                durable_io::durable_sync_async(dst_file).await?;
                if let Some(ref s) = session {
                    let _ = s.save();
                }
                blocks_since_checkpoint = 0;

                #[cfg(target_os = "linux")]
                {
                    use std::os::unix::io::AsRawFd;
                    let pos = src_file.stream_position().await.unwrap_or(0);
                    let evict_end = pos as libc::off_t;
                    unsafe {
                        libc::posix_fadvise(
                            src_file.as_raw_fd(),
                            0,
                            evict_end,
                            libc::POSIX_FADV_DONTNEED,
                        );
                        libc::posix_fadvise(
                            dst_file.as_raw_fd(),
                            0,
                            evict_end,
                            libc::POSIX_FADV_DONTNEED,
                        );
                    }
                }
            }
        }
    }

    if bytes_in_block > 0 {
        let block_hash = block_hasher.finalize();
        if let Some(ref mut s) = session {
            s.add_block(*block_hash.as_bytes(), bytes_in_block);
        }
    }

    if pending_hole > 0 {
        let current_pos = dst_file.stream_position().await?;
        dst_file.set_len(current_pos + pending_hole).await?;
    }

    let final_hash = src_hasher.map(|h| h.finalize());
    if start_offset == 0 {
        if let (Some(ref mut s), Some(h)) = (session.as_mut(), final_hash) {
            s.set_src_hash(*h.as_bytes());
            let _ = s.save();
        }
        Ok(final_hash)
    } else {
        Ok(None)
    }
}
