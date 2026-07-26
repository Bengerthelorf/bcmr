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
    pub truncate_tail: bool,
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
            truncate_tail: false,
        });
    }

    let src_pb = src.to_path_buf();
    let dst_pb = dst.to_path_buf();
    let (dst_len, mut loaded_session) =
        tokio::task::spawn_blocking(move || -> Result<(u64, Option<Session>), BcmrError> {
            let dst_len = dst_pb.metadata()?.len();
            let session = if resume && !strict && !append {
                load_and_validate_session(&src_pb, &dst_pb, file_size)?
            } else {
                None
            };
            Ok((dst_len, session))
        })
        .await??;

    let mut session_proof = None;
    let decision = if strict {
        resolve_strict(src, dst, file_size, dst_len).await?
    } else if append {
        resolve_append(file_size, dst_len)?
    } else if let Some(session) = loaded_session.take() {
        let src_pb = src.to_path_buf();
        let dst_pb = dst.to_path_buf();
        let (verified, session) = tokio::task::spawn_blocking(move || {
            let verified = session.find_verified_resume_offset(&src_pb, &dst_pb);
            (verified, session)
        })
        .await?;
        let verified = verified?;
        loaded_session = Some(session);
        session_proof = Some(verified);
        if verified == file_size && dst_len == file_size {
            Decision::AlreadyComplete
        } else if verified > 0 {
            Decision::Resume
        } else {
            Decision::Overwrite
        }
    } else {
        resolve_without_session(src, dst, file_size, dst_len).await?
    };

    match decision {
        Decision::AlreadyComplete => {
            callback(file_size);
            return Ok(ResumeState {
                start_offset: 0,
                already_complete: true,
                loaded_session,
                truncate_tail: false,
            });
        }
        Decision::Overwrite => {
            return Ok(ResumeState {
                start_offset: 0,
                already_complete: false,
                loaded_session,
                truncate_tail: false,
            });
        }
        Decision::Resume => {}
    }

    let start_offset = session_proof.unwrap_or(dst_len);

    if start_offset > 0 {
        callback(start_offset);
    }

    Ok(ResumeState {
        start_offset,
        already_complete: false,
        loaded_session,
        truncate_tail: session_proof.is_some() && dst_len > start_offset,
    })
}

fn load_and_validate_session(
    src: &Path,
    dst: &Path,
    file_size: u64,
) -> Result<Option<Session>, BcmrError> {
    let session = match Session::load(src, dst)? {
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

    if session.source_matches(file_size, src_mtime, src_inode)
        && session.has_valid_resume_structure()
    {
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

fn resolve_append(file_size: u64, dst_len: u64) -> Result<Decision, BcmrError> {
    if dst_len == file_size {
        Ok(Decision::AlreadyComplete)
    } else if dst_len < file_size {
        Ok(Decision::Resume)
    } else {
        Err(BcmrError::InvalidInput(format!(
            "append destination is {dst_len} bytes, larger than the {file_size}-byte source"
        )))
    }
}

async fn resolve_without_session(
    src: &Path,
    dst: &Path,
    file_size: u64,
    dst_len: u64,
) -> Result<Decision, BcmrError> {
    if dst_len != file_size {
        return Ok(Decision::Overwrite);
    }

    let src_path = src.to_path_buf();
    let dst_path = dst.to_path_buf();
    let (src_hash, dst_hash) = tokio::join!(
        tokio::task::spawn_blocking(move || checksum::calculate_hash(&src_path)),
        tokio::task::spawn_blocking(move || checksum::calculate_hash(&dst_path)),
    );
    Ok(if src_hash?? == dst_hash?? {
        Decision::AlreadyComplete
    } else {
        Decision::Overwrite
    })
}
