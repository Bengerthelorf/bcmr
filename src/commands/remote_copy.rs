use crate::cli::{Commands, ProgressMode};
use crate::config::CONFIG;
use crate::core::error::BcmrError;
use crate::core::remote::{self, parse_remote_path, RemotePath};
use crate::ui::runner::ProgressHandle;
use anyhow::Result;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;

mod composite;
mod endpoint;
mod legacy;
mod links;
mod preflight;
mod serve;

use legacy::{handle_remote_download, handle_remote_upload};
use serve::{handle_serve_download, handle_serve_upload};

pub(super) fn transfer_options_from_cli(cli: &Commands) -> remote::TransferOptions {
    remote::TransferOptions {
        force: cli.is_force(),
        preserve: cli.is_preserve(),
        verify: cli.is_verify(),
        resume: cli.is_resume(),
        strict: cli.is_strict(),
        append: cli.is_append(),
        sync: cli.is_sync(),
    }
}

pub fn progress_mode(args: &Commands) -> ProgressMode {
    let requested = args.progress_mode();
    // Per-file diagnostics share the terminal cursor with the live renderer.
    // Keep verbose output in scrollback instead of redrawing over those lines.
    if args.is_verbose() && requested != ProgressMode::Off {
        return ProgressMode::Plain;
    }
    if requested != ProgressMode::Auto {
        return requested;
    }
    match CONFIG.progress.style.to_ascii_lowercase().as_str() {
        "tui" | "fancy" => ProgressMode::Tui,
        "inline" => ProgressMode::Inline,
        "plain" => ProgressMode::Plain,
        "off" => ProgressMode::Off,
        _ => ProgressMode::Auto,
    }
}

pub(crate) fn finish_json_operation(operation: &str) -> Result<()> {
    if crate::config::is_json_mode() {
        let log = crate::config::log_file();
        let mut renderer =
            crate::ui::progress::create_renderer(0, ProgressMode::Off, true, true, log.as_ref())?;
        renderer.set_operation_type(operation);
        renderer.finish()?;
    }
    Ok(())
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
        let path_str = if item.is_upload {
            item.local_path.to_str().unwrap_or("")
        } else {
            &item.remote.path
        };
        let ext = std::path::Path::new(path_str)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        let already_compressed = COMPRESSED_EXTENSIONS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(ext));
        if !already_compressed {
            compressible += item.size;
        }
    }
    total > 0 && compressible * 100 / total > 30
}

pub(super) async fn run_parallel_transfers(
    items: Vec<TransferItem>,
    parallel: usize,
    progress: ProgressHandle,
    opts: &remote::TransferOptions,
) -> Result<(), BcmrError> {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(parallel));
    let slot_pool: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new((0..parallel).rev().collect()));
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();

    for item in items {
        let sem = Arc::clone(&semaphore);
        let pool = Arc::clone(&slot_pool);
        let prog = progress.clone();
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
                item.remote.file_name().to_string()
            };

            let worker_bytes = Arc::new(AtomicU64::new(0));
            let fsize = item.size;

            let wb = Arc::clone(&worker_bytes);
            let p = prog.clone();
            let fname = file_name.clone();
            let progress_cb = move |n: u64| {
                let total = wb.fetch_add(n, AtomicOrdering::Relaxed) + n;
                p.with(|r| {
                    r.inc_current(n);
                    r.update_worker(slot, &fname, fsize, total);
                });
            };

            let p2 = prog.clone();
            let noop_file_cb = move |name: &str, size: u64| {
                p2.update_worker(slot, name, size, 0);
            };

            let p_skip = prog.clone();
            let skip_cb = move |n: u64| {
                p_skip.inc_skipped(n);
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
                )
                .await
            };

            if let Err(e) = result {
                errs.lock().push(e.to_string());
            }

            prog.finish_worker(slot);
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

pub(super) struct DirectoryTransfer {
    pub local_path: PathBuf,
    pub remote: RemotePath,
}

pub(super) struct TransferTreePlan {
    pub directories: Vec<DirectoryTransfer>,
    pub files: Vec<TransferItem>,
    pub total_size: u64,
}

fn add_transfer_size(total: &mut u64, size: u64) -> Result<()> {
    *total = total
        .checked_add(size)
        .ok_or_else(|| anyhow::anyhow!("declared transfer size exceeds u64"))?;
    Ok(())
}

impl TransferTreePlan {
    fn new() -> Self {
        Self {
            directories: Vec::new(),
            files: Vec::new(),
            total_size: 0,
        }
    }

    fn push_file(&mut self, file: TransferItem) -> Result<()> {
        add_transfer_size(&mut self.total_size, file.size)?;
        self.files.push(file);
        Ok(())
    }
}

