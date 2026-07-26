use crate::cli::SparseMode;
use crate::core::error::BcmrError;
use crate::core::io as durable_io;
use crate::core::session::{Session, CHECKPOINT_INTERVAL_BLOCKS, COPY_BLOCK_SIZE};
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

use crate::commands::copy::AtomicStaging;

pub struct FinalizeParams<'a> {
    pub write_target: &'a Path,
    pub dst: &'a Path,
    pub src: &'a Path,
    pub use_atomic: bool,
    pub staging: Option<AtomicStaging>,
    pub replace_existing: bool,
    pub sync: bool,
    pub preserve: bool,
    pub verify: bool,
    pub inline_src_hash: Option<blake3::Hash>,
    pub corrupt_before_verify: bool,
}

pub async fn finalize(
    dst_file: tokio::fs::File,
    mut p: FinalizeParams<'_>,
) -> Result<(), BcmrError> {
    if p.use_atomic {
        let mut stage_file = Some(dst_file);
        let prepare = async {
            if p.corrupt_before_verify {
                // Test-only fault injection must close the writer before reopening
                // the stage, which also exercises Windows handle discipline.
                drop(stage_file.take());
                let mut stage = tokio::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(p.write_target)
                    .await?;
                let mut first_byte = [0u8; 1];
                stage.read_exact(&mut first_byte).await?;
                stage.seek(SeekFrom::Start(0)).await?;
                first_byte[0] ^= 0xff;
                stage.write_all(&first_byte).await?;
                stage.sync_data().await?;
            }
            if p.verify {
                // verify_copy removes its second argument on a mismatch.  This must
                // remain the private stage, never the pre-existing final destination.
                super::copy::verify_copy(p.src, p.write_target, p.inline_src_hash).await?;
            }
            if p.preserve {
                super::copy::preserve_attributes(p.src, p.write_target).await?;
            }
            if p.sync {
                if let Some(ref stage_file) = stage_file {
                    durable_io::durable_sync_async(stage_file).await?;
                }
            }
            Ok::<(), BcmrError>(())
        }
        .await;
        drop(stage_file);
        prepare?;

        let staging = p.staging.take().ok_or_else(|| {
            BcmrError::InvalidInput("atomic finalization requires a staging file".into())
        })?;
        staging.commit(p.dst, p.replace_existing)?;
        if p.sync {
            if let Some(parent) = p.dst.parent() {
                durable_io::fsync_dir_async(parent).await;
            }
        }
    } else {
        // Resume/append/strict retain their pre-existing direct-path ordering.
        if p.sync {
            durable_io::durable_sync_async(&dst_file).await?;
        }
        drop(dst_file);
        if p.preserve {
            super::copy::preserve_attributes(p.src, p.write_target).await?;
        }
        if p.verify {
            super::copy::verify_copy(p.src, p.write_target, p.inline_src_hash).await?;
        }
    }

    Session::remove(p.src, p.dst);
    Ok(())
}

#[derive(Clone, Copy)]
pub struct SessionIntent {
    pub resume: bool,
    pub append: bool,
    pub strict: bool,
}

impl SessionIntent {
    fn supports_checkpoint(&self) -> bool {
        self.resume && !self.append && !self.strict
    }
}

pub fn create_session(
    src: &Path,
    dst: &Path,
    file_size: u64,
    start_offset: u64,
    intent: SessionIntent,
    loaded_session: &Option<Session>,
) -> Option<Session> {
    if !intent.supports_checkpoint() {
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
        if !start_offset.is_multiple_of(COPY_BLOCK_SIZE) {
            return None;
        }
        let loaded = loaded_session.as_ref()?;
        if !loaded.has_valid_resume_structure() {
            return None;
        }
        let keep = usize::try_from(start_offset / COPY_BLOCK_SIZE).ok()?;
        if keep > loaded.block_hashes.len() {
            return None;
        }
        s.block_hashes = loaded.block_hashes[..keep].to_vec();
        s.bytes_written = start_offset;
    }

    Some(s)
}

