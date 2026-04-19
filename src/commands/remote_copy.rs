use crate::cli::Commands;
use crate::config::CONFIG;
use crate::core::error::BcmrError;
use crate::core::remote::{self, parse_remote_path, RemotePath};
use crate::ui::progress::ProgressRenderer;
use anyhow::Result;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;

mod legacy;
mod serve;

use legacy::{handle_remote_download, handle_remote_upload};
use serve::{handle_serve_download, handle_serve_upload};

pub(super) fn transfer_options_from_cli(cli: &Commands) -> remote::TransferOptions {
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

pub(super) struct TransferItem {
    pub local_path: PathBuf,
    pub remote: RemotePath,
    pub size: u64,
    pub is_upload: bool,
}

pub(super) const COMPRESSED_EXTENSIONS: &[&str] = &[
    "gz", "bz2", "xz", "zst", "lz4", "zip", "rar", "7z", "jpg", "jpeg", "png", "gif", "webp",
    "avif", "heic", "mp4", "mkv", "avi", "mov", "webm", "mp3", "aac", "ogg", "flac", "opus", "pdf",
    "docx", "xlsx", "pptx", "dmg", "iso", "whl", "egg",
];

pub(super) const STRIPING_MIN_FILE_SIZE: u64 = 64 * 1024 * 1024;

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

pub(super) async fn run_parallel_transfers(
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
                    remote::TransferCallbacks {
                        on_progress: &progress_cb,
                        on_skip: &skip_cb,
                        on_new_file: &noop_file_cb,
                    },
                    &task_opts,
                    Some(slot),
                )
                .await
            } else {
                remote::download_file(
                    &item.remote,
                    &item.local_path,
                    remote::TransferCallbacks {
                        on_progress: &progress_cb,
                        on_skip: &skip_cb,
                        on_new_file: &noop_file_cb,
                    },
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

pub(super) fn collect_upload_files(
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

pub(super) fn resolve_upload_remote(
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

    let parallel = args.get_parallel().unwrap_or(CONFIG.scp.parallel_transfers);
    let serve_parallel = parallel.max(1);

    let ssh_target = check_target.ssh_target();
    let needs_resume_semantics = args.is_resume() || args.is_strict() || args.is_append();
    let serve_result = if needs_resume_semantics {
        Err(anyhow::anyhow!(
            "serve: --resume/--strict/--append not implemented, fallback to legacy"
        ))
    } else if let Some(ref rdest) = remote_dest {
        handle_serve_upload(args, sources, rdest, &ssh_target, excludes, serve_parallel).await
    } else {
        handle_serve_download(args, sources, dest, &ssh_target, excludes, serve_parallel).await
    };

    match serve_result {
        Ok(()) => return Ok(()),
        Err(e) => {
            let msg = e.to_string();
            let is_dry_run_redirect = msg.contains("dry-run fallback");
            let is_resume_redirect = msg.contains("not implemented, fallback to legacy");
            if !is_dry_run_redirect && !is_resume_redirect && CONFIG.transfer.fallback_warning {
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
