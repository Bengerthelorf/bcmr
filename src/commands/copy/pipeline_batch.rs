use crate::cli::Commands;
use crate::core::error::BcmrError;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;

use super::exec::{preserve_attributes, ProgressCallback};
use super::file_copy::{copy_file, CopyFileOptions};
use super::overwrite::check_overwrite;
use super::plan::{scan_sources, PlanEntry};
use super::symlinks::{check_symlink_overwrite, create_symlink_replacing};

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
    pub on_reflink: BoxNotify,
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
    #[cfg(not(unix))]
    if cli.is_sync() && !cli.is_dry_run() {
        return Err(BcmrError::InvalidInput(
            "--sync cannot guarantee durable local namespace publication on this platform; refusing before transfer".into(),
        ));
    }
    let test_mode = cli.get_test_mode();
    CopyFileOptions::from_cli(cli, test_mode.clone()).validate_reflink_compatibility()?;
    let recursive = cli.is_recursive();
    let no_deref = cli.is_no_deref() || matches!(cli, Commands::Move { .. });
    let jobs = cli.local_jobs();
    let verbose = cli.is_verbose();
    let callback = ProgressCallback {
        callback: cb.on_progress,
        on_new_file: Arc::from(cb.on_new_file),
        on_reflink: Arc::from(cb.on_reflink),
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

        let result = scan_sources(
            &sources,
            &dst,
            recursive,
            no_deref,
            &excludes,
            |entry, size| {
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
            },
        );

        let _ = tx.blocking_send(ScanMessage::Done);
        result
    });

    let mut dir_entries: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut in_flight = tokio::task::JoinSet::new();

    while let Some(msg) = rx.recv().await {
        match msg {
            ScanMessage::Entry(entry) => match entry {
                PlanEntry::CreateDir { ref src, ref dst } => {
                    if cli.is_sync() {
                        crate::core::io::create_dir_all_durable_async(dst).await?;
                    } else {
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
                PlanEntry::Symlink {
                    ref dst,
                    ref target,
                    kind,
                    ..
                } => {
                    check_symlink_overwrite(dst, kind, cli)?;
                    if let Some(parent) =
                        dst.parent().filter(|parent| !parent.as_os_str().is_empty())
                    {
                        if cli.is_sync() {
                            crate::core::io::create_dir_all_durable_async(parent).await?;
                        } else {
                            fs::create_dir_all(parent).await?;
                        }
                    }
                    create_symlink_replacing(
                        dst,
                        target,
                        kind,
                        cli.is_force(),
                        cli.is_sync(),
                        &test_mode,
                    )
                    .await?;
                    if verbose {
                        eprintln!("'{}' -> '{}' (symlink)", target.display(), dst.display());
                    }
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
            #[cfg(unix)]
            let handle = if cli.is_sync() {
                Some(fs::File::open(dst).await?.into_std().await)
            } else {
                None
            };
            preserve_attributes(src, dst).await?;
            #[cfg(unix)]
            if let Some(handle) = handle {
                tokio::task::spawn_blocking(move || {
                    crate::core::io::durable_sync_directory_handle_strict(&handle)
                })
                .await??;
            }
        }
    }

    Ok(())
}
