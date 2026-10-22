use crate::cli::Commands;
use crate::commands::copy;
use crate::core::error::BcmrError;
use crate::core::io as durable_io;
use crate::core::move_file::{rename_no_replace, LocalCleanupPlan};
use crate::core::traversal;
use crate::ui::display::{print_dry_run, ActionType};

pub use copy::FileToOverwrite;
use std::path::{Path, PathBuf};
use tokio::fs;

pub(crate) fn refuse_same_file_move(src: &Path, dst: &Path) -> std::result::Result<(), BcmrError> {
    if std::fs::symlink_metadata(dst)
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(());
    }
    let same_spelling = std::path::absolute(src)? == std::path::absolute(dst)?;
    let same_entry = crate::core::move_file::same_entry(src, dst)?;
    let source_is_link = src.symlink_metadata()?.file_type().is_symlink();
    let destination_is_link = dst.symlink_metadata()?.file_type().is_symlink();
    // Preserve the existing protection against replacing a link's own target.
    // Two distinct link entries may share a referent, and an unavailable
    // referent must not prevent moving a dangling or cyclic link object.
    let same_referent = source_is_link != destination_is_link
        && match (
            same_file::Handle::from_path(src),
            same_file::Handle::from_path(dst),
        ) {
            (Ok(source), Ok(destination)) => source == destination,
            _ => false,
        };
    if same_spelling || same_entry || same_referent {
        return Err(BcmrError::InvalidInput(format!(
            "cannot move '{}' onto the same file '{}'",
            src.display(),
            dst.display()
        )));
    }

    Ok(())
}

fn is_cross_device_error(err: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        err.raw_os_error() == Some(libc::EXDEV)
    }

    #[cfg(windows)]
    {
        // 17 = ERROR_NOT_SAME_DEVICE
        err.raw_os_error() == Some(17)
    }

    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

pub async fn check_overwrites(
    sources: &[PathBuf],
    dst: &Path,
    recursive: bool,
    cli: &Commands,
    excludes: &[regex::Regex],
) -> std::result::Result<Vec<FileToOverwrite>, BcmrError> {
    copy::check_overwrites(sources, dst, recursive, cli, excludes).await
}

pub async fn get_total_size(
    sources: &[PathBuf],
    recursive: bool,
    cli: &Commands,
    excludes: &[regex::Regex],
) -> std::result::Result<u64, BcmrError> {
    copy::get_total_size(sources, recursive, cli, excludes).await
}

