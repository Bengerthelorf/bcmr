use std::io;
use std::path::Path;

/// Durable sync: ensures data is on persistent storage, not just OS cache.
///
/// On macOS, `fsync()` only flushes to the drive's write cache, not to the
/// actual storage medium. `F_FULLFSYNC` issues a full cache flush command
/// to the drive controller, guaranteeing durability. This is what SQLite,
/// RocksDB, and PostgreSQL use on macOS.
///
/// On Linux, `fdatasync()` provides sufficient guarantees with ext4's
/// `data=ordered` mode (the default) and XFS.
#[cfg(target_os = "macos")]
pub fn durable_sync(file: &std::fs::File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let ret = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
pub fn durable_sync(file: &std::fs::File) -> io::Result<()> {
    file.sync_data()
}

/// Async version of durable_sync for tokio::fs::File.
///
/// Extracts the std::fs::File via `try_into_std()`, performs the sync on a
/// blocking thread, then converts back.
pub async fn durable_sync_async(file: &tokio::fs::File) -> io::Result<()> {
    let std_file = file
        .try_clone()
        .await?
        .into_std()
        .await;
    tokio::task::spawn_blocking(move || durable_sync(&std_file)).await
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?
}

/// Fsync a directory to ensure that directory entry changes (renames, creates)
/// are durable. Required after rename() on XFS and other filesystems that don't
/// have ext4's `auto_da_alloc` heuristic.
///
/// This is a best-effort operation: if the directory can't be opened or synced,
/// we silently continue rather than failing the copy.
pub fn fsync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = durable_sync(&d);
    }
}

/// Async version of fsync_dir.
pub async fn fsync_dir_async(dir: &Path) {
    let dir = dir.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || fsync_dir(&dir)).await;
}

/// Get the inode number of a file (Unix only).
/// Returns 0 on non-Unix platforms.
/// Used by session creation for source identity tracking.
#[cfg(unix)]
#[allow(dead_code)]
pub fn get_inode(path: &Path) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(path.metadata()?.ino())
}

#[cfg(not(unix))]
pub fn get_inode(_path: &Path) -> io::Result<u64> {
    Ok(0)
}
