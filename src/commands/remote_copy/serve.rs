use super::{
    collect_download_plan, collect_upload_plan, preserve_directories, progress_mode,
    transfer_options_from_cli, TransferItem, STRIPING_MIN_FILE_SIZE,
};
use crate::cli::Commands;
use crate::core::checksum::bytes_to_hex;
use crate::core::file_metadata::PortableFileMetadata;
use crate::core::remote::staged_upload::StagedUpload;
use crate::core::remote::{check_resume_state, RemotePath, ResumeDecision};
use crate::core::serve_client::{FileTransfer, ServeClientPool};
use crate::ui::runner::ProgressRunner;
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

type UploadMetadata = std::collections::HashMap<PathBuf, PortableFileMetadata>;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct ServeFallback(String);

fn fallback_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(ServeFallback(message.into()))
}

pub(super) fn allows_legacy_fallback(error: &anyhow::Error) -> bool {
    error.downcast_ref::<ServeFallback>().is_some()
}

enum UploadDecision {
    Skip,
    Overwrite,
    Append(u64),
}

async fn upload_resume_offset(
    pool: &mut ServeClientPool,
    args: &Commands,
    local_src: &Path,
    remote_path: &str,
    local_size: u64,
) -> Result<Option<UploadDecision>> {
    let opts = transfer_options_from_cli(args);
    if !(opts.resume || opts.append || opts.strict) {
        return Ok(None);
    }
    let existing_size = match pool.first_mut().stat(remote_path).await {
        Ok((size, _mtime, is_dir)) => {
            if is_dir {
                bail!("remote path {remote_path} is a directory");
            }
            Some(size)
        }
        Err(_) => None,
    };
    let local_clone = local_src.to_path_buf();
    let remote_str = remote_path.to_string();
    let decision = check_resume_state(
        &opts,
        existing_size,
        local_size,
        async move || {
            let bytes = pool_hash(pool, &remote_str, None).await?;
            Ok(bytes_to_hex(&bytes))
        },
        async move || {
            let p = local_clone.clone();
            let h = tokio::task::spawn_blocking(move || crate::core::checksum::calculate_hash(&p))
                .await??;
            Ok(h)
        },
        async move |limit| {
            let p = local_src.to_path_buf();
            let h = tokio::task::spawn_blocking(move || {
                crate::core::checksum::calculate_partial_hash(&p, limit)
            })
            .await??;
            Ok(h)
        },
    )
    .await?;
    Ok(Some(to_upload_decision(decision)))
}

async fn pool_hash(
    pool: &mut ServeClientPool,
    remote: &str,
    limit: Option<u64>,
) -> Result<[u8; 32], crate::core::error::BcmrError> {
    pool.first_mut().hash(remote, 0, limit).await
}

fn to_upload_decision(d: ResumeDecision) -> UploadDecision {
    if d.skip_entirely {
        UploadDecision::Skip
    } else if d.use_append_mode && d.skip_bytes > 0 {
        UploadDecision::Append(d.skip_bytes)
    } else {
        UploadDecision::Overwrite
    }
}

fn serve_upload_pool_size(has_recursive_directory: bool, parallel: usize) -> usize {
    if has_recursive_directory {
        parallel
    } else {
        1
    }
}

