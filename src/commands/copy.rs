use crate::cli::{Commands, TestMode, SparseMode}; // Added SparseMode
use crate::core::traversal;
use crate::core::checksum;
use crate::core::error::BcmrError;
use crate::ui::display::{print_dry_run, ActionType};



use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::fs::{self, File};
use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncSeekExt, SeekFrom};

pub struct FileToOverwrite {
    pub path: PathBuf,
    pub is_dir: bool,
}

pub async fn check_overwrites(
    sources: &[PathBuf],
    dst: &Path,
    recursive: bool,
    _cli: &Commands,
    excludes: &[regex::Regex],
) -> std::result::Result<Vec<FileToOverwrite>, BcmrError> {
    let mut files_to_overwrite = Vec::new();

    let dst_is_dir = dst.exists() && dst.is_dir();

    for src in sources {
        if traversal::is_excluded(src, excludes) {
            continue;
        }

        if src.is_file() {
            let dst_path = if dst_is_dir {
                dst.join(
                    src.file_name()
                        .ok_or_else(|| BcmrError::InvalidInput("Invalid source file name".to_string()))?,
                )
            } else {
                dst.to_path_buf()
            };

            if dst_path.exists() && !traversal::is_excluded(&dst_path, excludes) {
                files_to_overwrite.push(FileToOverwrite {
                    path: dst_path,
                    is_dir: false,
                });
            }
        } else if recursive && src.is_dir() {
            let src_name = src
                .file_name()
                .ok_or_else(|| BcmrError::InvalidInput("Invalid source directory name".to_string()))?;
            let new_dst = if dst_is_dir {
                dst.join(src_name)
            } else {
                dst.to_path_buf()
            };

            // If the target directory exists, check for files that will be overwritten
            if new_dst.exists() {
                for entry in traversal::walk(src, true, false, 1, excludes) {
                    let entry = entry?;
                    let path = entry.path();

                    let relative_path = path.strip_prefix(src)?;
                    let target_path = new_dst.join(relative_path);

                    if target_path.exists() {
                        files_to_overwrite.push(FileToOverwrite {
                            path: target_path,
                            is_dir: path.is_dir(),
                        });
                    }
                }
            }
        }
    }

    Ok(files_to_overwrite)
}

// Helper function to run running blocking directory traversal
fn get_total_size_sync(
    sources: Vec<PathBuf>,
    recursive: bool,
    excludes: Vec<regex::Regex>,
) -> std::result::Result<u64, BcmrError> {
    let mut total_size = 0;

    for src in sources {
        if traversal::is_excluded(&src, &excludes) {
            continue;
        }

        if src.is_file() {
            total_size += src.metadata()?.len();
        } else if src.is_dir() {
            if recursive {
                for entry in traversal::walk(&src, true, false, 1, &excludes) {
                    let entry = entry?;
                    let path = entry.path();
                    if path.is_file() {
                        total_size += entry.metadata()?.len();
                    }
                }
            } else {
                return Err(BcmrError::InvalidInput(format!(
                    "Source '{}' is a directory. Use -r flag for recursive copy.",
                    src.display()
                )));
            }
        } else {
            return Err(BcmrError::SourceNotFound(src));
        }
    }

    Ok(total_size)
}

pub async fn get_total_size(
    sources: &[PathBuf],
    recursive: bool,
    _cli: &Commands,
    excludes: &[regex::Regex],
) -> std::result::Result<u64, BcmrError> {
    let sources = sources.to_vec();
    let recursive = recursive;
    let excludes = excludes.to_vec();

    tokio::task::spawn_blocking(move || get_total_size_sync(sources, recursive, excludes)).await?
}

pub struct ProgressCallback<F> {
    callback: F,
    on_new_file: Box<dyn Fn(&str, u64) + Send + Sync>,
}

