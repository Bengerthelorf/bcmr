use std::io;
use std::path::Path;

/// macOS `fsync()` only reaches the drive cache; `F_FULLFSYNC` forces a
/// controller-level flush.
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

pub async fn durable_sync_async(file: &tokio::fs::File) -> io::Result<()> {
    let std_file = file.try_clone().await?.into_std().await;
    tokio::task::spawn_blocking(move || durable_sync(&std_file))
        .await
        .map_err(io::Error::other)?
}

/// Required after rename() on XFS; ext4's `auto_da_alloc` is the exception.
pub fn fsync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = durable_sync(&d);
    }
}

pub async fn fsync_dir_async(dir: &Path) {
    let dir = dir.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || fsync_dir(&dir)).await;
}

#[cfg(unix)]
pub fn get_inode(path: &Path) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(path.metadata()?.ino())
}

#[cfg(not(unix))]
pub fn get_inode(_path: &Path) -> io::Result<u64> {
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_durable_sync_on_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"hello").unwrap();
        durable_sync(&f).unwrap();
    }

    #[test]
    fn test_fsync_dir_on_valid_dir() {
        let dir = tempfile::tempdir().unwrap();
        fsync_dir(dir.path());
    }

    #[test]
    fn test_fsync_dir_on_nonexistent() {
        fsync_dir(Path::new("/nonexistent/dir/abc"));
    }

    #[cfg(unix)]
    #[test]
    fn test_get_inode_returns_nonzero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inode_test.bin");
        std::fs::write(&path, b"data").unwrap();
        let inode = get_inode(&path).unwrap();
        assert!(inode > 0);
    }

    #[cfg(unix)]
    #[test]
    fn test_get_inode_different_files() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        std::fs::write(&a, b"aaa").unwrap();
        std::fs::write(&b, b"bbb").unwrap();
        let ia = get_inode(&a).unwrap();
        let ib = get_inode(&b).unwrap();
        assert_ne!(ia, ib);
    }

    #[cfg(unix)]
    #[test]
    fn test_get_inode_nonexistent() {
        assert!(get_inode(Path::new("/nonexistent/file")).is_err());
    }
}
