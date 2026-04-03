use crate::core::error::BcmrError;
use crate::core::traversal;
use crate::output::{CheckResult, CheckSummary, FileDiff, Status};

use std::path::{Path, PathBuf};

/// Compare source(s) against a destination and return a structured diff.
///
/// No files are read or modified — only metadata (existence, size, mtime) is inspected.
pub async fn run(
    sources: &[PathBuf],
    dest: &Path,
    recursive: bool,
    excludes: &[regex::Regex],
) -> Result<CheckResult, BcmrError> {
    let sources = sources.to_vec();
    let dest = dest.to_path_buf();
    let excludes = excludes.to_vec();

    tokio::task::spawn_blocking(move || check_sync(&sources, &dest, recursive, &excludes)).await?
}

fn check_sync(
    sources: &[PathBuf],
    dest: &Path,
    recursive: bool,
    excludes: &[regex::Regex],
) -> Result<CheckResult, BcmrError> {
    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut missing = Vec::new();

    let dest_is_dir = dest.exists() && dest.is_dir();

    for src in sources {
        if traversal::is_excluded(src, excludes) {
            continue;
        }

        if src.is_file() {
            let dst_path = if dest_is_dir {
                dest.join(
                    src.file_name()
                        .ok_or_else(|| BcmrError::InvalidInput("Invalid source file name".into()))?,
                )
            } else {
                dest.to_path_buf()
            };

            compare_file(src, &dst_path, src, &mut added, &mut modified)?;
        } else if src.is_dir() {
            if !recursive {
                return Err(BcmrError::InvalidInput(format!(
                    "Source '{}' is a directory. Use -r flag for recursive check.",
                    src.display()
                )));
            }

            let src_name = src
                .file_name()
                .ok_or_else(|| BcmrError::InvalidInput("Invalid source directory name".into()))?;

            let new_dst = if dest_is_dir {
                dest.join(src_name)
            } else {
                dest.to_path_buf()
            };

            // Walk source: find added / modified
            for entry in traversal::walk(src, true, false, 1, excludes) {
                let entry = entry?;
                let path = entry.path();
                let relative = path.strip_prefix(src)?;
                let target = new_dst.join(relative);

                if path.is_dir() {
                    if !target.exists() {
                        added.push(FileDiff {
                            path: relative.to_path_buf(),
                            size: None,
                            src_size: None,
                            dst_size: None,
                            is_dir: true,
                        });
                    }
                } else if path.is_file() {
                    compare_file(path, &target, src, &mut added, &mut modified)?;
                }
            }

            // Walk destination: find files missing from source
            if new_dst.exists() {
                for entry in traversal::walk(&new_dst, true, false, 1, excludes) {
                    let entry = entry?;
                    let path = entry.path();
                    let relative = path.strip_prefix(&new_dst)?;
                    let src_counterpart = src.join(relative);

                    if !src_counterpart.exists() {
                        let meta = entry.metadata().ok();
                        let size = meta.as_ref().map(|m| m.len());
                        missing.push(FileDiff {
                            path: relative.to_path_buf(),
                            size,
                            src_size: None,
                            dst_size: size,
                            is_dir: path.is_dir(),
                        });
                    }
                }
            }
        } else {
            return Err(BcmrError::SourceNotFound(src.clone()));
        }
    }

    let total_bytes: u64 = added
        .iter()
        .chain(modified.iter())
        .filter_map(|d| d.src_size.or(d.size))
        .sum();

    let summary = CheckSummary {
        added: added.len() as u64,
        modified: modified.len() as u64,
        missing: missing.len() as u64,
        total_bytes,
    };

    let in_sync = added.is_empty() && modified.is_empty() && missing.is_empty();

    Ok(CheckResult {
        status: Status::Success,
        in_sync,
        added,
        modified,
        missing,
        summary,
        error: None,
        error_kind: None,
    })
}

fn compare_file(
    src_path: &Path,
    dst_path: &Path,
    strip_base: &Path,
    added: &mut Vec<FileDiff>,
    modified: &mut Vec<FileDiff>,
) -> Result<(), BcmrError> {
    let relative = src_path
        .strip_prefix(strip_base)
        .unwrap_or(src_path.as_ref());

    let src_meta = src_path.metadata()?;
    let src_size = src_meta.len();

    if !dst_path.exists() {
        added.push(FileDiff {
            path: relative.to_path_buf(),
            size: Some(src_size),
            src_size: Some(src_size),
            dst_size: None,
            is_dir: false,
        });
        return Ok(());
    }

    let dst_meta = dst_path.metadata()?;
    let dst_size = dst_meta.len();

    if src_size != dst_size {
        modified.push(FileDiff {
            path: relative.to_path_buf(),
            size: None,
            src_size: Some(src_size),
            dst_size: Some(dst_size),
            is_dir: false,
        });
        return Ok(());
    }

    // Same size — compare mtime
    let src_mtime = src_meta.modified()?;
    let dst_mtime = dst_meta.modified()?;
    if src_mtime != dst_mtime {
        modified.push(FileDiff {
            path: relative.to_path_buf(),
            size: None,
            src_size: Some(src_size),
            dst_size: Some(dst_size),
            is_dir: false,
        });
    }

    Ok(())
}
