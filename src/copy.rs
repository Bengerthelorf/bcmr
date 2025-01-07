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

pub async fn check_overwrites(src: &Path, dst: &Path, recursive: bool, cli: &Commands) -> Result<Vec<FileToOverwrite>> {
    let mut files_to_overwrite = Vec::new();

    if src.is_file() {
        let dst_path = if dst.is_dir() {
            dst.join(src.file_name().ok_or_else(|| anyhow::anyhow!("Invalid source file name"))?)
        } else {
            dst.to_path_buf()
        };

        if dst_path.exists() && !cli.should_exclude(&dst_path.to_string_lossy()) {
            files_to_overwrite.push(FileToOverwrite {
                path: dst_path,
                is_dir: false,
            });
        }
    } else if recursive && src.is_dir() {
        let src_name = src.file_name().ok_or_else(|| anyhow::anyhow!("Invalid source directory name"))?;
        let new_dst = if dst.is_dir() {
            dst.join(src_name)
        } else {
            dst.to_path_buf()
        };

        // If the target directory exists, check for files that will be overwritten
        if new_dst.exists() {
            for entry in WalkDir::new(src).min_depth(1) {
                let entry = entry?;
                let path = entry.path();

                if cli.should_exclude(&path.to_string_lossy()) {
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

    Ok(files_to_overwrite)
}

pub async fn get_total_size(path: &Path, recursive: bool, cli: &Commands) -> Result<u64> {
    let mut total_size = 0;

    if recursive && path.is_dir() {
        for entry in WalkDir::new(path).min_depth(1) {
            let entry = entry?;
            if entry.path().is_file() {
                if !cli.should_exclude(&entry.path().to_string_lossy()) {
                    total_size += entry.metadata()?.len();
                }
            }
        }
    } else if path.is_file() {
        if !cli.should_exclude(&path.to_string_lossy()) {
            total_size = path.metadata()?.len();
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

    if cli.should_exclude(&src.to_string_lossy()) {
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

        // Create the target directory (if it does not exist)
        if !new_dst.exists() {
            fs::create_dir_all(&new_dst).await?;
        }

        // Collect files and directories to copy
        let mut files_to_copy = Vec::new();
        for entry in WalkDir::new(src).min_depth(1) {
            let entry = entry?;
            let path = entry.path();

            if cli.should_exclude(&path.to_string_lossy()) {
                continue;
            }

            let relative_path = path.strip_prefix(src)?;
            let target_path = new_dst.join(relative_path);

            if path.is_dir() {
                if !target_path.exists() {
                    fs::create_dir_all(&target_path).await?;
                }
                if preserve {
                    let src_metadata = path.metadata()?;
                    let permissions = src_metadata.permissions();
                    tokio::fs::set_permissions(&target_path, permissions).await?;

                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        let atime = filetime::FileTime::from_unix_time(src_metadata.atime(), 0);
                        let mtime = filetime::FileTime::from_unix_time(src_metadata.mtime(), 0);
                        filetime::set_file_times(&target_path, atime, mtime)?;
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
                            filetime::set_file_times(&target_path, atime, mtime)?;
                        }
                    }
                }
            } else if path.is_file() {
                files_to_copy.push((path.to_path_buf(), target_path));
            }
        }

        // Copy files
        for (src_path, dst_path) in files_to_copy {
            if let Some(parent) = dst_path.parent() {
                if !parent.exists() {
                    fs::create_dir_all(parent).await?;
                }
            }

            // Check each file to see if it needs to be overwritten
            if dst_path.exists() && !cli.is_force() {
                bail!("Destination '{}' already exists. Use -f to force overwrite.", dst_path.display());
            }

            if dst_path.exists() && cli.is_force() {
                fs::remove_file(&dst_path).await?;
            }

            copy_file(&src_path, &dst_path, preserve, test_mode.clone(), &callback).await?;
        }

        // Set the attributes of the target directory (if needed)
        if preserve {
            let src_metadata = src.metadata()?;
            let permissions = src_metadata.permissions();
            tokio::fs::set_permissions(&new_dst, permissions).await?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let atime = filetime::FileTime::from_unix_time(src_metadata.atime(), 0);
                let mtime = filetime::FileTime::from_unix_time(src_metadata.mtime(), 0);
                filetime::set_file_times(&new_dst, atime, mtime)?;
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
                    filetime::set_file_times(&new_dst, atime, mtime)?;
                }
            }
        }
    } else if src.is_dir() {
        bail!("Source '{}' is a directory. Use -r flag for recursive copy.", src.display());
    } else {
        bail!("Source '{}' does not exist or is not accessible.", src.display());
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
    }

    Ok(())
}