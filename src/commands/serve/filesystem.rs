//! Filesystem capabilities rooted at one directory captured for the session.
//!
//! Canonicalization retains in-root symlink semantics. Unix operations then
//! traverse the resulting components from the retained root, without following
//! any replacement symlink. Portable targets keep the documented path checks.
use anyhow::{bail, Result};
use std::ffi::OsStr;
#[cfg(unix)]
use std::ffi::OsString;
use std::fs::File;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct RootFs {
    path: PathBuf,
    root: Arc<same_file::Handle>,
}

pub(super) struct FileInfo {
    pub size: u64,
    pub mtime: i64,
    pub is_dir: bool,
    pub is_file: bool,
}

impl FileInfo {
    pub(super) fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            size: metadata.len(),
            mtime: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs() as i64)
                .unwrap_or(0),
            is_dir: metadata.is_dir(),
            is_file: metadata.is_file(),
        }
    }
}

impl RootFs {
    pub(super) fn new(root: &Path) -> Result<Self> {
        #[cfg(unix)]
        let handle = {
            use std::os::unix::fs::OpenOptionsExt;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(search_flags())
                .open(root)?;
            same_file::Handle::from_file(file)?
        };
        #[cfg(not(unix))]
        let handle = same_file::Handle::from_path(root)?;
        Ok(Self {
            path: root.to_owned(),
            root: Arc::new(handle),
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn root_file(&self) -> &File {
        self.root.as_file()
    }

    fn components<'a>(&self, physical: &'a Path) -> Result<Vec<&'a OsStr>> {
        physical
            .strip_prefix(&self.path)?
            .components()
            .map(|component| match component {
                Component::Normal(name) => Ok(name),
                _ => bail!("invalid canonical serve path component"),
            })
            .collect()
    }

    #[cfg(unix)]
    fn directory(&self, components: &[&OsStr], create: bool, sync: bool) -> Result<File> {
        use std::os::fd::AsRawFd;
        let mut parent = self.root.as_file().try_clone()?;
        for name in components {
            let next = match open_at(&parent, name, search_flags(), 0) {
                Ok(directory) => directory,
                Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                    let name_c = c_name(name)?;
                    if unsafe { libc::mkdirat(parent.as_raw_fd(), name_c.as_ptr(), 0o777) } != 0 {
                        let error = std::io::Error::last_os_error();
                        if error.kind() != std::io::ErrorKind::AlreadyExists {
                            return Err(error.into());
                        }
                    }
                    let directory = open_at(&parent, name, search_flags(), 0)?;
                    if sync {
                        sync_directory(&directory)?;
                        sync_directory(&parent)?;
                    }
                    directory
                }
                Err(error) => return Err(error.into()),
            };
            parent = next;
        }
        Ok(parent)
    }

    #[cfg(unix)]
    fn parent(&self, physical: &Path, create: bool, sync: bool) -> Result<(File, OsString)> {
        let components = self.components(physical)?;
        let (name, ancestors) = components
            .split_last()
            .map(|(last, ancestors)| (*last, ancestors))
            .unwrap_or((OsStr::new("."), &[]));
        Ok((self.directory(ancestors, create, sync)?, name.to_owned()))
    }