pub async fn copy_path<F>(
    src: &Path,
    dst: &Path,
    recursive: bool,
    preserve: bool,
    test_mode: TestMode,
    cli: &Commands,
    excludes: &[regex::Regex],
    progress_callback: F,
    on_new_file: impl Fn(&str, u64) + Send + Sync + 'static,
) -> std::result::Result<(), BcmrError>
where
    F: Fn(u64) + Send + Sync,
{
    let callback = ProgressCallback {
        callback: progress_callback,
        on_new_file: Box::new(on_new_file),
    };

    // Main exclusion check for the root item
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

        // For files, only check when the target file exists
        if dst_path.exists() && !cli.is_force() && !cli.is_resume() && !cli.is_append() && !cli.is_strict() {
            return Err(BcmrError::TargetExists(dst_path));
        }

        if cli.is_dry_run() {
            let action = if !dst_path.exists() {
                ActionType::Add
            } else {
                // Logic duplicating copy_file checks for Dry Run accuracy
                let src_meta = src.metadata()?;
                let dst_meta = dst_path.metadata()?;
                let src_len = src_meta.len();
                let dst_len = dst_meta.len();

                let should_resume = if cli.is_strict() {
                    if dst_len == src_len {
                         // Hash check -> SKIP | OVERWRITE
                         ActionType::Skip // Optimistic
                    } else if dst_len < src_len {
                        ActionType::Append
                    } else {
                        ActionType::Overwrite
                    }
                } else if cli.is_append() {
                    if dst_len == src_len {
                        ActionType::Skip
                    } else if dst_len < src_len {
                        ActionType::Append
                    } else {
                        ActionType::Overwrite
                    }
                } else if cli.is_resume() {
                    // Check Mtime
                    let src_mtime = src_meta.modified()?;
                    let dst_mtime = dst_meta.modified()?;
                    
                    if src_mtime != dst_mtime {
                        ActionType::Overwrite
                    } else {
                        if dst_len == src_len {
                            ActionType::Skip
                        } else if dst_len < src_len {
                            ActionType::Append
                        } else {
                            ActionType::Overwrite
                        }
                    }
                } else {
                    ActionType::Overwrite
                };
                
                should_resume
            };
            
            print_dry_run(
                action,
                &src.to_string_lossy(),
                Some(&dst_path.to_string_lossy())
            );
            return Ok(());
        }

        if dst_path.exists() && cli.is_force() && !cli.is_resume() && !cli.is_append() && !cli.is_strict() {
            fs::remove_file(&dst_path).await?;
        }

        copy_file(src, &dst_path, preserve, cli.is_verify(), cli.is_resume(), cli.is_strict(), cli.is_append(), cli.get_reflink_mode(), cli.get_sparse_mode(), test_mode, &callback).await?;
    } else if recursive && src.is_dir() {
        let src_dir_name = src
            .file_name()
            .ok_or_else(|| BcmrError::InvalidInput("Invalid source directory name".to_string()))?;
        let new_dst = if dst.is_dir() {
            dst.join(src_dir_name)
        } else {
            dst.to_path_buf()
        };

        if cli.is_dry_run() {
             if !new_dst.exists() {
                 print_dry_run(
                     ActionType::Add,
                     &src.to_string_lossy(),
                     Some(&format!("(DIR) -> {}", new_dst.display()))
                 );
             }
        }

        // Create the target directory (if it does not exist)
        if !new_dst.exists() && !cli.is_dry_run() {
            fs::create_dir_all(&new_dst).await?;
        }

        let mut files_to_copy = Vec::new();
        for entry in traversal::walk(src, true, false, 1, excludes) {
            let entry = entry?;
            let path = entry.path();
            
            let relative_path = path.strip_prefix(src)?;
            let target_path = new_dst.join(relative_path);

            if path.is_dir() {
                if !cli.is_dry_run() {
                    if !target_path.exists() {
                        fs::create_dir_all(&target_path).await?;
                    }
                    if preserve {
                        set_dir_attributes(path, &target_path).await?;
                    }
                } else {
                    if !target_path.exists() {
                        print_dry_run(
                            ActionType::Add,
                            &path.to_string_lossy(),
                            Some(&format!("(DIR) -> {}", target_path.display()))
                        );
                    }
                }
            } else if path.is_file() {
                files_to_copy.push((path.to_path_buf(), target_path));
            }
        }

        // Copy files
        for (src_path, dst_path) in files_to_copy {
            if let Some(parent) = dst_path.parent() {
                if !parent.exists() && !cli.is_dry_run() {
                    fs::create_dir_all(parent).await?;
                }
            }

            // Check each file to see if it needs to be overwritten
            if dst_path.exists() && !cli.is_force() && !cli.is_resume() && !cli.is_append() && !cli.is_strict() {
                return Err(BcmrError::TargetExists(dst_path));
            }

            if cli.is_dry_run() {
                let action = if !dst_path.exists() {
                    ActionType::Add
                } else {
                    let src_meta = src_path.metadata()?;
                    let dst_meta = dst_path.metadata()?;
                    let src_len = src_meta.len();
                    let dst_len = dst_meta.len();

                    if cli.is_strict() {
                        if dst_len == src_len { ActionType::Skip }
                        else if dst_len < src_len { ActionType::Append }
                        else { ActionType::Overwrite }
                    } else if cli.is_append() {
                        if dst_len == src_len { ActionType::Skip }
                        else if dst_len < src_len { ActionType::Append }
                        else { ActionType::Overwrite }
                    } else if cli.is_resume() {
                        let src_mtime = src_meta.modified()?;
                        let dst_mtime = dst_meta.modified()?;
                        if src_mtime != dst_mtime { ActionType::Overwrite }
                        else if dst_len == src_len { ActionType::Skip }
                        else if dst_len < src_len { ActionType::Append }
                        else { ActionType::Overwrite }
                    } else {
                        ActionType::Overwrite
                    }
                };

                print_dry_run(
                    action,
                    &src_path.to_string_lossy(),
                    Some(&dst_path.to_string_lossy())
                );
            } else {
                if dst_path.exists() && cli.is_force() && !cli.is_resume() && !cli.is_append() && !cli.is_strict() {
                    fs::remove_file(&dst_path).await?;
                }

                copy_file(
                    &src_path,
                    &dst_path,
                    preserve,
                    cli.is_verify(),
                    cli.is_resume(),
                    cli.is_strict(),
                    cli.is_append(),
                    cli.get_reflink_mode(),
                    cli.get_sparse_mode(),
                    test_mode.clone(),
                    &callback,
                )
                .await?;
            }
        }

        // Set the attributes of the target directory (if needed)
        if preserve && !cli.is_dry_run() {
            set_dir_attributes(src, &new_dst).await?;
        }
    } else if src.is_dir() {
        return Err(BcmrError::InvalidInput(format!(
            "Source '{}' is a directory. Use -r flag for recursive copy.",
            src.display()
        )));
    } else {
        return Err(BcmrError::SourceNotFound(src.to_path_buf()));
    }

    Ok(())
}

