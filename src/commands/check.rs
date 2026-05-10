use crate::core::error::BcmrError;
use crate::core::remote::{self, parse_remote_path, RemotePath};
use crate::core::serve_client::ServeClient;
use crate::core::traversal;
use crate::output::{CheckResult, CheckSummary, FileDiff, Status};

use std::collections::HashMap;
use std::path::{Path, PathBuf};

struct Entry {
    rel_path: String,
    size: u64,
    mtime: i64,
    is_dir: bool,
}

enum SideRoot {
    LocalDir(PathBuf),
    LocalFile(PathBuf),
    RemoteDir(RemotePath),
    RemoteFile(RemotePath),
}

impl SideRoot {
    fn is_file(&self) -> bool {
        matches!(self, SideRoot::LocalFile(_) | SideRoot::RemoteFile(_))
    }
}

struct CollectResult {
    src_entries: Vec<Entry>,
    dst_entries: Vec<Entry>,
    src_root: SideRoot,
    dst_root: SideRoot,
}

pub async fn run(
    sources: &[PathBuf],
    dest: &Path,
    recursive: bool,
    excludes: &[regex::Regex],
    no_hash: bool,
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

    if let Some(ref rd) = remote_dest {
        rd.reject_unsafe()?;
    }
    for src in sources {
        if let Some(rsrc) = parse_remote_path(&src.to_string_lossy()) {
            rsrc.reject_unsafe()?;
        }
    }

    let expanded_dest_buf: PathBuf = if let Some(mut rd) = remote_dest.clone() {
        rd.expand_tilde().await?;
        PathBuf::from(rd.display())
    } else {
        dest.to_path_buf()
    };
    let mut expanded_sources: Vec<PathBuf> = Vec::with_capacity(sources.len());
    for src in sources {
        if let Some(mut rsrc) = parse_remote_path(&src.to_string_lossy()) {
            rsrc.expand_tilde().await?;
            expanded_sources.push(PathBuf::from(rsrc.display()));
        } else {
            expanded_sources.push(src.clone());
        }
    }
    let sources: &[PathBuf] = &expanded_sources;
    let dest: &Path = expanded_dest_buf.as_path();
    let dest_str = dest.to_string_lossy();
    let remote_dest = parse_remote_path(&dest_str);

    let remote_host = if let Some(ref rd) = remote_dest {
        Some(rd.ssh_target())
    } else if any_remote_source {
        let src_str = sources[0].to_string_lossy();
        parse_remote_path(&src_str).map(|rp| rp.ssh_target())
    } else {
        None
    };

    let mut serve_client = if let Some(ref host) = remote_host {
        emit_connecting(host);
        ServeClient::connect(host).await.ok()
    } else {
        None
    };

    let mut all_added = Vec::new();
    let mut all_modified = Vec::new();
    let mut all_missing = Vec::new();

    for src in sources {
        let src_str = src.to_string_lossy();
        let is_remote_src = parse_remote_path(&src_str).is_some();

        if !is_remote_src && !src.exists() {
            return Err(BcmrError::SourceNotFound(src.clone()));
        }

        let collected = collect_both(
            src,
            dest,
            recursive,
            excludes,
            is_remote_src,
            remote_dest.is_some(),
            &mut serve_client,
        )
        .await?;

        let both_file_roots = collected.src_root.is_file() && collected.dst_root.is_file();
        let (added, mut modified, missing) =
            diff_entries(&collected.src_entries, &collected.dst_entries);
        // diff_entries trusts size+mtime; for explicit file-to-file we always
        // confirm with a hash since the user named these two files specifically.
        if both_file_roots && !no_hash && modified.is_empty() {
            if let (Some(s), Some(d)) =
                (collected.src_entries.first(), collected.dst_entries.first())
            {
                modified.push(modified_diff(s, d));
            }
        }
        let modified = if no_hash {
            modified
        } else {
            filter_size_match_via_hash(
                modified,
                &collected.src_root,
                &collected.dst_root,
                &mut serve_client,
            )
            .await?
        };
        all_added.extend(added);
        all_modified.extend(modified);
        all_missing.extend(missing);
    }

    if let Some(client) = serve_client {
        let _ = client.close().await;
    }

    Ok(build_result(all_added, all_modified, all_missing))
}