pub(super) async fn handle_serve_upload(
    args: &Commands,
    sources: &[PathBuf],
    rdest: &RemotePath,
    ssh_target: &str,
    excludes: &[regex::Regex],
    parallel: usize,
) -> Result<()> {
    crate::core::interrupt::check()?;
    if args.is_dry_run() {
        return Err(fallback_error("serve: dry-run fallback to legacy"));
    }
    let plan = collect_upload_plan(sources, rdest, args.is_recursive(), excludes)?;
    let original_metadata: UploadMetadata = if args.is_preserve() {
        plan.files
            .iter()
            .map(|file| {
                Ok((
                    file.local_path.clone(),
                    super::endpoint::local_metadata(&file.local_path.metadata()?),
                ))
            })
            .collect::<std::io::Result<_>>()?
    } else {
        Default::default()
    };
    let has_recursive_directory = !plan.directories.is_empty();
    if has_recursive_directory && (args.is_resume() || args.is_strict() || args.is_append()) {
        return Err(fallback_error(
            "serve: recursive --resume/--strict/--append not supported, fallback to legacy",
        ));
    }
    if plan.files.is_empty() && plan.directories.is_empty() {
        return Ok(());
    }
    if args.is_sync() && (args.is_preserve() || args.is_verify()) {
        bail!("--sync cannot guarantee durable remote namespace publication with --preserve or --verify uploads; refusing before transfer");
    }
    let pool_size = serve_upload_pool_size(has_recursive_directory, parallel);
    let mut pool = if args.use_direct_tcp() {
        ServeClientPool::connect_direct_with_caps(ssh_target, args.protocol_caps(), pool_size).await
    } else {
        ServeClientPool::connect_with_caps(ssh_target, args.protocol_caps(), pool_size).await
    }
    .map_err(|e| fallback_error(format!("serve unavailable: {e}")))?;
    if args.is_sync() && !pool.supports_sync() {
        pool.close().await?;
        bail!("--sync requires every remote serve connection to advertise CAP_SYNC; refusing before transfer");
    }
    crate::core::interrupt::check()?;
    let runner = ProgressRunner::new(
        plan.total_size,
        progress_mode(args),
        args.is_quiet(),
        crate::config::is_json_mode(),
        crate::core::cleanup::cleanup_partial_files,
    )?;
    runner.set_operation_type("Uploading (serve)");
    runner.set_verify_mode(args.is_verify());
    for directory in &plan.directories {
        pool.mkdir(&directory.remote.path).await?;
    }

    if args.is_resume() || args.is_strict() || args.is_append() {
        for file in plan.files {
            let decision = upload_resume_offset(
                &mut pool,
                args,
                &file.local_path,
                &file.remote.path,
                file.size,
            )
            .await?;
            let resume_overwrite = matches!(
                &decision,
                Some(UploadDecision::Overwrite | UploadDecision::Append(_))
            );
            if matches!(&decision, Some(UploadDecision::Skip)) {
                (runner.inc_callback())(file.size);
                continue;
            }
            let mut offset = match decision {
                Some(UploadDecision::Append(offset)) => offset,
                _ => 0,
            };
            if offset > 0 && !pool.first_mut().supports_put_offset() {
                if !crate::config::is_json_mode() {
                    eprintln!(
                        "bcmr: remote server does not advertise CAP_PUT_OFFSET; re-uploading '{}' from scratch",
                        file.local_path.display()
                    );
                }
                offset = 0;
            }
            if offset == 0 {
                serve_upload_fresh_files(
                    &mut pool,
                    vec![file],
                    &runner,
                    args,
                    args.is_force() || resume_overwrite,
                    &original_metadata,
                )
                .await?;
                continue;
            }
            (runner.file_callback())(
                &file
                    .local_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                file.size,
            );
            pool.first_mut()
                .put_at(&file.remote.path, &file.local_path, offset)
                .await?;
            if args.is_verify() {
                let source = file.local_path.clone();
                let local_hash = tokio::task::spawn_blocking(move || {
                    crate::core::checksum::calculate_hash(&source)
                })
                .await??;
                let remote_hash = pool.first_mut().hash(&file.remote.path, 0, None).await?;
                if bytes_to_hex(&remote_hash) != local_hash {
                    bail!("hash mismatch for {}", file.local_path.display());
                }
            }
            if args.is_preserve() {
                crate::core::remote::set_remote_attrs(
                    &file.remote,
                    original_metadata[&file.local_path],
                )
                .await?;
            }
            (runner.inc_callback())(file.size);
        }
    } else {
        serve_upload_fresh_files(
            &mut pool,
            plan.files,
            &runner,
            args,
            args.is_force(),
            &original_metadata,
        )
        .await?;
    }
    if args.is_preserve() {
        preserve_directories(&plan.directories, true, args.is_sync()).await?;
    }
    pool.close().await?;
    runner.finish_ok()
}

