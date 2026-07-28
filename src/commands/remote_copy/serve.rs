use super::{is_plain_mode, transfer_options_from_cli, STRIPING_MIN_FILE_SIZE};
use crate::cli::Commands;
use crate::core::checksum::bytes_to_hex;
use crate::core::remote::{check_resume_state, parse_remote_path, RemotePath, ResumeDecision};
use crate::core::serve_client::{FileTransfer, ServeClientPool};
use crate::ui::runner::ProgressRunner;
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct ServeFallback(String);

fn fallback_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(ServeFallback(message.into()))
}

fn add_transfer_size(total: &mut u64, size: u64) -> Result<()> {
    *total = total
        .checked_add(size)
        .ok_or_else(|| anyhow::anyhow!("declared transfer size exceeds u64"))?;
    Ok(())
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

pub(super) async fn handle_serve_upload(
    args: &Commands,
    sources: &[PathBuf],
    rdest: &RemotePath,
    ssh_target: &str,
    excludes: &[regex::Regex],
    parallel: usize,
) -> Result<()> {
    if args.is_dry_run() {
        return Err(fallback_error("serve: dry-run fallback to legacy"));
    }
    if (args.is_resume() || args.is_strict() || args.is_append())
        && args.is_recursive()
        && sources.iter().any(|source| source.is_dir())
    {
        return Err(fallback_error(
            "serve: recursive --resume/--strict/--append not supported, fallback to legacy",
        ));
    }

    let mut pool = if args.use_direct_tcp() {
        ServeClientPool::connect_direct_with_caps(ssh_target, args.protocol_caps(), parallel).await
    } else {
        ServeClientPool::connect_with_caps(ssh_target, args.protocol_caps(), parallel).await
    }
    .map_err(|e| fallback_error(format!("serve unavailable: {e}")))?;

    let mut total_size = 0u64;
    for src in sources {
        if src.is_file() {
            add_transfer_size(&mut total_size, src.metadata()?.len())?;
        } else if src.is_dir() && args.is_recursive() {
            add_transfer_size(
                &mut total_size,
                crate::commands::copy::get_total_size(std::slice::from_ref(src), true, args, &[])
                    .await?,
            )?;
        }
    }

    let runner = ProgressRunner::new(
        total_size,
        is_plain_mode(args),
        args.is_quiet(),
        crate::config::is_json_mode(),
        crate::core::cleanup::cleanup_partial_files,
    )?;
    runner.set_operation_type("Uploading (serve)");
    runner.set_verify_mode(args.is_verify());

    let multi_source = sources.len() > 1;
    for src in sources {
        if crate::core::traversal::is_excluded(src, excludes) {
            continue;
        }
        if src.is_file() {
            let remote_path = if multi_source || rdest.path.ends_with('/') {
                format!(
                    "{}/{}",
                    rdest.path,
                    src.file_name().unwrap_or_default().to_string_lossy()
                )
            } else {
                rdest.path.clone()
            };
            let size = src.metadata()?.len();
            (runner.file_callback())(&src.file_name().unwrap_or_default().to_string_lossy(), size);

            let resume_offset =
                upload_resume_offset(&mut pool, args, src, &remote_path, size).await?;
            if let Some(UploadDecision::Skip) = resume_offset {
                (runner.inc_callback())(size);
                continue;
            }
            let mut offset = match resume_offset {
                Some(UploadDecision::Append(o)) => o,
                _ => 0,
            };
            if offset > 0 && !pool.first_mut().supports_put_offset() {
                if !crate::config::is_json_mode() {
                    eprintln!(
                        "bcmr: remote server does not advertise CAP_PUT_OFFSET; \
                         re-uploading '{}' from scratch (resume not supported by this server)",
                        src.display()
                    );
                }
                offset = 0;
            }

            if offset > 0 {
                pool.first_mut().put_at(&remote_path, src, offset).await?;
                if args.is_verify() {
                    let p = src.to_path_buf();
                    let local_hash = tokio::task::spawn_blocking(move || {
                        crate::core::checksum::calculate_hash(&p)
                    })
                    .await??;
                    let remote_hash = pool.first_mut().hash(&remote_path, 0, None).await?;
                    let remote_hex = bytes_to_hex(&remote_hash);
                    if remote_hex != local_hash {
                        pool.close().await?;
                        return runner.finish_err(format!("hash mismatch for {}", src.display()));
                    }
                }
            } else {
                // Multi-connection striped PUT currently writes directly
                // into the visible destination and cannot provide crash-safe
                // publication. Keep production uploads on the handle-bound
                // transaction path until the protocol has a server-side
                // transaction token shared across connections.
                let server_hash = if args.is_force() {
                    pool.first_mut().put_overwrite(&remote_path, src).await?
                } else {
                    pool.first_mut().put(&remote_path, src).await?
                };
                if args.is_verify() {
                    let p = src.to_path_buf();
                    let local_hash = tokio::task::spawn_blocking(move || {
                        crate::core::checksum::calculate_hash(&p)
                    })
                    .await??;
                    let server_hex = bytes_to_hex(&server_hash);
                    if server_hex != local_hash {
                        pool.close().await?;
                        return runner.finish_err(format!("hash mismatch for {}", src.display()));
                    }
                }
            }
            if args.is_preserve() {
                let target = RemotePath {
                    user: rdest.user.clone(),
                    host: rdest.host.clone(),
                    path: remote_path.clone(),
                };
                crate::core::remote::preserve_remote_attrs(src, &target).await?;
            }
            (runner.inc_callback())(size);
        } else if src.is_dir() && args.is_recursive() {
            if args.is_resume() || args.is_strict() || args.is_append() {
                pool.close().await?;
                bail!(
                    "serve: source changed to a directory after recursive resume preflight; \
                     refusing fallback after transfer processing began"
                );
            }
            serve_upload_dir(&mut pool, src, rdest, &runner, excludes, args).await?;
        } else if src.is_dir() {
            pool.close().await?;
            bail!(
                "Source '{}' is a directory. Use -r flag for recursive copy.",
                src.display()
            );
        }
    }

    pool.close().await?;
    runner.finish_ok()
}

async fn serve_upload_dir(
    pool: &mut ServeClientPool,
    local_dir: &std::path::Path,
    remote_base: &RemotePath,
    runner: &ProgressRunner,
    excludes: &[regex::Regex],
    args: &Commands,
) -> Result<()> {
    let dir_name = local_dir.file_name().unwrap_or_default().to_string_lossy();
    let remote_dir = format!("{}/{}", remote_base.path, dir_name);
    pool.mkdir(&remote_dir).await?;

    let mut files_to_put: Vec<FileTransfer> = Vec::new();
    for entry in crate::core::traversal::walk(local_dir, true, false, 1, excludes) {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(local_dir)?;
        let remote_path = format!("{}/{}", remote_dir, rel.to_string_lossy());
        if path.is_dir() {
            pool.mkdir(&remote_path).await?;
        } else if path.is_file() {
            files_to_put.push(FileTransfer {
                remote: remote_path,
                local: path.to_path_buf(),
                size: entry.metadata()?.len(),
                metadata: None,
            });
        }
    }

    let per_file_inputs: Vec<(PathBuf, String)> = files_to_put
        .iter()
        .map(|f| (f.local.clone(), f.remote.clone()))
        .collect();

    let file_cb = runner.file_callback();
    let inc_for_chunks = runner.inc_callback();
    let server_hashes = pool
        .pipelined_put_files_striped(
            files_to_put,
            args.is_force(),
            inc_for_chunks,
            move |_idx, path, size| {
                file_cb(
                    &path.file_name().unwrap_or_default().to_string_lossy(),
                    size,
                );
            },
        )
        .await?;

    if args.is_verify() {
        for ((local_path, _), server_hash) in per_file_inputs.iter().zip(server_hashes.iter()) {
            let p = local_path.clone();
            let local_hash =
                tokio::task::spawn_blocking(move || crate::core::checksum::calculate_hash(&p))
                    .await??;
            let server_hex = bytes_to_hex(server_hash);
            if server_hex != local_hash {
                bail!("hash mismatch for {}", local_path.display());
            }
        }
    }
    if args.is_preserve() {
        for (local_path, remote_path) in &per_file_inputs {
            let target = RemotePath {
                user: remote_base.user.clone(),
                host: remote_base.host.clone(),
                path: remote_path.clone(),
            };
            crate::core::remote::preserve_remote_attrs(local_path, &target).await?;
        }
    }
    Ok(())
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
        bail!(
            "--sync cannot guarantee durable local namespace publication on Windows; refusing before transfer"
        );
    }
    if args.is_resume() || args.is_strict() || args.is_append() {
        return Err(fallback_error(
            "serve: download --resume/--strict/--append not yet supported, fallback to legacy",
        ));
    }
    if args.is_dry_run() {
        return Err(fallback_error("serve: dry-run fallback to legacy"));
    }

    let mut pool = if args.use_direct_tcp() {
        ServeClientPool::connect_direct_with_caps(ssh_target, args.protocol_caps(), parallel).await
    } else {
        ServeClientPool::connect_with_caps(ssh_target, args.protocol_caps(), parallel).await
    }
    .map_err(|e| fallback_error(format!("serve unavailable: {e}")))?;

    struct DownloadItem {
        remote_path: String,
        local_path: PathBuf,
        size: u64,
        is_dir: bool,
        metadata: Option<crate::core::file_metadata::PortableFileMetadata>,
    }

    let mut total_size = 0u64;
    let mut items: Vec<DownloadItem> = Vec::new();

    for src in sources {
        if crate::core::traversal::is_excluded(src, excludes) {
            continue;
        }
        let src_str = src.to_string_lossy();
        if let Some(rp) = parse_remote_path(&src_str) {
            let (size, _mtime, is_dir) = pool.first_mut().stat(&rp.path).await?;
            if is_dir && !args.is_recursive() {
                pool.close().await?;
                bail!(
                    "Remote source '{}' is a directory. Use -r flag for recursive copy.",
                    rp
                );
            }
            if is_dir && args.is_recursive() {
                let entries = pool.first_mut().list(&rp.path).await?;
                let dir_name = rp.file_name();
                let local_base = dest.join(dir_name);
                items.push(DownloadItem {
                    remote_path: String::new(),
                    local_path: local_base.clone(),
                    size: 0,
                    is_dir: true,
                    metadata: None,
                });
                for entry in &entries {
                    if crate::core::traversal::is_excluded(
                        std::path::Path::new(&entry.path),
                        excludes,
                    ) {
                        continue;
                    }
                    let local = local_base.join(&entry.path);
                    let remote = format!("{}/{}", rp.path, entry.path);
                    if entry.is_dir {
                        items.push(DownloadItem {
                            remote_path: remote,
                            local_path: local,
                            size: 0,
                            is_dir: true,
                            metadata: None,
                        });
                    } else {
                        add_transfer_size(&mut total_size, entry.size)?;
                        items.push(DownloadItem {
                            remote_path: remote,
                            local_path: local,
                            size: entry.size,
                            is_dir: false,
                            metadata: None,
                        });
                    }
                }
            } else if !is_dir {
                add_transfer_size(&mut total_size, size)?;
                let local = if dest.is_dir() {
                    dest.join(rp.file_name())
                } else {
                    dest.to_path_buf()
                };
                items.push(DownloadItem {
                    remote_path: rp.path.clone(),
                    local_path: local,
                    size,
                    is_dir: false,
                    metadata: None,
                });
            }
        }
    }

    if args.is_preserve() {
        let (user, host) = match ssh_target.split_once('@') {
            Some((user, host)) => (Some(user.to_string()), host.to_string()),
            None => (None, ssh_target.to_string()),
        };
        for item in &mut items {
            if item.is_dir {
                continue;
            }
            let remote = RemotePath {
                user: user.clone(),
                host: host.clone(),
                path: item.remote_path.clone(),
            };
            item.metadata = Some(crate::core::remote::get_remote_attrs(&remote).await?);
        }
    }

    let runner = ProgressRunner::new(
        total_size,
        is_plain_mode(args),
        args.is_quiet(),
        crate::config::is_json_mode(),
        crate::core::cleanup::cleanup_partial_files,
    )?;
    runner.set_operation_type("Downloading (serve)");
    runner.set_verify_mode(args.is_verify());

    let use_stripe = args.use_direct_tcp() && pool.len() > 1 && !args.is_verify();
    let mut big_files: Vec<(
        String,
        PathBuf,
        u64,
        Option<crate::core::file_metadata::PortableFileMetadata>,
    )> = Vec::new();
    let mut files_to_get: Vec<FileTransfer> = Vec::new();
    for item in &items {
        if item.is_dir {
            if args.is_sync() {
                crate::core::io::create_dir_all_durable_async(&item.local_path).await?;
            } else {
                tokio::fs::create_dir_all(&item.local_path).await?;
            }
        } else if use_stripe && item.size >= STRIPING_MIN_FILE_SIZE {
            big_files.push((
                item.remote_path.clone(),
                item.local_path.clone(),
                item.size,
                item.metadata,
            ));
        } else {
            files_to_get.push(FileTransfer {
                remote: item.remote_path.clone(),
                local: item.local_path.clone(),
                size: item.size,
                metadata: item.metadata,
            });
        }
    }

    for (remote_path, local_path, size, metadata) in &big_files {
        (runner.file_callback())(
            &local_path.file_name().unwrap_or_default().to_string_lossy(),
            *size,
        );
        let _ = pool
            .striped_get_file_synced_with_metadata(
                remote_path,
                local_path,
                *size,
                args.is_sync(),
                *metadata,
            )
            .await?;
        (runner.inc_callback())(*size);
    }

    if !files_to_get.is_empty() {
        let file_cb = runner.file_callback();
        let inc = runner.inc_callback();
        let sync = args.is_sync();
        pool.pipelined_get_files_striped(
            files_to_get,
            sync,
            args.is_verify(),
            move |_idx, path, size| {
                file_cb(
                    &path.file_name().unwrap_or_default().to_string_lossy(),
                    size,
                );
            },
            inc,
        )
        .await?;
    }

    pool.close().await?;
    runner.finish_ok()
}

#[cfg(test)]
mod fallback_tests {
    use super::{add_transfer_size, allows_legacy_fallback, fallback_error};

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
}
