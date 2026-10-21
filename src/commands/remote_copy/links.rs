//! Link-preserving copy supplements ordinary payload transfer with SSH link
//! operations. A link target is always data and is never sent as shell code.
use super::{preserve_directories, progress_mode, transfer_options_from_cli, DirectoryTransfer};
use crate::cli::Commands;
use crate::commands::copy::{create_symlink_replacing, SymlinkKind};
use crate::core::remote::{self, parse_remote_path, RemotePath};
use crate::core::traversal;
use crate::ui::runner::ProgressRunner;
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
fn local_link_kind(metadata: &std::fs::Metadata) -> SymlinkKind {
    use std::os::windows::fs::MetadataExt;
    if metadata.file_attributes() & 0x10 != 0 {
        SymlinkKind::Directory
    } else {
        SymlinkKind::File
    }
}

#[cfg(not(windows))]
fn local_link_kind(_metadata: &std::fs::Metadata) -> SymlinkKind {
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
    let remote_dest = parse_remote_path(&dest.to_string_lossy());
    let upload = remote_dest.is_some();
    let mut directories = Vec::<DirectoryTransfer>::new();
    let mut leaves = Vec::<Leaf>::new();

    if let Some(destination) = &remote_dest {
        let selected = sources
            .iter()
            .filter(|source| !traversal::is_copy_excluded(source, source, excludes))
            .collect::<Vec<_>>();
        let multi = selected.len() > 1;
        for source in selected {
            if parse_remote_path(&source.to_string_lossy()).is_some() {
                bail!("remote-to-remote transfers are not supported");
            }
            let target = super::resolve_upload_remote(source, destination, multi);
            for entry in traversal::copy_walk(source, args.is_recursive(), 0, true, excludes) {
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
            let remote = parse_remote_path(&source.to_string_lossy())
                .ok_or_else(|| anyhow::anyhow!("mixed local/remote sources are not supported"))?;
            let inventory = remote::inventory_remote(&remote, true, excludes).await?;
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
    let runner = ProgressRunner::new(
        total,
        progress_mode(args),
        args.is_quiet(),
        crate::config::is_json_mode(),
        crate::core::cleanup::cleanup_partial_files,
    )?;
    runner.set_operation_type(if upload { "Uploading" } else { "Downloading" });
    runner.set_verify_mode(args.is_verify());
    let result = async {
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
                if args.is_sync() {
                    crate::core::io::create_dir_all_durable_async(&directory.local_path).await?;
                } else {
                    tokio::fs::create_dir_all(&directory.local_path).await?;
                }
            }
        }
        let options = transfer_options_from_cli(args);
        for leaf in leaves {
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
                    } else {
                        remote::download_file(&remote, &local, callbacks, size, &options).await?;
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
                    }
                }
            }
        }
        if args.is_preserve() {
            preserve_directories(&directories, upload, args.is_sync()).await?;
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    match result {
        Ok(()) => runner.finish_ok(),
        Err(error) => runner.finish_err(error.to_string()),
    }
}
