use crate::cli::Commands;
use crate::config::CONFIG;
use crate::core::error::BcmrError;
use crate::core::remote::{self, parse_remote_path, RemotePath};
use crate::core::serve_client::{FileTransfer, ServeClientPool};
use crate::ui::progress::ProgressRenderer;
use crate::ui::runner::ProgressRunner;
use crate::ui::utils::format_bytes;
use anyhow::{bail, Result};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;

fn transfer_options_from_cli(cli: &Commands) -> remote::TransferOptions {
    remote::TransferOptions {
        preserve: cli.is_preserve(),
        verify: cli.is_verify(),
        resume: cli.is_resume(),
        strict: cli.is_strict(),
        append: cli.is_append(),
    }
}

pub fn is_plain_mode(args: &Commands) -> bool {
    args.is_tui_mode() || CONFIG.progress.style.eq_ignore_ascii_case("plain")
}

pub struct TransferItem {
    pub local_path: PathBuf,
    pub remote: RemotePath,
    pub size: u64,
    pub is_upload: bool,
}

const COMPRESSED_EXTENSIONS: &[&str] = &[
    "gz", "bz2", "xz", "zst", "lz4", "zip", "rar", "7z", "jpg", "jpeg", "png", "gif", "webp",
    "avif", "heic", "mp4", "mkv", "avi", "mov", "webm", "mp3", "aac", "ogg", "flac", "opus", "pdf",
    "docx", "xlsx", "pptx", "dmg", "iso", "whl", "egg",
];

/// Below this size the per-pool-member rendezvous handshake cost
/// outweighs a striped stream's throughput win. 64 MiB is the knee
/// on typical LAN links.
const STRIPING_MIN_FILE_SIZE: u64 = 64 * 1024 * 1024;

fn should_compress(items: &[TransferItem]) -> bool {
    let (mut compressible, mut total) = (0u64, 0u64);
    for item in items {
        total += item.size;
        let ext = std::path::Path::new(if item.is_upload {
            item.local_path.to_str().unwrap_or("")
        } else {
            &item.remote.path
        })
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
        if !COMPRESSED_EXTENSIONS.contains(&ext.as_str()) {
            compressible += item.size;
        }
    }
    total > 0 && compressible * 100 / total > 30
}

async fn run_parallel_transfers(
    items: Vec<TransferItem>,
    parallel: usize,
    progress: &Arc<Mutex<Box<dyn ProgressRenderer>>>,
    opts: &remote::RemoteTransferOptions,
) -> Result<(), BcmrError> {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(parallel));
    let slot_pool: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new((0..parallel).rev().collect()));
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();

    for item in items {
        let sem = Arc::clone(&semaphore);
        let pool = Arc::clone(&slot_pool);
        let prog = Arc::clone(progress);
        let errs = Arc::clone(&errors);
        let task_opts = opts.clone();

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let slot = match pool.lock().pop() {
                Some(s) => s,
                None => {
                    errs.lock().push("no available worker slot".to_string());
                    return;
                }
            };

            let file_name = if item.is_upload {
                item.local_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            } else {
                item.remote
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&item.remote.path)
                    .to_string()
            };

            let worker_bytes = Arc::new(AtomicU64::new(0));
            let fsize = item.size;

            let wb = Arc::clone(&worker_bytes);
            let p = Arc::clone(&prog);
            let fname = file_name.clone();
            let progress_cb = move |n: u64| {
                let total = wb.fetch_add(n, AtomicOrdering::Relaxed) + n;
                let mut guard = p.lock();
                guard.inc_current(n);
                guard.update_worker(slot, &fname, fsize, total);
            };

            let p2 = Arc::clone(&prog);
            let noop_file_cb = move |name: &str, size: u64| {
                p2.lock().update_worker(slot, name, size, 0);
            };

            let p_skip = Arc::clone(&prog);
            let skip_cb = move |n: u64| {
                p_skip.lock().inc_skipped(n);
            };

            let result = if item.is_upload {
                remote::upload_file(
                    &item.local_path,
                    &item.remote,
                    &progress_cb,
                    &skip_cb,
                    &noop_file_cb,
                    &task_opts,
                    Some(slot),
                )
                .await
            } else {
                remote::download_file(
                    &item.remote,
                    &item.local_path,
                    &progress_cb,
                    &skip_cb,
                    &noop_file_cb,
                    item.size,
                    &task_opts,
                    Some(slot),
                )
                .await
            };

            if let Err(e) = result {
                errs.lock().push(e.to_string());
            }

            prog.lock().finish_worker(slot);
            pool.lock().push(slot);
        }));
    }

    for handle in handles {
        handle.await?;
    }

    let errs = errors.lock();
    if !errs.is_empty() {
        return Err(BcmrError::InvalidInput(errs.join("; ")));
    }
    Ok(())
}

