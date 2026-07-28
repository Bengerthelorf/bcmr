use std::io;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

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
    // Transfer publication may apply permissions and extended attributes to
    // the staging inode. `sync_data` is allowed to omit that metadata.
    file.sync_all()
}

pub async fn durable_sync_async(file: &tokio::fs::File) -> io::Result<()> {
    let std_file = file.try_clone().await?.into_std().await;
    tokio::task::spawn_blocking(move || durable_sync(&std_file))
        .await
        .map_err(io::Error::other)?
}

#[cfg(unix)]
fn sync_progress_directory_with<SyncFn>(directory: &std::fs::File, sync: SyncFn) -> io::Result<()>
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
fn sync_directory_strict_with<SyncFn>(directory: &std::fs::File, sync: SyncFn) -> io::Result<()>
where
    SyncFn: FnOnce(&std::fs::File) -> io::Result<()>,
{
    sync(directory)
}

#[cfg(unix)]
pub(crate) fn durable_sync_directory_handle_strict(directory: &std::fs::File) -> io::Result<()> {
    sync_directory_strict_with(directory, std::fs::File::sync_all)
}

#[cfg(unix)]
pub fn durable_sync_dir(dir: &Path) -> io::Result<()> {
    let directory = std::fs::File::open(dir)?;
    sync_progress_directory_with(&directory, std::fs::File::sync_all)
}

#[cfg(not(unix))]
pub fn durable_sync_dir(_dir: &Path) -> io::Result<()> {
    // std::fs cannot open directory handles on Windows. The session file
    // itself is already flushed before rename; directory flushing remains
    // best-effort on that platform.
    Ok(())
}

#[cfg(unix)]
fn resolve_existing_directory_prefix(path: &Path) -> io::Result<PathBuf> {
    let mut existing = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut missing = Vec::new();

    loop {
        match std::fs::symlink_metadata(&existing) {
            Ok(_) => {
                // Resolve every symlink in the existing prefix before any
                // mutation. The creation loop below then operates only on
                // this canonical path and never re-traverses the link.
                let mut resolved = existing.canonicalize()?;
                for component in missing.into_iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let Some(component) = existing.file_name() else {
                    return Err(error);
                };
                missing.push(component.to_os_string());
                if !existing.pop() {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
pub fn create_dir_all_durable(path: &Path) -> io::Result<()> {
    let path = resolve_existing_directory_prefix(path)?;
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if current.as_os_str().is_empty() {
            continue;
        }

        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
                continue;
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "durable directory path '{}' contains a non-directory component",
                        current.display()
                    ),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        match std::fs::create_dir(&current) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(&current)?;
                if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "durable directory path '{}' was replaced during creation",
                            current.display()
                        ),
                    ));
                }
            }
            Err(error) => return Err(error),
        }

        let created = std::fs::File::open(&current)?;
        durable_sync_directory_handle_strict(&created)?;
        let parent = current
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = std::fs::File::open(parent)?;
        durable_sync_directory_handle_strict(&parent)?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn create_dir_all_durable(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "durable recursive directory creation is not implemented on this platform",
    ))
}

pub async fn create_dir_all_durable_async(path: &Path) -> io::Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || create_dir_all_durable(&path))
        .await
        .map_err(io::Error::other)?
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

        sync_progress_directory_with(&directory, |_| {
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
            sync_progress_directory_with(&directory, |_| Err(io::Error::from_raw_os_error(errno)))
                .expect("unsupported directory fsync is a safe progress-only downgrade");
        }
    }

    #[cfg(unix)]
    #[test]
    fn directory_sync_helper_propagates_genuine_storage_errors() {
        let dir = tempfile::tempdir().unwrap();
        let directory = std::fs::File::open(dir.path()).unwrap();

        for errno in [libc::EIO, libc::ENOSPC, libc::EROFS, libc::EACCES] {
            let error = sync_progress_directory_with(&directory, |_| {
                Err(io::Error::from_raw_os_error(errno))
            })
            .expect_err("genuine directory sync failures must remain visible");
            assert_eq!(error.raw_os_error(), Some(errno));
        }
    }

    #[cfg(unix)]
    #[test]
    fn strict_directory_sync_never_downgrades_unsupported_errno() {
        let dir = tempfile::tempdir().unwrap();
        let directory = std::fs::File::open(dir.path()).unwrap();

        for errno in [libc::EINVAL, libc::ENOTSUP, libc::EOPNOTSUPP] {
            let error = sync_directory_strict_with(&directory, |_| {
                Err(io::Error::from_raw_os_error(errno))
            })
            .expect_err("a requested durable namespace publish must fail closed");
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
    fn durable_recursive_creation_builds_the_complete_path() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("one").join("two").join("three");

        create_dir_all_durable(&nested).unwrap();

        assert!(nested.is_dir());
        create_dir_all_durable(&nested).expect("an existing durable path is idempotent");
    }

    #[cfg(unix)]
    #[test]
    fn durable_recursive_creation_supports_an_existing_symlink_prefix() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let link = dir.path().join("link");
        symlink(&outside, &link).unwrap();

        create_dir_all_durable(&link.join("child"))
            .expect("a pre-existing symlinked directory prefix should resolve once");
        assert!(outside.join("child").is_dir());
        assert!(
            link.symlink_metadata().unwrap().file_type().is_symlink(),
            "resolving the existing prefix must not replace the link itself"
        );
    }

    #[cfg(unix)]
    #[test]
    fn durable_recursive_creation_rejects_a_broken_symlink_prefix() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let link = dir.path().join("link");
        symlink(&missing, &link).unwrap();

        let error = create_dir_all_durable(&link.join("child"))
            .expect_err("a broken existing prefix cannot be resolved safely");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!missing.exists());
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