async fn set_dir_attributes(src: &Path, dst: &Path) -> std::result::Result<(), BcmrError> {
    let src_metadata = src.metadata()?;
    let permissions = src_metadata.permissions();
    tokio::fs::set_permissions(dst, permissions).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let atime = filetime::FileTime::from_unix_time(src_metadata.atime(), 0);
        let mtime = filetime::FileTime::from_unix_time(src_metadata.mtime(), 0);
        filetime::set_file_times(dst, atime, mtime)?;
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if let (Ok(atime), Ok(mtime)) = (
            src_metadata.last_access_time().try_into(),
            src_metadata.last_write_time().try_into(),
        ) {
            let atime = filetime::FileTime::from_windows_file_time(atime);
            let mtime = filetime::FileTime::from_windows_file_time(mtime);
            filetime::set_file_times(dst, atime, mtime)?;
        }
    }
    Ok(())
}

// Add to imports if not present. But create::config::CONFIG is needed.
// Add `use crate::config::CONFIG;` to top of file? No, we need to import it.
// Let's modify the imports first in a separate call if needed, or assume it's available?
// Better to import it inside the function or make sure it's imported.
// Let's assume we need to import it.
// Better to update imports first.

async fn copy_file<F>(
    src: &Path,
    dst: &Path,
    preserve: bool,
    verify: bool,
    resume: bool,
    strict: bool,
    append: bool, // Fixed duplicate
    reflink_arg: Option<String>,
    sparse_mode: SparseMode,
    test_mode: TestMode,
    callback: &ProgressCallback<F>,
) -> std::result::Result<(), BcmrError>
where
    F: Fn(u64),
{
    let file_size = src.metadata()?.len();
    let file_name = src
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    (callback.on_new_file)(&file_name, file_size);

    // Reflink: CLI > Config

    let config_reflink = &crate::config::CONFIG.copy.reflink;
    
    // Action: (try, fail_on_err)
    // - CLI: force=(T,T), disable=(F,F), auto=(T,F)
    // - Cfg: never/disable=(F,F), *=(T,F)
    let (try_reflink, fail_on_error) = if let Some(mode) = reflink_arg {
         match mode.to_lowercase().as_str() {
             "force" => (true, true),
             "disable" => (false, false),
             "auto" => (true, false),
             _ => {
                 return Err(BcmrError::InvalidInput(format!("Invalid reflink mode '{}'. Supported modes: force, disable, auto.", mode)));
             }
         }
    } else {
        match config_reflink.to_lowercase().as_str() {
            "never" | "disable" => (false, false),
            _ => (true, false),
        }
    };

    // Try reflink if requested
    if try_reflink {
        let src_path = src.to_path_buf();
        let dst_path = dst.to_path_buf();
        let result = tokio::task::spawn_blocking(move || reflink_copy::reflink(&src_path, &dst_path))
            .await?;
        
        match result {
            Ok(_) => {
                // Reflink successful!
                (callback.callback)(file_size);
                return Ok(());
            }
            Err(e) => {
                if fail_on_error {
                   return Err(BcmrError::Reflink(format!("Reflink failed (forced): {}", e)));
                }
            }
        }
    }

    let mut start_offset = 0;
    let mut file_flags = fs::OpenOptions::new();
    file_flags.write(true);

    if (resume || append || strict) && dst.exists() {
        let dst_len = dst.metadata()?.len();
        
        let should_resume = if strict {
            // STRICT: Hash check
            if dst_len == file_size {
                 let src_path = src.to_path_buf();
                 let dst_path = dst.to_path_buf();
                 let src_hash = tokio::task::spawn_blocking(move || checksum::calculate_hash(&src_path)).await??;
                 let dst_hash = tokio::task::spawn_blocking(move || checksum::calculate_hash(&dst_path)).await??;
                 
                 if src_hash == dst_hash {
                     (callback.callback)(file_size);
                     return Ok(());
                 }
                 false 
            } else if dst_len < file_size {
                 let src_path = src.to_path_buf();
                 let dst_path = dst.to_path_buf();
                 let limit = dst_len;
                 let dst_hash = tokio::task::spawn_blocking(move || checksum::calculate_hash(&dst_path)).await??;
                 let src_partial_hash = tokio::task::spawn_blocking(move || checksum::calculate_partial_hash(&src_path, limit)).await??;
                 
                 dst_hash == src_partial_hash 
            } else {
                 false
            }
        } else if append {
            // APPEND: Size check (ignore mtime)
            // dst == src -> Skip
            // dst < src -> Append
            // dst > src -> Overwrite
            if dst_len == file_size {
                (callback.callback)(file_size);
                return Ok(());
            } else if dst_len < file_size {
                // File smaller -> Append
                true
            } else {
                // File larger -> Overwrite
                false
            }
        } else {
            // DEFAULT: Mtime + Size
            let src_mtime = src.metadata()?.modified()?;
            let dst_mtime = dst.metadata()?.modified()?;

            if src_mtime != dst_mtime {
                false
            } else {
                if dst_len == file_size {
                    (callback.callback)(file_size);
                    return Ok(());
                } else if dst_len < file_size {
                    // Partial match -> Append
                    true
                } else {
                    // dst > src -> Overwrite
                    false 
                }
            }
        };

        if should_resume {
             start_offset = dst_len;
             file_flags.append(true);
             (callback.callback)(start_offset);
        } else {
             start_offset = 0;
             file_flags.create(true).truncate(true);
        }

    } else {
        file_flags.create(true).truncate(true);
    }

    let mut src_file = File::open(src).await?;
    let mut dst_file = file_flags.open(dst).await?;

    if start_offset > 0 {
        src_file.seek(SeekFrom::Start(start_offset)).await?;
    }

    let mut buffer = vec![0; 1024 * 1024]; // 1MB buffer

    match test_mode {
        TestMode::Delay(ms) => loop {
            let n = src_file.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            dst_file.write_all(&buffer[..n]).await?;
            (callback.callback)(n as u64);
            tokio::time::sleep(Duration::from_millis(ms)).await;
        },
        TestMode::SpeedLimit(bps) => {
            let chunk_size = bps.min(buffer.len() as u64);
            let mut start_time = Instant::now();

            loop {
                // Read a chunk
                let n = src_file.read(&mut buffer[..chunk_size as usize]).await?;
                if n == 0 {
                    break;
                }

                dst_file.write_all(&buffer[..n]).await?;

                let elapsed = start_time.elapsed();
                let target_duration = Duration::from_secs_f64(n as f64 / bps as f64);
                if elapsed < target_duration {
                    tokio::time::sleep(target_duration - elapsed).await;
                    start_time = Instant::now();
                }

                (callback.callback)(n as u64);
            }
        }
        TestMode::None => {
            let mut pending_hole = 0u64;

            loop {
                let n = src_file.read(&mut buffer).await?;
                if n == 0 {
                    break;
                }

                let is_zeros = match sparse_mode {
                    SparseMode::Always => buffer[..n].iter().all(|&b| b == 0),
                    SparseMode::Auto => n >= 4096 && buffer[..n].iter().all(|&b| b == 0),
                    SparseMode::Never => false,
                };

                if is_zeros {
                    pending_hole += n as u64;
                } else {
                    if pending_hole > 0 {
                        dst_file.seek(SeekFrom::Current(pending_hole as i64)).await?;
                        pending_hole = 0;
                    }
                    dst_file.write_all(&buffer[..n]).await?;
                }
                (callback.callback)(n as u64);
            }

            if pending_hole > 0 {
                // If the file ends with a hole, extend the file size
                let current_pos = dst_file.stream_position().await?;
                dst_file.set_len(current_pos + pending_hole).await?;
            }
        },
    }

    if preserve {
        set_dir_attributes(src, dst).await?;
    }

    // Verify if requested AND not already verified (e.g. strict match skipped)
    if verify {
        let src_path = src.to_path_buf();
        let dst_path = dst.to_path_buf();
        let src_hash = tokio::task::spawn_blocking(move || checksum::calculate_hash(&src_path)).await??;
        let dst_hash = tokio::task::spawn_blocking(move || checksum::calculate_hash(&dst_path)).await??;

        if src_hash != dst_hash {
            return Err(BcmrError::VerificationError(dst.to_path_buf()));
        }
    }

    Ok(())
}
