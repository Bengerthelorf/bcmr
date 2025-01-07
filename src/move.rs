use crate::cli::{Commands, TestMode};
use crate::copy;
use anyhow::{Result, bail};
use std::path::Path;
use tokio::fs;
use walkdir::WalkDir;

// Reuse FileToOverwrite from copy module
pub use copy::FileToOverwrite;

// Similar to copy, but will use the same function for checking overwrites
pub async fn check_overwrites(src: &Path, dst: &Path, recursive: bool, cli: &Commands) -> Result<Vec<FileToOverwrite>> {
    copy::check_overwrites(src, dst, recursive, cli).await
}

// Reuse the total size calculation from copy
pub async fn get_total_size(path: &Path, recursive: bool, cli: &Commands) -> Result<u64> {
    copy::get_total_size(path, recursive, cli).await
}

pub async fn move_path<F>(
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
    if cli.should_exclude(&src.to_string_lossy()) {
        return Ok(());
    }

    // First try to move using rename (this works fast if on same filesystem)
    let move_result = if src.is_file() {
        let dst_path = if dst.is_dir() {
            dst.join(src.file_name().ok_or_else(|| anyhow::anyhow!("Invalid source file name"))?)
        } else {
            dst.to_path_buf()
        };

        // For files, check when target exists
        if dst_path.exists() && !cli.is_force() {
            bail!("Destination '{}' already exists. Use -f to force overwrite.", dst_path.display());
        }

        if dst_path.exists() && cli.is_force() {
            fs::remove_file(&dst_path).await?;
        }

        fs::rename(src, &dst_path).await
    } else if recursive && src.is_dir() {
        let src_name = src.file_name().ok_or_else(|| anyhow::anyhow!("Invalid source directory name"))?;
        let new_dst = if dst.is_dir() {
            dst.join(src_name)
        } else {
            dst.to_path_buf()
        };

        // For directories, try renaming the whole directory
        fs::rename(src, &new_dst).await
    } else if src.is_dir() {
        bail!("Source '{}' is a directory. Use -r flag for recursive move.", src.display());
    } else {
        bail!("Source '{}' does not exist or is not accessible.", src.display());
    };

    // If rename failed (e.g., across filesystems), fall back to copy and delete
    if let Err(e) = move_result {
        // Only proceed with copy+delete if it's a cross-device error
        if e.raw_os_error() == Some(libc::EXDEV) {
            // First copy everything
            copy::copy_path(
                src,
                dst,
                recursive,
                preserve,
                test_mode,
                cli,
                progress_callback,
                on_new_file,
            )
            .await?;

            // Then remove the source
            if src.is_file() {
                fs::remove_file(src).await?;
            } else if recursive && src.is_dir() {
                // Remove directory and all its contents
                remove_directory_contents(src).await?;
                fs::remove_dir(src).await?;
            }
        } else {
            // If it's a different error, propagate it
            return Err(e.into());
        }
    }

    Ok(())
}

async fn remove_directory_contents(dir: &Path) -> Result<()> {
    // Remove contents in reverse order (files first, then directories)
    let mut entries: Vec<_> = WalkDir::new(dir)
        .min_depth(1)
        .contents_first(true) // This ensures we process files before directories
        .into_iter()
        .collect::<std::result::Result<_, _>>()?;

    // Sort in reverse order to handle deeper paths first
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.depth()));

    for entry in entries {
        let path = entry.path();
        if path.is_file() {
            fs::remove_file(path).await?;
        } else if path.is_dir() {
            fs::remove_dir(path).await?;
        }
    }

    Ok(())
}