async fn collect_both(
    src: &Path,
    dest: &Path,
    recursive: bool,
    excludes: &[regex::Regex],
    is_remote_src: bool,
    is_remote_dest: bool,
    serve: &mut Option<ServeClient>,
) -> Result<CollectResult, BcmrError> {
    let src_str = src.to_string_lossy();
    let dest_str = dest.to_string_lossy();

    let remote_src = if is_remote_src {
        Some(
            parse_remote_path(&src_str)
                .ok_or_else(|| BcmrError::invalid_remote_path(src.display()))?,
        )
    } else {
        None
    };

    let src_name = if let Some(ref rp) = remote_src {
        rp.file_name().to_string()
    } else {
        src.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    };

    let src_is_dir = if let Some(ref rp) = remote_src {
        remote_is_dir(rp, serve).await?
    } else {
        src.is_dir()
    };

    if src_is_dir && !recursive {
        return Err(BcmrError::InvalidInput(format!(
            "Source '{}' is a directory. Use -r flag for recursive check.",
            src.display()
        )));
    }

    let src_entries = if let Some(ref rp) = remote_src {
        if src_is_dir {
            emit_scanning(&rp.display());
            let entries = filter_entries(collect_remote_entries(rp, serve).await?, excludes);
            emit_scanning_done(entries.len());
            entries
        } else {
            let (size, mtime, _) = remote_stat_full(rp, serve).await?;
            vec![Entry {
                rel_path: src_name.clone(),
                size,
                mtime,
                is_dir: false,
            }]
        }
    } else if src_is_dir {
        collect_local_entries(src, excludes)?
    } else {
        let meta = src.metadata()?;
        vec![Entry {
            rel_path: src_name.clone(),
            size: meta.len(),
            mtime: mtime_secs(&meta),
            is_dir: false,
        }]
    };

    let dst_entries = if is_remote_dest {
        let rdest = parse_remote_path(&dest_str)
            .ok_or_else(|| BcmrError::invalid_remote_path(dest.display()))?;
        let rdest_is_dir = remote_is_dir(&rdest, serve).await.unwrap_or(false);
        let rdest_sub = if rdest_is_dir {
            rdest.join(&src_name)
        } else {
            rdest
        };
        emit_scanning(&rdest_sub.display());
        if src_is_dir {
            let entries = filter_entries(
                collect_remote_entries(&rdest_sub, serve)
                    .await
                    .unwrap_or_default(),
                excludes,
            );
            emit_scanning_done(entries.len());
            entries
        } else {
            let entry = match remote_stat_full(&rdest_sub, serve).await {
                Ok((size, mtime, _)) => vec![Entry {
                    rel_path: src_name.clone(),
                    size,
                    mtime,
                    is_dir: false,
                }],
                Err(_) => Vec::new(),
            };
            emit_scanning_done(entry.len());
            entry
        }
    } else {
        let dest_is_dir = dest.exists() && dest.is_dir();
        let resolved_dest = if dest_is_dir {
            dest.join(&src_name)
        } else {
            dest.to_path_buf()
        };
        if resolved_dest.exists() && resolved_dest.is_dir() {
            collect_local_entries(&resolved_dest, excludes).unwrap_or_default()
        } else if resolved_dest.exists() {
            let meta = resolved_dest.metadata()?;
            vec![Entry {
                rel_path: src_name.clone(),
                size: meta.len(),
                mtime: mtime_secs(&meta),
                is_dir: false,
            }]
        } else {
            Vec::new()
        }
    };

    let src_root = match (&remote_src, src_is_dir) {
        (Some(rp), true) => SideRoot::RemoteDir(rp.clone()),
        (Some(rp), false) => SideRoot::RemoteFile(rp.clone()),
        (None, true) => SideRoot::LocalDir(src.to_path_buf()),
        (None, false) => SideRoot::LocalFile(src.to_path_buf()),
    };

    let dst_root = if is_remote_dest {
        let rdest = parse_remote_path(&dest_str)
            .ok_or_else(|| BcmrError::invalid_remote_path(dest.display()))?;
        let rdest_is_dir = remote_is_dir(&rdest, serve).await.unwrap_or(false);
        match (rdest_is_dir, src_is_dir) {
            (true, true) => SideRoot::RemoteDir(rdest.join(&src_name)),
            (true, false) => SideRoot::RemoteFile(rdest.join(&src_name)),
            (false, true) => SideRoot::RemoteDir(rdest),
            (false, false) => SideRoot::RemoteFile(rdest),
        }
    } else {
        let dest_is_dir = dest.exists() && dest.is_dir();
        match (dest_is_dir, src_is_dir) {
            (true, true) => SideRoot::LocalDir(dest.join(&src_name)),
            (true, false) => SideRoot::LocalFile(dest.join(&src_name)),
            (false, true) => SideRoot::LocalDir(dest.to_path_buf()),
            (false, false) => SideRoot::LocalFile(dest.to_path_buf()),
        }
    };

    Ok(CollectResult {
        src_entries,
        dst_entries,
        src_root,
        dst_root,
    })
}

