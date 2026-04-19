use super::{is_plain_mode, STRIPING_MIN_FILE_SIZE};
use crate::cli::Commands;
use crate::core::remote::{parse_remote_path, RemotePath};
use crate::core::serve_client::{FileTransfer, ServeClientPool};
use crate::ui::runner::ProgressRunner;
use anyhow::{bail, Result};
use std::path::PathBuf;

pub(super) async fn handle_serve_upload(
    args: &Commands,
    sources: &[PathBuf],
    rdest: &RemotePath,
    ssh_target: &str,
    excludes: &[regex::Regex],
    parallel: usize,
) -> Result<()> {
    let mut pool = if args.use_direct_tcp() {
        ServeClientPool::connect_direct_with_caps(ssh_target, args.protocol_caps(), parallel).await
    } else {
        ServeClientPool::connect_with_caps(ssh_target, args.protocol_caps(), parallel).await
    }
    .map_err(|e| anyhow::anyhow!("serve unavailable: {}", e))?;

    if args.is_dry_run() {
        pool.close().await?;
        return Err(anyhow::anyhow!("serve: dry-run fallback to legacy"));
    }

    let mut total_size = 0u64;
    for src in sources {
        if src.is_file() {
            total_size += src.metadata()?.len();
        } else if src.is_dir() && args.is_recursive() {
            total_size +=
                crate::commands::copy::get_total_size(std::slice::from_ref(src), true, args, &[])
                    .await?;
        }
    }

    let runner = ProgressRunner::new(
        total_size,
        is_plain_mode(args),
        false,
        crate::config::is_json_mode(),
        crate::commands::copy::cleanup_partial_files,
    )?;
    runner
        .progress()
        .lock()
        .set_operation_type("Uploading (serve)");

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

            let use_stripe = args.use_direct_tcp()
                && pool.len() > 1
                && size >= STRIPING_MIN_FILE_SIZE
                && !args.is_verify();

            if use_stripe {
                let _ = pool.striped_put_file(src, &remote_path).await?;
            } else {
                let server_hash = pool.first_mut().put(&remote_path, src).await?;
                if args.is_verify() {
                    let p = src.to_path_buf();
                    let local_hash = tokio::task::spawn_blocking(move || {
                        crate::core::checksum::calculate_hash(&p)
                    })
                    .await??;
                    let server_hex: String =
                        server_hash.iter().map(|b| format!("{:02x}", b)).collect();
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
            serve_upload_dir(&mut pool, src, rdest, &runner, excludes, args).await?;
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
        .pipelined_put_files_striped(files_to_put, inc_for_chunks, move |_idx, path, size| {
            file_cb(
                &path.file_name().unwrap_or_default().to_string_lossy(),
                size,
            );
        })
        .await?;

    if args.is_verify() {
        for ((local_path, _), server_hash) in per_file_inputs.iter().zip(server_hashes.iter()) {
            let p = local_path.clone();
            let local_hash =
                tokio::task::spawn_blocking(move || crate::core::checksum::calculate_hash(&p))
                    .await??;
            let server_hex: String = server_hash.iter().map(|b| format!("{:02x}", b)).collect();
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
    let mut pool = if args.use_direct_tcp() {
        ServeClientPool::connect_direct_with_caps(ssh_target, args.protocol_caps(), parallel).await
    } else {
        ServeClientPool::connect_with_caps(ssh_target, args.protocol_caps(), parallel).await
    }
    .map_err(|e| anyhow::anyhow!("serve unavailable: {}", e))?;

    if args.is_dry_run() {
        pool.close().await?;
        return Err(anyhow::anyhow!("serve: dry-run fallback to legacy"));
    }

    struct DownloadItem {
        remote_path: String,
        local_path: PathBuf,
        size: u64,
        is_dir: bool,
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
            if is_dir && args.is_recursive() {
                let entries = pool.first_mut().list(&rp.path).await?;
                let dir_name = rp.path.rsplit('/').next().unwrap_or(&rp.path);
                let local_base = dest.join(dir_name);
                items.push(DownloadItem {
                    remote_path: String::new(),
                    local_path: local_base.clone(),
                    size: 0,
                    is_dir: true,
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
                        });
                    } else {
                        total_size += entry.size;
                        items.push(DownloadItem {
                            remote_path: remote,
                            local_path: local,
                            size: entry.size,
                            is_dir: false,
                        });
                    }
                }
            } else if !is_dir {
                total_size += size;
                let local = if dest.is_dir() {
                    dest.join(rp.path.rsplit('/').next().unwrap_or(&rp.path))
                } else {
                    dest.to_path_buf()
                };
                items.push(DownloadItem {
                    remote_path: rp.path.clone(),
                    local_path: local,
                    size,
                    is_dir: false,
                });
            }
        }
    }

    let runner = ProgressRunner::new(
        total_size,
        is_plain_mode(args),
        false,
        crate::config::is_json_mode(),
        crate::commands::copy::cleanup_partial_files,
    )?;
    runner
        .progress()
        .lock()
        .set_operation_type("Downloading (serve)");

    let use_stripe = args.use_direct_tcp() && pool.len() > 1 && !args.is_verify();
    let mut big_files: Vec<(String, PathBuf, u64)> = Vec::new();
    let mut files_to_get: Vec<FileTransfer> = Vec::new();
    for item in &items {
        if item.is_dir {
            tokio::fs::create_dir_all(&item.local_path).await?;
        } else if use_stripe && item.size >= STRIPING_MIN_FILE_SIZE {
            big_files.push((item.remote_path.clone(), item.local_path.clone(), item.size));
        } else {
            files_to_get.push(FileTransfer {
                remote: item.remote_path.clone(),
                local: item.local_path.clone(),
                size: item.size,
            });
        }
    }

    for (remote_path, local_path, size) in &big_files {
        (runner.file_callback())(
            &local_path.file_name().unwrap_or_default().to_string_lossy(),
            *size,
        );
        let _ = pool
            .striped_get_file(remote_path, local_path, *size)
            .await?;
        if args.is_sync() {
            let f = tokio::fs::File::open(local_path).await?;
            crate::core::io::durable_sync_async(&f).await?;
        }
        (runner.inc_callback())(*size);
    }

    if !files_to_get.is_empty() {
        let file_cb = runner.file_callback();
        let inc = runner.inc_callback();
        let sync = args.is_sync();
        pool.pipelined_get_files_striped(
            files_to_get,
            sync,
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

    if args.is_verify() {
        for item in &items {
            if item.is_dir {
                continue;
            }
            let p = item.local_path.clone();
            let local_hash =
                tokio::task::spawn_blocking(move || crate::core::checksum::calculate_hash(&p))
                    .await??;
            let remote_hash = pool.first_mut().hash(&item.remote_path, 0, None).await?;
            let remote_hex: String = remote_hash.iter().map(|b| format!("{:02x}", b)).collect();
            if remote_hex != local_hash {
                pool.close().await?;
                return runner
                    .finish_err(format!("hash mismatch for {}", item.local_path.display()));
            }
        }
    }
    if args.is_preserve() {
        let (user, host) = match ssh_target.split_once('@') {
            Some((u, h)) => (Some(u.to_string()), h.to_string()),
            None => (None, ssh_target.to_string()),
        };
        for item in &items {
            if item.is_dir {
                continue;
            }
            let target = RemotePath {
                user: user.clone(),
                host: host.clone(),
                path: item.remote_path.clone(),
            };
            crate::core::remote::apply_remote_attrs_locally(&target, &item.local_path).await?;
        }
    }

    pool.close().await?;
    runner.finish_ok()
}
