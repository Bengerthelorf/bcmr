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

#[cfg(unix)]
fn sync_directory_with<SyncFn>(directory: &std::fs::File, sync: SyncFn) -> io::Result<()>
where
    SyncFn: FnOnce(&std::fs::File) -> io::Result<()>,
{
    match sync(directory) {
        Ok(()) => Ok(()),
        Err(error)
            if error.raw_os_error().is_some_and(|errno| {
                [libc::EINVAL, libc::ENOTSUP, libc::EOPNOTSUPP].contains(&errno)
            }) =>
        {
            // Losing the directory-entry flush can lose only resume progress:
            // restart revalidates every published source/destination block.
            Ok(())
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
pub fn durable_sync_dir(dir: &Path) -> io::Result<()> {
    let directory = std::fs::File::open(dir)?;
    sync_directory_with(&directory, std::fs::File::sync_all)
}

#[cfg(not(unix))]
pub fn durable_sync_dir(_dir: &Path) -> io::Result<()> {
    // std::fs cannot open directory handles on Windows. The session file
    // itself is already flushed before rename; directory flushing remains
    // best-effort on that platform.
    Ok(())
}

pub fn fsync_dir(dir: &Path) {
    let _ = durable_sync_dir(dir);
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

    #[cfg(unix)]
    #[test]
    fn directory_sync_helper_accepts_success() {
        let dir = tempfile::tempdir().unwrap();
        let directory = std::fs::File::open(dir.path()).unwrap();
        let called = std::cell::Cell::new(false);

        sync_directory_with(&directory, |_| {
            called.set(true);
            Ok(())
        })
        .unwrap();

        assert!(called.get(), "the injected directory sync must be called");
    }

    #[cfg(unix)]
    #[test]
    fn directory_sync_helper_downgrades_only_unsupported_errno() {
        let dir = tempfile::tempdir().unwrap();
        let directory = std::fs::File::open(dir.path()).unwrap();

        for errno in [libc::EINVAL, libc::ENOTSUP, libc::EOPNOTSUPP] {
            sync_directory_with(&directory, |_| Err(io::Error::from_raw_os_error(errno)))
                .expect("unsupported directory fsync is a safe progress-only downgrade");
        }
    }

    #[cfg(unix)]
    #[test]
    fn directory_sync_helper_propagates_genuine_storage_errors() {
        let dir = tempfile::tempdir().unwrap();
        let directory = std::fs::File::open(dir.path()).unwrap();

        for errno in [libc::EIO, libc::ENOSPC, libc::EROFS, libc::EACCES] {
            let error =
                sync_directory_with(&directory, |_| Err(io::Error::from_raw_os_error(errno)))
                    .expect_err("genuine directory sync failures must remain visible");
            assert_eq!(error.raw_os_error(), Some(errno));
        }
    }

    #[test]
    fn test_fsync_dir_on_valid_dir() {
        let dir = tempfile::tempdir().unwrap();
        durable_sync_dir(dir.path()).unwrap();
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