pub(super) fn collect_upload_plan(
    sources: &[PathBuf],
    destination: &RemotePath,
    recursive: bool,
    excludes: &[regex::Regex],
) -> Result<TransferTreePlan> {
    use crate::core::traversal;

    let selected: Vec<_> = sources
        .iter()
        .filter(|source| !traversal::is_copy_excluded(source, source, excludes))
        .collect();
    let multi_source = selected.len() > 1;
    let mut plan = TransferTreePlan::new();
    for source in selected {
        let remote = resolve_upload_remote(source, destination, multi_source);
        let metadata = source.metadata()?;
        if metadata.is_file() {
            plan.push_file(TransferItem {
                local_path: source.clone(),
                remote,
                size: metadata.len(),
                is_upload: true,
            })?;
        } else if metadata.is_dir() {
            if !recursive {
                anyhow::bail!(
                    "Source '{}' is a directory. Use -r flag for recursive copy.",
                    source.display()
                );
            }
            plan.directories.push(DirectoryTransfer {
                local_path: source.clone(),
                remote: remote.clone(),
            });
            for entry in traversal::copy_walk(source, true, 1, false, excludes) {
                let entry = entry?;
                let path = entry.path();
                let relative = path.strip_prefix(source)?;
                let metadata = entry.metadata()?;
                let relative = relative
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                let target = remote.join(&relative);
                if metadata.is_dir() {
                    plan.directories.push(DirectoryTransfer {
                        local_path: path.to_path_buf(),
                        remote: target,
                    });
                } else if metadata.is_file() {
                    plan.push_file(TransferItem {
                        local_path: path.to_path_buf(),
                        remote: target,
                        size: metadata.len(),
                        is_upload: true,
                    })?;
                } else {
                    anyhow::bail!(
                        "Source '{}' is not a regular file or directory",
                        path.display()
                    );
                }
            }
        } else {
            anyhow::bail!(
                "Source '{}' is not a regular file or directory",
                source.display()
            );
        }
    }
    Ok(plan)
}

pub(super) async fn collect_download_plan(
    sources: &[PathBuf],
    destination: &std::path::Path,
    recursive: bool,
    excludes: &[regex::Regex],
) -> Result<TransferTreePlan> {
    let mut plan = TransferTreePlan::new();
    for source in sources {
        let remote = parse_remote_path(&source.to_string_lossy()).ok_or_else(|| {
            anyhow::anyhow!("Mixed local/remote sources without remote destination")
        })?;
        let entries = remote::inventory_remote(&remote, false, excludes).await?;
        let Some(root) = entries.first() else {
            continue;
        };
        if root.is_dir && !recursive {
            anyhow::bail!(
                "Remote source '{}' is a directory. Use -r flag for recursive copy.",
                remote
            );
        }
        let local_base = resolve_download_local(&remote, destination)?;
        for entry in entries {
            let (local_path, entry_remote) = if entry.path.is_empty() {
                (local_base.clone(), remote.clone())
            } else {
                (
                    crate::core::destination::remote_entry_path(&local_base, &entry.path)?,
                    remote.join(&entry.path),
                )
            };
            if entry.link_target.is_some() {
                anyhow::bail!("dereferenced remote inventory contains a symbolic link");
            }
            if entry.is_dir {
                plan.directories.push(DirectoryTransfer {
                    local_path,
                    remote: entry_remote,
                });
            } else {
                plan.push_file(TransferItem {
                    local_path,
                    remote: entry_remote,
                    size: entry.size,
                    is_upload: false,
                })?;
            }
        }
    }
    Ok(plan)
}