async fn remote_is_dir(
    rp: &RemotePath,
    serve: &mut Option<ServeClient>,
) -> Result<bool, BcmrError> {
    if let Some(ref mut client) = serve {
        let (_, _, is_dir) = client.stat(&rp.path).await?;
        return Ok(is_dir);
    }
    let info = remote::remote_stat(rp).await?;
    Ok(info.is_dir)
}

async fn remote_stat_full(
    rp: &RemotePath,
    serve: &mut Option<ServeClient>,
) -> Result<(u64, i64, bool), BcmrError> {
    if let Some(ref mut client) = serve {
        let (size, mtime, is_dir) = client.stat(&rp.path).await?;
        return Ok((size, mtime as i64, is_dir));
    }
    let info = remote::remote_stat(rp).await?;
    Ok((info.size, 0, info.is_dir))
}

async fn collect_remote_entries(
    rp: &RemotePath,
    serve: &mut Option<ServeClient>,
) -> Result<Vec<Entry>, BcmrError> {
    if let Some(ref mut client) = serve {
        match client.list(&rp.path).await {
            Ok(entries) => {
                return Ok(entries
                    .into_iter()
                    .map(|e| Entry {
                        rel_path: e.path,
                        size: e.size,
                        mtime: e.mtime,
                        is_dir: e.is_dir,
                    })
                    .collect());
            }
            Err(_) => {
                *serve = None;
            }
        }
    }
    let files = remote::remote_list_files(rp).await?;
    Ok(files
        .into_iter()
        .map(|(rel_path, size, is_dir)| Entry {
            rel_path,
            size,
            mtime: 0,
            is_dir,
        })
        .collect())
}

fn filter_entries(entries: Vec<Entry>, excludes: &[regex::Regex]) -> Vec<Entry> {
    if excludes.is_empty() {
        return entries;
    }
    entries
        .into_iter()
        .filter(|e| !excludes.iter().any(|r| r.is_match(&e.rel_path)))
        .collect()
}