pub async fn move_path<F>(
    src: &Path,
    dst: &Path,
    cli: &Commands,
    excludes: &[regex::Regex],
    progress_callback: F,
    on_new_file: impl Fn(&str, u64) + Send + Sync + 'static + Clone,
    on_reflink: impl Fn() + Send + Sync + 'static + Clone,
) -> std::result::Result<(), BcmrError>
where
    F: Fn(u64) + Send + Sync + Clone + 'static,
{
    let recursive = cli.is_recursive();
    if traversal::is_copy_excluded(src, src, excludes) {
        return Ok(());
    }
    let source_metadata = match src.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(BcmrError::SourceNotFound(src.to_path_buf()))
        }
        Err(error) => return Err(error.into()),
    };
    #[cfg(windows)]
    if cli.is_sync() {
        return Err(BcmrError::InvalidInput(
            "--sync cannot guarantee durable move publication on Windows; refusing before mutation"
                .into(),
        ));
    }

    if source_metadata.is_file() || source_metadata.file_type().is_symlink() {
        let dst_path = if crate::core::destination::local_is_directory(dst)? {
            dst.join(
                src.file_name()
                    .ok_or_else(BcmrError::invalid_source_file_name)?,
            )
        } else {
            dst.to_path_buf()
        };

        refuse_same_file_move(src, &dst_path)?;

        let destination_exists = dst_path.symlink_metadata().is_ok();
        if cli.is_dry_run() {
            print_dry_run(
                if destination_exists {
                    ActionType::Overwrite
                } else {
                    ActionType::Move
                },
                &src.to_string_lossy(),
                Some(&dst_path.to_string_lossy()),
            );
            return Ok(());
        }
        if destination_exists && !cli.is_force() {
            return Err(BcmrError::TargetExists(dst_path));
        }

        let file_size = source_metadata.len();
        let file_name = src
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if let Some(parent) = dst_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            if cli.is_sync() {
                durable_io::create_dir_all_durable_async(parent).await?;
            } else {
                fs::create_dir_all(parent).await?;
            }
        }
        if cli.is_sync() && source_metadata.is_file() {
            durable_io::durable_sync(&std::fs::File::open(src)?)?;
        }
        #[cfg(feature = "test-support")]
        if matches!(
            cli.get_test_mode(),
            crate::cli::TestMode::CreateDestinationBeforeFinalize
        ) {
            fs::write(&dst_path, b"concurrent target").await?;
        }
        let rename_result = if cli.is_force() {
            fs::rename(src, &dst_path).await
        } else {
            rename_no_replace(src, &dst_path)
        };
        if let Err(e) = rename_result {
            if is_cross_device_error(&e) {
                copy_exact_and_cleanup(
                    src,
                    &dst_path,
                    cli,
                    excludes,
                    progress_callback.clone(),
                    on_new_file.clone(),
                    on_reflink.clone(),
                )
                .await?;
            } else if crate::core::move_file::is_target_exists(&e) {
                return Err(BcmrError::TargetExists(dst_path));
            } else {
                return Err(BcmrError::Io(e));
            }
        } else {
            if cli.is_sync() {
                sync_rename_parents(src, &dst_path)?;
            }
            on_new_file(&file_name, file_size);
            progress_callback(file_size);
            if cli.is_verbose() {
                eprintln!("renamed '{}' -> '{}'", src.display(), dst_path.display());
            }
        }
    } else if recursive && source_metadata.is_dir() {
        let src_name = src
            .file_name()
            .ok_or_else(BcmrError::invalid_source_dir_name)?;
        let new_dst = if crate::core::destination::local_is_directory(dst)? {
            dst.join(src_name)
        } else {
            dst.to_path_buf()
        };

        // The resolved destination is fixed for both rename and merge. Never
        // create a nested destination before rejecting self/descendant moves.
        if copy::is_dst_self_or_under_src(src, &new_dst)? {
            return Err(BcmrError::InvalidInput(format!(
                "cannot move directory '{}' into itself ('{}')",
                src.display(),
                new_dst.display()
            )));
        }
        if cli.is_dry_run() {
            let plan = copy::plan_copy_exact(src, &new_dst, true, true, excludes).await?;
            return copy::dry_run_plan(&plan, cli);
        }
        let destination_exists = match new_dst.symlink_metadata() {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => true,
            Ok(_) => return Err(BcmrError::TargetExists(new_dst)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        if !excludes.is_empty() || destination_exists {
            copy_exact_and_cleanup(
                src,
                &new_dst,
                cli,
                excludes,
                progress_callback.clone(),
                on_new_file.clone(),
                on_reflink.clone(),
            )
            .await?;
        } else {
            let dir_size = copy::get_total_size(&[src.to_path_buf()], true, cli, excludes).await?;
            let dir_name = src
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            if let Some(parent) = new_dst
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                if cli.is_sync() {
                    durable_io::create_dir_all_durable_async(parent).await?;
                } else {
                    fs::create_dir_all(parent).await?;
                }
            }

            if let Err(e) = rename_no_replace(src, &new_dst) {
                if is_cross_device_error(&e) {
                    copy_exact_and_cleanup(
                        src,
                        &new_dst,
                        cli,
                        excludes,
                        progress_callback.clone(),
                        on_new_file.clone(),
                        on_reflink.clone(),
                    )
                    .await?;
                } else if crate::core::move_file::is_target_exists(&e) {
                    return Err(BcmrError::TargetExists(new_dst));
                } else {
                    return Err(e.into());
                }
            } else {
                if cli.is_sync() {
                    sync_rename_parents(src, &new_dst)?;
                }
                on_new_file(&dir_name, dir_size);
                progress_callback(dir_size);
                if cli.is_verbose() {
                    eprintln!("renamed '{}' -> '{}'", src.display(), new_dst.display());
                }
            }
        }
    } else if source_metadata.is_dir() {
        return Err(BcmrError::InvalidInput(format!(
            "Source '{}' is a directory. Use -r flag for recursive move.",
            src.display()
        )));
    } else {
        return Err(BcmrError::SourceNotFound(src.to_path_buf()));
    };

    Ok(())
}

async fn copy_exact_and_cleanup<F>(
    source: &Path,
    destination: &Path,
    cli: &Commands,
    excludes: &[regex::Regex],
    progress_callback: F,
    on_new_file: impl Fn(&str, u64) + Send + Sync + 'static,
    on_reflink: impl Fn() + Send + Sync + 'static,
) -> Result<(), BcmrError>
where
    F: Fn(u64) + Send + Sync + Clone + 'static,
{
    let cleanup = LocalCleanupPlan::capture(&[source.to_path_buf()], excludes)?;
    let plan =
        copy::plan_copy_exact(source, destination, cli.is_recursive(), true, excludes).await?;
    copy::execute_plan(&plan, cli, progress_callback, on_new_file, on_reflink).await?;
    cleanup.remove().await?;
    if cli.is_sync() {
        sync_rename_parents(source, destination)?;
    }
    Ok(())
}

fn sync_rename_parents(source: &Path, destination: &Path) -> Result<(), BcmrError> {
    let parent = |path: &Path| {
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    };
    let source_parent = parent(source);
    let destination_parent = parent(destination);
    #[cfg(unix)]
    for path in [&destination_parent, &source_parent] {
        durable_io::durable_sync_directory_handle_strict(&std::fs::File::open(path)?)?;
    }
    #[cfg(not(unix))]
    {
        let _ = (source_parent, destination_parent);
    }
    Ok(())
}
