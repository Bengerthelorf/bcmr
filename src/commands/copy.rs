use crate::cli::{Commands, TestMode};
use crate::core::traversal;
use crate::core::checksum;

use anyhow::{bail, Result};
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
) -> Result<Vec<FileToOverwrite>> {
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
                        .ok_or_else(|| anyhow::anyhow!("Invalid source file name"))?,
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
                .ok_or_else(|| anyhow::anyhow!("Invalid source directory name"))?;
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
) -> Result<u64> {
    let mut total_size = 0;

    for src in sources {
        if traversal::is_excluded(&src, &excludes) {
            continue;
        }

        if recursive && src.is_dir() {
            for entry in traversal::walk(&src, true, false, 1, &excludes) {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() {
                    total_size += entry.metadata()?.len();
                }
            }
        } else if src.is_file() {
            total_size += src.metadata()?.len();
        }
    }

    Ok(total_size)
}

pub async fn get_total_size(
    sources: &[PathBuf],
    recursive: bool,
    _cli: &Commands,
    excludes: &[regex::Regex],
) -> Result<u64> {
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
) -> Result<()>
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
                    .ok_or_else(|| anyhow::anyhow!("Invalid source file name"))?,
            )
        } else {
            dst.to_path_buf()
        };

        // For files, only check when the target file exists
        if dst_path.exists() && !cli.is_force() && !cli.is_resume() {
            bail!(
                "Destination '{}' already exists. Use -f to force overwrite.",
                dst_path.display()
            );
        }

        if cli.is_dry_run() {
            println!("Would copy '{}' to '{}'", src.display(), dst_path.display());
            return Ok(());
        }

        if dst_path.exists() && cli.is_force() {
            fs::remove_file(&dst_path).await?;
        }

        copy_file(src, &dst_path, preserve, cli.is_verify(), cli.is_resume(), cli.is_strict(), test_mode, &callback).await?;
    } else if recursive && src.is_dir() {
        let src_dir_name = src
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Invalid source directory name"))?;
        let new_dst = if dst.is_dir() {
            dst.join(src_dir_name)
        } else {
            dst.to_path_buf()
        };

        if cli.is_dry_run() {
            println!(
                "Would copy directory '{}' to '{}'",
                src.display(),
                new_dst.display()
            );
        }

        // Create the target directory (if it does not exist)
        if !new_dst.exists() && !cli.is_dry_run() {
            fs::create_dir_all(&new_dst).await?;
        }

        // Collect files and directories to copy
        let mut files_to_copy = Vec::new();
        // Use unified traversal
        for entry in traversal::walk(src, true, false, 1, excludes) {
            let entry = entry?;
            let path = entry.path();
            
            // Exclude check is handled by traversal::walk now!

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
                    println!("Would create directory '{}'", target_path.display());
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
            if dst_path.exists() && !cli.is_force() && !cli.is_resume() {
                bail!(
                    "Destination '{}' already exists. Use -f to force overwrite.",
                    dst_path.display()
                );
            }

            if cli.is_dry_run() {
                println!(
                    "Would copy '{}' to '{}'",
                    src_path.display(),
                    dst_path.display()
                );
                continue;
            }

            if dst_path.exists() && cli.is_force() {
                fs::remove_file(&dst_path).await?;
            }

            copy_file(&src_path, &dst_path, preserve, cli.is_verify(), cli.is_resume(), cli.is_strict(), test_mode.clone(), &callback).await?;
        }

        // Set the attributes of the target directory (if needed)
        if preserve && !cli.is_dry_run() {
            set_dir_attributes(src, &new_dst).await?;
        }
    } else if src.is_dir() {
        bail!(
            "Source '{}' is a directory. Use -r flag for recursive copy.",
            src.display()
        );
    } else {
        bail!(
            "Source '{}' does not exist or is not accessible.",
            src.display()
        );
    }

    Ok(())
}

async fn set_dir_attributes(src: &Path, dst: &Path) -> Result<()> {
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

async fn copy_file<F>(
    src: &Path,
    dst: &Path,
    preserve: bool,
    verify: bool,
    resume: bool,
    strict: bool,
    test_mode: TestMode,
    callback: &ProgressCallback<F>,
) -> Result<()>
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

    let mut start_offset = 0;
    let mut file_flags = fs::OpenOptions::new();
    file_flags.write(true);

    if resume && dst.exists() {
        let dst_len = dst.metadata()?.len();
        
        let should_resume = if strict {
            // STRICT MODE: Hash check (original logic)
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
        } else {
            // DEFAULT MODE: Size + Mtime check
            let src_mtime = src.metadata()?.modified()?;
            let dst_mtime = dst.metadata()?.modified()?;

            if src_mtime != dst_mtime {
                // Modified times differ -> assume files are different -> Overwrite
                false
            } else {
                // Mtimes match -> check size
                if dst_len == file_size {
                    // Full match -> Skip
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
             // Match! Resume
             start_offset = dst_len;
             file_flags.append(true);
             // Update progress bar to current state
             (callback.callback)(start_offset);
        } else {
             // Mismatch, overwrite
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
        TestMode::None => loop {
            let n = src_file.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            dst_file.write_all(&buffer[..n]).await?;
            (callback.callback)(n as u64);
        },
    }

    if preserve {
        set_dir_attributes(src, dst).await?;
    }

    // Only run full verification if specifically requested AND we didn't just verify it fully above
    // If we resumed (start_offset > 0) or overwrote, we might want to verify.
    // Optimization: If we just verified full hash above (dst_len == file_size case), we returned early.
    // So here we are in a case where we wrote/appended something.
    if verify {
        let src_path = src.to_path_buf();
        let dst_path = dst.to_path_buf();
        let src_hash = tokio::task::spawn_blocking(move || checksum::calculate_hash(&src_path)).await??;
        let dst_hash = tokio::task::spawn_blocking(move || checksum::calculate_hash(&dst_path)).await??;

        if src_hash != dst_hash {
            bail!("Verification failed: Hashes do not match for '{}'", dst.display());
        }
    }

    Ok(())
}
