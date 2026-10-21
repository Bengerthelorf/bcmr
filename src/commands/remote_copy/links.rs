//! Link-preserving copy supplements ordinary payload transfer with SSH link
//! operations. A link target is always data and is never sent as shell code.
use super::{preserve_directories, progress_mode, transfer_options_from_cli, DirectoryTransfer};
use crate::cli::Commands;
use crate::commands::copy::{create_symlink_replacing, SymlinkKind};
use crate::core::error::BcmrError;
use crate::core::interrupt;
use crate::core::remote::move_delivery::{self, Content, Target};
use crate::core::remote::{self, parse_remote_path, RemotePath};
use crate::core::traversal;
use crate::ui::runner::{ProgressCancellation, ProgressRunner};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

enum Leaf {
    File {
        local: PathBuf,
        remote: RemotePath,
        size: u64,
    },
    Link {
        local: PathBuf,
        remote: RemotePath,
        target: String,
        kind: SymlinkKind,
    },
}

struct UiInterruptTask(tokio::task::JoinHandle<()>);

impl Drop for UiInterruptTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn check_cancel(cancel: &ProgressCancellation) -> Result<()> {
    if cancel.is_cancelled() {
        interrupt::request();
    }
    interrupt::check()?;
    Ok(())
}

/// Fingerprint the actual completed file, including a destination reused by
/// resume. Preserve -p access time through the same descriptor used for hashing.
async fn local_delivery_hash(path: &Path, preserve_atime: bool) -> Result<String, BcmrError> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;

        interrupt::check()?;
        let mut file = move_delivery::open_local_witness(&path, preserve_atime)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(BcmrError::InvalidInput(format!(
                "delivered move payload '{}' is not a regular file",
                path.display()
            )));
        }
        let result = (|| {
            let mut hasher = blake3::Hasher::new();
            let mut buffer = [0; 64 * 1024];
            loop {
                interrupt::check()?;
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
            }
            Ok::<_, BcmrError>(hasher.finalize().to_hex().to_string())
        })();
        let restore = if preserve_atime {
            filetime::set_file_handle_times(
                &file,
                Some(filetime::FileTime::from_last_access_time(&metadata)),
                None,
            )
        } else {
            Ok(())
        };
        let hash = result?;
        restore?;
        Ok(hash)
    })
    .await?
}

fn remote_child(root: &RemotePath, relative: &str) -> RemotePath {
    if relative.is_empty() {
        root.clone()
    } else {
        root.join(relative)
    }
}

fn local_child(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.is_empty() {
        Ok(root.to_path_buf())
    } else {
        Ok(crate::core::destination::remote_entry_path(root, relative)?)
    }
}

fn source_relative(root: &Path, path: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(root)?
        .components()
        .map(|part| {
            part.as_os_str().to_str().ok_or_else(|| {
                anyhow::anyhow!("non-UTF-8 source path cannot be transferred over SSH")
            })
        })
        .collect::<Result<Vec<_>>>()?
        .join("/"))
}

#[cfg(windows)]
pub(super) fn local_link_kind(metadata: &std::fs::Metadata) -> SymlinkKind {
    use std::os::windows::fs::MetadataExt;
    crate::commands::copy::windows_symlink_kind_from_attributes(metadata.file_attributes())
}

#[cfg(not(windows))]
pub(super) fn local_link_kind(_metadata: &std::fs::Metadata) -> SymlinkKind {
    SymlinkKind::File
}