pub(super) async fn preserve_directories(
    directories: &[DirectoryTransfer],
    upload: bool,
    sync: bool,
) -> Result<()> {
    for directory in directories.iter().rev() {
        if upload {
            remote::preserve_remote_attrs(&directory.local_path, &directory.remote).await?;
        } else {
            #[cfg(not(unix))]
            if sync {
                anyhow::bail!(
                    "--sync cannot guarantee durable local directory metadata on this platform"
                );
            }
            #[cfg(unix)]
            let handle = if sync {
                Some(
                    tokio::fs::File::open(&directory.local_path)
                        .await?
                        .into_std()
                        .await,
                )
            } else {
                None
            };
            remote::apply_remote_attrs_locally(&directory.remote, &directory.local_path).await?;
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

pub(super) fn resolve_upload_remote(
    src: &std::path::Path,
    rdest: &RemotePath,
    multi_source: bool,
) -> RemotePath {
    if crate::commands::copy::copies_directory_contents(src) {
        rdest.clone()
    } else if multi_source || rdest.path == "." || rdest.path.ends_with('/') {
        rdest.join(&src.file_name().unwrap_or_default().to_string_lossy())
    } else {
        rdest.clone()
    }
}

pub(super) fn resolve_download_local(
    source: &RemotePath,
    dest: &std::path::Path,
) -> Result<PathBuf> {
    if crate::core::destination::local_is_directory(dest)? {
        if source.file_name() == "." {
            // An explicit source login-directory shorthand retains its
            // existing contents semantics; listing entries are still checked.
            return Ok(dest.to_path_buf());
        }
        Ok(crate::core::destination::remote_entry_path(
            dest,
            source.file_name(),
        )?)
    } else {
        Ok(dest.to_path_buf())
    }
}

pub async fn handle_remote_copy(
    args: &Commands,
    sources: &[std::path::PathBuf],
    dest: &std::path::Path,
    excludes: &[regex::Regex],
) -> Result<()> {
    let dest_str = dest.to_string_lossy();
    let remote_dest_initial = parse_remote_path(&dest_str);
    let is_upload = remote_dest_initial.is_some();

    if composite::required(sources, dest).await? {
        return composite::copy(args, sources, dest, excludes).await;
    }

    if !is_upload {
        if sources
            .iter()
            .any(|source| parse_remote_path(&source.to_string_lossy()).is_none())
        {
            anyhow::bail!(
                "Mixed local/remote sources to a local destination are not supported. Copy them in separate commands."
            );
        }
        if sources.len() > 1 && !crate::core::destination::local_is_directory(dest)? {
            anyhow::bail!(
                "When downloading multiple sources, destination '{}' must be a directory (existing or ending in '/')",
                dest.display()
            );
        }
    }

    if let Some(ref rd) = remote_dest_initial {
        rd.reject_unsafe()?;
    }
    for src in sources {
        if let Some(rsrc) = parse_remote_path(&src.to_string_lossy()) {
            rsrc.reject_unsafe()?;
            continue;
        }
        if let Ok(md) = src.symlink_metadata() {
            if md.is_dir() && !args.is_recursive() {
                anyhow::bail!(
                    "Source '{}' is a directory. Use -r flag for recursive copy.",
                    src.display()
                );
            }
        }
    }

    let expanded_dest_buf: PathBuf = if let Some(mut rd) = remote_dest_initial.clone() {
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

    let expanded_dest_str = expanded_dest_buf.to_string_lossy();
    let expanded_remote_dest = parse_remote_path(&expanded_dest_str);
    let dest_buf: PathBuf = if let Some(ref rd) = expanded_remote_dest {
        let needs_dir_probe = !rd.path.ends_with('/') && rd.path != "." && !rd.path.is_empty();
        if needs_dir_probe && remote::remote_path_is_directory(rd).await {
            let mut probed = rd.clone();
            probed.path.push('/');
            PathBuf::from(probed.display())
        } else {
            expanded_dest_buf.clone()
        }
    } else {
        expanded_dest_buf.clone()
    };
    let dest: &std::path::Path = dest_buf.as_path();
    let dest_str = dest.to_string_lossy();
    let remote_dest = parse_remote_path(&dest_str);

    if let Some(remote) = &remote_dest {
        if sources.len() > 1 && remote.path != "." && !remote.path.ends_with('/') {
            anyhow::bail!(
                "When uploading multiple sources, destination '{}' must be a directory (existing or ending in '/')",
                remote
            );
        }
    }

    preflight::confirm_remote_overwrites(args, sources, dest, excludes).await?;
    crate::core::interrupt::check()?;
    if args.is_no_deref() || matches!(args, Commands::Move { .. }) {
        return links::handle_link_copy(args, sources, dest, excludes).await;
    }

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
                                port: None,
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
    // A serve pool is bound to one host. Keep each source's original host on
    // mixed-host downloads rather than looking up every path on the first host.
    if !is_upload
        && sources.iter().any(|source| {
            parse_remote_path(&source.to_string_lossy())
                .is_some_and(|remote| remote.ssh_target() != ssh_target)
        })
    {
        return handle_remote_download(args, sources, dest, parallel, excludes).await;
    }
    let serve_result = if let Some(ref rdest) = remote_dest {
        handle_serve_upload(args, sources, rdest, &ssh_target, excludes, serve_parallel).await
    } else {
        handle_serve_download(args, sources, dest, &ssh_target, excludes, serve_parallel).await
    };

    match serve_result {
        Ok(()) => return Ok(()),
        Err(e) => {
            let msg = e.to_string();
            if !serve::allows_legacy_fallback(&e) {
                return Err(e);
            }
            if msg.contains("--append refused:") || msg.contains("Use -r flag for recursive copy") {
                return Err(e);
            }
            let is_dry_run_redirect = msg.contains("dry-run fallback");
            let is_resume_redirect = msg.contains("not yet supported, fallback to legacy")
                || msg.contains("not supported, fallback to legacy");
            if !is_dry_run_redirect && !is_resume_redirect && CONFIG.transfer.fallback_warning {
                let sync_caveat = if args.is_sync() && is_upload {
                    "\n                     bcmr: --sync on legacy uploads is best-effort \
                     (scp does not guarantee remote fsync)."
                } else {
                    ""
                };
                eprintln!(
                    "\nbcmr: serve fast path unavailable ({msg}).\n\
                     bcmr: falling back to legacy SSH (per-file shell \
                     workers) — slower by ~5-10× on many-file batches.\n\
                     bcmr: set `transfer.fallback_warning = false` in \
                     ~/.config/bcmr/config.toml to silence this.{sync_caveat}"
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
