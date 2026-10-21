use super::{resolve_download_local, resolve_upload_remote};
use crate::app::prompts::confirm_overwrite;
use crate::cli::Commands;
use crate::commands::copy::FileToOverwrite;
use crate::core::error::BcmrError;
use crate::core::remote::{self, parse_remote_path, RemotePath};
use crate::core::traversal;
use anyhow::Result;
use std::path::{Path, PathBuf};

async fn remote_existing_kind(remote: &RemotePath) -> Result<Option<bool>> {
    let command = format!(
        "if [ -L '{0}' ]; then printf l; elif [ -d '{0}' ]; then printf d; elif [ -e '{0}' ]; then printf f; fi",
        remote.path.replace('\'', "'\\''")
    );
    // Use the same connection policy as source inventory and payload transfer.
    let output = remote::metadata_command(&remote.ssh_target(), command).await?;
    match output.as_slice() {
        b"" => Ok(None),
        b"d" => Ok(Some(true)),
        b"f" | b"l" => Ok(Some(false)),
        _ => anyhow::bail!("Invalid remote destination metadata for '{remote}'"),
    }
}

fn local_existing_kind(path: &Path) -> Result<Option<bool>> {
    match path.symlink_metadata() {
        Ok(metadata) => Ok(Some(metadata.is_dir())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Obtain one approval for the selected leaves before any destination writes.
/// Cancellation is an error, as for local copy, so move cleanup cannot run.
pub(super) async fn confirm_remote_overwrites(
    args: &Commands,
    sources: &[PathBuf],
    dest: &Path,
    excludes: &[regex::Regex],
) -> Result<()> {
    if !args.should_prompt_for_overwrite() || args.is_dry_run() {
        return Ok(());
    }
    let preserve_links = args.is_no_deref() || matches!(args, Commands::Move { .. });
    let mut overwrites = Vec::new();
    if let Some(remote_dest) = parse_remote_path(&dest.to_string_lossy()) {
        for source in sources {
            let base = resolve_upload_remote(source, &remote_dest, sources.len() > 1);
            for entry in
                traversal::copy_walk(source, args.is_recursive(), 0, preserve_links, excludes)
            {
                let entry = entry?;
                if entry.file_type().is_dir() {
                    continue;
                }
                let relative = entry
                    .path()
                    .strip_prefix(source)?
                    .components()
                    .map(|part| {
                        part.as_os_str().to_str().ok_or_else(|| {
                            anyhow::anyhow!("non-UTF-8 source path cannot be transferred over SSH")
                        })
                    })
                    .collect::<Result<Vec<_>>>()?
                    .join("/");
                let target = if relative.is_empty() {
                    base.clone()
                } else {
                    base.join(&relative)
                };
                if let Some(is_dir) = remote_existing_kind(&target).await? {
                    overwrites.push(FileToOverwrite {
                        path: PathBuf::from(target.display()),
                        is_dir,
                    });
                }
            }
        }
    } else {
        for source in sources {
            let remote_source = parse_remote_path(&source.to_string_lossy()).ok_or_else(|| {
                anyhow::anyhow!("Mixed local/remote sources without remote destination")
            })?;
            let base = resolve_download_local(&remote_source, dest)?;
            let entries =
                remote::inventory_remote(&remote_source, preserve_links, excludes).await?;
            for entry in entries {
                if entry.is_dir {
                    continue;
                }
                let target = if entry.path.is_empty() {
                    base.clone()
                } else {
                    crate::core::destination::remote_entry_path(&base, &entry.path)?
                };
                if let Some(is_dir) = local_existing_kind(&target)? {
                    overwrites.push(FileToOverwrite {
                        path: target,
                        is_dir,
                    });
                }
            }
        }
    }
    overwrites.sort_by(|a, b| a.path.cmp(&b.path));
    overwrites.dedup_by(|a, b| a.path == b.path);
    if !overwrites.is_empty() && !confirm_overwrite(&overwrites)? {
        return Err(BcmrError::Cancelled.into());
    }
    Ok(())
}