async fn transfer_serve_batch(
    pool: &mut ServeClientPool,
    mut files: Vec<FileTransfer>,
    overwrite: bool,
    runner: &ProgressRunner,
) -> Result<Vec<[u8; 32]>> {
    if files.len() == 1 {
        let file = files.pop().expect("one transfer");
        (runner.file_callback())(
            &file.local.file_name().unwrap_or_default().to_string_lossy(),
            file.size,
        );
        let hash = if overwrite {
            pool.first_mut()
                .put_overwrite(&file.remote, &file.local)
                .await?
        } else {
            pool.first_mut().put(&file.remote, &file.local).await?
        };
        (runner.inc_callback())(file.size);
        return Ok(vec![hash]);
    }
    let file_callback = runner.file_callback();
    Ok(pool
        .pipelined_put_files_striped(
            files,
            overwrite,
            runner.inc_callback(),
            move |_index, path, size| {
                file_callback(
                    &path.file_name().unwrap_or_default().to_string_lossy(),
                    size,
                );
            },
        )
        .await?)
}

async fn serve_upload_fresh_files(
    pool: &mut ServeClientPool,
    files: Vec<TransferItem>,
    runner: &ProgressRunner,
    args: &Commands,
    allow_overwrite: bool,
    original_metadata: &UploadMetadata,
) -> Result<()> {
    if args.is_sync() && (args.is_preserve() || args.is_verify()) {
        bail!("--sync cannot guarantee durable remote namespace publication with --preserve or --verify uploads; refusing before transfer");
    }
    if !args.is_preserve() && !args.is_verify() {
        let transfers = files
            .into_iter()
            .map(|file| FileTransfer {
                remote: file.remote.path,
                local: file.local_path,
                size: file.size,
                metadata: None,
            })
            .collect();
        transfer_serve_batch(pool, transfers, allow_overwrite, runner).await?;
        return Ok(());
    }

    // The PUT protocol publishes before returning its hash and has no metadata
    // payload. Keep -V and -p private until their checks finish, then publish
    // through the same exact-target transaction used by legacy uploads.
    let parents = files
        .iter()
        .filter_map(|file| file.remote.path.rsplit_once('/').map(|(parent, _)| parent))
        .filter(|parent| !parent.is_empty())
        .collect::<std::collections::BTreeSet<_>>();
    for parent in parents {
        pool.mkdir(parent).await?;
    }
    let mut stages: Vec<StagedUpload> = Vec::with_capacity(files.len());
    for file in &files {
        match StagedUpload::create(&file.remote, allow_overwrite).await {
            Ok(stage) => stages.push(stage),
            Err(error) => {
                for stage in &stages {
                    stage.cleanup().await;
                }
                return Err(error.into());
            }
        }
    }
    let result: Result<()> = async {
        let transfers = files
            .iter()
            .zip(&stages)
            .map(|(file, stage)| FileTransfer {
                remote: stage.remote().path.clone(),
                local: file.local_path.clone(),
                size: file.size,
                metadata: None,
            })
            .collect();
        let hashes = transfer_serve_batch(pool, transfers, false, runner).await?;
        if hashes.len() != files.len() {
            bail!("serve upload returned an incomplete hash list");
        }
        for ((file, stage), hash) in files.iter().zip(&mut stages).zip(&hashes) {
            stage.prepare().await?;
            if args.is_verify() {
                let source = file.local_path.clone();
                let local_hash = tokio::task::spawn_blocking(move || {
                    crate::core::checksum::calculate_hash(&source)
                })
                .await??;
                if bytes_to_hex(hash) != local_hash {
                    bail!("hash mismatch for {}", file.local_path.display());
                }
            }
            if args.is_preserve() {
                crate::core::remote::set_remote_attrs(
                    stage.remote(),
                    original_metadata[&file.local_path],
                )
                .await?;
            }
        }
        for stage in &mut stages {
            stage.commit().await?;
        }
        Ok(())
    }
    .await;
    for stage in &stages {
        stage.cleanup().await;
    }
    result
}

