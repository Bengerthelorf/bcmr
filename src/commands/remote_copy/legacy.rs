use super::{
    collect_upload_files, is_plain_mode, resolve_upload_remote, run_parallel_transfers,
    transfer_options_from_cli, TransferItem,
};
use crate::cli::Commands;
use crate::core::remote::{self, parse_remote_path, RemotePath};
use crate::ui::runner::ProgressRunner;
use crate::ui::utils::format_bytes;
use anyhow::{bail, Result};

pub(super) async fn handle_remote_upload(
    args: &Commands,
    sources: &[std::path::PathBuf],
    rdest: &RemotePath,
    parallel: usize,
    excludes: &[regex::Regex],
) -> Result<()> {
    let excludes = excludes.to_vec();
    let mut total_size = 0u64;
    for src in sources {
        if parse_remote_path(&src.to_string_lossy()).is_some() {
            bail!("Cannot copy between two remote hosts. Use local as intermediary.");
        }
        if src.is_file() {
            total_size += src.metadata()?.len();
        } else if src.is_dir() && args.is_recursive() {
            total_size +=
                crate::commands::copy::get_total_size(std::slice::from_ref(src), true, args, &[])
                    .await?;
        } else if src.is_dir() {
            bail!(
                "Source '{}' is a directory. Use -r flag for recursive copy.",
                src.display()
            );
        } else {
            bail!("Source '{}' not found", src.display());
        }
    }

    if args.is_dry_run() {
        println!(
            "Dry-run: would upload {} to {}",
            format_bytes(total_size as f64),
            rdest
        );
        for src in sources {
            if src.is_file() {
                println!(
                    "  {} -> {}",
                    src.display(),
                    resolve_upload_remote(src, rdest, sources.len() > 1)
                );
            } else if src.is_dir() && args.is_recursive() {
                let dir_remote = rdest.join(&src.file_name().unwrap_or_default().to_string_lossy());
                for item in collect_upload_files(src, &dir_remote, &excludes)? {
                    println!("  {} -> {}", item.local_path.display(), item.remote);
                }
            }
        }
        return Ok(());
    }

    let opts = transfer_options_from_cli(args);

    let runner = ProgressRunner::new(
        total_size,
        is_plain_mode(args),
        false,
        crate::config::is_json_mode(),
        crate::commands::copy::cleanup_partial_files,
    )?;
    runner.progress().lock().set_operation_type("Uploading");
    let multi_source = sources.len() > 1;

    if parallel > 1 {
        runner.set_parallel_mode(parallel);

        let mut items: Vec<TransferItem> = Vec::new();
        for src in sources {
            if src.is_file() {
                let file_remote = resolve_upload_remote(src, rdest, multi_source);
                items.push(TransferItem {
                    local_path: src.clone(),
                    remote: file_remote,
                    size: src.metadata()?.len(),
                    is_upload: true,
                });
            } else if src.is_dir() && args.is_recursive() {
                let dir_remote = rdest.join(&src.file_name().unwrap_or_default().to_string_lossy());
                remote::ensure_remote_tree(src, &dir_remote).await?;
                items.extend(collect_upload_files(src, &dir_remote, &excludes)?);
            }
        }

        run_parallel_transfers(items, parallel, runner.progress(), &opts)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;
    } else {
        for src in sources {
            if src.is_file() {
                let file_remote = resolve_upload_remote(src, rdest, multi_source);
                remote::upload_file(
                    src,
                    &file_remote,
                    remote::TransferCallbacks {
                        on_progress: &runner.inc_callback(),
                        on_skip: &runner.skip_callback(),
                        on_new_file: &runner.file_callback(),
                    },
                    &opts,
                    None,
                )
                .await?;
            } else if src.is_dir() && args.is_recursive() {
                let dir_remote = rdest.join(&src.file_name().unwrap_or_default().to_string_lossy());
                remote::upload_directory(
                    src,
                    &dir_remote,
                    remote::TransferCallbacks {
                        on_progress: &runner.inc_callback(),
                        on_skip: &runner.skip_callback(),
                        on_new_file: &runner.file_callback(),
                    },
                    &excludes,
                    &opts,
                )
                .await?;
            }
        }
    }

    runner.finish_ok()
}

