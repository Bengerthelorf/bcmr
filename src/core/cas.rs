use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};
use std::time::SystemTime;

// Env is read once on first use — glibc's getenv lock shows up in profiles
// on dedup-heavy paths where every block lookup would otherwise hit it.
static CACHED_ROOT: OnceLock<PathBuf> = OnceLock::new();
static ROOT_OVERRIDE: RwLock<Option<PathBuf>> = RwLock::new(None);
static CACHED_CAP: OnceLock<Option<u64>> = OnceLock::new();
static CAP_OVERRIDE: RwLock<Option<Option<u64>>> = RwLock::new(None);

/// A serve cache is an authority boundary, separate from the local CAS. The
/// namespace includes the canonical root and its directory identity so a peer
/// cannot recover another root's content by presenting a known digest.
///
/// Linux/macOS use private, descriptor-relative cache access. Other platforms disable
/// scoped caching until equivalent no-follow and private-access guarantees are
/// available; uploads still work by requesting every block from the client.
#[derive(Clone)]
pub struct Store {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    directory: Option<std::sync::Arc<scoped::Directory>>,
}

impl Store {
    /// Disable optional caching while leaving ordinary transfers available.
    pub fn disabled() -> Self {
        Self {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            directory: None,
        }
    }

    #[allow(dead_code)] // Library convenience API; the CLI supplies its captured root handle.
    pub fn for_serve_root(root: &Path) -> io::Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            Ok(Self {
                directory: Some(std::sync::Arc::new(scoped::Directory::new(root)?)),
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            if !std::fs::metadata(std::fs::canonicalize(root)?)?.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "serve root is not a directory",
                ));
            }
            Ok(Self {})
        }
    }

    /// Bind the cache to the exact directory already retained by the server's
    /// filesystem boundary. `root` is its canonical spelling; reopening it here
    /// could otherwise choose a replacement directory with a different scope.
    pub fn for_serve_root_at(root: &Path, captured_root: &std::fs::File) -> io::Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            Ok(Self {
                directory: Some(std::sync::Arc::new(scoped::Directory::new_at(
                    root,
                    captured_root,
                )?)),
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (root, captured_root);
            Ok(Self::disabled())
        }
    }

    pub fn has(&self, hash: &[u8; 32]) -> bool {
        // Existence alone would advertise corrupted, replaced or linked blobs
        // as hits, and a peer may omit their bytes after this response.
        self.read(hash).is_ok()
    }

    pub fn read(&self, hash: &[u8; 32]) -> io::Result<Vec<u8>> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(directory) = &self.directory {
            return directory.read(hash);
        }
        let _ = hash;
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "scoped cache is disabled",
        ))
    }

    pub fn write(&self, hash: &[u8; 32], data: &[u8]) -> io::Result<()> {
        super::protocol::validate_content_block_size(data.len())?;
        if blake3::hash(data).as_bytes() != hash {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "cache block digest mismatch",
            ));
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(directory) = &self.directory {
            return directory.write(hash, data);
        }
        Ok(())
    }

    /// All served-root namespaces share this budget. The local CAS's existing
    /// API and eviction behavior remain independent and backwards compatible.
    pub fn evict_to_cap(&self, cap: u64) -> io::Result<u64> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(directory) = &self.directory {
            return directory.evict_to_cap(cap);
        }
        let _ = cap;
        Ok(0)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod scoped {
    use super::*;
    use std::ffi::{CStr, CString, OsStr, OsString};
    use std::fs::{File, Metadata};
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::MetadataExt;
    use std::path::Component;

    pub(super) struct Directory {
        root: File,
        root_path: PathBuf,
        collection: File,
        namespace: File,
    }

    fn denied(message: &str) -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, message)
    }

    fn search_flags() -> i32 {
        #[cfg(target_os = "linux")]
        let access = libc::O_PATH;
        #[cfg(target_os = "macos")]
        let access = libc::O_SEARCH;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let access = libc::O_RDONLY;
        access | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
    }

    fn open_at(parent: &File, name: &OsStr, flags: i32, mode: libc::mode_t) -> io::Result<File> {
        let name = CString::new(name.as_bytes())?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                flags,
                libc::c_uint::from(mode),
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn open_canonical_directory(path: &Path) -> io::Result<File> {
        let slash = CString::new("/").unwrap();
        let fd = unsafe { libc::open(slash.as_ptr(), search_flags()) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut directory = unsafe { File::from_raw_fd(fd) };
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    directory = open_at(&directory, name, search_flags(), 0)?;
                }
                _ => return Err(denied("invalid canonical cache directory")),
            }
        }
        Ok(directory)
    }

    fn same_object(a: &Metadata, b: &Metadata) -> bool {
        a.dev() == b.dev() && a.ino() == b.ino()
    }

    fn check_private(file: &File, directory: bool) -> io::Result<()> {
        let metadata = file.metadata()?;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || if directory {
                !metadata.is_dir()
            } else {
                !metadata.is_file() || metadata.nlink() != 1
            }
        {
            return Err(denied(
                "scoped cache object is not private or has an unsafe identity",
            ));
        }
        // Darwin ACL allows can grant access even when POSIX mode is 0700/0600.
        // Reject them before writing cache bytes; the server can continue
        // without this optional cache when inherited access is present.
        #[cfg(target_os = "macos")]
        if !super::super::atomic_file::macos_extended_acl_is_empty(file)
            .map_err(io::Error::other)?
        {
            return Err(denied("scoped cache object has an extended ACL"));
        }
        Ok(())
    }

    fn private_child(parent: &File, name: &OsStr, create: bool) -> io::Result<File> {
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let child = match open_at(parent, name, flags, 0) {
            Ok(child) => child,
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                let c_name = CString::new(name.as_bytes())?;
                if unsafe { libc::mkdirat(parent.as_raw_fd(), c_name.as_ptr(), 0o700) } != 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::AlreadyExists {
                        return Err(error);
                    }
                }
                open_at(parent, name, flags, 0)?
            }
            Err(error) => return Err(error),
        };
        check_private(&child, true)?;
        Ok(child)
    }

    fn cache_base(path: &Path) -> io::Result<File> {
        let mut existing = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()?.join(path)
        };
        let mut missing = Vec::new();
        loop {
            match std::fs::symlink_metadata(&existing) {
                Ok(metadata) => {
                    if missing.is_empty() && metadata.file_type().is_symlink() {
                        return Err(denied("scoped cache base must not be a symbolic link"));
                    }
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    missing.push(
                        existing
                            .file_name()
                            .ok_or_else(|| denied("cache path has no existing ancestor"))?
                            .to_os_string(),
                    );
                    existing.pop();
                }
                Err(error) => return Err(error),
            }
        }
        // Resolve platform aliases such as /var before walking their canonical
        // spelling without following links. Later cache operations use handles.
        let canonical = std::fs::canonicalize(existing)?;
        let mut directory = open_canonical_directory(&canonical)?;
        for name in missing.into_iter().rev() {
            directory = private_child(&directory, &name, true)?;
        }
        let metadata = directory.metadata()?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            return Err(denied(
                "scoped cache base must be owned by this user and not writable by others",
            ));
        }
        Ok(directory)
    }

    fn blob_name(hash: &[u8; 32]) -> (String, String, String) {
        let hex = blake3::Hash::from_bytes(*hash).to_hex();
        (
            hex[0..2].to_owned(),
            hex[2..4].to_owned(),
            format!("{}.blk", &hex[4..]),
        )
    }

    fn blob_file(parent: &File, name: &OsStr) -> io::Result<File> {
        let file = open_at(
            parent,
            name,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0,
        )?;
        check_private(&file, false)?;
        Ok(file)
    }

    fn read_blob(parent: &File, name: &OsStr, hash: &[u8; 32]) -> io::Result<Vec<u8>> {
        let mut file = blob_file(parent, name)?;
        let before = file.metadata()?;
        let limit = super::super::protocol::MAX_CONTENT_BLOCK_SIZE as u64;
        if before.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oversized cache block",
            ));
        }
        let mut data = Vec::with_capacity(before.len() as usize);
        (&mut file).take(limit + 1).read_to_end(&mut data)?;
        let after = file.metadata()?;
        check_private(&file, false)?;
        if !same_object(&before, &after)
            || after.len() != data.len() as u64
            || data.len() as u64 > limit
            || blake3::hash(&data).as_bytes() != hash
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "corrupted cache block",
            ));
        }
        let now = filetime::FileTime::from_system_time(SystemTime::now());
        let _ = filetime::set_file_handle_times(&file, None, Some(now));
        Ok(data)
    }

    fn unlink_at(parent: &File, name: &OsStr) -> io::Result<()> {
        let name = CString::new(name.as_bytes())?;
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    impl Directory {
        #[allow(dead_code)] // Used by the retained library convenience constructor.
        pub(super) fn new(root: &Path) -> io::Result<Self> {
            let root_path = std::fs::canonicalize(root)?;
            let root = open_canonical_directory(&root_path)?;
            Self::new_at(&root_path, &root)
        }

        pub(super) fn new_at(root_path: &Path, captured_root: &File) -> io::Result<Self> {
            if !root_path.is_absolute()
                || root_path.components().any(|component| {
                    !matches!(component, Component::RootDir | Component::Normal(_))
                })
            {
                return Err(denied("serve cache root spelling must be canonical"));
            }
            let root = captured_root.try_clone()?;
            let metadata = root.metadata()?;
            if !metadata.is_dir() {
                return Err(denied("captured serve root is not a directory"));
            }
            let mut scope = blake3::Hasher::new();
            scope.update(b"bcmr-serve-cas-v1\0");
            scope.update(&(root_path.as_os_str().len() as u64).to_le_bytes());
            scope.update(root_path.as_os_str().as_bytes());
            scope.update(&metadata.dev().to_le_bytes());
            scope.update(&metadata.ino().to_le_bytes());
            // Birth time, when provided by the filesystem, also separates a
            // removed directory from a later reuse of its device/inode pair.
            // Unlike ctime/mtime, it does not change when uploads add files.
            if let Some(created) = metadata
                .created()
                .ok()
                .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
            {
                scope.update(&[1]);
                scope.update(&created.as_secs().to_le_bytes());
                scope.update(&created.subsec_nanos().to_le_bytes());
            } else {
                scope.update(&[0]);
            }
            let base = cache_base(&cas_root())?;
            let collection = private_child(&base, OsStr::new("serve-v1"), true)?;
            let namespace = private_child(
                &collection,
                OsStr::new(scope.finalize().to_hex().as_str()),
                true,
            )?;
            Ok(Self {
                root,
                root_path: root_path.to_owned(),
                collection,
                namespace,
            })
        }

        fn check_root(&self) -> io::Result<()> {
            let current = open_canonical_directory(&self.root_path)?;
            if !same_object(&self.root.metadata()?, &current.metadata()?) {
                return Err(denied("serve root directory identity changed"));
            }
            check_private(&self.namespace, true)?;
            check_private(&self.collection, true)
        }

        fn shard(&self, hash: &[u8; 32], create: bool) -> io::Result<(File, String)> {
            self.check_root()?;
            let (first, second, name) = blob_name(hash);
            let first = private_child(&self.namespace, OsStr::new(&first), create)?;
            let second = private_child(&first, OsStr::new(&second), create)?;
            Ok((second, name))
        }

        pub(super) fn read(&self, hash: &[u8; 32]) -> io::Result<Vec<u8>> {
            let (parent, name) = self.shard(hash, false)?;
            read_blob(&parent, OsStr::new(&name), hash)
        }

        pub(super) fn write(&self, hash: &[u8; 32], data: &[u8]) -> io::Result<()> {
            let (parent, name) = self.shard(hash, true)?;
            match read_blob(&parent, OsStr::new(&name), hash) {
                Ok(_) => return Ok(()),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::InvalidData
                    ) => {}
                Err(error) => return Err(error),
            }
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let temporary = format!(
                ".{name}.tmp.{}.{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            );
            let mut file = open_at(
                &parent,
                OsStr::new(&temporary),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )?;
            let result = (|| {
                check_private(&file, false)?;
                file.write_all(data)?;
                let from = CString::new(temporary.as_bytes())?;
                let to = CString::new(name.as_bytes())?;
                if unsafe {
                    libc::renameat(
                        parent.as_raw_fd(),
                        from.as_ptr(),
                        parent.as_raw_fd(),
                        to.as_ptr(),
                    )
                } != 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            })();
            if result.is_err() {
                let _ = unlink_at(&parent, OsStr::new(&temporary));
            }
            result
        }

        pub(super) fn evict_to_cap(&self, cap: u64) -> io::Result<u64> {
            self.check_root()?;
            let mut entries = Vec::new();
            collect_blobs(&self.collection, &mut Vec::new(), &mut entries)?;
            let total = entries
                .iter()
                .fold(0u64, |total, entry| total.saturating_add(entry.size));
            if total <= cap {
                return Ok(0);
            }
            entries.sort_by_key(|entry| entry.modified);
            let mut freed = 0u64;
            for entry in entries {
                if total.saturating_sub(freed) <= cap {
                    break;
                }
                let mut parent = self.collection.try_clone()?;
                let (name, directories) = entry.relative.split_last().unwrap();
                let opened = (|| {
                    for directory in directories {
                        parent = private_child(&parent, directory, false)?;
                    }
                    blob_file(&parent, name)
                })();
                if let Ok(file) = opened {
                    let metadata = file.metadata()?;
                    if metadata.dev() == entry.device
                        && metadata.ino() == entry.inode
                        && unlink_at(&parent, name).is_ok()
                    {
                        freed = freed.saturating_add(entry.size);
                    }
                }
            }
            Ok(freed)
        }
    }

    struct Entry {
        relative: Vec<OsString>,
        size: u64,
        modified: SystemTime,
        device: u64,
        inode: u64,
    }

    fn lower_hex(bytes: &[u8], length: usize) -> bool {
        bytes.len() == length
            && bytes
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    }

    fn collect_blobs(
        directory: &File,
        relative: &mut Vec<OsString>,
        entries: &mut Vec<Entry>,
    ) -> io::Result<()> {
        for name in directory_names(directory)? {
            let bytes = name.as_bytes();
            if relative.len() < 3 {
                if !lower_hex(bytes, if relative.is_empty() { 64 } else { 2 }) {
                    continue;
                }
                if let Ok(child) = private_child(directory, &name, false) {
                    relative.push(name);
                    collect_blobs(&child, relative, entries)?;
                    relative.pop();
                }
            } else if bytes.ends_with(b".blk") && lower_hex(&bytes[..bytes.len() - 4], 60) {
                if let Ok(file) = blob_file(directory, &name) {
                    let metadata = file.metadata()?;
                    let mut path = relative.clone();
                    path.push(name);
                    entries.push(Entry {
                        relative: path,
                        size: metadata.len(),
                        modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                        device: metadata.dev(),
                        inode: metadata.ino(),
                    });
                }
            }
        }
        Ok(())
    }

    struct DirectoryStream(*mut libc::DIR);

    impl Drop for DirectoryStream {
        fn drop(&mut self) {
            unsafe { libc::closedir(self.0) };
        }
    }

    fn directory_names(directory: &File) -> io::Result<Vec<OsString>> {
        // Opening '.' gives this enumeration an independent directory offset;
        // dup/try_clone would share offsets with concurrent users of the store.
        let reader = open_at(
            directory,
            OsStr::new("."),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0,
        )?;
        let fd = reader.into_raw_fd();
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            let error = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(error);
        }
        let stream = DirectoryStream(stream);
        let mut names = Vec::new();
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
                #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
                {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(0) {
                        return Err(error);
                    }
                }
                break;
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                names.push(OsString::from_vec(name.to_vec()));
            }
        }
        Ok(names)
    }
}