pub(super) async fn handle_serve_download(
    args: &Commands,
    sources: &[PathBuf],
    dest: &std::path::Path,
    ssh_target: &str,
    excludes: &[regex::Regex],
    parallel: usize,
) -> Result<()> {
    #[cfg(windows)]
    if args.is_sync() {
        bail!("--sync cannot guarantee durable local namespace publication on Windows; refusing before transfer");
    }
    if args.is_resume() || args.is_strict() || args.is_append() {
        return Err(fallback_error(
            "serve: download --resume/--strict/--append not yet supported, fallback to legacy",
        ));
    }
    crate::core::interrupt::check()?;
    if args.is_dry_run() {
        return Err(fallback_error("serve: dry-run fallback to legacy"));
    }
    // SSH inventory is shared with legacy downloads. Older serve peers whose
    // LIST does not follow directory links therefore retain correct selection.
    let plan = collect_download_plan(sources, dest, args.is_recursive(), excludes).await?;
    if plan.files.is_empty() && plan.directories.is_empty() {
        return Ok(());
    }
    let mut pool = if args.use_direct_tcp() {
        ServeClientPool::connect_direct_with_caps(ssh_target, args.protocol_caps(), parallel).await
    } else {
        ServeClientPool::connect_with_caps(ssh_target, args.protocol_caps(), parallel).await
    }
    .map_err(|error| fallback_error(format!("serve unavailable: {error}")))?;
    crate::core::interrupt::check()?;
    let runner = ProgressRunner::new(
        plan.total_size,
        progress_mode(args),
        args.is_quiet(),
        crate::config::is_json_mode(),
        crate::core::cleanup::cleanup_partial_files,
    )?;
    runner.set_operation_type("Downloading (serve)");
    runner.set_verify_mode(args.is_verify());
    for directory in &plan.directories {
        if args.is_sync() {
            crate::core::io::create_dir_all_durable_async(&directory.local_path).await?;
        } else {
            tokio::fs::create_dir_all(&directory.local_path).await?;
        }
    }
    let use_stripe = args.use_direct_tcp() && pool.len() > 1 && !args.is_verify();
    let mut big_files = Vec::new();
    let mut files_to_get = Vec::new();
    for item in plan.files {
        let metadata = if args.is_preserve() {
            Some(crate::core::remote::get_remote_attrs(&item.remote).await?)
        } else {
            None
        };
        let transfer = FileTransfer {
            remote: item.remote.path,
            local: item.local_path,
            size: item.size,
            metadata,
        };
        if use_stripe && transfer.size >= STRIPING_MIN_FILE_SIZE {
            big_files.push(transfer);
        } else {
            files_to_get.push(transfer);
        }
    }
    for file in big_files {
        (runner.file_callback())(
            &file.local.file_name().unwrap_or_default().to_string_lossy(),
            file.size,
        );
        pool.striped_get_file_synced_with_metadata_and_overwrite(
            &file.remote,
            &file.local,
            file.size,
            args.is_sync(),
            file.metadata,
            args.is_force(),
        )
        .await?;
        (runner.inc_callback())(file.size);
    }
    if !files_to_get.is_empty() {
        let file_callback = runner.file_callback();
        pool.pipelined_get_files_striped_with_overwrite(
            files_to_get,
            args.is_sync(),
            args.is_verify(),
            args.is_force(),
            move |_index, path, size| {
                file_callback(
                    &path.file_name().unwrap_or_default().to_string_lossy(),
                    size,
                );
            },
            runner.inc_callback(),
        )
        .await?;
    }
    if args.is_preserve() {
        preserve_directories(&plan.directories, false, args.is_sync()).await?;
    }
    pool.close().await?;
    runner.finish_ok()
}

#[cfg(test)]
mod fallback_tests {
    use super::super::add_transfer_size;
    use super::{allows_legacy_fallback, fallback_error, serve_upload_pool_size};

    #[test]
    fn only_typed_preflight_failures_allow_legacy_fallback() {
        assert!(allows_legacy_fallback(&fallback_error(
            "serve unavailable: protocol negotiation failed"
        )));
        assert!(
            !allows_legacy_fallback(&anyhow::anyhow!(
                "transfer failed after mutation; dry-run fallback"
            )),
            "message text alone must never authorize a second transport to mutate the destination"
        );
        assert!(!allows_legacy_fallback(&anyhow::anyhow!(
            "destination changed during atomic publish"
        )));
    }

    #[test]
    fn aggregate_transfer_size_fails_closed_on_overflow() {
        let mut total = u64::MAX - 1;
        assert!(add_transfer_size(&mut total, 2).is_err());
        assert_eq!(total, u64::MAX - 1);
    }

    #[test]
    fn single_file_upload_does_not_open_unused_parallel_sessions() {
        assert_eq!(serve_upload_pool_size(false, 8), 1);
        assert_eq!(serve_upload_pool_size(true, 8), 8);
    }
}