fn collect_local_entries(root: &Path, excludes: &[regex::Regex]) -> Result<Vec<Entry>, BcmrError> {
    let mut entries = Vec::new();
    for entry in traversal::walk(root, true, false, 1, excludes) {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(root)?;
        let meta = entry.metadata()?;
        let mt = if meta.is_dir() { 0 } else { mtime_secs(&meta) };
        entries.push(Entry {
            rel_path: relative.to_string_lossy().to_string(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            mtime: mt,
            is_dir: meta.is_dir(),
        });
    }
    Ok(entries)
}

fn emit_connecting(host: &str) {
    if crate::config::is_json_mode() {
        return;
    }
    eprint!("Connecting to {}... ", host);
}

fn emit_scanning(target: &str) {
    if crate::config::is_json_mode() {
        let line = serde_json::json!({"type": "scanning", "target": target});
        println!("{}", line);
    } else {
        eprint!("\rScanning {}...", target);
    }
}

fn emit_scanning_done(count: usize) {
    if crate::config::is_json_mode() {
        return;
    }
    eprintln!(" {} entries", count);
}

fn mtime_secs(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

async fn hash_at(
    root: &SideRoot,
    rel: &Path,
    serve: &mut Option<ServeClient>,
) -> Result<String, BcmrError> {
    match root {
        SideRoot::LocalDir(base) => hash_local(base.join(rel)).await,
        SideRoot::LocalFile(path) => hash_local(path.clone()).await,
        SideRoot::RemoteDir(base) => hash_remote(base.join(&rel.to_string_lossy()), serve).await,
        SideRoot::RemoteFile(rp) => hash_remote(rp.clone(), serve).await,
    }
}

async fn hash_local(abs: PathBuf) -> Result<String, BcmrError> {
    tokio::task::spawn_blocking(move || crate::core::checksum::calculate_hash(&abs))
        .await
        .map_err(|e| BcmrError::InvalidInput(e.to_string()))?
        .map_err(BcmrError::Io)
}

async fn hash_remote(
    target: RemotePath,
    serve: &mut Option<ServeClient>,
) -> Result<String, BcmrError> {
    if let Some(client) = serve.as_mut() {
        let bytes = client.hash(&target.path, 0, None).await?;
        return Ok(bytes.iter().map(|b| format!("{:02x}", b)).collect());
    }
    remote::remote_file_hash(&target, None).await
}

async fn filter_size_match_via_hash(
    candidates: Vec<FileDiff>,
    src_root: &SideRoot,
    dst_root: &SideRoot,
    serve: &mut Option<ServeClient>,
) -> Result<Vec<FileDiff>, BcmrError> {
    let mut kept = Vec::with_capacity(candidates.len());
    for diff in candidates {
        if diff.is_dir {
            kept.push(diff);
            continue;
        }
        if diff.src_size != diff.dst_size {
            kept.push(diff);
            continue;
        }
        let rel = diff.path.clone();
        let src_hash = hash_at(src_root, &rel, serve).await;
        let dst_hash = hash_at(dst_root, &rel, serve).await;
        match (src_hash, dst_hash) {
            (Ok(s), Ok(d)) if s == d => {}
            _ => kept.push(diff),
        }
    }
    Ok(kept)
}

fn modified_diff(s: &Entry, d: &Entry) -> FileDiff {
    FileDiff {
        path: PathBuf::from(&s.rel_path),
        size: None,
        src_size: Some(s.size),
        dst_size: Some(d.size),
        is_dir: false,
    }
}

fn diff_entries(src: &[Entry], dst: &[Entry]) -> (Vec<FileDiff>, Vec<FileDiff>, Vec<FileDiff>) {
    let dst_map: HashMap<&str, &Entry> = dst.iter().map(|e| (e.rel_path.as_str(), e)).collect();
    let src_map: HashMap<&str, &Entry> = src.iter().map(|e| (e.rel_path.as_str(), e)).collect();

    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut missing = Vec::new();

    for s in src {
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
            Some(d)
                if !s.is_dir
                    && (s.size != d.size
                        || (s.mtime != 0 && d.mtime != 0 && s.mtime != d.mtime)) =>
            {
                modified.push(modified_diff(s, d));
            }
            _ => {}
        }
    }

    for d in dst {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> Entry {
        Entry {
            rel_path: path.into(),
            size: 0,
            mtime: 0,
            is_dir: false,
        }
    }

    #[test]
    fn filter_entries_matches_local_traversal_semantics() {
        let excludes = vec![regex::Regex::new(r"\.log$").unwrap()];
        let out = filter_entries(
            vec![entry("a/b.txt"), entry("a/c.log"), entry("d.LOG")],
            &excludes,
        );
        let paths: Vec<_> = out.iter().map(|e| e.rel_path.as_str()).collect();
        assert_eq!(paths, vec!["a/b.txt", "d.LOG"]);
    }

    #[test]
    fn filter_entries_noop_without_rules() {
        let out = filter_entries(vec![entry("x"), entry("y")], &[]);
        assert_eq!(out.len(), 2);
    }
}