pub(super) async fn handle_remote_download(
    args: &Commands,
    sources: &[std::path::PathBuf],
    dest_local: &std::path::Path,
    parallel: usize,
    excludes: &[regex::Regex],
) -> Result<()> {
    let excludes = excludes.to_vec();

    let mut remote_sources = Vec::new();
    for src in sources {
        let rsrc = parse_remote_path(&src.to_string_lossy()).ok_or_else(|| {
            anyhow::anyhow!("Mixed local/remote sources without remote destination")
        })?;
        let size = remote::remote_total_size(&rsrc, args.is_recursive()).await?;
        remote_sources.push((rsrc, size));
    }

    let total_size: u64 = remote_sources.iter().map(|(_, s)| *s).sum();

    if args.is_dry_run() {
        println!(
            "Dry-run: would download {} to {}",
            format_bytes(total_size as f64),
            dest_local.display()
        );
        for (rsrc, _) in &remote_sources {
            let info = remote::remote_stat(rsrc).await?;
            if info.is_dir && args.is_recursive() {
                let dir_name = rsrc.path.rsplit('/').next().unwrap_or(&rsrc.path);
                let local_dir = if dest_local.is_dir() {
                    dest_local.join(dir_name)
                } else {
                    dest_local.to_path_buf()
                };
                let entries = remote::remote_list_files(rsrc).await?;
                for (rel_path, _, is_dir_entry) in &entries {
                    if *is_dir_entry {
                        continue;
                    }
                    if crate::core::traversal::is_excluded(
                        std::path::Path::new(rel_path),
                        &excludes,
                    ) {
                        continue;
                    }
                    println!(
                        "  {} -> {}",
                        rsrc.join(rel_path),
                        local_dir.join(rel_path).display()
                    );
                }
            } else {
                let local_path = if dest_local.is_dir() {
                    dest_local.join(rsrc.path.rsplit('/').next().unwrap_or(&rsrc.path))
                } else {
                    dest_local.to_path_buf()
                };
                println!("  {} -> {}", rsrc, local_path.display());
            }
        }
        return Ok(());
    }

    let opts = transfer_options_from_cli(args);

    let runner = ProgressRunner::new(
        total_size,
        is_plain_mode(args),
        false,
        crate::config::is_json_mode(),
        crate::commands::copy::cleanup_partial_files,
    )?;
    runner.progress().lock().set_operation_type("Downloading");

    if parallel > 1 {
        runner.set_parallel_mode(parallel);

        let mut items: Vec<TransferItem> = Vec::new();
        for (rsrc, _) in &remote_sources {
            let info = remote::remote_stat(rsrc).await?;
            if info.is_dir {
                if !args.is_recursive() {
                    bail!("Remote source '{}' is a directory. Use -r flag.", rsrc);
                }
                let dir_name = rsrc.path.rsplit('/').next().unwrap_or(&rsrc.path);
                let local_dir = if dest_local.is_dir() {
                    dest_local.join(dir_name)
                } else {
                    dest_local.to_path_buf()
                };
                let entries = remote::remote_list_files(rsrc).await?;
                for (rel_path, _, is_dir_entry) in &entries {
                    if *is_dir_entry
                        && !crate::core::traversal::is_excluded(
                            std::path::Path::new(rel_path),
                            &excludes,
                        )
                    {
                        tokio::fs::create_dir_all(local_dir.join(rel_path)).await?;
                    }
                }
                for (rel_path, size, is_dir_entry) in &entries {
                    if *is_dir_entry {
                        continue;
                    }
                    if crate::core::traversal::is_excluded(
                        std::path::Path::new(rel_path),
                        &excludes,
                    ) {
                        continue;
                    }
                    items.push(TransferItem {
                        local_path: local_dir.join(rel_path),
                        remote: rsrc.join(rel_path),
                        size: *size,
                        is_upload: false,
                    });
                }
            } else {
                let local_path = if dest_local.is_dir() {
                    dest_local.join(rsrc.path.rsplit('/').next().unwrap_or(&rsrc.path))
                } else {
                    dest_local.to_path_buf()
                };
                items.push(TransferItem {
                    local_path,
                    remote: rsrc.clone(),
                    size: info.size,
                    is_upload: false,
                });
            }
        }

        run_parallel_transfers(items, parallel, runner.progress(), &opts)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;
    } else {
        for (rsrc, _) in &remote_sources {
            let info = remote::remote_stat(rsrc).await?;
            let inc = runner.inc_callback();
            let skip = runner.skip_callback();
            let file_cb = runner.file_callback();

            if info.is_dir {
                if !args.is_recursive() {
                    bail!("Remote source '{}' is a directory. Use -r flag.", rsrc);
                }
                let dir_name = rsrc.path.rsplit('/').next().unwrap_or(&rsrc.path);
                let local_dir = if dest_local.is_dir() {
                    dest_local.join(dir_name)
                } else {
                    dest_local.to_path_buf()
                };
                if !local_dir.exists() {
                    tokio::fs::create_dir_all(&local_dir).await?;
                }
                remote::download_directory(
                    rsrc,
                    &local_dir,
                    remote::TransferCallbacks {
                        on_progress: &inc,
                        on_skip: &skip,
                        on_new_file: &file_cb,
                    },
                    &excludes,
                    &opts,
                )
                .await?;
            } else {
                let local_path = if dest_local.is_dir() {
                    dest_local.join(rsrc.path.rsplit('/').next().unwrap_or(&rsrc.path))
                } else {
                    dest_local.to_path_buf()
                };
                remote::download_file(
                    rsrc,
                    &local_path,
                    remote::TransferCallbacks {
                        on_progress: &inc,
                        on_skip: &skip,
                        on_new_file: &file_cb,
                    },
                    info.size,
                    &opts,
                    None,
                )
                .await?;
            }
        }
    }

    runner.finish_ok()
}
