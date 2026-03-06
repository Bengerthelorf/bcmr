use crate::cli::{Commands, TestMode};
use crate::commands::copy;
use crate::core::traversal;
use crate::core::error::BcmrError;
use crate::ui::display::{print_dry_run, ActionType};

use std::path::{Path, PathBuf};
use tokio::fs;
pub use copy::FileToOverwrite;

/// Check if a rename error indicates a cross-device move.
fn is_cross_device_error(err: &std::io::Error) -> bool {
    #[cfg(unix)]
    { err.raw_os_error() == Some(libc::EXDEV) }

    #[cfg(windows)]
    { err.raw_os_error() == Some(17) } // ERROR_NOT_SAME_DEVICE

    #[cfg(not(any(unix, windows)))]
    { false }
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

// Reuse the total size calculation from copy
pub async fn get_total_size(
    sources: &[PathBuf],
    recursive: bool,
    cli: &Commands,
    excludes: &[regex::Regex],
) -> std::result::Result<u64, BcmrError> {
    copy::get_total_size(sources, recursive, cli, excludes).await
}

// use std::sync::Arc;

#[allow(clippy::too_many_arguments)]
pub async fn move_path<F>(
    src: &Path,
    dst: &Path,
    recursive: bool,
    preserve: bool,
    test_mode: TestMode,
    cli: &Commands,
    excludes: &[regex::Regex],
    progress_callback: F,
    on_new_file: impl Fn(&str, u64) + Send + Sync + 'static + Clone,
) -> std::result::Result<(), BcmrError>
where
    F: Fn(u64) + Send + Sync + Clone,
{
    if traversal::is_excluded(src, excludes) {
        return Ok(());
    }

    if src.is_file() {
        let dst_path = if dst.is_dir() {
            dst.join(
                src.file_name()
                    .ok_or_else(|| BcmrError::InvalidInput("Invalid source file name".to_string()))?,
            )
        } else {
            dst.to_path_buf()
        };

        if dst_path.exists() && !cli.is_force() {
            return Err(BcmrError::TargetExists(dst_path));
        }

        if cli.is_dry_run() {
            print_dry_run(
                ActionType::Move,
                &src.to_string_lossy(),
                Some(&dst_path.to_string_lossy())
            );
            return Ok(());
        }

        if dst_path.exists() && cli.is_force() {
            fs::remove_file(&dst_path).await?;
        }

        // Try rename -> EXDEV? Copy+Rm : Err
        let file_size = src.metadata()?.len();
        let file_name = src.file_name().unwrap_or_default().to_string_lossy().to_string();
        if let Err(e) = fs::rename(src, &dst_path).await {
            if is_cross_device_error(&e) {
                copy::copy_path(
                    src,
                    &dst_path,
                    false,
                    preserve,
                    test_mode,
                    cli,
                    excludes,
                    progress_callback.clone(),
                    on_new_file.clone()
                ).await?;
                fs::remove_file(src).await?;
            } else {
                return Err(BcmrError::Io(e));
            }
        } else {
            // Rename succeeded instantly — report full progress
            on_new_file(&file_name, file_size);
            progress_callback(file_size);
            if cli.is_verbose() {
                eprintln!("renamed '{}' -> '{}'", src.display(), dst_path.display());
            }
        }
    } else if recursive && src.is_dir() {
        let src_name = src
            .file_name()
            .ok_or_else(|| BcmrError::InvalidInput("Invalid source directory name".to_string()))?;
        let new_dst = if dst.is_dir() {
            dst.join(src_name)
        } else {
            dst.to_path_buf()
        };

        // Excludes OR dry-run -> inspect contents
        if !excludes.is_empty() || cli.is_dry_run() {
            if cli.is_dry_run() {
                 if !new_dst.exists() {
                     print_dry_run(
                        ActionType::Add, // Moves involving dir creation
                        &src.to_string_lossy(),
                        Some(&format!("(DIR) -> {}", new_dst.display()))
                     );
                 }
                 
                 // Simulate walking to show all moves
                 for entry in traversal::walk(src, true, false, 1, excludes) {
                     let entry = entry?;
                     let path = entry.path();
                     let relative_path = path.strip_prefix(src)?;
                     let target_path = new_dst.join(relative_path);
                     
                     if path.is_dir() {
                         if !target_path.exists() {
                             print_dry_run(
                                ActionType::Add, // Creating dir
                                &path.to_string_lossy(),
                                Some(&format!("(DIR) -> {}", target_path.display()))
                            );
                         }
                     } else {
                         print_dry_run(
                            ActionType::Move, 
                            &path.to_string_lossy(),
                            Some(&target_path.to_string_lossy())
                        );
                     }
                 }
                 return Ok(());
            }

            // Excludes: rename ignores excludes -> Copy + Remove source(files) + Remove source(dir, if empty)
            
            // 1. Copy
            copy::copy_path(
                src,
                dst, 
                recursive,
                preserve,
                test_mode.clone(),
                cli,
                excludes,
                progress_callback.clone(),
                on_new_file.clone()
            ).await?;

            remove_directory_contents(src, excludes).await?;
            let _ = fs::remove_dir(src).await; 

        } else {
            // Calculate size before rename (for progress reporting on success)
            let dir_size = copy::get_total_size(&[src.to_path_buf()], true, cli, excludes).await.unwrap_or(0);
            let dir_name = src.file_name().unwrap_or_default().to_string_lossy().to_string();

            if let Err(e) = fs::rename(src, &new_dst).await {
                if is_cross_device_error(&e) {
                    copy::copy_path(
                        src,
                        dst,
                        recursive,
                        preserve,
                        test_mode,
                        cli,
                        excludes,
                        progress_callback.clone(),
                        on_new_file.clone()
                    ).await?;
                    fs::remove_dir_all(src).await?;
                } else {
                    return Err(e.into());
                }
            } else {
                // Rename succeeded instantly — report full progress
                on_new_file(&dir_name, dir_size);
                progress_callback(dir_size);
                if cli.is_verbose() {
                    eprintln!("renamed '{}' -> '{}'", src.display(), new_dst.display());
                }
            }
        }
    } else if src.is_dir() {
        return Err(BcmrError::InvalidInput(format!(
            "Source '{}' is a directory. Use -r flag for recursive move.",
            src.display()
        )));
    } else {
        return Err(BcmrError::SourceNotFound(src.to_path_buf()));
    };

    Ok(())
}

async fn remove_directory_contents(dir: &Path, excludes: &[regex::Regex]) -> std::result::Result<(), BcmrError> {
    // Walk with contents_first=true: children are yielded before parents
    for entry in traversal::walk(dir, true, true, 0, excludes) {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            fs::remove_file(path).await?;
        } else if path.is_dir() {
            // remove_dir ensures only empty dirs are removed
            let _ = fs::remove_dir(path).await;
        }
    }

    Ok(())
}
