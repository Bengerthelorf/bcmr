use crate::core::error::BcmrError;
use crate::core::remote::{self, parse_remote_path};
use crate::core::traversal;
use crate::output::{CheckResult, CheckSummary, FileDiff, Status};

use std::path::{Path, PathBuf};

/// Compare source(s) against a destination and return a structured diff.
pub async fn run(
    sources: &[PathBuf],
    dest: &Path,
    recursive: bool,
    excludes: &[regex::Regex],
) -> Result<CheckResult, BcmrError> {
    let dest_str = dest.to_string_lossy();
    let remote_dest = parse_remote_path(&dest_str);
    let any_remote_source = sources
        .iter()
        .any(|s| parse_remote_path(&s.to_string_lossy()).is_some());

    if remote_dest.is_some() && any_remote_source {
        return Err(BcmrError::InvalidInput(
            "Cannot check between two remote hosts".into(),
        ));
    }

    if remote_dest.is_some() {
        // local source → remote destination
        return check_local_vs_remote(sources, dest, recursive, excludes).await;
    }

    if any_remote_source {
        // remote source → local destination
        return check_remote_vs_local(sources, dest, recursive, excludes).await;
    }

    // local source → local destination
    let sources = sources.to_vec();
    let dest = dest.to_path_buf();
    let excludes = excludes.to_vec();
    tokio::task::spawn_blocking(move || check_local(&sources, &dest, recursive, &excludes)).await?
}

// ---------------------------------------------------------------------------
// Local vs Local
// ---------------------------------------------------------------------------

fn check_local(
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
            let dst_path =
                if dest_is_dir {
                    dest.join(src.file_name().ok_or_else(|| {
                        BcmrError::InvalidInput("Invalid source file name".into())
                    })?)
                } else {
                    dest.to_path_buf()
                };

            compare_local_file(src, &dst_path, src, &mut added, &mut modified)?;
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
                    compare_local_file(path, &target, src, &mut added, &mut modified)?;
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

    Ok(build_result(added, modified, missing))
}