pub fn cas_root() -> PathBuf {
    if let Some(p) = ROOT_OVERRIDE.read().unwrap().as_ref() {
        return p.clone();
    }
    CACHED_ROOT.get_or_init(compute_root).clone()
}

fn compute_root() -> PathBuf {
    if let Ok(custom) = std::env::var("BCMR_CAS_DIR") {
        return PathBuf::from(custom);
    }
    directories::BaseDirs::new()
        .map(|d| d.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("bcmr")
        .join("cas")
}

#[cfg(test)]
pub fn set_root_override(p: Option<PathBuf>) {
    *ROOT_OVERRIDE.write().unwrap() = p;
}

#[allow(dead_code)] // Retained local-CAS library API; serve uses the scoped Store.
fn path_for(hash: &[u8; 32]) -> PathBuf {
    let hex = blake3::Hash::from_bytes(*hash).to_hex();
    cas_root()
        .join(&hex[0..2])
        .join(&hex[2..4])
        .join(format!("{}.blk", &hex[4..]))
}

#[allow(dead_code)] // Retained local-CAS library API; serve uses the scoped Store.
pub fn has(hash: &[u8; 32]) -> bool {
    path_for(hash).exists()
}

#[allow(dead_code)] // Retained local-CAS library API; serve uses the scoped Store.
pub fn read(hash: &[u8; 32]) -> io::Result<Vec<u8>> {
    let p = path_for(hash);
    let data = std::fs::read(&p)?;
    let now = filetime::FileTime::from_system_time(SystemTime::now());
    let _ = filetime::set_file_mtime(&p, now);
    Ok(data)
}

#[allow(dead_code)] // Retained local-CAS library API; serve uses the scoped Store.
pub fn write(hash: &[u8; 32], data: &[u8]) -> io::Result<()> {
    let dst = path_for(hash);
    if dst.exists() {
        let now = filetime::FileTime::from_system_time(SystemTime::now());
        let _ = filetime::set_file_mtime(&dst, now);
        return Ok(());
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = unique_tmp_path(&dst);
    std::fs::write(&tmp, data)?;
    match std::fs::rename(&tmp, &dst) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            if dst.exists() {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

#[allow(dead_code)] // Helper for the retained local-CAS write API.
fn unique_tmp_path(dst: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    dst.with_extension(format!("blk.tmp.{}.{}", std::process::id(), n))
}

pub fn cap_bytes() -> Option<u64> {
    if let Some(p) = CAP_OVERRIDE.read().unwrap().as_ref() {
        return *p;
    }
    *CACHED_CAP.get_or_init(compute_cap)
}

fn compute_cap() -> Option<u64> {
    let raw = std::env::var("BCMR_CAS_CAP_MB").ok();
    let mb: u64 = match raw.as_deref().and_then(|s| s.parse().ok()) {
        Some(0) => return None,
        Some(n) => n,
        None => 1024,
    };
    Some(mb * 1024 * 1024)
}

#[cfg(test)]
pub fn set_cap_override(v: Option<Option<u64>>) {
    *CAP_OVERRIDE.write().unwrap() = v;
}

#[allow(dead_code)] // Retained local-CAS library API; serve uses the scoped Store.
pub fn evict_to_cap(cap: u64) -> io::Result<u64> {
    let root = cas_root();
    if !root.exists() {
        return Ok(0);
    }

    let mut entries: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    let mut total: u64 = 0;
    walk_blobs(&root, &mut |path, size, mtime| {
        total += size;
        entries.push((mtime, size, path));
    })?;

    if total <= cap {
        return Ok(0);
    }

    entries.sort_by_key(|(m, _, _)| *m);

    let mut freed: u64 = 0;
    for (_, size, path) in entries {
        if total - freed <= cap {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            freed += size;
        }
    }
    Ok(freed)
}

#[allow(dead_code)] // Helper for the retained local-CAS capacity API.
fn walk_blobs<F>(dir: &std::path::Path, sink: &mut F) -> io::Result<()>
where
    F: FnMut(PathBuf, u64, SystemTime),
{
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in read.flatten() {
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk_blobs(&entry.path(), sink)?;
        } else if ft.is_file() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("blk") {
                continue;
            }
            let md = entry.metadata()?;
            let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            sink(path, md.len(), mtime);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_hash() -> [u8; 32] {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let pid = std::process::id() as u128;
        let mut data = Vec::with_capacity(40);
        data.extend_from_slice(&now.to_le_bytes());
        data.extend_from_slice(&pid.to_le_bytes());
        data.extend_from_slice(&n.to_le_bytes());
        let h = blake3::hash(&data);
        let mut out = [0u8; 32];
        out.copy_from_slice(h.as_bytes());
        out
    }

    #[test]
    fn write_then_read_roundtrip() {
        let _g = lock_cas();
        let _tmp = isolated_cas();
        let h = unique_hash();
        let payload = b"the quick brown fox".repeat(100);
        write(&h, &payload).unwrap();
        assert!(has(&h));
        assert_eq!(read(&h).unwrap(), payload);
        clear_root();
    }

    #[test]
    fn double_write_is_idempotent() {
        let _g = lock_cas();
        let _tmp = isolated_cas();
        let h = unique_hash();
        write(&h, b"hello").unwrap();
        write(&h, b"hello").unwrap();
        clear_root();
    }

    #[test]
    fn missing_hash_returns_false() {
        let _g = lock_cas();
        let _tmp = isolated_cas();
        let h = [0xab; 32];
        assert!(!has(&h));
        clear_root();
    }

    fn isolated_cas() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        set_root_override(Some(tmp.path().to_path_buf()));
        tmp
    }

    static CAS_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_cas() -> std::sync::MutexGuard<'static, ()> {
        CAS_DIR_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn clear_root() {
        set_root_override(None);
    }

    #[test]
    fn evict_drops_oldest_first_until_under_cap() {
        let _g = lock_cas();
        let _tmp = isolated_cas();

        let h_old = unique_hash();
        let h_mid = unique_hash();
        let h_new = unique_hash();
        let payload = vec![0xa1u8; 1024];
        write(&h_old, &payload).unwrap();
        write(&h_mid, &payload).unwrap();
        write(&h_new, &payload).unwrap();

        let now = std::time::SystemTime::now();
        let t_old = filetime::FileTime::from_system_time(now - std::time::Duration::from_secs(300));
        let t_mid = filetime::FileTime::from_system_time(now - std::time::Duration::from_secs(100));
        let t_new = filetime::FileTime::from_system_time(now);
        filetime::set_file_mtime(path_for(&h_old), t_old).unwrap();
        filetime::set_file_mtime(path_for(&h_mid), t_mid).unwrap();
        filetime::set_file_mtime(path_for(&h_new), t_new).unwrap();

        let freed = evict_to_cap(1536).unwrap();
        assert!(freed >= 2 * 1024, "freed={}", freed);
        assert!(!has(&h_old));
        assert!(!has(&h_mid));
        assert!(has(&h_new), "newest blob should survive eviction");

        clear_root();
    }

    #[test]
    fn evict_under_cap_is_noop() {
        let _g = lock_cas();
        let _tmp = isolated_cas();

        let h = unique_hash();
        write(&h, b"tiny").unwrap();
        let freed = evict_to_cap(u64::MAX).unwrap();
        assert_eq!(freed, 0);
        assert!(has(&h));

        clear_root();
    }

    #[test]
    fn cap_override_zero_disables() {
        let _g = lock_cas();
        set_cap_override(Some(None));
        assert!(cap_bytes().is_none());
        set_cap_override(Some(Some(10 * 1024 * 1024)));
        assert_eq!(cap_bytes(), Some(10 * 1024 * 1024));
        set_cap_override(None);
    }

    #[test]
    fn concurrent_put_same_hash_all_succeed() {
        let _g = lock_cas();
        let _tmp = isolated_cas();

        let h = unique_hash();
        let payload = vec![0x7cu8; 8 * 1024];
        let payload = std::sync::Arc::new(payload);

        let mut handles = Vec::new();
        for _ in 0..8 {
            let p = payload.clone();
            handles.push(std::thread::spawn(move || write(&h, &p)));
        }
        for h in handles {
            h.join().unwrap().unwrap();
        }
        assert!(has(&h));
        assert_eq!(&*read(&h).unwrap(), &*payload);

        let parent = path_for(&h).parent().unwrap().to_path_buf();
        let orphans: Vec<_> = std::fs::read_dir(&parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                name.to_string_lossy().contains("blk.tmp")
            })
            .collect();
        assert!(
            orphans.is_empty(),
            "orphan tmp files left behind: {:?}",
            orphans
        );

        clear_root();
    }

    #[test]
    fn read_touches_mtime_for_lru() {
        let _g = lock_cas();
        let _tmp = isolated_cas();

        let h_a = unique_hash();
        let h_b = unique_hash();
        let payload = vec![0xa1u8; 1024];
        write(&h_a, &payload).unwrap();
        write(&h_b, &payload).unwrap();

        let old = filetime::FileTime::from_system_time(
            std::time::SystemTime::now() - std::time::Duration::from_secs(300),
        );
        filetime::set_file_mtime(path_for(&h_a), old).unwrap();
        filetime::set_file_mtime(path_for(&h_b), old).unwrap();

        let _ = read(&h_a).unwrap();

        let _ = evict_to_cap(1024).unwrap();
        assert!(has(&h_a), "recently-read blob should win the LRU");
        assert!(!has(&h_b));

        clear_root();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn scoped_blob(cache: &Path, hash: &[u8; 32]) -> PathBuf {
        let hex = blake3::Hash::from_bytes(*hash).to_hex();
        let name = format!("{}.blk", &hex[4..]);
        walkdir::WalkDir::new(cache.join("serve-v1"))
            .into_iter()
            .filter_map(Result::ok)
            .find(|entry| entry.file_name() == OsStr::new(&name))
            .expect("scoped blob must have been created")
            .into_path()
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    use std::ffi::OsStr;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn scoped_roundtrip_reuses_only_the_same_canonical_root() {
        let _guard = lock_cas();
        let _cache = isolated_cas();
        let roots = tempfile::tempdir().unwrap();
        let first = roots.path().join("first");
        let second = roots.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let alias = roots.path().join("alias");
        std::os::unix::fs::symlink(&first, &alias).unwrap();
        let data = b"harmless scoped roundtrip payload";
        let hash = *blake3::hash(data).as_bytes();
        let store = Store::for_serve_root(&first).unwrap();
        store.write(&hash, data).unwrap();
        assert_eq!(store.read(&hash).unwrap(), data);
        assert_eq!(
            Store::for_serve_root(&alias).unwrap().read(&hash).unwrap(),
            data
        );
        assert_eq!(
            Store::for_serve_root(&first).unwrap().read(&hash).unwrap(),
            data
        );
        let other = Store::for_serve_root(&second).unwrap();
        assert!(!other.has(&hash));
        assert!(other.read(&hash).is_err());
        assert!(!has(&hash), "serve writes must not populate the local CAS");
        let local = b"harmless local-cache fixture";
        let local_hash = *blake3::hash(local).as_bytes();
        write(&local_hash, local).unwrap();
        assert!(
            !store.has(&local_hash),
            "local CAS must not bypass serve scoping"
        );
        clear_root();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn scoped_cache_rejects_corruption_and_repairs_it_with_verified_bytes() {
        let _guard = lock_cas();
        let cache = isolated_cas();
        let root = tempfile::tempdir().unwrap();
        let store = Store::for_serve_root(root.path()).unwrap();
        let data = b"a harmless verified block";
        let hash = *blake3::hash(data).as_bytes();
        store.write(&hash, data).unwrap();
        let blob = scoped_blob(cache.path(), &hash);
        std::fs::write(&blob, b"corrupt fixture bytes").unwrap();
        assert!(
            !store.has(&hash),
            "corrupted bytes must never be advertised as a hit"
        );
        assert_eq!(
            store.read(&hash).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(store.write(&hash, b"wrong payload").is_err());
        assert_eq!(std::fs::read(&blob).unwrap(), b"corrupt fixture bytes");
        store.write(&hash, data).unwrap();
        assert_eq!(store.read(&hash).unwrap(), data);
        clear_root();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn scoped_cache_uses_private_directories_and_files() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = lock_cas();
        let cache = isolated_cas();
        let root = tempfile::tempdir().unwrap();
        let store = Store::for_serve_root(root.path()).unwrap();
        let data = b"private cache fixture";
        store.write(blake3::hash(data).as_bytes(), data).unwrap();
        for entry in walkdir::WalkDir::new(cache.path().join("serve-v1")) {
            let entry = entry.unwrap();
            let mode = entry.metadata().unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode,
                if entry.file_type().is_dir() {
                    0o700
                } else {
                    0o600
                }
            );
        }
        clear_root();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn scoped_cache_does_not_follow_blob_symlinks_or_trust_hardlinks() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = lock_cas();
        let cache = isolated_cas();
        let root = tempfile::tempdir().unwrap();
        let store = Store::for_serve_root(root.path()).unwrap();
        let data = b"harmless external fixture";
        let hash = *blake3::hash(data).as_bytes();
        store.write(&hash, data).unwrap();
        let blob = scoped_blob(cache.path(), &hash);
        let outside = root.path().join("outside");
        std::fs::write(&outside, data).unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::remove_file(&blob).unwrap();
        std::os::unix::fs::symlink(&outside, &blob).unwrap();
        assert!(!store.has(&hash));
        assert!(store.read(&hash).is_err());
        assert!(store.write(&hash, data).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), data);
        std::fs::remove_file(&blob).unwrap();
        std::fs::hard_link(&outside, &blob).unwrap();
        assert!(!store.has(&hash));
        assert!(store.read(&hash).is_err());
        assert!(store.write(&hash, data).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), data);
        clear_root();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn scoped_cache_rejects_existing_public_blobs_and_namespaces() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = lock_cas();
        let cache = isolated_cas();
        let root = tempfile::tempdir().unwrap();
        let store = Store::for_serve_root(root.path()).unwrap();
        let data = b"private access-check fixture";
        let hash = *blake3::hash(data).as_bytes();
        store.write(&hash, data).unwrap();
        let blob = scoped_blob(cache.path(), &hash);
        std::fs::set_permissions(&blob, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!store.has(&hash));
        assert!(store.write(&hash, data).is_err());
        std::fs::set_permissions(&blob, std::fs::Permissions::from_mode(0o600)).unwrap();
        let namespace = blob.parent().unwrap().parent().unwrap().parent().unwrap();
        std::fs::set_permissions(namespace, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!store.has(&hash));
        assert!(store.write(&hash, data).is_err());
        assert!(Store::for_serve_root(root.path()).is_err());
        clear_root();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn scoped_cache_rejects_replaced_shards_and_namespace_links() {
        let _guard = lock_cas();
        let cache = isolated_cas();
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let store = Store::for_serve_root(root.path()).unwrap();
        let data = b"safe pinned cache fixture";
        let hash = *blake3::hash(data).as_bytes();
        store.write(&hash, data).unwrap();
        let blob = scoped_blob(cache.path(), &hash);
        let shard = blob.parent().unwrap();
        let saved = shard.with_extension("saved");
        std::fs::rename(shard, &saved).unwrap();
        std::os::unix::fs::symlink(outside.path(), shard).unwrap();
        assert!(!store.has(&hash));
        assert!(store.write(&hash, data).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        std::fs::remove_file(shard).unwrap();
        std::fs::rename(saved, shard).unwrap();
        let namespace = shard.parent().unwrap().parent().unwrap();
        std::fs::rename(namespace, namespace.with_extension("saved")).unwrap();
        std::os::unix::fs::symlink(outside.path(), namespace).unwrap();
        assert!(Store::for_serve_root(root.path()).is_err());
        assert_eq!(
            store.read(&hash).unwrap(),
            data,
            "the existing store stays pinned to its safe directory"
        );
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        clear_root();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn scoped_cache_does_not_reuse_a_replaced_root_directory() {
        let _guard = lock_cas();
        let _cache = isolated_cas();
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let store = Store::for_serve_root(&root).unwrap();
        let data = b"old directory fixture";
        let hash = *blake3::hash(data).as_bytes();
        store.write(&hash, data).unwrap();
        std::fs::rename(&root, parent.path().join("saved-root")).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert!(!store.has(&hash));
        assert!(store.write(&hash, data).is_err());
        let replacement = Store::for_serve_root(&root).unwrap();
        assert!(!replacement.has(&hash));
        assert!(replacement.read(&hash).is_err());
        clear_root();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn scoped_capacity_is_shared_across_root_namespaces() {
        let _guard = lock_cas();
        let cache = isolated_cas();
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        let first = Store::for_serve_root(root_a.path()).unwrap();
        let second = Store::for_serve_root(root_b.path()).unwrap();
        let old = vec![b'a'; 1024];
        let recent = vec![b'b'; 1024];
        let old_hash = *blake3::hash(&old).as_bytes();
        let recent_hash = *blake3::hash(&recent).as_bytes();
        first.write(&old_hash, &old).unwrap();
        second.write(&recent_hash, &recent).unwrap();
        let old_time = filetime::FileTime::from_system_time(
            SystemTime::now() - std::time::Duration::from_secs(300),
        );
        filetime::set_file_mtime(scoped_blob(cache.path(), &old_hash), old_time).unwrap();
        assert_eq!(second.evict_to_cap(1024).unwrap(), 1024);
        assert!(
            !first.has(&old_hash),
            "a per-root budget would retain both blobs"
        );
        assert_eq!(second.read(&recent_hash).unwrap(), recent);
        clear_root();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn scoped_cache_cannot_write_using_a_mismatched_captured_root() {
        let _guard = lock_cas();
        let cache = isolated_cas();
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        let captured = std::fs::File::open(root_a.path()).unwrap();
        let canonical_b = std::fs::canonicalize(root_b.path()).unwrap();
        let mismatch = Store::for_serve_root_at(&canonical_b, &captured).unwrap();
        let data = b"harmless captured-root fixture";
        let hash = *blake3::hash(data).as_bytes();
        assert!(!mismatch.has(&hash));
        assert!(mismatch.write(&hash, data).is_err());
        let correct = Store::for_serve_root(root_b.path()).unwrap();
        assert!(!correct.has(&hash));
        assert!(walkdir::WalkDir::new(cache.path())
            .into_iter()
            .filter_map(Result::ok)
            .all(|entry| entry.path().extension() != Some(OsStr::new("blk"))));
        clear_root();
    }

    #[test]
    fn disabled_scoped_cache_never_advertises_or_retains_a_block() {
        let store = Store::disabled();
        let data = b"uncached harmless fixture";
        let hash = *blake3::hash(data).as_bytes();
        store.write(&hash, data).unwrap();
        assert!(!store.has(&hash));
        assert_eq!(
            store.read(&hash).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(store.evict_to_cap(0).unwrap(), 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scoped_cache_rejects_inherited_extended_acl_access() {
        let _guard = lock_cas();
        let cache = isolated_cas();
        let root = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("/bin/chmod")
            .args([
                "+a",
                "everyone allow read,readattr,readextattr,readsecurity,file_inherit,directory_inherit",
            ])
            .arg(cache.path())
            .status()
            .unwrap();
        assert!(status.success());
        let store = Store::for_serve_root(root.path());
        assert!(
            store.is_err(),
            "inherited ACLs must not make a shared cache private"
        );
        assert!(walkdir::WalkDir::new(cache.path())
            .into_iter()
            .filter_map(Result::ok)
            .all(|entry| entry.path().extension() != Some(OsStr::new("blk"))));
        clear_root();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scoped_cache_rejects_extended_acl_access_on_existing_blobs() {
        let _guard = lock_cas();
        let cache = isolated_cas();
        let root = tempfile::tempdir().unwrap();
        let store = Store::for_serve_root(root.path()).unwrap();
        let data = b"private cache fixture";
        let hash = *blake3::hash(data).as_bytes();
        store.write(&hash, data).unwrap();
        let status = std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone allow read"])
            .arg(scoped_blob(cache.path(), &hash))
            .status()
            .unwrap();
        assert!(status.success());
        assert!(
            !store.has(&hash),
            "ACL-exposed cache bytes must not be advertised"
        );
        assert!(store.read(&hash).is_err());
        clear_root();
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn scoped_cache_fails_closed_without_platform_security_primitives() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::for_serve_root(root.path()).unwrap();
        let data = b"uncached portable fixture";
        let hash = *blake3::hash(data).as_bytes();
        store.write(&hash, data).unwrap();
        assert!(!store.has(&hash));
        assert_eq!(
            store.read(&hash).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(store.evict_to_cap(0).unwrap(), 0);
    }
}
