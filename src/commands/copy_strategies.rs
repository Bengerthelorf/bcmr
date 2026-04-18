use crate::cli::SparseMode;
use crate::core::error::BcmrError;
use crate::core::io as durable_io;
use crate::core::session::{Session, CHECKPOINT_INTERVAL_BLOCKS, COPY_BLOCK_SIZE};
use std::path::Path;
use tokio::fs;

use super::copy::TempFileGuard;

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

/// Gate session creation on explicit intent (-C / -s / -a). A session
/// costs per-block hash + checkpoint fdatasync + session-file save;
/// creating one implicitly for large files showed up as a ~2x gap vs
/// `cp` on one-shot copies.
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
    if !(resume || append || strict) {
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
/// `need_src_hash` is tri-state: -V needs it, resume sessions store it
/// to detect source-changed-between-runs, everything else skips (BLAKE3
/// at ~1 GB/s would otherwise double wall time on large streaming copies).
///
/// One spawn_blocking owns the whole loop — tokio's async fs dispatches
/// a blocking task per read/write, which cost us ~1024 bounces per 2 GB
/// and ~6x throughput on Linux NVMe.
pub async fn streaming_copy(
    src_file: &mut tokio::fs::File,
    dst_file: &mut tokio::fs::File,
    session: &mut Option<Session>,
    sparse_mode: &SparseMode,
    start_offset: u64,
    need_src_hash: bool,
    callback: &(impl Fn(u64) + Send + Sync + Clone + 'static),
) -> Result<Option<blake3::Hash>, BcmrError> {
    let src_std = src_file.try_clone().await?.into_std().await;
    let dst_std = dst_file.try_clone().await?.into_std().await;
    let session_in = session.take();
    let sparse_mode = sparse_mode.clone();
    let cb = callback.clone();

    let join = tokio::task::spawn_blocking(move || {
        streaming_copy_sync(
            src_std,
            dst_std,
            session_in,
            &sparse_mode,
            start_offset,
            need_src_hash,
            cb,
        )
    });

    let (returned_session, hash) = join.await??;
    *session = returned_session;
    Ok(hash)
}

#[allow(clippy::too_many_arguments)]
fn streaming_copy_sync(
    mut src_file: std::fs::File,
    mut dst_file: std::fs::File,
    mut session: Option<Session>,
    sparse_mode: &SparseMode,
    start_offset: u64,
    need_src_hash: bool,
    callback: impl Fn(u64) + Send + Sync,
) -> Result<(Option<Session>, Option<blake3::Hash>), BcmrError> {
    use std::io::{Read, Seek, SeekFrom as StdSeekFrom, Write};

    const SPARSE_DETECT_SIZE: usize = 4096;

    let mut buffer = vec![0u8; COPY_BLOCK_SIZE as usize];
    let mut pending_hole = 0u64;
    let mut src_hasher = need_src_hash.then(blake3::Hasher::new);
    // Per-block hashes exist only to populate the session. With no
    // session (one-shot copy, no -C/-s/-a), skip entirely — the BLAKE3
    // pass was the largest remaining gap vs `cp`.
    let mut block_hasher = session.as_ref().map(|_| blake3::Hasher::new());
    let mut bytes_in_block = 0u64;
    let mut blocks_since_checkpoint = 0u32;

    loop {
        let n = src_file.read(&mut buffer)?;
        if n == 0 {
            break;
        }

        if let Some(h) = src_hasher.as_mut() {
            h.update(&buffer[..n]);
        }
        if let Some(h) = block_hasher.as_mut() {
            h.update(&buffer[..n]);
        }
        bytes_in_block += n as u64;

        match sparse_mode {
            SparseMode::Never => {
                dst_file.write_all(&buffer[..n])?;
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
                            dst_file.seek(StdSeekFrom::Current(pending_hole as i64))?;
                            pending_hole = 0;
                        }
                        dst_file.write_all(chunk)?;
                    }
                    offset = end;
                }
            }
        }

        callback(n as u64);

        if bytes_in_block >= COPY_BLOCK_SIZE {
            if let (Some(h), Some(s)) = (block_hasher.as_mut(), session.as_mut()) {
                let block_hash = h.finalize();
                s.add_block(*block_hash.as_bytes(), COPY_BLOCK_SIZE);
                *h = blake3::Hasher::new();
            }
            bytes_in_block -= COPY_BLOCK_SIZE;
            blocks_since_checkpoint += 1;

            if blocks_since_checkpoint >= CHECKPOINT_INTERVAL_BLOCKS {
                // Crash-safety invariant: every block in the session is
                // physically on disk before the session file records it.
                // No session → no reader of that promise → skip fsync.
                if let Some(ref s) = session {
                    durable_io::durable_sync(&dst_file)?;
                    let _ = s.save();
                }
                blocks_since_checkpoint = 0;

                #[cfg(target_os = "linux")]
                {
                    use std::os::unix::io::AsRawFd;
                    let pos = src_file.stream_position().unwrap_or(0);
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
        if let (Some(h), Some(s)) = (block_hasher, session.as_mut()) {
            let block_hash = h.finalize();
            s.add_block(*block_hash.as_bytes(), bytes_in_block);
        }
    }

    if pending_hole > 0 {
        let current_pos = dst_file.stream_position()?;
        dst_file.set_len(current_pos + pending_hole)?;
    }

    let final_hash = src_hasher.map(|h| h.finalize());
    if start_offset == 0 {
        if let (Some(ref mut s), Some(h)) = (session.as_mut(), final_hash) {
            s.set_src_hash(*h.as_bytes());
            let _ = s.save();
        }
        Ok((session, final_hash))
    } else {
        Ok((session, None))
    }
}