fn compare_local_file(
    src_path: &Path,
    dst_path: &Path,
    strip_base: &Path,
    added: &mut Vec<FileDiff>,
    modified: &mut Vec<FileDiff>,
) -> Result<(), BcmrError> {
    let relative = src_path.strip_prefix(strip_base).unwrap_or(src_path);
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

// ---------------------------------------------------------------------------
// Local source → Remote destination
// ---------------------------------------------------------------------------

async fn check_local_vs_remote(
    sources: &[PathBuf],
    dest: &Path,
    recursive: bool,
    excludes: &[regex::Regex],
) -> Result<CheckResult, BcmrError> {
    let dest_str = dest.to_string_lossy();
    let rdest = parse_remote_path(&dest_str)
        .ok_or_else(|| BcmrError::InvalidInput("Invalid remote path".into()))?;

    let mut added = Vec::new();
    let mut modified = Vec::new();

    for src in sources {
        if traversal::is_excluded(src, excludes) {
            continue;
        }

        if src.is_file() {
            let fname = src
                .file_name()
                .ok_or_else(|| BcmrError::InvalidInput("Invalid source file name".into()))?
                .to_string_lossy();
            let remote_path = rdest.join(&fname);

            match remote::remote_stat(&remote_path).await {
                Ok(info) => {
                    let src_size = src.metadata()?.len();
                    if src_size != info.size {
                        modified.push(FileDiff {
                            path: PathBuf::from(fname.as_ref()),
                            size: None,
                            src_size: Some(src_size),
                            dst_size: Some(info.size),
                            is_dir: false,
                        });
                    }
                }
                Err(_) => {
                    let src_size = src.metadata()?.len();
                    added.push(FileDiff {
                        path: PathBuf::from(fname.as_ref()),
                        size: Some(src_size),
                        src_size: Some(src_size),
                        dst_size: None,
                        is_dir: false,
                    });
                }
            }
        } else if src.is_dir() && recursive {
            let src_name = src
                .file_name()
                .ok_or_else(|| BcmrError::InvalidInput("Invalid source directory name".into()))?;
            let remote_dir = rdest.join(&src_name.to_string_lossy());

            // Get remote file listing
            let remote_files = remote::remote_list_files(&remote_dir)
                .await
                .unwrap_or_default();

            let remote_map: std::collections::HashMap<String, (u64, bool)> = remote_files
                .into_iter()
                .map(|(path, size, is_dir)| (path, (size, is_dir)))
                .collect();

            // Walk local source
            for entry in traversal::walk(src, true, false, 1, excludes) {
                let entry = entry?;
                let path = entry.path();
                let relative = path.strip_prefix(src)?;
                let rel_str = relative.to_string_lossy().to_string();

                if path.is_dir() {
                    if !remote_map.contains_key(&rel_str) {
                        added.push(FileDiff {
                            path: relative.to_path_buf(),
                            size: None,
                            src_size: None,
                            dst_size: None,
                            is_dir: true,
                        });
                    }
                } else if path.is_file() {
                    let src_size = entry.metadata()?.len();
                    match remote_map.get(&rel_str) {
                        Some(&(dst_size, _)) => {
                            if src_size != dst_size {
                                modified.push(FileDiff {
                                    path: relative.to_path_buf(),
                                    size: None,
                                    src_size: Some(src_size),
                                    dst_size: Some(dst_size),
                                    is_dir: false,
                                });
                            }
                        }
                        None => {
                            added.push(FileDiff {
                                path: relative.to_path_buf(),
                                size: Some(src_size),
                                src_size: Some(src_size),
                                dst_size: None,
                                is_dir: false,
                            });
                        }
                    }
                }
            }
        } else if src.is_dir() {
            return Err(BcmrError::InvalidInput(format!(
                "Source '{}' is a directory. Use -r flag for recursive check.",
                src.display()
            )));
        } else {
            return Err(BcmrError::SourceNotFound(src.clone()));
        }
    }

    Ok(build_result(added, modified, Vec::new()))
}

// ---------------------------------------------------------------------------
// Remote source → Local destination
// ---------------------------------------------------------------------------

async fn check_remote_vs_local(
    sources: &[PathBuf],
    dest: &Path,
    recursive: bool,
    excludes: &[regex::Regex],
) -> Result<CheckResult, BcmrError> {
    let mut added = Vec::new();
    let mut modified = Vec::new();

    for src in sources {
        let src_str = src.to_string_lossy();
        let rsrc = match parse_remote_path(&src_str) {
            Some(r) => r,
            None => continue,
        };

        let info = remote::remote_stat(&rsrc).await?;

        if !info.is_dir {
            // Single remote file
            let fname = rsrc.path.rsplit('/').next().unwrap_or(&rsrc.path);
            let local_path = if dest.is_dir() {
                dest.join(fname)
            } else {
                dest.to_path_buf()
            };

            if !local_path.exists() {
                added.push(FileDiff {
                    path: PathBuf::from(fname),
                    size: Some(info.size),
                    src_size: Some(info.size),
                    dst_size: None,
                    is_dir: false,
                });
            } else {
                let local_size = local_path.metadata()?.len();
                if info.size != local_size {
                    modified.push(FileDiff {
                        path: PathBuf::from(fname),
                        size: None,
                        src_size: Some(info.size),
                        dst_size: Some(local_size),
                        is_dir: false,
                    });
                }
            }
        } else if recursive {
            let dir_name = rsrc.path.rsplit('/').next().unwrap_or(&rsrc.path);
            let local_dir = if dest.is_dir() {
                dest.join(dir_name)
            } else {
                dest.to_path_buf()
            };

            let remote_files = remote::remote_list_files(&rsrc).await?;

            for (rel_path, size, is_dir) in &remote_files {
                if traversal::is_excluded(Path::new(rel_path), excludes) {
                    continue;
                }

                let local_path = local_dir.join(rel_path);

                if *is_dir {
                    if !local_path.exists() {
                        added.push(FileDiff {
                            path: PathBuf::from(rel_path),
                            size: None,
                            src_size: None,
                            dst_size: None,
                            is_dir: true,
                        });
                    }
                } else if !local_path.exists() {
                    added.push(FileDiff {
                        path: PathBuf::from(rel_path),
                        size: Some(*size),
                        src_size: Some(*size),
                        dst_size: None,
                        is_dir: false,
                    });
                } else {
                    let local_size = local_path.metadata()?.len();
                    if *size != local_size {
                        modified.push(FileDiff {
                            path: PathBuf::from(rel_path),
                            size: None,
                            src_size: Some(*size),
                            dst_size: Some(local_size),
                            is_dir: false,
                        });
                    }
                }
            }
        } else {
            return Err(BcmrError::InvalidInput(format!(
                "Remote source '{}' is a directory. Use -r flag for recursive check.",
                rsrc
            )));
        }
    }

    Ok(build_result(added, modified, Vec::new()))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_result(
    added: Vec<FileDiff>,
    modified: Vec<FileDiff>,
    missing: Vec<FileDiff>,
) -> CheckResult {
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

    CheckResult {
        status: Status::Success,
        in_sync,
        added,
        modified,
        missing,
        summary,
        error: None,
        error_kind: None,
    }
}
