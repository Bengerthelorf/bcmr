use crate::core::error::BcmrError;
use crate::core::remote::{self, parse_remote_path};
use crate::core::traversal;
use crate::output::{CheckResult, CheckSummary, FileDiff, Status};

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A single file/directory entry with metadata, used for diff comparison.
struct Entry {
    rel_path: String,
    size: u64,
    is_dir: bool,
}

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

    let mut all_added = Vec::new();
    let mut all_modified = Vec::new();
    let mut all_missing = Vec::new();

    for src in sources {
        let src_str = src.to_string_lossy();
        let is_remote_src = parse_remote_path(&src_str).is_some();

        if !is_remote_src && !src.exists() {
            return Err(BcmrError::SourceNotFound(src.clone()));
        }

        // Determine the resolved destination subdirectory
        let (src_entries, dst_entries) = collect_both(
            src,
            dest,
            recursive,
            excludes,
            is_remote_src,
            remote_dest.is_some(),
        )
        .await?;

        let (added, modified, missing) = diff_entries(src_entries, dst_entries);
        all_added.extend(added);
        all_modified.extend(modified);
        all_missing.extend(missing);
    }

    Ok(build_result(all_added, all_modified, all_missing))
}

/// Collect entries from both source and destination, handling local/remote dispatch.
async fn collect_both(
    src: &Path,
    dest: &Path,
    recursive: bool,
    excludes: &[regex::Regex],
    is_remote_src: bool,
    is_remote_dest: bool,
) -> Result<(Vec<Entry>, Vec<Entry>), BcmrError> {
    let src_str = src.to_string_lossy();
    let dest_str = dest.to_string_lossy();

    // Resolve source name for destination subdirectory
    let src_name = if is_remote_src {
        let rp = parse_remote_path(&src_str).unwrap();
        rp.path.rsplit('/').next().unwrap_or(&rp.path).to_string()
    } else {
        src.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    };

    // Check if source is a directory
    let src_is_dir = if is_remote_src {
        let rp = parse_remote_path(&src_str).unwrap();
        let info = remote::remote_stat(&rp).await?;
        info.is_dir
    } else {
        src.is_dir()
    };

    if src_is_dir && !recursive {
        return Err(BcmrError::InvalidInput(format!(
            "Source '{}' is a directory. Use -r flag for recursive check.",
            src.display()
        )));
    }

    // Collect source entries
    let src_entries = if is_remote_src {
        let rp = parse_remote_path(&src_str).unwrap();
        if src_is_dir {
            collect_remote_entries(&rp).await?
        } else {
            let info = remote::remote_stat(&rp).await?;
            vec![Entry {
                rel_path: src_name.clone(),
                size: info.size,
                is_dir: false,
            }]
        }
    } else if src_is_dir {
        collect_local_entries(src, excludes)?
    } else {
        let size = src.metadata()?.len();
        vec![Entry {
            rel_path: src_name.clone(),
            size,
            is_dir: false,
        }]
    };

    // Resolve destination path (append source dir name if dest is a directory)
    let dst_entries = if is_remote_dest {
        let rdest = parse_remote_path(&dest_str).unwrap();
        let rdest_sub = if src_is_dir {
            rdest.join(&src_name)
        } else {
            rdest
        };
        collect_remote_entries(&rdest_sub).await.unwrap_or_default()
    } else {
        let dest_is_dir = dest.exists() && dest.is_dir();
        let resolved_dest = if src_is_dir && dest_is_dir {
            dest.join(&src_name)
        } else {
            dest.to_path_buf()
        };
        if resolved_dest.exists() && resolved_dest.is_dir() {
            collect_local_entries(&resolved_dest, excludes).unwrap_or_default()
        } else if resolved_dest.exists() {
            let size = resolved_dest.metadata()?.len();
            let fname = resolved_dest
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            vec![Entry {
                rel_path: fname,
                size,
                is_dir: false,
            }]
        } else {
            Vec::new()
        }
    };

    Ok((src_entries, dst_entries))
}

/// Collect entries from a local directory.
fn collect_local_entries(root: &Path, excludes: &[regex::Regex]) -> Result<Vec<Entry>, BcmrError> {
    let mut entries = Vec::new();
    for entry in traversal::walk(root, true, false, 1, excludes) {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(root)?;
        let meta = entry.metadata()?;
        entries.push(Entry {
            rel_path: relative.to_string_lossy().to_string(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            is_dir: meta.is_dir(),
        });
    }
    Ok(entries)
}

/// Collect entries from a remote directory via SSH.
async fn collect_remote_entries(remote: &remote::RemotePath) -> Result<Vec<Entry>, BcmrError> {
    let files = remote::remote_list_files(remote).await?;
    Ok(files
        .into_iter()
        .map(|(rel_path, size, is_dir)| Entry {
            rel_path,
            size,
            is_dir,
        })
        .collect())
}

/// Diff two entry lists. Returns (added, modified, missing).
fn diff_entries(src: Vec<Entry>, dst: Vec<Entry>) -> (Vec<FileDiff>, Vec<FileDiff>, Vec<FileDiff>) {
    let dst_map: HashMap<&str, &Entry> = dst.iter().map(|e| (e.rel_path.as_str(), e)).collect();
    let src_map: HashMap<&str, &Entry> = src.iter().map(|e| (e.rel_path.as_str(), e)).collect();

    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut missing = Vec::new();

    // Files in source but not in destination → added
    // Files in both but different size → modified
    for s in &src {
        match dst_map.get(s.rel_path.as_str()) {
            None => {
                added.push(FileDiff {
                    path: PathBuf::from(&s.rel_path),
                    size: if s.is_dir { None } else { Some(s.size) },
                    src_size: if s.is_dir { None } else { Some(s.size) },
                    dst_size: None,
                    is_dir: s.is_dir,
                });
            }
            Some(d) if !s.is_dir && s.size != d.size => {
                modified.push(FileDiff {
                    path: PathBuf::from(&s.rel_path),
                    size: None,
                    src_size: Some(s.size),
                    dst_size: Some(d.size),
                    is_dir: false,
                });
            }
            _ => {} // in sync
        }
    }

    // Files in destination but not in source → missing
    for d in &dst {
        if !src_map.contains_key(d.rel_path.as_str()) {
            missing.push(FileDiff {
                path: PathBuf::from(&d.rel_path),
                size: if d.is_dir { None } else { Some(d.size) },
                src_size: None,
                dst_size: if d.is_dir { None } else { Some(d.size) },
                is_dir: d.is_dir,
            });
        }
    }

    (added, modified, missing)
}

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