/// `dest` already has tilde/directory inference resolved and overwrite approval
/// has already been obtained by the caller. This function does not prompt.
pub(super) async fn handle_link_copy(
    args: &Commands,
    sources: &[PathBuf],
    dest: &Path,
    excludes: &[regex::Regex],
) -> Result<()> {
    interrupt::check()?;
    let remote_dest = parse_remote_path(&dest.to_string_lossy());
    let upload = remote_dest.is_some();
    let moving = matches!(args, Commands::Move { .. });
    let mut directories = Vec::<DirectoryTransfer>::new();
    let mut leaves = Vec::<Leaf>::new();

    if let Some(destination) = &remote_dest {
        let selected = sources
            .iter()
            .filter(|source| !traversal::is_copy_excluded(source, source, excludes))
            .collect::<Vec<_>>();
        let multi = selected.len() > 1;
        for source in selected {
            interrupt::check()?;
            if parse_remote_path(&source.to_string_lossy()).is_some() {
                bail!("remote-to-remote transfers are not supported");
            }
            let target = super::resolve_upload_remote(source, destination, multi);
            for entry in traversal::copy_walk(source, args.is_recursive(), 0, true, excludes) {
                interrupt::check()?;
                let entry = entry?;
                let path = entry.path();
                let relative = source_relative(source, path)?;
                let remote = remote_child(&target, &relative);
                let metadata = entry.metadata()?;
                if metadata.is_dir() {
                    if !args.is_recursive() {
                        bail!(
                            "Source '{}' is a directory. Use -r flag for recursive copy.",
                            source.display()
                        );
                    }
                    directories.push(DirectoryTransfer {
                        local_path: path.to_path_buf(),
                        remote,
                    });
                } else if metadata.file_type().is_symlink() {
                    let link_target = std::fs::read_link(path)?;
                    let link_target = link_target
                        .to_str()
                        .ok_or_else(|| {
                            anyhow::anyhow!("non-UTF-8 link target cannot be transferred over SSH")
                        })?
                        .to_owned();
                    leaves.push(Leaf::Link {
                        local: path.to_path_buf(),
                        remote,
                        target: link_target,
                        kind: local_link_kind(&metadata),
                    });
                } else if metadata.is_file() {
                    leaves.push(Leaf::File {
                        local: path.to_path_buf(),
                        remote,
                        size: metadata.len(),
                    });
                } else {
                    bail!("unsupported source entry '{}'", path.display());
                }
            }
        }
    } else {
        for source in sources {
            interrupt::check()?;
            let remote = parse_remote_path(&source.to_string_lossy())
                .ok_or_else(|| anyhow::anyhow!("mixed local/remote sources are not supported"))?;
            let inventory =
                interrupt::read_only(remote::inventory_remote(&remote, true, excludes)).await?;
            let Some(root) = inventory.first() else {
                continue;
            };
            if root.is_dir && !args.is_recursive() {
                bail!(
                    "Remote source '{}' is a directory. Use -r flag for recursive copy.",
                    remote
                );
            }
            let target = super::resolve_download_local(&remote, dest)?;
            for entry in inventory {
                interrupt::check()?;
                let local = local_child(&target, &entry.path)?;
                let remote = remote_child(&remote, &entry.path);
                if let Some(link_target) = entry.link_target {
                    // POSIX links have no intrinsic file/directory kind. On
                    // Windows, detect a live directory target for its API.
                    let kind = if cfg!(windows) && remote::remote_path_is_directory(&remote).await {
                        SymlinkKind::Directory
                    } else {
                        SymlinkKind::File
                    };
                    leaves.push(Leaf::Link {
                        local,
                        remote,
                        target: link_target,
                        kind,
                    });
                } else if entry.is_dir {
                    directories.push(DirectoryTransfer {
                        local_path: local,
                        remote,
                    });
                } else {
                    leaves.push(Leaf::File {
                        local,
                        remote,
                        size: entry.size,
                    });
                }
            }
        }
    }

    if moving && args.is_preserve() {
        use super::endpoint::Endpoint;
        for directory in &directories {
            let source = if upload {
                Endpoint::Local(directory.local_path.clone())
            } else {
                Endpoint::Ssh(directory.remote.clone())
            };
            let mode = source.metadata().await?.mode;
            if mode & 0o100 == 0 {
                bail!("cannot validate final move destination with preserved directory permissions {mode:o} for '{}'; use copy -p and review source removal separately",source.display());
            }
        }
        for leaf in &leaves {
            if let Leaf::File { local, remote, .. } = leaf {
                let source = if upload {
                    Endpoint::Local(local.clone())
                } else {
                    Endpoint::Ssh(remote.clone())
                };
                let mode = source.metadata().await?.mode;
                if mode & 0o400 == 0 {
                    bail!("cannot validate final move destination with preserved permissions {mode:o} for '{}'; use copy -p and review source removal separately",source.display());
                }
            }
        }
    }
    for leaf in &leaves {
        if let Leaf::Link {
            local,
            remote,
            target,
            kind,
        } = leaf
        {
            use super::endpoint::Endpoint;
            let local = Endpoint::Local(local.clone());
            let remote = Endpoint::Ssh(remote.clone());
            let (source, destination) = if upload {
                (local, remote)
            } else {
                (remote, local)
            };
            super::composite::validate_link_plan(
                source,
                destination,
                target.clone(),
                *kind,
                moving,
            )
            .await?;
        }
    }
    interrupt::check()?;
    if args.is_dry_run() {
        for directory in &directories {
            println!(
                "Dry-run: would create directory {}",
                if upload {
                    directory.remote.display()
                } else {
                    directory.local_path.display().to_string()
                }
            );
        }
        for leaf in &leaves {
            match leaf {
                Leaf::File { local, remote, .. } => println!(
                    "Dry-run: would copy {} -> {}",
                    if upload {
                        local.display().to_string()
                    } else {
                        remote.display()
                    },
                    if upload {
                        remote.display()
                    } else {
                        local.display().to_string()
                    }
                ),
                Leaf::Link {
                    local,
                    remote,
                    target,
                    ..
                } => println!(
                    "Dry-run: would preserve symbolic link {} -> {:?}",
                    if upload {
                        remote.display()
                    } else {
                        local.display().to_string()
                    },
                    target
                ),
            }
        }
        return Ok(());
    }

    if directories.is_empty() && leaves.is_empty() {
        return Ok(());
    }
    if args.use_direct_tcp() {
        bail!("--direct direct is not supported for link-preserving remote transfers; use --direct ssh");
    }
    if upload && args.is_sync() {
        bail!("--sync cannot guarantee durable remote namespace publication for link-preserving uploads; refusing before transfer");
    }
    #[cfg(not(unix))]
    if args.is_sync() {
        bail!("--sync cannot guarantee durable local namespace publication on this platform; refusing before transfer");
    }

    let total = leaves.iter().try_fold(0u64, |total, leaf| match leaf {
        Leaf::File { size, .. } => total
            .checked_add(*size)
            .ok_or_else(|| anyhow::anyhow!("selected source size exceeds u64")),
        Leaf::Link { .. } => Ok(total),
    })?;
    let runner = ProgressRunner::new_without_signal_handler(
        total,
        progress_mode(args),
        args.is_quiet(),
        crate::config::is_json_mode(),
    )?;
    runner.set_operation_type(if upload { "Uploading" } else { "Downloading" });
    runner.set_verify_mode(args.is_verify());
    let cancel = runner.cancellation_handle();
    let ui_cancel = cancel.clone();
    let _ui_interrupt = UiInterruptTask(tokio::spawn(async move {
        ui_cancel.cancelled().await;
        interrupt::request();
    }));
    let result = async {
        check_cancel(&cancel)?;
        if upload {
            remote::ensure_remote_directories(
                &directories
                    .iter()
                    .map(|directory| directory.remote.clone())
                    .collect::<Vec<_>>(),
            )
            .await?;
        } else {
            for directory in &directories {
                check_cancel(&cancel)?;
                if args.is_sync() {
                    crate::core::io::create_dir_all_durable_async(&directory.local_path).await?;
                } else {
                    tokio::fs::create_dir_all(&directory.local_path).await?;
                }
            }
        }
        let options = transfer_options_from_cli(args);
        for leaf in leaves {
            check_cancel(&cancel)?;
            if !upload && args.is_sync() {
                let local = match &leaf {
                    Leaf::File { local, .. } | Leaf::Link { local, .. } => local,
                };
                if let Some(parent) = local
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                {
                    crate::core::io::create_dir_all_durable_async(parent).await?;
                }
            }
            let progress = runner.inc_callback();
            let skip = runner.skip_callback();
            let file = runner.file_callback();
            match leaf {
                Leaf::File {
                    local,
                    remote,
                    size,
                } => {
                    let callbacks = remote::TransferCallbacks {
                        on_progress: &progress,
                        on_skip: &skip,
                        on_new_file: &file,
                    };
                    if upload {
                        remote::upload_file(&local, &remote, callbacks, &options).await?;
                        if moving {
                            // Source ctime/identity remains guarded by the
                            // captured local manifest. Validate these expected
                            // bytes at the remote destination after quarantine.
                            let hash = local_delivery_hash(&local, false).await?;
                            move_delivery::remember_local(
                                &local,
                                Target::Remote(remote),
                                Content::File(hash),
                                args.is_preserve(),
                            )?;
                        }
                    } else {
                        remote::download_file(&remote, &local, callbacks, size, &options).await?;
                        if moving {
                            let hash = local_delivery_hash(&local, args.is_preserve()).await?;
                            move_delivery::remember(
                                &remote,
                                Target::Local(local),
                                Content::File(hash),
                                args.is_preserve(),
                            );
                        }
                    }
                }
                Leaf::Link {
                    local,
                    remote,
                    target,
                    kind,
                } => {
                    if upload {
                        if let Some((parent, _)) = remote.path.rsplit_once('/') {
                            if !parent.is_empty() {
                                remote::ensure_remote_directories(&[RemotePath {
                                    path: parent.to_owned(),
                                    ..remote.clone()
                                }])
                                .await?;
                            }
                        }
                        remote::create_remote_symlink(&remote, &target, args.is_force()).await?;
                        if moving {
                            move_delivery::remember_local(
                                &local,
                                Target::Remote(remote),
                                Content::Link(target.into_bytes()),
                                args.is_preserve(),
                            )?;
                        }
                    } else {
                        if !args.is_sync() {
                            if let Some(parent) = local.parent() {
                                tokio::fs::create_dir_all(parent).await?;
                            }
                        }
                        create_symlink_replacing(
                            &local,
                            Path::new(&target),
                            kind,
                            args.is_force(),
                            args.is_sync(),
                            &args.get_test_mode(),
                        )
                        .await?;
                        if moving {
                            let target = tokio::fs::read_link(&local).await?;
                            move_delivery::remember(
                                &remote,
                                Target::Local(local),
                                Content::Link(target.as_os_str().as_encoded_bytes().to_vec()),
                                args.is_preserve(),
                            );
                        }
                    }
                }
            }
            check_cancel(&cancel)?;
        }
        if args.is_preserve() {
            check_cancel(&cancel)?;
            preserve_directories(&directories, upload, args.is_sync()).await?;
        }
        check_cancel(&cancel)?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    match result {
        Ok(()) => {
            runner.finish_ok()?;
            check_cancel(&cancel)
        }
        Err(error) => {
            let error = check_cancel(&cancel).err().unwrap_or(error);
            runner.finish_with_error(&error.to_string());
            Err(error)
        }
    }
}
