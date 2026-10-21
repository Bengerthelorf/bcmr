use crate::cli::Commands;
use crate::core::checksum;
use crate::core::error::BcmrError;
use crate::core::io as durable_io;
use crate::core::session::Session;
use crate::core::traversal;
use crate::ui::display::ActionType;

use super::validate_direct_destination;
use std::path::{Path, PathBuf};
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
    let plan = super::plan::plan_copy(
        sources,
        dst,
        recursive,
        _cli.is_no_deref() || matches!(_cli, Commands::Move { .. }),
        excludes,
    )
    .await?;
    Ok(plan.overwrites)
}

fn get_total_size_sync(
    sources: Vec<PathBuf>,
    recursive: bool,
    no_deref: bool,
    excludes: Vec<regex::Regex>,
) -> std::result::Result<u64, BcmrError> {
    let mut total_size = 0u64;
    for src in sources {
        if traversal::is_copy_excluded(&src, &src, &excludes) {
            continue;
        }
        let metadata = src.symlink_metadata()?;
        if no_deref && metadata.file_type().is_symlink() {
            continue;
        }
        if src.metadata()?.is_dir() && !recursive {
            return Err(BcmrError::InvalidInput(format!(
                "Source '{}' is a directory. Use -r flag for recursive copy.",
                src.display()
            )));
        }
        for entry in traversal::copy_walk(&src, recursive, 0, no_deref, &excludes) {
            let entry = entry?;
            if entry.file_type().is_file() {
                total_size = total_size
                    .checked_add(entry.metadata()?.len())
                    .ok_or_else(|| {
                        BcmrError::InvalidInput("selected source size exceeds u64".into())
                    })?;
            } else if !entry.file_type().is_dir() && !(no_deref && entry.file_type().is_symlink()) {
                return Err(BcmrError::InvalidInput(format!(
                    "unsupported source entry '{}'",
                    entry.path().display()
                )));
            }
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

    let no_deref = _cli.is_no_deref() || matches!(_cli, Commands::Move { .. });
    tokio::task::spawn_blocking(move || get_total_size_sync(sources, recursive, no_deref, excludes))
        .await?
}

pub(super) fn is_normal_write(cli: &Commands) -> bool {
    !cli.is_resume() && !cli.is_append() && !cli.is_strict()
}

pub(super) async fn check_overwrite(
    dst: &Path,
    cli: &Commands,
) -> std::result::Result<(), BcmrError> {
    match dst.symlink_metadata() {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(BcmrError::Io(error)),
    }
    if !cli.is_force() && is_normal_write(cli) {
        return Err(BcmrError::TargetExists(dst.to_path_buf()));
    }
    Ok(())
}

pub(super) fn determine_dry_run_action(
    src: &Path,
    dst: &Path,
    cli: &Commands,
) -> std::result::Result<ActionType, BcmrError> {
    let entry_metadata = match dst.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ActionType::Add);
        }
        Err(error) => return Err(BcmrError::Io(error)),
    };
    if !is_normal_write(cli) {
        let force_fresh = validate_direct_destination(dst, Some(&entry_metadata), cli.is_force())?;
        if force_fresh {
            return Ok(ActionType::Overwrite);
        }
    } else if !entry_metadata.file_type().is_file() {
        return Ok(ActionType::Overwrite);
    }
    let src_meta = src.metadata()?;
    let dst_meta = dst.metadata()?;
    let src_len = src_meta.len();
    let dst_len = dst_meta.len();

    if cli.is_strict() {
        if dst_len == src_len {
            return Ok(if files_have_same_hash(src, dst)? {
                ActionType::Skip
            } else {
                ActionType::Overwrite
            });
        } else if dst_len < src_len {
            let dst_hash = checksum::calculate_hash(dst)?;
            let src_prefix_hash = checksum::calculate_partial_hash(src, dst_len)?;
            return Ok(if dst_hash == src_prefix_hash {
                ActionType::Append
            } else {
                ActionType::Overwrite
            });
        }
        return Ok(ActionType::Overwrite);
    }

    if cli.is_append() {
        if dst_len == src_len {
            return Ok(ActionType::Skip);
        } else if dst_len < src_len {
            return Ok(ActionType::Append);
        }
        return Err(BcmrError::InvalidInput(format!(
            "append destination is {dst_len} bytes, larger than the {src_len}-byte source"
        )));
    }

    if cli.is_resume() {
        let src_mtime = src_meta
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let src_inode = durable_io::get_inode(src).unwrap_or(0);
        if let Some(session) = Session::try_load_read_only(src, dst)?.filter(|session| {
            session.source_matches(src_len, src_mtime, src_inode)
                && session.has_valid_resume_structure()
        }) {
            let verified = session.find_verified_resume_offset(src, dst)?;
            if verified == src_len && dst_len == src_len {
                return Ok(ActionType::Skip);
            }
            return Ok(if verified > 0 {
                if dst_len > verified {
                    ActionType::Overwrite
                } else {
                    ActionType::Append
                }
            } else {
                ActionType::Overwrite
            });
        }

        if dst_len == src_len && files_have_same_hash(src, dst)? {
            return Ok(ActionType::Skip);
        }
        return Ok(ActionType::Overwrite);
    }

    Ok(ActionType::Overwrite)
}

fn files_have_same_hash(src: &Path, dst: &Path) -> Result<bool, BcmrError> {
    Ok(checksum::calculate_hash(src)? == checksum::calculate_hash(dst)?)
}
