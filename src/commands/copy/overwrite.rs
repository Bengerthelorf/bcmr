use crate::cli::Commands;
use crate::core::checksum;
use crate::core::error::BcmrError;
use crate::core::io as durable_io;
use crate::core::session::Session;
use crate::core::traversal;
use crate::ui::display::ActionType;

use std::path::{Path, PathBuf};
use tokio::fs;

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
                        .ok_or_else(BcmrError::invalid_source_file_name)?,
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
                .ok_or_else(BcmrError::invalid_source_dir_name)?;
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

pub(super) fn is_normal_write(cli: &Commands) -> bool {
    !cli.is_resume() && !cli.is_append() && !cli.is_strict()
}

pub(super) async fn check_overwrite(
    dst: &Path,
    cli: &Commands,
) -> std::result::Result<(), BcmrError> {
    if !dst.exists() {
        return Ok(());
    }
    if !cli.is_force() && is_normal_write(cli) {
        return Err(BcmrError::TargetExists(dst.to_path_buf()));
    }
    if cli.is_force() && !is_normal_write(cli) {
        fs::remove_file(dst).await?;
    }
    Ok(())
}

pub(super) fn determine_dry_run_action(
    src: &Path,
    dst: &Path,
    cli: &Commands,
) -> std::result::Result<ActionType, BcmrError> {
    if !dst.exists() {
        return Ok(ActionType::Add);
    }
    if cli.is_force() && !is_normal_write(cli) {
        // The existing execution contract removes direct-mode destinations
        // before resolution when force is explicit. Preview that overwrite
        // without reading or mutating session state.
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
                ActionType::Append
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
