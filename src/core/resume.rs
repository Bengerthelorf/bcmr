use crate::core::checksum;
use crate::core::error::BcmrError;
use crate::core::io as durable_io;
use crate::core::session::Session;
use std::path::Path;

enum Decision {
    Resume,
    AlreadyComplete,
    Overwrite,
}

pub struct ResumeState {
    pub start_offset: u64,
    pub already_complete: bool,
    pub loaded_session: Option<Session>,
}

pub async fn resolve(
    src: &Path,
    dst: &Path,
    file_size: u64,
    resume: bool,
    strict: bool,
    append: bool,
    callback: &impl Fn(u64),
) -> Result<ResumeState, BcmrError> {
    if !(resume || append || strict) || !dst.exists() {
        return Ok(ResumeState {
            start_offset: 0,
            already_complete: false,
            loaded_session: None,
        });
    }

    let dst_len = dst.metadata()?.len();
    let mut loaded_session = load_and_validate_session(src, dst, file_size)?;

    let decision = if strict {
        resolve_strict(src, dst, file_size, dst_len).await?
    } else if append {
        resolve_append(file_size, dst_len)
    } else if let Some(ref session) = loaded_session {
        resolve_with_session(file_size, dst_len, session)
    } else {
        resolve_mtime(src, dst, file_size, dst_len)?
    };

    match decision {
        Decision::AlreadyComplete => {
            callback(file_size);
            return Ok(ResumeState {
                start_offset: 0,
                already_complete: true,
                loaded_session,
            });
        }
        Decision::Overwrite => {
            return Ok(ResumeState {
                start_offset: 0,
                already_complete: false,
                loaded_session,
            });
        }
        Decision::Resume => {}
    }

    // find_resume_offset does blocking disk I/O — offloaded so `-r --jobs N`
    // doesn't starve the runtime.
    let start_offset = if let Some(session) = loaded_session.take() {
        let dst_pb = dst.to_path_buf();
        let (verified, session) = tokio::task::spawn_blocking(move || {
            let v = session.find_resume_offset(&dst_pb);
            (v, session)
        })
        .await?;
        loaded_session = Some(session);
        verified
    } else {
        dst_len
    };

    if start_offset > 0 {
        callback(start_offset);
    }

    Ok(ResumeState {
        start_offset,
        already_complete: false,
        loaded_session,
    })
}

fn load_and_validate_session(
    src: &Path,
    dst: &Path,
    file_size: u64,
) -> Result<Option<Session>, BcmrError> {
    let session = match Session::load(src, dst) {
        Some(s) => s,
        None => return Ok(None),
    };

    let src_meta = src.metadata()?;
    let src_mtime = src_meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let src_inode = durable_io::get_inode(src).unwrap_or(0);

    if session.source_matches(file_size, src_mtime, src_inode) {
        Ok(Some(session))
    } else {
        Session::remove(src, dst);
        Ok(None)
    }
}

async fn resolve_strict(
    src: &Path,
    dst: &Path,
    file_size: u64,
    dst_len: u64,
) -> Result<Decision, BcmrError> {
    if dst_len == file_size {
        let src_path = src.to_path_buf();
        let dst_path = dst.to_path_buf();
        let (src_hash, dst_hash) = tokio::join!(
            tokio::task::spawn_blocking(move || checksum::calculate_hash(&src_path)),
            tokio::task::spawn_blocking(move || checksum::calculate_hash(&dst_path)),
        );
        if src_hash?? == dst_hash?? {
            return Ok(Decision::AlreadyComplete);
        }
        Ok(Decision::Overwrite)
    } else if dst_len < file_size {
        let src_path = src.to_path_buf();
        let dst_path = dst.to_path_buf();
        let limit = dst_len;
        let (dst_hash, src_partial) = tokio::join!(
            tokio::task::spawn_blocking(move || checksum::calculate_hash(&dst_path)),
            tokio::task::spawn_blocking(move || checksum::calculate_partial_hash(&src_path, limit)),
        );
        Ok(if dst_hash?? == src_partial?? {
            Decision::Resume
        } else {
            Decision::Overwrite
        })
    } else {
        Ok(Decision::Overwrite)
    }
}

fn resolve_append(file_size: u64, dst_len: u64) -> Decision {
    if dst_len == file_size {
        Decision::AlreadyComplete
    } else if dst_len < file_size {
        Decision::Resume
    } else {
        Decision::Overwrite
    }
}

fn resolve_with_session(file_size: u64, dst_len: u64, session: &Session) -> Decision {
    if dst_len == file_size {
        Decision::AlreadyComplete
    } else if dst_len < file_size && !session.block_hashes.is_empty() {
        Decision::Resume
    } else {
        Decision::Overwrite
    }
}

fn resolve_mtime(
    src: &Path,
    dst: &Path,
    file_size: u64,
    dst_len: u64,
) -> Result<Decision, BcmrError> {
    let src_mtime = src.metadata()?.modified()?;
    let dst_mtime = dst.metadata()?.modified()?;

    if src_mtime != dst_mtime {
        Ok(Decision::Overwrite)
    } else if dst_len == file_size {
        Ok(Decision::AlreadyComplete)
    } else if dst_len < file_size {
        Ok(Decision::Resume)
    } else {
        Ok(Decision::Overwrite)
    }
}
