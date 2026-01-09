use crate::cli::{Commands, TestMode};
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::fs::{self, File};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use walkdir::WalkDir;

pub struct FileToOverwrite {
    pub path: PathBuf,
    pub is_dir: bool,
}

pub async fn check_overwrites(
    sources: &[PathBuf],
    dst: &Path,
    recursive: bool,
    cli: &Commands,
    excludes: &[regex::Regex],
) -> Result<Vec<FileToOverwrite>> {
    let mut files_to_overwrite = Vec::new();

    // If we have multiple sources, dst MUST be a directory (or we bail, but validation might happen earlier)
    // Actually, cp file1 file2 file3 -> fails. cp file1 file2 dir -> works.
    let dst_is_dir = dst.exists() && dst.is_dir();
    
    // If multiple sources and dest is not a dir (and exists), we can't proceed really, but let's check basic logic.
    if sources.len() > 1 && !dst_is_dir {
        // We will fail later, but here we just check overwrites if we were to proceed?
        // Let's assume the caller ensures validity or we check it here.
        // But for overwrite check, we need to know the target path.
    }

    for src in sources {
         if cli.should_exclude(&src.to_string_lossy(), excludes) {
            continue;
        }

        if src.is_file() {
            let dst_path = if dst_is_dir {
                dst.join(src.file_name().ok_or_else(|| anyhow::anyhow!("Invalid source file name"))?)
            } else {
                // If single source and dest is file/doesn't exist
                dst.to_path_buf()
            };

            if dst_path.exists() && !cli.should_exclude(&dst_path.to_string_lossy(), excludes) {
                files_to_overwrite.push(FileToOverwrite {
                    path: dst_path,
                    is_dir: false,
                });
            }
        } else if recursive && src.is_dir() {
            let src_name = src.file_name().ok_or_else(|| anyhow::anyhow!("Invalid source directory name"))?;
            let new_dst = if dst_is_dir {
                dst.join(src_name)
            } else {
                dst.to_path_buf()
            };

            // If the target directory exists, check for files that will be overwritten
            if new_dst.exists() {
                for entry in WalkDir::new(src).min_depth(1) {
                    let entry = entry?;
                    let path = entry.path();

                    if cli.should_exclude(&path.to_string_lossy(), excludes) {
                        continue;
                    }

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

pub async fn get_total_size(
    sources: &[PathBuf],
    recursive: bool,
    cli: &Commands,
    excludes: &[regex::Regex],
) -> Result<u64> {
    let mut total_size = 0;

    for src in sources {
        if cli.should_exclude(&src.to_string_lossy(), excludes) {
            continue;
        }

        if recursive && src.is_dir() {
            for entry in WalkDir::new(src).min_depth(1) {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() {
                    if !cli.should_exclude(&path.to_string_lossy(), excludes) {
                        total_size += entry.metadata()?.len();
                    }
                }
            }
        } else if src.is_file() {
            total_size += src.metadata()?.len();
        }
    }

    Ok(total_size)
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
    if cli.should_exclude(&src.to_string_lossy(), excludes) {
        return Ok(());
    }

    if src.is_file() {
        let dst_path = if dst.is_dir() {
             dst.join(src.file_name().ok_or_else(|| anyhow::anyhow!("Invalid source file name"))?)
        } else {
             dst.to_path_buf()
        };

        // For files, only check when the target file exists
        if dst_path.exists() && !cli.is_force() {
            bail!("Destination '{}' already exists. Use -f to force overwrite.", dst_path.display());
        }

        if cli.is_dry_run() {
            println!("Would copy '{}' to '{}'", src.display(), dst_path.display());
            return Ok(());
        }

        if dst_path.exists() && cli.is_force() {
            fs::remove_file(&dst_path).await?;
        }

        copy_file(src, &dst_path, preserve, test_mode, &callback).await?;
    } else if recursive && src.is_dir() {
        let src_dir_name = src.file_name().ok_or_else(|| anyhow::anyhow!("Invalid source directory name"))?;
        let new_dst = if dst.is_dir() {
            dst.join(src_dir_name)
        } else {
            dst.to_path_buf()
        };

        if cli.is_dry_run() {
            println!("Would copy directory '{}' to '{}'", src.display(), new_dst.display());
            // In dry run we also iterate to show what files would be copied?
            // "Would copy ..." is implemented below for recursive files if we want verbose dry run.
            // For now, let's just log the directory copy and walk to log files?
            // But we simulate the walk.
        }

        // Create the target directory (if it does not exist)
        if !new_dst.exists() && !cli.is_dry_run() {
            fs::create_dir_all(&new_dst).await?;
        }

        // Collect files and directories to copy
        // We use WalkDir even in dry-run to show what would happen or to at least process excludes.
        // But for dry-run of a huge dir, maybe just saying "Would copy directory recursively" is enough?
        // User requirements: "Dry run... preview operations". Detailed is better.
        
        // Note: WalkDir returns entries in current dir.
        let mut files_to_copy = Vec::new();
        for entry in WalkDir::new(src).min_depth(1) {
            let entry = entry?;
            let path = entry.path();

            if cli.should_exclude(&path.to_string_lossy(), excludes) {
                continue;
            }

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
            if dst_path.exists() && !cli.is_force() {
                bail!("Destination '{}' already exists. Use -f to force overwrite.", dst_path.display());
            }

            if cli.is_dry_run() {
                 println!("Would copy '{}' to '{}'", src_path.display(), dst_path.display());
                 continue;
            }

            if dst_path.exists() && cli.is_force() {
                fs::remove_file(&dst_path).await?;
            }

            copy_file(&src_path, &dst_path, preserve, test_mode.clone(), &callback).await?;
        }

        // Set the attributes of the target directory (if needed)
        if preserve && !cli.is_dry_run() {
            set_dir_attributes(src, &new_dst).await?;
        }
    } else if src.is_dir() {
        bail!("Source '{}' is a directory. Use -r flag for recursive copy.", src.display());
    } else {
        bail!("Source '{}' does not exist or is not accessible.", src.display());
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

    let mut src_file = File::open(src).await?;
    let mut dst_file = File::create(dst).await?;

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

    Ok(())
}