fn collect_upload_files(
    local_src: &std::path::Path,
    remote_base: &RemotePath,
    excludes: &[regex::Regex],
) -> Result<Vec<TransferItem>> {
    use crate::core::traversal;

    let mut items = Vec::new();

    for entry in traversal::walk(local_src, true, false, 1, excludes) {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(local_src)?;

        if path.is_file() {
            items.push(TransferItem {
                local_path: path.to_path_buf(),
                remote: remote_base.join(&rel.to_string_lossy()),
                size: entry.metadata()?.len(),
                is_upload: true,
            });
        }
    }

    Ok(items)
}

fn resolve_upload_remote(
    src: &std::path::Path,
    rdest: &RemotePath,
    multi_source: bool,
) -> RemotePath {
    if multi_source || rdest.path == "." {
        rdest.join(&src.file_name().unwrap_or_default().to_string_lossy())
    } else {
        rdest.clone()
    }
}

pub async fn handle_remote_copy(
    args: &Commands,
    sources: &[std::path::PathBuf],
    dest: &std::path::Path,
    excludes: &[regex::Regex],
) -> Result<()> {
    let dest_str = dest.to_string_lossy();
    let remote_dest = parse_remote_path(&dest_str);
    let is_upload = remote_dest.is_some();

    let compression_mode = CONFIG.scp.compression.to_lowercase();
    let compress = match compression_mode.as_str() {
        "force" => true,
        "off" | "never" => false,
        _ => {
            if is_upload {
                let probe: Vec<TransferItem> = sources
                    .iter()
                    .filter_map(|s| {
                        if parse_remote_path(&s.to_string_lossy()).is_some() {
                            return None;
                        }
                        let size = s.metadata().ok()?.len();
                        Some(TransferItem {
                            local_path: s.clone(),
                            remote: RemotePath {
                                user: None,
                                host: String::new(),
                                path: String::new(),
                            },
                            size,
                            is_upload: true,
                        })
                    })
                    .collect();
                should_compress(&probe)
            } else {
                let probe: Vec<TransferItem> = sources
                    .iter()
                    .filter_map(|s| {
                        let rp = parse_remote_path(&s.to_string_lossy())?;
                        Some(TransferItem {
                            local_path: PathBuf::new(),
                            remote: rp,
                            size: 1,
                            is_upload: false,
                        })
                    })
                    .collect();
                should_compress(&probe)
            }
        }
    };
    remote::set_ssh_compression(compress);

    let check_target = if let Some(ref rd) = remote_dest {
        rd.clone()
    } else {
        let src_str = sources[0].to_string_lossy();
        parse_remote_path(&src_str).ok_or_else(|| anyhow::anyhow!("No remote path found"))?
    };
    remote::validate_ssh_connection(&check_target).await?;

    // `scp.parallel_transfers` is the legacy config key; it now also
    // controls the serve-pool size. Default 4: beats scp under normal
    // load, stays inside sshd's default MaxStartups headroom.
    let parallel = args.get_parallel().unwrap_or(CONFIG.scp.parallel_transfers);
    let serve_parallel = parallel.max(1);

    // Falls back to legacy SSH if remote has no bcmr, or if serve's
    // --root jail (default $HOME) rejects a path like /tmp/foo.
    let ssh_target = check_target.ssh_target();
    let serve_result = if let Some(ref rdest) = remote_dest {
        handle_serve_upload(args, sources, rdest, &ssh_target, excludes, serve_parallel).await
    } else {
        handle_serve_download(args, sources, dest, &ssh_target, excludes, serve_parallel).await
    };

    match serve_result {
        Ok(()) => return Ok(()),
        Err(e) => {
            // Silent fallback costs users 5-10× throughput on many-file
            // batches; warn loudly unless they opted out.
            let msg = e.to_string();
            let is_dry_run_redirect = msg.contains("dry-run fallback");
            if !is_dry_run_redirect && CONFIG.transfer.fallback_warning {
                // Leading '\n' keeps the warning off the last TUI line
                // that may still be on the terminal.
                eprintln!(
                    "\nbcmr: serve fast path unavailable ({msg}).\n\
                     bcmr: falling back to legacy SSH (per-file scp \
                     workers) — slower by ~5-10× on many-file batches.\n\
                     bcmr: set `transfer.fallback_warning = false` in \
                     ~/.config/bcmr/config.toml to silence this."
                );
            }
        }
    }

    if let Some(ref rdest) = remote_dest {
        handle_remote_upload(args, sources, rdest, parallel, excludes).await
    } else {
        handle_remote_download(args, sources, dest, parallel, excludes).await
    }
}

