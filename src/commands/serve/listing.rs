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

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
pub(super) use unix::BoundListing;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
mod unix {
    use super::*;
    use anyhow::bail;
    use std::ffi::CStr;
    use std::fs::File;
    use std::os::fd::IntoRawFd;
    use std::os::unix::ffi::OsStringExt;

    pub(in crate::commands::serve) struct BoundListing {
        filesystem: super::super::filesystem::RootFs,
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
            #[cfg(any(target_os = "macos", target_os = "freebsd"))]
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
        #[cfg(test)]
        pub(in crate::commands::serve) fn new(root: &Path) -> Result<Self> {
            Ok(Self::from_filesystem(
                super::super::filesystem::RootFs::new(root)?,
            ))
        }

        pub(in crate::commands::serve) fn from_filesystem(
            filesystem: super::super::filesystem::RootFs,
        ) -> Self {
            Self { filesystem }
        }

        pub(in crate::commands::serve) fn observe(&self, physical: &Path) -> Result<Observation> {
            let mut info = self.filesystem.info(physical)?;
            let names = if info.is_dir {
                let directory = self.filesystem.open_directory(physical)?;
                info = super::super::filesystem::FileInfo::from_metadata(&directory.metadata()?);
                children(directory)?
            } else if info.is_file {
                Vec::new()
            } else {
                bail!(
                    "LIST target changed or is unsupported: '{}'",
                    physical.display()
                );
            };
            Ok(Observation {
                size: info.size,
                mtime: info.mtime,
                is_dir: info.is_dir,
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

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
pub(super) struct BoundListing {
    filesystem: super::filesystem::RootFs,
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
impl BoundListing {
    pub(super) fn from_filesystem(filesystem: super::filesystem::RootFs) -> Self {
        Self { filesystem }
    }

    pub(super) fn observe(&self, physical: &Path) -> Result<Observation> {
        let info = self.filesystem.info(physical)?;
        if !info.is_dir && !info.is_file {
            anyhow::bail!(
                "LIST target changed or is unsupported: '{}'",
                physical.display()
            );
        }
        let children = if info.is_dir {
            std::fs::read_dir(physical)?
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect::<std::io::Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        Ok(Observation {
            size: info.size,
            mtime: info.mtime,
            is_dir: info.is_dir,
            children,
        })
    }
}