pub async fn streaming_copy(
    src_file: &mut tokio::fs::File,
    dst_file: &mut tokio::fs::File,
    session: &mut Option<Session>,
    sparse_mode: &SparseMode,
    start_offset: u64,
    need_src_hash: bool,
    callback: &(impl Fn(u64) + Send + Sync + Clone + 'static),
) -> Result<Option<blake3::Hash>, BcmrError> {
    // dup fds into std handles so the whole copy loop runs under one
    // spawn_blocking — tokio::fs would dispatch per-read/-write, costing
    // ~1024 pool bounces per 2 GB (≈6× slowdown observed on Linux NVMe).
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

fn persist_checkpoint_with<SyncFn, SaveFn>(
    dst_file: &std::fs::File,
    session: &Session,
    sync_destination: SyncFn,
    save_session: SaveFn,
) -> Result<(), BcmrError>
where
    SyncFn: FnOnce(&std::fs::File) -> std::io::Result<()>,
    SaveFn: FnOnce(&Session) -> std::io::Result<()>,
{
    // Sparse writes may still be represented only by a pending seek. Publish
    // the logical EOF before making this byte range durable and discoverable.
    if dst_file.metadata()?.len() < session.bytes_written {
        dst_file.set_len(session.bytes_written)?;
    }
    sync_destination(dst_file)?;
    save_session(session)?;
    Ok(())
}

fn persist_checkpoint(dst_file: &std::fs::File, session: &Session) -> Result<(), BcmrError> {
    persist_checkpoint_with(dst_file, session, durable_io::durable_sync, Session::save)
}

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
                if let Some(ref s) = session {
                    persist_checkpoint(&dst_file, s)?;
                }
                blocks_since_checkpoint = 0;

                #[cfg(target_os = "linux")]
                {
                    use std::os::unix::io::AsRawFd;
                    let src_end = src_file.stream_position().unwrap_or(0) as libc::off_t;
                    let dst_end = dst_file.stream_position().unwrap_or(0) as libc::off_t;
                    unsafe {
                        libc::posix_fadvise(
                            src_file.as_raw_fd(),
                            0,
                            src_end,
                            libc::POSIX_FADV_DONTNEED,
                        );
                        libc::posix_fadvise(
                            dst_file.as_raw_fd(),
                            0,
                            dst_end,
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
        if let (Some(ref mut s), Some(h)) = (session.as_mut(), final_hash.as_ref()) {
            s.set_src_hash(*h.as_bytes());
        }
    }
    if let Some(ref s) = session {
        persist_checkpoint(&dst_file, s)?;
    }

    Ok((session, if start_offset == 0 { final_hash } else { None }))
}

#[cfg(test)]
mod checkpoint_tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::io;

    #[test]
    fn checkpoint_materializes_and_syncs_before_publish_and_propagates_failures() {
        let destination = tempfile::tempfile().unwrap();
        let mut session = Session::new(Path::new("/source"), Path::new("/destination"), 4096, 0, 0);
        session.add_block([0; 32], 4096);
        let events = RefCell::new(Vec::new());

        let publish_error = persist_checkpoint_with(
            &destination,
            &session,
            |file| {
                assert_eq!(
                    file.metadata().unwrap().len(),
                    session.bytes_written,
                    "a pending sparse hole must have a logical EOF before sync"
                );
                events.borrow_mut().push("sync");
                Ok(())
            },
            |_| {
                assert_eq!(events.borrow().as_slice(), ["sync"]);
                events.borrow_mut().push("save");
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected session publish failure",
                ))
            },
        )
        .expect_err("session save failures must reach the copy caller");
        assert!(matches!(publish_error, BcmrError::Io(_)));
        assert_eq!(events.borrow().as_slice(), ["sync", "save"]);

        let save_called = Cell::new(false);
        let sync_error = persist_checkpoint_with(
            &destination,
            &session,
            |_| Err(io::Error::other("injected destination sync failure")),
            |_| {
                save_called.set(true);
                Ok(())
            },
        )
        .expect_err("destination sync failures must reach the copy caller");
        assert!(matches!(sync_error, BcmrError::Io(_)));
        assert!(
            !save_called.get(),
            "a session must not publish after destination sync fails"
        );

        let written_ahead = session.bytes_written + 123;
        destination.set_len(written_ahead).unwrap();
        persist_checkpoint_with(&destination, &session, |_| Ok(()), |_| Ok(())).unwrap();
        assert_eq!(
            destination.metadata().unwrap().len(),
            written_ahead,
            "publishing an older block boundary must not truncate bytes already written ahead"
        );
    }

    #[test]
    fn session_is_not_created_for_an_unproved_nonzero_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("source");
        let dst = dir.path().join("destination");
        std::fs::write(&src, b"complete source").unwrap();

        let session = create_session(
            &src,
            &dst,
            src.metadata().unwrap().len(),
            8,
            SessionIntent {
                resume: false,
                append: true,
                strict: false,
            },
            &None,
        );

        assert!(
            session.is_none(),
            "checkpoint bytes are absolute; an unhashed destination prefix cannot be published"
        );
    }
}
