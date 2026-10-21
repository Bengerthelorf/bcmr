//! Read checked LIST targets without reopening a mutable symlink spelling.
use anyhow::Result;
use std::ffi::OsString;
use std::path::Path;

pub(super) struct Observation {
    pub size: u64,
    pub mtime: i64,
    pub is_dir: bool,
    pub children: Vec<OsString>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use unix::BoundListing;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
    use super::*;
    use anyhow::bail;
    use std::ffi::{CStr, CString, OsStr};
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::{Component, PathBuf};

    pub(in crate::commands::serve) struct BoundListing {
        root: File,
        path: PathBuf,
    }

    fn search_flags() -> i32 {
        #[cfg(target_os = "linux")]
        let access = libc::O_PATH;
        #[cfg(target_os = "macos")]
        let access = libc::O_SEARCH;
        access | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
    }

    fn open_at(parent: RawFd, name: &OsStr, flags: i32) -> Result<File> {
        let name = CString::new(name.as_bytes())?;
        let descriptor = unsafe { libc::openat(parent, name.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }

    struct DirectoryStream(*mut libc::DIR);

    impl Drop for DirectoryStream {
        fn drop(&mut self) {
            unsafe { libc::closedir(self.0) };
        }
    }

    fn children(directory: File) -> Result<Vec<OsString>> {
        let descriptor = directory.into_raw_fd();
        let stream = unsafe { libc::fdopendir(descriptor) };
        if stream.is_null() {
            let error = std::io::Error::last_os_error();
            unsafe { libc::close(descriptor) };
            return Err(error.into());
        }
        let stream = DirectoryStream(stream);
        let mut result = Vec::new();
        loop {
            #[cfg(target_os = "linux")]
            unsafe {
                *libc::__errno_location() = 0
            };
            #[cfg(target_os = "macos")]
            unsafe {
                *libc::__error() = 0
            };
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(0) {
                    return Err(error.into());
                }
                break;
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                result.push(OsString::from_vec(name.to_vec()));
            }
        }
        result.sort();
        Ok(result)
    }

    impl BoundListing {
        pub(in crate::commands::serve) fn new(root: &Path) -> Result<Self> {
            Ok(Self {
                root: open_at(libc::AT_FDCWD, root.as_os_str(), search_flags())?,
                path: root.to_owned(),
            })
        }

        pub(in crate::commands::serve) fn observe(&self, physical: &Path) -> Result<Observation> {
            let relative = physical.strip_prefix(&self.path)?;
            let components = relative
                .components()
                .map(|component| match component {
                    Component::Normal(name) => Ok(name),
                    _ => anyhow::bail!("invalid canonical LIST component"),
                })
                .collect::<Result<Vec<_>>>()?;
            let (last, ancestors) = components
                .split_last()
                .map(|(last, ancestors)| (*last, ancestors))
                .unwrap_or((OsStr::new("."), &[]));
            let mut parent = self.root.try_clone()?;
            for component in ancestors {
                parent = open_at(parent.as_raw_fd(), component, search_flags())?;
            }
            let name = CString::new(last.as_bytes())?;
            let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    metadata.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut metadata = unsafe { metadata.assume_init() };
            let is_dir = metadata.st_mode & libc::S_IFMT == libc::S_IFDIR;
            let names = if is_dir {
                let directory = open_at(
                    parent.as_raw_fd(),
                    last,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )?;
                if unsafe { libc::fstat(directory.as_raw_fd(), &mut metadata) } != 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                children(directory)?
            } else if metadata.st_mode & libc::S_IFMT == libc::S_IFREG {
                Vec::new()
            } else {
                bail!(
                    "LIST target changed or is unsupported: '{}'",
                    physical.display()
                );
            };
            #[allow(clippy::unnecessary_cast)]
            let mtime = metadata.st_mtime as i64;
            Ok(Observation {
                size: u64::try_from(metadata.st_size)?,
                mtime,
                is_dir,
                children: names,
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::BoundListing;
        use std::os::unix::fs::PermissionsExt;

        #[test]
        fn file_metadata_requires_only_search_on_ancestors_and_no_file_read_access() {
            let directory = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(directory.path()).unwrap();
            let ancestor = root.join("search-only");
            std::fs::create_dir(&ancestor).unwrap();
            let file = ancestor.join("unreadable");
            std::fs::write(&file, b"metadata only").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o0)).unwrap();
            std::fs::set_permissions(&ancestor, std::fs::Permissions::from_mode(0o111)).unwrap();
            let observation = BoundListing::new(&root).unwrap().observe(&file);
            std::fs::set_permissions(&ancestor, std::fs::Permissions::from_mode(0o700)).unwrap();
            let observation = observation.unwrap();
            assert_eq!(observation.size, 13);
            assert!(!observation.is_dir);
        }

        #[test]
        fn canonical_ancestor_replaced_with_outside_link_is_not_followed() {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join("jail");
            let ancestor = root.join("ancestor");
            std::fs::create_dir_all(&ancestor).unwrap();
            let root = std::fs::canonicalize(root).unwrap();
            let checked = root.join("ancestor/file");
            std::fs::write(&checked, b"safe").unwrap();
            let outside = directory.path().join("outside");
            std::fs::create_dir(&outside).unwrap();
            std::fs::write(outside.join("file"), b"outside secret").unwrap();
            let binding = BoundListing::new(&root).unwrap();
            std::fs::rename(&ancestor, root.join("retained")).unwrap();
            std::os::unix::fs::symlink(&outside, &ancestor).unwrap();
            assert!(binding.observe(&checked).is_err());
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) struct BoundListing;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl BoundListing {
    pub(super) fn new(_root: &Path) -> Result<Self> {
        Ok(Self)
    }

    pub(super) fn observe(&self, physical: &Path) -> Result<Observation> {
        let metadata = std::fs::symlink_metadata(physical)?;
        if !metadata.is_dir() && !metadata.is_file() {
            anyhow::bail!(
                "LIST target changed or is unsupported: '{}'",
                physical.display()
            );
        }
        let children = if metadata.is_dir() {
            std::fs::read_dir(physical)?
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect::<std::io::Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        Ok(Observation {
            size: metadata.len(),
            mtime: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs() as i64)
                .unwrap_or(0),
            is_dir: metadata.is_dir(),
            children,
        })
    }
}