async fn handle_remote_upload(
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
                super::copy::get_total_size(std::slice::from_ref(src), true, args, &[]).await?;
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
        super::copy::cleanup_partial_files,
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
                    &runner.inc_callback(),
                    &runner.skip_callback(),
                    &runner.file_callback(),
                    &opts,
                    None,
                )
                .await?;
            } else if src.is_dir() && args.is_recursive() {
                let dir_remote = rdest.join(&src.file_name().unwrap_or_default().to_string_lossy());
                remote::upload_directory(
                    src,
                    &dir_remote,
                    &runner.inc_callback(),
                    &runner.skip_callback(),
                    &runner.file_callback(),
                    &excludes,
                    &opts,
                )
                .await?;
            }
        }
    }

    runner.finish_ok()
}

async fn handle_remote_download(
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
        super::copy::cleanup_partial_files,
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
                    rsrc, &local_dir, &inc, &skip, &file_cb, &excludes, &opts,
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
                    &inc,
                    &skip,
                    &file_cb,
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

/// Upload via bcmr serve protocol. Err if serve is unavailable.
///
/// `parallel` = SSH connections opened by the pool; each has its own
/// cipher stream, so throughput scales near-linearly on multi-core
/// until NIC/disk saturate.
async fn handle_serve_upload(
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
                super::copy::get_total_size(std::slice::from_ref(src), true, args, &[]).await?;
        }
    }

    let runner = ProgressRunner::new(
        total_size,
        is_plain_mode(args),
        false,
        crate::config::is_json_mode(),
        super::copy::cleanup_partial_files,
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

            // Striping requires direct-TCP (AEAD per-frame MAC gives
            // chunk-level integrity without a whole-file hash). Skipped
            // for --verify and dedup since those need a server-side
            // hash we can't assemble across chunks.
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
            (runner.inc_callback())(size);
        } else if src.is_dir() && args.is_recursive() {
            serve_upload_dir(&mut pool, src, rdest, &runner, excludes, args.is_verify()).await?;
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
    verify: bool,
) -> Result<()> {
    let dir_name = local_dir.file_name().unwrap_or_default().to_string_lossy();
    let remote_dir = format!("{}/{}", remote_base.path, dir_name);
    pool.mkdir(&remote_dir).await?;

    // Two-pass: mkdir all subdirs sequentially (parents before
    // children), then striped-pipeline all file PUTs through the pool.
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

    // The pool's striped PUT moves files into per-bucket writer tasks,
    // so snapshot local paths before handing over when --verify needs
    // them post-transfer.
    let verify_inputs: Option<Vec<PathBuf>> =
        verify.then(|| files_to_put.iter().map(|f| f.local.clone()).collect());

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

    if let Some(local_paths) = verify_inputs {
        for (local_path, server_hash) in local_paths.iter().zip(server_hashes.iter()) {
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
    Ok(())
}

/// Download via bcmr serve protocol. Err if serve is unavailable.
///
/// `parallel` = SSH connections in the pool; N>1 stripes files round-
/// robin across connections for N× crypto throughput.
async fn handle_serve_download(
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
        super::copy::cleanup_partial_files,
    )?;
    runner
        .progress()
        .lock()
        .set_operation_type("Downloading (serve)");

    // Directories go serially (parents before children); files split
    // into "big" (striped, one at a time, whole pool) and "normal"
    // (pipelined batch). Stripe gate matches PUT: direct-TCP, pool>1,
    // file above knee, no --verify.
    let use_stripe = args.use_direct_tcp() && pool.len() > 1 && !args.is_verify();
    let mut big_files: Vec<(String, PathBuf, u64)> = Vec::new();
    let mut files_to_get: Vec<FileTransfer> = Vec::new();
    for item in &items {
        if item.is_dir {
            tokio::fs::create_dir_all(&item.local_path).await?;
        } else if use_stripe && item.size >= STRIPING_MIN_FILE_SIZE {
            big_files.push((
                item.remote_path.clone(),
                item.local_path.clone(),
                item.size,
            ));
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
            &local_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            *size,
        );
        let _ = pool.striped_get_file(remote_path, local_path, *size).await?;
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

    pool.close().await?;
    runner.finish_ok()
}