    #[cfg(not(unix))]
    fn portable_path(&self, physical: &Path) -> Result<PathBuf> {
        self.components(physical)?;
        if same_file::Handle::from_path(&self.path)? != *self.root {
            bail!("serve root directory changed");
        }
        let raw = physical
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("serve path is not UTF-8"))?;
        super::validate_path(raw, &self.path)
    }

    pub(super) fn read(&self, physical: &Path) -> Result<File> {
        #[cfg(unix)]
        let file = {
            let (parent, name) = self.parent(physical, false, false)?;
            open_at(
                &parent,
                &name,
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                0,
            )?
        };
        #[cfg(not(unix))]
        let file = File::open(self.portable_path(physical)?)?;
        if !file.metadata()?.is_file() {
            bail!("serve source is not a regular file");
        }
        Ok(file)
    }

    pub(super) fn info(&self, physical: &Path) -> Result<FileInfo> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let (parent, name) = self.parent(physical, false, false)?;
            let name = c_name(&name)?;
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
            let metadata = unsafe { metadata.assume_init() };
            let kind = metadata.st_mode & libc::S_IFMT;
            if kind != libc::S_IFDIR && kind != libc::S_IFREG {
                bail!("serve target changed or is not a regular file/directory");
            }
            #[allow(clippy::unnecessary_cast)]
            return Ok(FileInfo {
                size: u64::try_from(metadata.st_size)?,
                mtime: metadata.st_mtime as i64,
                is_dir: kind == libc::S_IFDIR,
                is_file: kind == libc::S_IFREG,
            });
        }
        #[cfg(not(unix))]
        Ok(FileInfo::from_metadata(&std::fs::metadata(
            self.portable_path(physical)?,
        )?))
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
    pub(super) fn open_directory(&self, physical: &Path) -> Result<File> {
        #[cfg(unix)]
        {
            let components = self.components(physical)?;
            let directory = self.directory(&components, false, false)?;
            Ok(open_at(
                &directory,
                OsStr::new("."),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0,
            )?)
        }
        #[cfg(not(unix))]
        Ok(same_file::Handle::from_path(self.portable_path(physical)?)?
            .as_file()
            .try_clone()?)
    }

    pub(super) fn create_directories(&self, physical: &Path, sync: bool) -> Result<()> {
        #[cfg(unix)]
        {
            self.directory(&self.components(physical)?, true, sync)?;
        }
        #[cfg(not(unix))]
        {
            let path = self.portable_path(physical)?;
            if sync {
                crate::core::io::create_dir_all_durable(&path)?;
            } else {
                std::fs::create_dir_all(path)?;
            }
        }
        Ok(())
    }

    pub(super) fn write(&self, physical: &Path, truncate: bool, sync: bool) -> Result<File> {
        if self.components(physical)?.is_empty() {
            bail!("serve root cannot be a file destination");
        }
        #[cfg(unix)]
        let file = {
            let (parent, name) = self.parent(physical, true, sync)?;
            let file = open_at(
                &parent,
                &name,
                libc::O_WRONLY
                    | libc::O_CREAT
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
                0o666,
            )?;
            if sync {
                sync_directory(&parent)?;
            }
            file
        };
        #[cfg(not(unix))]
        let file = {
            let path = self.portable_path(physical)?;
            if let Some(parent) = path.parent() {
                if sync {
                    crate::core::io::create_dir_all_durable(parent)?;
                } else {
                    std::fs::create_dir_all(parent)?;
                }
            }
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)?
        };
        if !file.metadata()?.is_file() {
            bail!("serve destination is not a regular file");
        }
        if truncate {
            file.set_len(0)?;
        }
        Ok(file)
    }

    pub(super) fn atomic_file(
        &self,
        physical: &Path,
        overwrite: bool,
        sync: bool,
    ) -> Result<crate::core::atomic_file::AtomicFile> {
        if self.components(physical)?.is_empty() {
            bail!("serve root cannot be a file destination");
        }
        #[cfg(unix)]
        {
            let (parent, _) = self.parent(physical, true, sync)?;
            let parent = open_at(
                &parent,
                OsStr::new("."),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0,
            )?;
            Ok(crate::core::atomic_file::AtomicFile::new_at(
                physical, parent, overwrite,
            )?)
        }
        #[cfg(not(unix))]
        {
            let path = self.portable_path(physical)?;
            if let Some(parent) = path.parent() {
                if sync {
                    crate::core::io::create_dir_all_durable(parent)?;
                } else {
                    std::fs::create_dir_all(parent)?;
                }
            }
            Ok(if overwrite {
                crate::core::atomic_file::AtomicFile::new(&path)?
            } else {
                crate::core::atomic_file::AtomicFile::new_no_replace(&path)?
            })
        }
    }
}

#[cfg(unix)]
fn search_flags() -> libc::c_int {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
    let access = libc::O_PATH;
    #[cfg(target_os = "macos")]
    let access = libc::O_SEARCH;
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "macos"
    )))]
    let access = libc::O_RDONLY;
    access | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
}

#[cfg(unix)]
fn c_name(name: &OsStr) -> std::io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(name.as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "serve path contains NUL")
    })
}

#[cfg(unix)]
#[allow(clippy::useless_conversion)] // mode_t is u16 on Darwin, u32 on Linux.
fn open_at(
    parent: &File,
    name: &OsStr,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> std::io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags,
            libc::c_uint::from(mode),
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn sync_directory(directory: &File) -> Result<()> {
    let directory = open_at(
        directory,
        OsStr::new("."),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0,
    )?;
    Ok(crate::core::io::durable_sync_directory_handle_strict(
        &directory,
    )?)
}
