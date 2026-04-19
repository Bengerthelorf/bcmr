use crate::cli::Commands;
use crate::core::error::BcmrError;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;

use super::file_copy::{copy_file, CopyFileOptions};
use super::overwrite::check_overwrite;
use super::{preserve_attributes, scan_sources, PlanEntry, ProgressCallback};

enum ScanMessage {
    Entry(PlanEntry),
    Done,
}

type BoxCallback = Box<dyn Fn(u64) + Send + Sync>;
type BoxFileCallback = Box<dyn Fn(&str, u64) + Send + Sync>;
type BoxNotify = Box<dyn Fn() + Send + Sync>;

pub struct PipelineCallbacks<F: Fn(u64) + Send + Sync> {
    pub on_progress: F,
    pub on_new_file: BoxFileCallback,
    pub on_total_update: BoxCallback,
    pub on_scan_complete: BoxNotify,
    pub on_file_found: BoxCallback,
}

pub async fn pipeline_copy<F>(
    sources: &[PathBuf],
    dst: &Path,
    cli: &Commands,
    excludes: &[regex::Regex],
    cb: PipelineCallbacks<F>,
) -> std::result::Result<(), BcmrError>
where
    F: Fn(u64) + Send + Sync + Clone + 'static,
{
    let test_mode = cli.get_test_mode();
    let recursive = cli.is_recursive();
    let jobs = cli.local_jobs();
    let verbose = cli.is_verbose();
    let callback = ProgressCallback {
        callback: cb.on_progress,
        on_new_file: Arc::from(cb.on_new_file),
    };
    let on_total_update = cb.on_total_update;
    let on_scan_complete = cb.on_scan_complete;
    let on_file_found = cb.on_file_found;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<ScanMessage>(256);

    let sources = sources.to_vec();
    let dst = dst.to_path_buf();
    let excludes = excludes.to_vec();
    let scanner = tokio::task::spawn_blocking(move || {
        let mut total_size = 0u64;
        let mut files_found = 0u64;

        let result = scan_sources(&sources, &dst, recursive, &excludes, |entry, size| {
            total_size += size;
            if size > 0 {
                files_found += 1;
                on_total_update(total_size);
                on_file_found(files_found);
            }
            if tx.blocking_send(ScanMessage::Entry(entry)).is_err() {
                return Ok(());
            }
            Ok(())
        });

        let _ = tx.blocking_send(ScanMessage::Done);
        result
    });

    let mut dir_entries: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut in_flight = tokio::task::JoinSet::new();

    while let Some(msg) = rx.recv().await {
        match msg {
            ScanMessage::Entry(entry) => match entry {
                PlanEntry::CreateDir { ref src, ref dst } => {
                    if !dst.exists() {
                        fs::create_dir_all(dst).await?;
                    }
                    dir_entries.push((src.clone(), dst.clone()));
                }
                PlanEntry::CopyFile { ref src, ref dst } => {
                    check_overwrite(dst, cli).await?;

                    while in_flight.len() >= jobs {
                        match in_flight.join_next().await {
                            Some(res) => res??,
                            None => break,
                        }
                    }

                    let src = src.clone();
                    let dst = dst.clone();
                    let opts = CopyFileOptions::from_cli(cli, test_mode.clone());
                    let cb = callback.clone();
                    in_flight.spawn(async move {
                        copy_file(&src, &dst, opts, &cb).await?;
                        if verbose {
                            eprintln!("'{}' -> '{}'", src.display(), dst.display());
                        }
                        Ok::<(), BcmrError>(())
                    });
                }
            },
            ScanMessage::Done => {
                on_scan_complete();
                break;
            }
        }
    }

    while let Some(res) = in_flight.join_next().await {
        res??;
    }

    scanner.await??;

    if cli.is_preserve() {
        for (src, dst) in dir_entries.iter().rev() {
            preserve_attributes(src, dst).await?;
        }
    }

    Ok(())
}
