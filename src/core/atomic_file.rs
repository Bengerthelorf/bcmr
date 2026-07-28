use crate::core::error::BcmrError;
use crate::core::file_metadata::PortableFileMetadata;
use std::ffi::OsStr;
#[cfg(unix)]
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

#[cfg(unix)]
pub(crate) fn positional_write_all(
    file: &File,
    mut bytes: &[u8],
    mut offset: u64,
) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    while !bytes.is_empty() {
        let written = file.write_at(bytes, offset)?;
        if written == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "positional write returned zero bytes",
            ));
        }
        bytes = &bytes[written..];
        offset = offset
            .checked_add(written as u64)
            .ok_or_else(|| std::io::Error::other("positional write offset overflow"))?;
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn positional_write_all(
    file: &File,
    mut bytes: &[u8],
    mut offset: u64,
) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !bytes.is_empty() {
        let written = file.seek_write(bytes, offset)?;
        if written == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "positional write returned zero bytes",
            ));
        }
        bytes = &bytes[written..];
        offset = offset
            .checked_add(written as u64)
            .ok_or_else(|| std::io::Error::other("positional write offset overflow"))?;
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn positional_write_all(
    _file: &File,
    _bytes: &[u8],
    _offset: u64,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "positioned writes are not implemented on this platform",
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DestinationFingerprint {
    len: u64,
    modified: Option<std::time::SystemTime>,
    kind: u8,
    readonly: bool,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    uid: u32,
    #[cfg(unix)]
    gid: u32,
    #[cfg(unix)]
    mtime_seconds: i64,
    #[cfg(unix)]
    mtime_nanoseconds: i64,
    #[cfg(unix)]
    ctime_seconds: i64,
    #[cfg(unix)]
    ctime_nanoseconds: i64,
    #[cfg(windows)]
    file_attributes: u32,
    #[cfg(windows)]
    creation_time: u64,
    #[cfg(windows)]
    last_write_time: u64,
}

impl DestinationFingerprint {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        let file_type = metadata.file_type();
        let kind = if file_type.is_file() {
            1
        } else if file_type.is_dir() {
            2
        } else if file_type.is_symlink() {
            3
        } else {
            4
        };

        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        #[cfg(windows)]
        use std::os::windows::fs::MetadataExt;

        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            kind,
            readonly: metadata.permissions().readonly(),
            #[cfg(unix)]
            mode: metadata.mode(),
            #[cfg(unix)]
            uid: metadata.uid(),
            #[cfg(unix)]
            gid: metadata.gid(),
            #[cfg(unix)]
            mtime_seconds: metadata.mtime(),
            #[cfg(unix)]
            mtime_nanoseconds: metadata.mtime_nsec(),
            #[cfg(unix)]
            ctime_seconds: metadata.ctime(),
            #[cfg(unix)]
            ctime_nanoseconds: metadata.ctime_nsec(),
            #[cfg(windows)]
            file_attributes: metadata.file_attributes(),
            #[cfg(windows)]
            creation_time: metadata.creation_time(),
            #[cfg(windows)]
            last_write_time: metadata.last_write_time(),
        }
    }

    fn matches_after_namespace_move(&self, other: &Self) -> bool {
        let common_matches = self.len == other.len
            && self.modified == other.modified
            && self.kind == other.kind
            && self.readonly == other.readonly;

        #[cfg(unix)]
        {
            // A successful rename/exchange is allowed to update ctime on the
            // displaced inode. Keep ctime in the pre-publish race check, but
            // compare only metadata that is stable across the namespace move
            // when validating the displaced entry afterwards.
            common_matches
                && self.mode == other.mode
                && self.uid == other.uid
                && self.gid == other.gid
                && self.mtime_seconds == other.mtime_seconds
                && self.mtime_nanoseconds == other.mtime_nanoseconds
        }
        #[cfg(windows)]
        {
            common_matches
                && self.file_attributes == other.file_attributes
                && self.creation_time == other.creation_time
                && self.last_write_time == other.last_write_time
        }
        #[cfg(not(any(unix, windows)))]
        {
            common_matches
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EntryIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    windows: WindowsFileIdentity,
    #[cfg(not(any(unix, windows)))]
    fingerprint: DestinationFingerprint,
}

impl EntryIdentity {
    #[cfg(not(windows))]
    fn from_metadata(metadata: &std::fs::Metadata) -> Result<Self, BcmrError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(Self {
                fingerprint: DestinationFingerprint::from_metadata(metadata),
            })
        }
    }
}

#[cfg(windows)]
#[derive(Clone, Debug, Eq, PartialEq)]
enum WindowsFileIdentity {
    Modern { volume: u64, identifier: [u8; 16] },
    Legacy { volume: u32, identifier: u64 },
}

#[cfg(windows)]
impl WindowsFileIdentity {
    fn stable_across_rename(&self) -> bool {
        matches!(self, Self::Modern { .. })
    }
}

#[cfg(windows)]
fn select_windows_file_identity<F>(
    modern: Result<WindowsFileIdentity, BcmrError>,
    legacy: F,
) -> Result<WindowsFileIdentity, BcmrError>
where
    F: FnOnce() -> Result<WindowsFileIdentity, BcmrError>,
{
    modern.or_else(|_| legacy())
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> Result<WindowsFileIdentity, BcmrError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
        BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO,
    };

    let handle = file.as_raw_handle() as HANDLE;
    let mut modern = FILE_ID_INFO::default();
    let modern_ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut modern as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    let modern_identity = if modern_ok != 0 {
        Ok(WindowsFileIdentity::Modern {
            volume: modern.VolumeSerialNumber,
            identifier: modern.FileId.Identifier,
        })
    } else {
        Err(BcmrError::Io(std::io::Error::last_os_error()))
    };

    select_windows_file_identity(modern_identity, || {
        let mut legacy = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(handle, &mut legacy) } == 0 {
            return Err(BcmrError::Io(std::io::Error::last_os_error()));
        }
        Ok(WindowsFileIdentity::Legacy {
            volume: legacy.dwVolumeSerialNumber,
            identifier: (u64::from(legacy.nFileIndexHigh) << 32) | u64::from(legacy.nFileIndexLow),
        })
    })
}

fn identity_from_file(file: &File) -> Result<EntryIdentity, BcmrError> {
    #[cfg(windows)]
    {
        Ok(EntryIdentity {
            windows: windows_file_identity(file)?,
        })
    }
    #[cfg(not(windows))]
    {
        EntryIdentity::from_metadata(&file.metadata()?)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EntrySnapshot {
    identity: EntryIdentity,
    fingerprint: DestinationFingerprint,
}

fn snapshot_from_file(file: &File) -> Result<EntrySnapshot, BcmrError> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(BcmrError::InvalidInput(
            "atomic receive entry is no longer a regular file".to_string(),
        ));
    }
    Ok(EntrySnapshot {
        identity: identity_from_file(file)?,
        fingerprint: DestinationFingerprint::from_metadata(&metadata),
    })
}

fn open_snapshot_file(path: &Path) -> Result<File, BcmrError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(BcmrError::Io)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        return OpenOptions::new()
            .access_mode(0)
            .share_mode(FILE_SHARE_DELETE | FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(BcmrError::Io);
    }
    #[cfg(not(any(unix, windows)))]
    {
        return OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(BcmrError::Io);
    }
}

fn snapshot_from_path(path: &Path) -> Result<EntrySnapshot, BcmrError> {
    snapshot_from_file(&open_snapshot_file(path)?)
}

#[cfg(unix)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct ExistingSecuritySnapshot {
    uid: u32,
    gid: u32,
    mode: u32,
    xattrs: Vec<(OsString, Vec<u8>)>,
    #[cfg(target_os = "macos")]
    acl: Option<Vec<u8>>,
}

#[cfg(unix)]
impl ExistingSecuritySnapshot {
    fn capture(file: &File) -> Result<Self, BcmrError> {
        use std::os::unix::fs::MetadataExt;
        use xattr::FileExt;

        let metadata = file.metadata()?;
        let mut names: Vec<_> = file.list_xattr()?.collect();
        names.sort();
        let mut xattrs = Vec::with_capacity(names.len());
        for name in names {
            let value = file.get_xattr(&name)?.ok_or_else(|| {
                BcmrError::InvalidInput(format!(
                    "extended attribute '{}' changed while it was being captured",
                    name.to_string_lossy()
                ))
            })?;
            xattrs.push((name, value));
        }

        Ok(Self {
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode(),
            xattrs,
            #[cfg(target_os = "macos")]
            acl: capture_macos_acl(file)?,
        })
    }
}

#[cfg(target_os = "macos")]
fn capture_macos_acl(file: &File) -> Result<Option<Vec<u8>>, BcmrError> {
    use std::os::fd::AsRawFd;

    type Acl = *mut std::ffi::c_void;
    const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, acl_type: libc::c_int) -> Acl;
        fn acl_size(acl: Acl) -> libc::ssize_t;
        fn acl_copy_ext_native(
            buffer: *mut std::ffi::c_void,
            acl: Acl,
            size: libc::ssize_t,
        ) -> libc::ssize_t;
        fn acl_free(object: *mut std::ffi::c_void) -> libc::c_int;
    }

    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = std::io::Error::last_os_error();
        if error
            .raw_os_error()
            .is_some_and(|errno| [libc::ENOENT, libc::ENOTSUP, libc::EOPNOTSUPP].contains(&errno))
        {
            return Ok(None);
        }
        return Err(BcmrError::Io(error));
    }
    let size = unsafe { acl_size(acl) };
    if size < 0 {
        let error = std::io::Error::last_os_error();
        unsafe {
            acl_free(acl);
        }
        return Err(BcmrError::Io(error));
    }
    let mut bytes = vec![0u8; size as usize];
    let copied = unsafe { acl_copy_ext_native(bytes.as_mut_ptr().cast(), acl, size) };
    let copy_error = if copied < 0 {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };
    unsafe {
        acl_free(acl);
    }
    if let Some(error) = copy_error {
        return Err(BcmrError::Io(error));
    }
    if copied != size {
        return Err(BcmrError::InvalidInput(
            "macOS returned a truncated ACL snapshot".into(),
        ));
    }
    Ok(Some(bytes))
}

struct ExistingDestination {
    fingerprint: DestinationFingerprint,
    identity: EntryIdentity,
    #[cfg(unix)]
    security_source: File,
    #[cfg(unix)]
    security_snapshot: ExistingSecuritySnapshot,
    #[cfg(windows)]
    _identity_source: File,
}

impl ExistingDestination {
    fn capture(path: &Path) -> Result<Self, BcmrError> {
        let source = open_snapshot_file(path)?;
        let snapshot = snapshot_from_file(&source).map_err(|error| {
            BcmrError::InvalidInput(format!(
                "atomic receive destination '{}' must remain a regular file: {error}",
                path.display()
            ))
        })?;
        #[cfg(unix)]
        {
            let security_snapshot = ExistingSecuritySnapshot::capture(&source)?;
            if snapshot_from_file(&source)? != snapshot
                || ExistingSecuritySnapshot::capture(&source)? != security_snapshot
            {
                return Err(BcmrError::DestinationChanged(path.to_path_buf()));
            }
            Ok(Self {
                fingerprint: snapshot.fingerprint,
                identity: snapshot.identity,
                security_source: source,
                security_snapshot,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                fingerprint: snapshot.fingerprint,
                identity: snapshot.identity,
                _identity_source: source,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(Self {
                fingerprint: snapshot.fingerprint,
                identity: snapshot.identity,
            })
        }
    }

    fn matches_path(&self, path: &Path) -> Result<bool, BcmrError> {
        let snapshot = match snapshot_from_path(path) {
            Ok(snapshot) => snapshot,
            Err(BcmrError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        Ok(snapshot.identity == self.identity && snapshot.fingerprint == self.fingerprint)
    }
}

enum DestinationObservation {
    Missing,
    Existing(ExistingDestination),
}

impl DestinationObservation {
    fn capture(path: &Path) -> Result<Self, BcmrError> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                Ok(Self::Existing(ExistingDestination::capture(path)?))
            }
            Ok(_) => Err(BcmrError::InvalidInput(format!(
                "atomic receive destination '{}' must be a regular file, not a symlink, directory, or special file",
                path.display()
            ))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::Missing),
            Err(error) => Err(BcmrError::Io(error)),
        }
    }

    fn matches_path(&self, path: &Path) -> Result<bool, BcmrError> {
        match self {
            Self::Missing => match std::fs::symlink_metadata(path) {
                Ok(_) => Ok(false),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
                Err(error) => Err(BcmrError::Io(error)),
            },
            Self::Existing(existing) => existing.matches_path(path),
        }
    }
}

struct BoundDirectory {
    path: PathBuf,
    handle: same_file::Handle,
}

#[cfg(target_os = "macos")]
struct MacosInheritedFileAcl {
    serialized: Vec<u8>,
}

#[cfg(target_os = "macos")]
fn capture_macos_inherited_file_acl(
    parent: &File,
) -> Result<Option<MacosInheritedFileAcl>, BcmrError> {
    use std::os::fd::AsRawFd;

    type Acl = *mut std::ffi::c_void;
    type AclEntry = *mut std::ffi::c_void;
    type AclFlagset = *mut std::ffi::c_void;
    const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;
    const ACL_FIRST_ENTRY: libc::c_int = 0;
    const ACL_NEXT_ENTRY: libc::c_int = -1;
    const ACL_ENTRY_INHERITED: libc::c_int = 1 << 4;
    const ACL_ENTRY_FILE_INHERIT: libc::c_int = 1 << 5;

    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, acl_type: libc::c_int) -> Acl;
        fn acl_init(count: libc::c_int) -> Acl;
        fn acl_get_entry(acl: Acl, entry_id: libc::c_int, entry: *mut AclEntry) -> libc::c_int;
        fn acl_create_entry(acl: *mut Acl, entry: *mut AclEntry) -> libc::c_int;
        fn acl_copy_entry(destination: AclEntry, source: AclEntry) -> libc::c_int;
        fn acl_get_flagset_np(object: *mut std::ffi::c_void, flags: *mut AclFlagset)
            -> libc::c_int;
        fn acl_get_flag_np(flags: AclFlagset, flag: libc::c_int) -> libc::c_int;
        fn acl_clear_flags_np(flags: AclFlagset) -> libc::c_int;
        fn acl_add_flag_np(flags: AclFlagset, flag: libc::c_int) -> libc::c_int;
        fn acl_set_flagset_np(object: *mut std::ffi::c_void, flags: AclFlagset) -> libc::c_int;
        fn acl_size(acl: Acl) -> isize;
        fn acl_copy_ext_native(buffer: *mut std::ffi::c_void, acl: Acl, size: isize) -> isize;
        fn acl_free(object: *mut std::ffi::c_void) -> libc::c_int;
    }

    struct OwnedAcl(Acl);
    impl Drop for OwnedAcl {
        fn drop(&mut self) {
            unsafe {
                acl_free(self.0);
            }
        }
    }

    fn check(result: libc::c_int) -> Result<(), BcmrError> {
        if result == 0 {
            Ok(())
        } else {
            Err(BcmrError::Io(std::io::Error::last_os_error()))
        }
    }

    let parent_acl = unsafe { acl_get_fd_np(parent.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if parent_acl.is_null() {
        let error = std::io::Error::last_os_error();
        if error
            .raw_os_error()
            .is_some_and(|errno| [libc::ENOENT, libc::ENOTSUP, libc::EOPNOTSUPP].contains(&errno))
        {
            return Ok(None);
        }
        return Err(BcmrError::Io(error));
    }
    let _parent_acl = OwnedAcl(parent_acl);

    let inherited_acl = unsafe { acl_init(0) };
    if inherited_acl.is_null() {
        return Err(BcmrError::Io(std::io::Error::last_os_error()));
    }
    let mut inherited_acl = OwnedAcl(inherited_acl);
    let mut entry_count = 0usize;
    let mut source_entry: AclEntry = std::ptr::null_mut();
    let mut selector = ACL_FIRST_ENTRY;
    loop {
        let result = unsafe { acl_get_entry(parent_acl, selector, &mut source_entry) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if result == -1 && error.raw_os_error() == Some(libc::EINVAL) {
                break;
            }
            return Err(BcmrError::Io(error));
        }
        selector = ACL_NEXT_ENTRY;

        let mut source_flags: AclFlagset = std::ptr::null_mut();
        check(unsafe { acl_get_flagset_np(source_entry, &mut source_flags) })?;
        let inherits_file = unsafe { acl_get_flag_np(source_flags, ACL_ENTRY_FILE_INHERIT) };
        if inherits_file < 0 {
            return Err(BcmrError::Io(std::io::Error::last_os_error()));
        }
        if inherits_file == 0 {
            continue;
        }

        let mut destination_entry: AclEntry = std::ptr::null_mut();
        check(unsafe { acl_create_entry(&mut inherited_acl.0, &mut destination_entry) })?;
        check(unsafe { acl_copy_entry(destination_entry, source_entry) })?;

        let mut destination_flags: AclFlagset = std::ptr::null_mut();
        check(unsafe { acl_get_flagset_np(destination_entry, &mut destination_flags) })?;
        check(unsafe { acl_clear_flags_np(destination_flags) })?;
        check(unsafe { acl_add_flag_np(destination_flags, ACL_ENTRY_INHERITED) })?;
        check(unsafe { acl_set_flagset_np(destination_entry, destination_flags) })?;
        entry_count += 1;
    }

    if entry_count == 0 {
        return Ok(None);
    }
    let serialized_size = unsafe { acl_size(inherited_acl.0) };
    if serialized_size <= 0 {
        return Err(BcmrError::Io(std::io::Error::last_os_error()));
    }
    let mut serialized = vec![0u8; serialized_size as usize];
    let copied = unsafe {
        acl_copy_ext_native(
            serialized.as_mut_ptr().cast(),
            inherited_acl.0,
            serialized_size,
        )
    };
    if copied != serialized_size {
        return Err(BcmrError::Io(std::io::Error::last_os_error()));
    }
    Ok(Some(MacosInheritedFileAcl { serialized }))
}

#[cfg(target_os = "macos")]
fn apply_macos_inherited_file_acl(
    file: &File,
    inherited: &MacosInheritedFileAcl,
) -> Result<(), BcmrError> {
    use std::os::fd::AsRawFd;

    type Acl = *mut std::ffi::c_void;
    const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;
    unsafe extern "C" {
        fn acl_copy_int_native(buffer: *const std::ffi::c_void) -> Acl;
        fn acl_set_fd_np(fd: libc::c_int, acl: Acl, acl_type: libc::c_int) -> libc::c_int;
        fn acl_free(object: *mut std::ffi::c_void) -> libc::c_int;
    }

    struct OwnedAcl(Acl);
    impl Drop for OwnedAcl {
        fn drop(&mut self) {
            unsafe {
                acl_free(self.0);
            }
        }
    }

    let acl = unsafe { acl_copy_int_native(inherited.serialized.as_ptr().cast()) };
    if acl.is_null() {
        return Err(BcmrError::Io(std::io::Error::last_os_error()));
    }
    let acl = OwnedAcl(acl);
    if unsafe { acl_set_fd_np(file.as_raw_fd(), acl.0, ACL_TYPE_EXTENDED) } != 0 {
        return Err(BcmrError::Io(std::io::Error::last_os_error()));
    }
    if capture_macos_acl(file)?.as_deref() != Some(inherited.serialized.as_slice()) {
        return Err(BcmrError::InvalidInput(
            "atomic receive inherited ACL changed while applying it".into(),
        ));
    }
    Ok(())
}

impl BoundDirectory {
    fn capture(path: &Path) -> Result<Self, BcmrError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;

            let mut options = OpenOptions::new();
            options
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
            let file = options.open(path)?;
            if !file.metadata()?.file_type().is_dir() {
                return Err(BcmrError::InvalidInput(format!(
                    "atomic receive parent '{}' must be a real directory",
                    path.display()
                )));
            }
            Ok(Self {
                path: path.to_path_buf(),
                handle: same_file::Handle::from_file(file)?,
            })
        }

        #[cfg(not(unix))]
        let metadata = std::fs::symlink_metadata(path)?;
        #[cfg(not(unix))]
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(BcmrError::InvalidInput(format!(
                "atomic receive parent '{}' must be a real directory",
                path.display()
            )));
        }
        #[cfg(not(unix))]
        Ok(Self {
            path: path.to_path_buf(),
            handle: same_file::Handle::from_path(path)?,
        })
    }

    fn matches_path(&self) -> Result<bool, BcmrError> {
        let metadata = match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(BcmrError::Io(error)),
        };
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Ok(false);
        }
        Ok(same_file::Handle::from_path(&self.path)? == self.handle)
    }

    fn durable_sync(&self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            crate::core::io::durable_sync_directory_handle_strict(self.handle.as_file())
        }
        #[cfg(not(unix))]
        {
            // Windows `--sync` is rejected before publication because the
            // public API does not provide the Unix-style directory namespace
            // flush required by this contract.
            Ok(())
        }
    }
}

#[cfg(unix)]
fn validate_private_transaction(
    directory: &BoundDirectory,
    expected_uid: u32,
    expected_mode: u32,
) -> Result<(), BcmrError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = directory.handle.as_file().metadata()?;
    let actual_mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_dir()
        || metadata.uid() != expected_uid
        || actual_mode != expected_mode
    {
        return Err(BcmrError::InvalidInput(format!(
            "atomic receive transaction '{}' is not the expected private directory \
             (owner {}, mode {:04o}; expected owner {}, mode {:04o})",
            directory.path.display(),
            metadata.uid(),
            actual_mode,
            expected_uid,
            expected_mode
        )));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn macos_extended_acl_is_empty(file: &File) -> Result<bool, BcmrError> {
    use std::os::fd::AsRawFd;

    type Acl = *mut std::ffi::c_void;
    type AclEntry = *mut std::ffi::c_void;
    const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;
    const ACL_FIRST_ENTRY: libc::c_int = 0;
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, acl_type: libc::c_int) -> Acl;
        fn acl_get_entry(acl: Acl, entry_id: libc::c_int, entry: *mut AclEntry) -> libc::c_int;
        fn acl_free(object: *mut std::ffi::c_void) -> libc::c_int;
    }

    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = std::io::Error::last_os_error();
        if error
            .raw_os_error()
            .is_some_and(|errno| [libc::ENOENT, libc::ENOTSUP, libc::EOPNOTSUPP].contains(&errno))
        {
            return Ok(true);
        }
        return Err(BcmrError::Io(error));
    }

    let mut entry: AclEntry = std::ptr::null_mut();
    let first_result = unsafe { acl_get_entry(acl, ACL_FIRST_ENTRY, &mut entry) };
    let first_error = if first_result == 0 {
        None
    } else {
        Some(std::io::Error::last_os_error())
    };
    unsafe {
        acl_free(acl);
    }
    match first_error {
        None => Ok(false),
        Some(error) if error.raw_os_error() == Some(libc::EINVAL) => Ok(true),
        Some(error) => Err(BcmrError::Io(error)),
    }
}

#[cfg(target_os = "macos")]
fn clear_and_verify_private_transaction_acl(directory: &BoundDirectory) -> Result<(), BcmrError> {
    use std::os::fd::AsRawFd;

    type Acl = *mut std::ffi::c_void;
    const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;
    unsafe extern "C" {
        fn acl_init(count: libc::c_int) -> Acl;
        fn acl_set_fd_np(fd: libc::c_int, acl: Acl, acl_type: libc::c_int) -> libc::c_int;
        fn acl_free(object: *mut std::ffi::c_void) -> libc::c_int;
    }

    let empty_acl = unsafe { acl_init(0) };
    if empty_acl.is_null() {
        return Err(BcmrError::Io(std::io::Error::last_os_error()));
    }
    let set_result = unsafe {
        acl_set_fd_np(
            directory.handle.as_file().as_raw_fd(),
            empty_acl,
            ACL_TYPE_EXTENDED,
        )
    };
    let set_error = if set_result == 0 {
        None
    } else {
        Some(std::io::Error::last_os_error())
    };
    unsafe {
        acl_free(empty_acl);
    }
    if let Some(error) = set_error {
        if !error
            .raw_os_error()
            .is_some_and(|errno| [libc::ENOTSUP, libc::EOPNOTSUPP].contains(&errno))
        {
            return Err(BcmrError::Io(error));
        }
    }

    if macos_extended_acl_is_empty(directory.handle.as_file())? {
        Ok(())
    } else {
        Err(BcmrError::InvalidInput(format!(
            "atomic receive transaction '{}' retained an extended ACL",
            directory.path.display()
        )))
    }
}

#[cfg(target_os = "linux")]
fn clear_and_verify_private_transaction_acl(directory: &BoundDirectory) -> Result<(), BcmrError> {
    use xattr::FileExt;

    let file = directory.handle.as_file();
    // A default ACL controls only future children; it grants no access to
    // this 0700 directory. Retain it until the empty payload is created so
    // that the payload receives the same access ACL as a direct child.
    let name = "system.posix_acl_access";
    if let Err(error) = file.remove_xattr(name) {
        if !error
            .raw_os_error()
            .is_some_and(|errno| [libc::ENODATA, libc::ENOTSUP, libc::EOPNOTSUPP].contains(&errno))
        {
            return Err(BcmrError::Io(error));
        }
    }
    match file.get_xattr(name) {
        Ok(None) => {}
        Ok(Some(_)) => {
            return Err(BcmrError::InvalidInput(format!(
                "atomic receive transaction '{}' retained POSIX access ACL",
                directory.path.display(),
            )));
        }
        Err(error)
            if error
                .raw_os_error()
                .is_some_and(|errno| [libc::ENOTSUP, libc::EOPNOTSUPP].contains(&errno)) => {}
        Err(error) => return Err(BcmrError::Io(error)),
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn clear_and_verify_private_transaction_inheritance_acl(
    directory: &BoundDirectory,
) -> Result<(), BcmrError> {
    use xattr::FileExt;

    let file = directory.handle.as_file();
    let name = "system.posix_acl_default";
    if let Err(error) = file.remove_xattr(name) {
        if !error
            .raw_os_error()
            .is_some_and(|errno| [libc::ENODATA, libc::ENOTSUP, libc::EOPNOTSUPP].contains(&errno))
        {
            return Err(BcmrError::Io(error));
        }
    }
    match file.get_xattr(name) {
        Ok(None) => clear_and_verify_private_transaction_acl(directory),
        Ok(Some(_)) => Err(BcmrError::InvalidInput(format!(
            "atomic receive transaction '{}' retained POSIX default ACL",
            directory.path.display(),
        ))),
        Err(error)
            if error
                .raw_os_error()
                .is_some_and(|errno| [libc::ENOTSUP, libc::EOPNOTSUPP].contains(&errno)) =>
        {
            clear_and_verify_private_transaction_acl(directory)
        }
        Err(error) => Err(BcmrError::Io(error)),
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn clear_and_verify_private_transaction_inheritance_acl(
    directory: &BoundDirectory,
) -> Result<(), BcmrError> {
    clear_and_verify_private_transaction_acl(directory)
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))
))]
fn clear_and_verify_private_transaction_acl(directory: &BoundDirectory) -> Result<(), BcmrError> {
    Err(BcmrError::InvalidInput(format!(
        "atomic receive cannot prove that transaction '{}' has no inherited ACL on this platform",
        directory.path.display()
    )))
}

#[cfg(target_os = "freebsd")]
fn clear_and_verify_private_transaction_acl(directory: &BoundDirectory) -> Result<(), BcmrError> {
    use std::os::fd::AsRawFd;

    type Acl = *mut std::ffi::c_void;
    const ACL_TYPE_ACCESS: libc::c_int = 0x0000_0002;
    const ACL_TYPE_DEFAULT: libc::c_int = 0x0000_0003;
    const ACL_TYPE_NFS4: libc::c_int = 0x0000_0004;
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, acl_type: libc::c_int) -> Acl;
        fn acl_set_fd_np(fd: libc::c_int, acl: Acl, acl_type: libc::c_int) -> libc::c_int;
        fn acl_delete_fd_np(fd: libc::c_int, acl_type: libc::c_int) -> libc::c_int;
        fn acl_strip_np(acl: Acl, recalculate_mask: libc::c_int) -> Acl;
        fn acl_is_trivial_np(acl: Acl, trivial: *mut libc::c_int) -> libc::c_int;
        fn acl_free(object: *mut std::ffi::c_void) -> libc::c_int;
    }

    fn freebsd_acl_error_is_absent_or_unsupported(error: &std::io::Error) -> bool {
        error.raw_os_error().is_some_and(|errno| {
            [libc::EINVAL, libc::ENOENT, libc::ENOTSUP, libc::EOPNOTSUPP].contains(&errno)
        })
    }

    unsafe fn strip_acl_type(fd: libc::c_int, acl_type: libc::c_int) -> Result<bool, BcmrError> {
        let acl = unsafe { acl_get_fd_np(fd, acl_type) };
        if acl.is_null() {
            let error = std::io::Error::last_os_error();
            if freebsd_acl_error_is_absent_or_unsupported(&error) {
                return Ok(false);
            }
            return Err(BcmrError::Io(error));
        }
        let stripped = unsafe { acl_strip_np(acl, 1) };
        let strip_error = if stripped.is_null() {
            Some(std::io::Error::last_os_error())
        } else {
            None
        };
        unsafe {
            acl_free(acl);
        }
        if let Some(error) = strip_error {
            return Err(BcmrError::Io(error));
        }
        let set_result = unsafe { acl_set_fd_np(fd, stripped, acl_type) };
        let set_error = if set_result == 0 {
            None
        } else {
            Some(std::io::Error::last_os_error())
        };
        unsafe {
            acl_free(stripped);
        }
        if let Some(error) = set_error {
            return Err(BcmrError::Io(error));
        }
        Ok(true)
    }

    unsafe fn verify_trivial_acl_type(
        fd: libc::c_int,
        acl_type: libc::c_int,
    ) -> Result<bool, BcmrError> {
        let acl = unsafe { acl_get_fd_np(fd, acl_type) };
        if acl.is_null() {
            let error = std::io::Error::last_os_error();
            if freebsd_acl_error_is_absent_or_unsupported(&error) {
                return Ok(false);
            }
            return Err(BcmrError::Io(error));
        }
        let mut trivial = 0;
        let result = unsafe { acl_is_trivial_np(acl, &mut trivial) };
        let error = if result == 0 {
            None
        } else {
            Some(std::io::Error::last_os_error())
        };
        unsafe {
            acl_free(acl);
        }
        if let Some(error) = error {
            return Err(BcmrError::Io(error));
        }
        Ok(trivial == 1)
    }

    let fd = directory.handle.as_file().as_raw_fd();
    let used_nfs4 = unsafe { strip_acl_type(fd, ACL_TYPE_NFS4)? };
    if !used_nfs4 {
        unsafe {
            strip_acl_type(fd, ACL_TYPE_ACCESS)?;
        }
    }

    let delete_default = unsafe { acl_delete_fd_np(fd, ACL_TYPE_DEFAULT) };
    if delete_default != 0 {
        let error = std::io::Error::last_os_error();
        if !freebsd_acl_error_is_absent_or_unsupported(&error) {
            return Err(BcmrError::Io(error));
        }
    }

    let selected_type = if used_nfs4 {
        ACL_TYPE_NFS4
    } else {
        ACL_TYPE_ACCESS
    };
    if !unsafe { verify_trivial_acl_type(fd, selected_type)? } {
        return Err(BcmrError::InvalidInput(format!(
            "atomic receive transaction '{}' retained a non-trivial ACL",
            directory.path.display()
        )));
    }
    let default_acl = unsafe { acl_get_fd_np(fd, ACL_TYPE_DEFAULT) };
    if !default_acl.is_null() {
        unsafe {
            acl_free(default_acl);
        }
        return Err(BcmrError::InvalidInput(format!(
            "atomic receive transaction '{}' retained a default ACL",
            directory.path.display()
        )));
    }
    let default_error = std::io::Error::last_os_error();
    if !freebsd_acl_error_is_absent_or_unsupported(&default_error) {
        return Err(BcmrError::Io(default_error));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn linux_access_acl_is_absent(file: &File) -> Result<bool, BcmrError> {
    use xattr::FileExt;

    match file.get_xattr("system.posix_acl_access") {
        Ok(value) => Ok(value.is_none()),
        Err(error)
            if error
                .raw_os_error()
                .is_some_and(|errno| [libc::ENOTSUP, libc::EOPNOTSUPP].contains(&errno)) =>
        {
            Ok(true)
        }
        Err(error) => Err(BcmrError::Io(error)),
    }
}

#[cfg(unix)]
fn parent_namespace_is_private(directory: &BoundDirectory) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Ok(metadata) = directory.handle.as_file().metadata() else {
        return false;
    };
    let mode = metadata.mode();
    let owned_without_shared_write =
        metadata.uid() == unsafe { libc::geteuid() } && mode & 0o022 == 0;
    // Darwin exposes this mode bit through a narrower libc integer type,
    // while Linux aliases it to u32.
    #[allow(clippy::useless_conversion)]
    let sticky_namespace = mode & u32::from(libc::S_ISVTX) != 0;
    if !owned_without_shared_write && !sticky_namespace {
        return false;
    }

    #[cfg(target_os = "macos")]
    {
        macos_extended_acl_is_empty(directory.handle.as_file()).unwrap_or(false)
    }
    #[cfg(target_os = "linux")]
    {
        linux_access_acl_is_absent(directory.handle.as_file()).unwrap_or(false)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

#[cfg(unix)]
fn create_payload_at(
    directory: &BoundDirectory,
    name: &OsStr,
    mode: u32,
) -> Result<File, BcmrError> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let name = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            directory.handle.as_file().as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(BcmrError::Io(std::io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn destination_parent(path: &Path) -> Result<PathBuf, BcmrError> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => Ok(parent.to_path_buf()),
        Some(_) => Ok(PathBuf::from(".")),
        None => Err(BcmrError::InvalidInput(format!(
            "destination '{}' has no parent directory",
            path.display()
        ))),
    }
}

fn file_name(path: &Path) -> Result<&OsStr, BcmrError> {
    path.file_name().ok_or_else(|| {
        BcmrError::InvalidInput(format!("destination '{}' has no file name", path.display()))
    })
}

#[cfg(unix)]
fn preserve_existing_security(
    security_source: &File,
    snapshot: &ExistingSecuritySnapshot,
    staging: &File,
) -> Result<(), BcmrError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[cfg(not(target_os = "macos"))]
    let _ = security_source;

    let staging_metadata = staging.metadata()?;
    if snapshot.uid != staging_metadata.uid() || snapshot.gid != staging_metadata.gid() {
        let result = unsafe { libc::fchown(staging.as_raw_fd(), snapshot.uid, snapshot.gid) };
        if result != 0 {
            return Err(BcmrError::Io(std::io::Error::last_os_error()));
        }
    }

    #[cfg(target_os = "macos")]
    {
        let result = unsafe {
            libc::fcopyfile(
                security_source.as_raw_fd(),
                staging.as_raw_fd(),
                std::ptr::null_mut(),
                libc::COPYFILE_ACL,
            )
        };
        if result != 0 {
            return Err(BcmrError::Io(std::io::Error::last_os_error()));
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::collections::HashSet;
        use xattr::FileExt;

        let preserved: Vec<_> = snapshot
            .xattrs
            .iter()
            .filter(|(name, _)| should_preserve_existing_xattr(name))
            .collect();
        let source_set: HashSet<_> = preserved.iter().map(|(name, _)| name.clone()).collect();
        for name in staging.list_xattr()? {
            if !source_set.contains(&name) {
                staging.remove_xattr(&name)?;
            }
        }
        for (name, value) in preserved {
            staging.set_xattr(name, value)?;
        }
    }

    staging.set_permissions(std::fs::Permissions::from_mode(
        // New remote bytes must never inherit executable privilege from the
        // replaced inode. This mirrors ordinary write semantics, which clear
        // set-user-ID and set-group-ID bits.
        snapshot.mode & 0o0777,
    ))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn should_preserve_existing_xattr(name: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;

    !matches!(
        name.as_bytes(),
        b"security.capability" | b"security.ima" | b"security.evm"
    )
}

#[cfg(target_os = "macos")]
fn should_preserve_existing_xattr(_name: &OsStr) -> bool {
    true
}

#[cfg(unix)]
fn c_name(name: &OsStr) -> Result<std::ffi::CString, BcmrError> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(name.as_bytes()).map_err(|_| {
        BcmrError::InvalidInput("filesystem name unexpectedly contained a NUL byte".into())
    })
}

#[cfg(unix)]
fn snapshot_at(directory: &BoundDirectory, name: &OsStr) -> Result<EntrySnapshot, BcmrError> {
    use std::os::fd::AsRawFd;
    use std::os::fd::FromRawFd;

    let name = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            directory.handle.as_file().as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(BcmrError::Io(std::io::Error::last_os_error()));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    snapshot_from_file(&file)
}

#[cfg(not(unix))]
fn snapshot_at(directory: &BoundDirectory, name: &OsStr) -> Result<EntrySnapshot, BcmrError> {
    snapshot_from_path(&directory.path.join(name))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn rename_noreplace(
    from_dir: &BoundDirectory,
    from_name: &OsStr,
    to_dir: &BoundDirectory,
    to_name: &OsStr,
) -> Result<(), BcmrError> {
    use std::os::fd::AsRawFd;
    let from_name = c_name(from_name)?;
    let to_name = c_name(to_name)?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            from_dir.handle.as_file().as_raw_fd(),
            from_name.as_ptr(),
            to_dir.handle.as_file().as_raw_fd(),
            to_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(BcmrError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos"
))]
fn rename_noreplace(
    from_dir: &BoundDirectory,
    from_name: &OsStr,
    to_dir: &BoundDirectory,
    to_name: &OsStr,
) -> Result<(), BcmrError> {
    use std::os::fd::AsRawFd;
    let from_name = c_name(from_name)?;
    let to_name = c_name(to_name)?;
    let result = unsafe {
        libc::renameatx_np(
            from_dir.handle.as_file().as_raw_fd(),
            from_name.as_ptr(),
            to_dir.handle.as_file().as_raw_fd(),
            to_name.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result != 0 {
        return Err(BcmrError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos",
        target_os = "visionos"
    ))
))]
fn rename_noreplace(
    from_dir: &BoundDirectory,
    from_name: &OsStr,
    to_dir: &BoundDirectory,
    to_name: &OsStr,
) -> Result<(), BcmrError> {
    use std::os::fd::AsRawFd;
    let from_name = c_name(from_name)?;
    let to_name = c_name(to_name)?;
    let linked = unsafe {
        libc::linkat(
            from_dir.handle.as_file().as_raw_fd(),
            from_name.as_ptr(),
            to_dir.handle.as_file().as_raw_fd(),
            to_name.as_ptr(),
            0,
        )
    };
    if linked != 0 {
        return Err(BcmrError::Io(std::io::Error::last_os_error()));
    }
    let unlinked =
        unsafe { libc::unlinkat(from_dir.handle.as_file().as_raw_fd(), from_name.as_ptr(), 0) };
    // The no-clobber hard link above is already the committed destination.
    // Guarded transaction cleanup can retry an old-name unlink without
    // turning a successful publish into a fallback-triggering error.
    let _ = unlinked;
    Ok(())
}

#[cfg(windows)]
fn set_windows_handle_rename_noreplace(
    source: &File,
    root_directory: usize,
    destination_name: &[u16],
) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FileRenameInfo, SetFileInformationByHandle, FILE_RENAME_INFO,
    };

    if destination_name.is_empty() || destination_name.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Windows rename destination must be non-empty and contain no NUL",
        ));
    }
    let name_bytes = destination_name
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .ok_or_else(|| std::io::Error::other("Windows destination name is too long"))?;
    // FILE_RENAME_INFO declares FileName as a one-element trailing array.
    // Windows requires a buffer of at least sizeof(FILE_RENAME_INFO) plus the
    // variable filename bytes, rather than merely offset_of(FileName) plus the
    // bytes. The zero-filled extra WCHAR is harmless because FileNameLength is
    // authoritative, and also leaves a terminator for compatibility layers.
    let buffer_len = std::mem::size_of::<FILE_RENAME_INFO>()
        .checked_add(name_bytes)
        .ok_or_else(|| std::io::Error::other("Windows rename buffer is too large"))?;
    let file_name_length = u32::try_from(name_bytes)
        .map_err(|_| std::io::Error::other("Windows destination name is too long"))?;
    let buffer_size = u32::try_from(buffer_len)
        .map_err(|_| std::io::Error::other("Windows rename buffer is too large"))?;
    let word_size = std::mem::size_of::<usize>();
    let word_count = buffer_len.div_ceil(word_size);
    let mut buffer = vec![0usize; word_count];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        (*info).Anonymous.ReplaceIfExists = false;
        // A nonzero handle binds relative resolution to the validated parent.
        // RootDirectory=0 is reserved for the absolute-path compatibility
        // retry required by SMB and some Windows filesystems.
        (*info).RootDirectory = root_directory as HANDLE;
        (*info).FileNameLength = file_name_length;
        std::ptr::copy_nonoverlapping(
            destination_name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            destination_name.len(),
        );
    }
    if unsafe {
        SetFileInformationByHandle(
            source.as_raw_handle() as HANDLE,
            FileRenameInfo,
            buffer.as_ptr().cast(),
            buffer_size,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn windows_root_relative_retry_error(raw_os_error: Option<i32>) -> bool {
    const ERROR_NOT_SUPPORTED: i32 = 50;
    const ERROR_INVALID_PARAMETER: i32 = 87;
    matches!(
        raw_os_error,
        Some(ERROR_NOT_SUPPORTED) | Some(ERROR_INVALID_PARAMETER)
    )
}

#[cfg(any(windows, test))]
fn windows_absolute_rename_name(absolute: &[u16]) -> std::io::Result<Vec<u16>> {
    const SEP: u16 = b'\\' as u16;
    const QUERY: u16 = b'?' as u16;
    const DOT: u16 = b'.' as u16;
    const COLON: u16 = b':' as u16;
    const VERBATIM_PREFIX: &[u16] = &[SEP, SEP, QUERY, SEP];
    const NT_PREFIX: &[u16] = &[SEP, QUERY, QUERY, SEP];
    const UNC_PREFIX: &[u16] = &[
        SEP,
        SEP,
        QUERY,
        SEP,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        SEP,
    ];
    const LEGACY_MAX_PATH: usize = 248;

    if absolute.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Windows rename destination contains an interior NUL",
        ));
    }
    if absolute.starts_with(VERBATIM_PREFIX) || absolute.starts_with(NT_PREFIX) {
        return Ok(absolute.to_vec());
    }
    if absolute.len().saturating_add(1) < LEGACY_MAX_PATH {
        return Ok(absolute.to_vec());
    }

    let mut output = Vec::with_capacity(absolute.len() + UNC_PREFIX.len());
    match absolute {
        [drive, COLON, SEP, ..] if *drive != SEP => {
            output.extend_from_slice(VERBATIM_PREFIX);
            output.extend_from_slice(absolute);
        }
        [SEP, SEP, DOT, SEP, rest @ ..] => {
            output.extend_from_slice(VERBATIM_PREFIX);
            output.extend_from_slice(rest);
        }
        [SEP, SEP, rest @ ..] => {
            output.extend_from_slice(UNC_PREFIX);
            output.extend_from_slice(rest);
        }
        _ => output.extend_from_slice(absolute),
    }
    Ok(output)
}

#[cfg(windows)]
fn rename_handle_noreplace(
    source: &File,
    to_dir: &BoundDirectory,
    to_name: &OsStr,
) -> Result<(), BcmrError> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;

    let basename: Vec<u16> = to_name.encode_wide().collect();
    if basename.is_empty()
        || basename.iter().any(|character| {
            *character == 0 || *character == u16::from(b'/') || *character == u16::from(b'\\')
        })
    {
        return Err(BcmrError::InvalidInput(
            "Windows atomic publish requires a single non-empty destination name".into(),
        ));
    }

    let root_directory = to_dir.handle.as_file().as_raw_handle() as usize;
    match set_windows_handle_rename_noreplace(source, root_directory, &basename) {
        Ok(()) => Ok(()),
        Err(error) if windows_root_relative_retry_error(error.raw_os_error()) => {
            // MS-FSCC requires RootDirectory=0 for network rename requests.
            // Retry through the same retained source handle with an absolute
            // destination. Revalidate the bound parent immediately before the
            // fallback so a stale path is never used knowingly.
            if !to_dir.matches_path()? {
                return Err(BcmrError::DestinationChanged(to_dir.path.join(to_name)));
            }
            let absolute_destination = std::path::absolute(to_dir.path.join(to_name))?;
            let absolute_wide: Vec<u16> = absolute_destination.as_os_str().encode_wide().collect();
            let absolute_name = windows_absolute_rename_name(&absolute_wide)?;
            set_windows_handle_rename_noreplace(source, 0, &absolute_name).map_err(BcmrError::Io)
        }
        Err(error) => Err(BcmrError::Io(error)),
    }
}

#[cfg(windows)]
fn mark_handle_for_deletion(file: &File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
    };

    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as HANDLE,
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn rename_noreplace(
    from_dir: &BoundDirectory,
    from_name: &OsStr,
    to_dir: &BoundDirectory,
    to_name: &OsStr,
) -> Result<(), BcmrError> {
    let from = from_dir.path.join(from_name);
    let to = to_dir.path.join(to_name);
    std::fs::hard_link(&from, &to)?;
    std::fs::remove_file(from)?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn exchange(
    from_dir: &BoundDirectory,
    from_name: &OsStr,
    to_dir: &BoundDirectory,
    to_name: &OsStr,
) -> Result<(), BcmrError> {
    use std::os::fd::AsRawFd;
    let from_name = c_name(from_name)?;
    let to_name = c_name(to_name)?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            from_dir.handle.as_file().as_raw_fd(),
            from_name.as_ptr(),
            to_dir.handle.as_file().as_raw_fd(),
            to_name.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result != 0 {
        return Err(BcmrError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos"
))]
fn exchange(
    from_dir: &BoundDirectory,
    from_name: &OsStr,
    to_dir: &BoundDirectory,
    to_name: &OsStr,
) -> Result<(), BcmrError> {
    use std::os::fd::AsRawFd;
    let from_name = c_name(from_name)?;
    let to_name = c_name(to_name)?;
    let result = unsafe {
        libc::renameatx_np(
            from_dir.handle.as_file().as_raw_fd(),
            from_name.as_ptr(),
            to_dir.handle.as_file().as_raw_fd(),
            to_name.as_ptr(),
            libc::RENAME_SWAP,
        )
    };
    if result != 0 {
        return Err(BcmrError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos",
        target_os = "visionos"
    ))
))]
fn exchange(
    _from_dir: &BoundDirectory,
    _from_name: &OsStr,
    _to_dir: &BoundDirectory,
    _to_name: &OsStr,
) -> Result<(), BcmrError> {
    Err(BcmrError::InvalidInput(
        "this filesystem platform does not provide safe atomic exchange for replacing an existing destination"
            .into(),
    ))
}

#[cfg(windows)]
fn exchange(
    _from_dir: &BoundDirectory,
    _from_name: &OsStr,
    _to_dir: &BoundDirectory,
    _to_name: &OsStr,
) -> Result<(), BcmrError> {
    Err(BcmrError::InvalidInput(
        "strict atomic replacement of an existing destination is unavailable on Windows; refusing before mutation"
            .into(),
    ))
}

#[cfg(not(any(unix, windows)))]
fn exchange(
    _from_dir: &BoundDirectory,
    _from_name: &OsStr,
    _to_dir: &BoundDirectory,
    _to_name: &OsStr,
) -> Result<(), BcmrError> {
    Err(BcmrError::InvalidInput(
        "this filesystem platform does not provide safe atomic exchange for replacing an existing destination"
            .into(),
    ))
}

pub(crate) struct AtomicFile {
    destination: PathBuf,
    destination_name: std::ffi::OsString,
    parent: BoundDirectory,
    #[cfg(not(windows))]
    transaction: tempfile::TempDir,
    #[cfg(not(windows))]
    transaction_binding: BoundDirectory,
    staging_name: std::ffi::OsString,
    staging_path: PathBuf,
    staging_identity: EntryIdentity,
    file: Option<File>,
    #[cfg(unix)]
    delete_transaction_on_drop: bool,
    #[cfg(windows)]
    delete_staging_on_drop: bool,
    observed: DestinationObservation,
    #[cfg(target_os = "macos")]
    inherited_file_acl: Option<MacosInheritedFileAcl>,
}

impl AtomicFile {
    pub(crate) fn new(destination: &Path) -> Result<Self, BcmrError> {
        Self::new_with_overwrite_policy(destination, true)
    }

    #[allow(dead_code)]
    pub(crate) fn new_no_replace(destination: &Path) -> Result<Self, BcmrError> {
        Self::new_with_overwrite_policy(destination, false)
    }

    fn new_with_overwrite_policy(
        destination: &Path,
        allow_overwrite: bool,
    ) -> Result<Self, BcmrError> {
        let parent_path = destination_parent(destination)?;
        let destination_name = file_name(destination)?.to_os_string();
        let parent = BoundDirectory::capture(&parent_path)?;
        let observed = DestinationObservation::capture(destination)?;
        #[cfg(target_os = "macos")]
        let inherited_file_acl = if matches!(&observed, DestinationObservation::Missing) {
            capture_macos_inherited_file_acl(parent.handle.as_file())?
        } else {
            None
        };
        if !allow_overwrite && matches!(&observed, DestinationObservation::Existing(_)) {
            return Err(BcmrError::TargetExists(destination.to_path_buf()));
        }
        #[cfg(windows)]
        if matches!(&observed, DestinationObservation::Existing(_)) {
            return Err(BcmrError::InvalidInput(
                "strict atomic replacement of an existing destination is unavailable on Windows; refusing before staging any data"
                    .into(),
            ));
        }

        #[cfg(not(windows))]
        let mut transaction = tempfile::Builder::new()
            .prefix(".bcmr.receive.")
            .tempdir_in(&parent_path)?;
        #[cfg(not(windows))]
        transaction.disable_cleanup(true);
        #[cfg(unix)]
        let transaction_mode = {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let parent_mode = parent.handle.as_file().metadata()?.mode();
            // Preserve only setgid inheritance from the final parent. The
            // transaction stays private (0700), while its payload receives the
            // same group inheritance as a direct child of the destination.
            #[allow(clippy::useless_conversion)]
            let transaction_mode = 0o700 | (parent_mode & u32::from(libc::S_ISGID));
            std::fs::set_permissions(
                transaction.path(),
                std::fs::Permissions::from_mode(transaction_mode),
            )?;
            transaction_mode
        };
        if !parent.matches_path()? {
            return Err(BcmrError::DestinationChanged(destination.to_path_buf()));
        }
        #[cfg(not(windows))]
        let transaction_binding = BoundDirectory::capture(transaction.path())?;
        #[cfg(unix)]
        validate_private_transaction(
            &transaction_binding,
            unsafe { libc::geteuid() },
            transaction_mode,
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            clear_and_verify_private_transaction_acl(&transaction_binding)?;
            transaction_binding
                .handle
                .as_file()
                .set_permissions(std::fs::Permissions::from_mode(transaction_mode))?;
            validate_private_transaction(
                &transaction_binding,
                unsafe { libc::geteuid() },
                transaction_mode,
            )?;
            clear_and_verify_private_transaction_acl(&transaction_binding)?;
        }

        #[cfg(not(windows))]
        let (file, staging_name, staging_path) = {
            let staging_name = std::ffi::OsString::from("payload");
            let staging_path = transaction.path().join(&staging_name);
            #[cfg(unix)]
            let file = create_payload_at(&transaction_binding, &staging_name, 0o666)?;
            #[cfg(not(unix))]
            let file = {
                let mut options = OpenOptions::new();
                options.read(true).write(true).create_new(true);
                options.open(&staging_path)?
            };
            (file, staging_name, staging_path)
        };
        // The payload has now inherited any safe platform ACL template.
        // Remove that template before untrusted bytes are written.
        #[cfg(unix)]
        clear_and_verify_private_transaction_inheritance_acl(&transaction_binding)?;
        #[cfg(windows)]
        let (file, staging_name, staging_path) = {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{
                DELETE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_DELETE,
            };

            let mut builder = tempfile::Builder::new();
            builder
                .prefix(".bcmr.receive.")
                .suffix(".payload")
                // Cleanup is bound to the retained file handle below. A
                // path-based TempPath cleanup could delete an unrelated file
                // if the parent namespace were concurrently replaced.
                .disable_cleanup(true);
            let named = builder.make_in(&parent_path, |path| {
                let mut options = OpenOptions::new();
                options
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .access_mode(FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE)
                    // Duplicate handles inside this process still work, while
                    // other processes cannot read or write the in-flight file.
                    .share_mode(FILE_SHARE_DELETE);
                options.open(path)
            })?;
            let staging_path = named.path().to_path_buf();
            let staging_name = file_name(&staging_path)?.to_os_string();
            let file = named.into_file();
            (file, staging_name, staging_path)
        };
        if !parent.matches_path()? {
            #[cfg(windows)]
            let _ = mark_handle_for_deletion(&file);
            return Err(BcmrError::DestinationChanged(destination.to_path_buf()));
        }
        let staging_identity = match identity_from_file(&file) {
            Ok(identity) => identity,
            Err(error) => {
                #[cfg(windows)]
                let _ = mark_handle_for_deletion(&file);
                return Err(error);
            }
        };
        #[cfg(windows)]
        {
            let staged = match snapshot_at(&parent, &staging_name) {
                Ok(staged) => staged,
                Err(error) => {
                    let _ = mark_handle_for_deletion(&file);
                    return Err(error);
                }
            };
            if staged.identity != staging_identity {
                let _ = mark_handle_for_deletion(&file);
                return Err(BcmrError::DestinationChanged(destination.to_path_buf()));
            }
        }
        Ok(Self {
            destination: destination.to_path_buf(),
            destination_name,
            parent,
            #[cfg(not(windows))]
            transaction,
            #[cfg(not(windows))]
            transaction_binding,
            staging_name,
            staging_path,
            staging_identity,
            file: Some(file),
            #[cfg(unix)]
            delete_transaction_on_drop: true,
            #[cfg(windows)]
            delete_staging_on_drop: true,
            observed,
            #[cfg(target_os = "macos")]
            inherited_file_acl,
        })
    }

    pub(crate) fn staging_path(&self) -> PathBuf {
        self.staging_path.clone()
    }

    pub(crate) fn try_clone_file(&self) -> Result<File, BcmrError> {
        self.file
            .as_ref()
            .ok_or_else(|| {
                BcmrError::InvalidInput("atomic receive lost its retained file handle".into())
            })?
            .try_clone()
            .map_err(BcmrError::Io)
    }

    fn stage_matches_path(&self) -> Result<bool, BcmrError> {
        #[cfg(windows)]
        let directory = &self.parent;
        #[cfg(not(windows))]
        let directory = &self.transaction_binding;
        Ok(snapshot_at(directory, &self.staging_name)?.identity == self.staging_identity)
    }

    fn apply_security_metadata(&self) -> Result<(), BcmrError> {
        #[cfg(unix)]
        if let DestinationObservation::Existing(existing) = &self.observed {
            preserve_existing_security(
                &existing.security_source,
                &existing.security_snapshot,
                self.file.as_ref().ok_or_else(|| {
                    BcmrError::InvalidInput("atomic receive lost its retained file handle".into())
                })?,
            )?;
        }
        #[cfg(target_os = "macos")]
        if let (DestinationObservation::Missing, Some(inherited_file_acl), Some(file)) =
            (&self.observed, &self.inherited_file_acl, self.file.as_ref())
        {
            apply_macos_inherited_file_acl(file, inherited_file_acl)?;
        }
        Ok(())
    }

    fn preserve_uncertain_transaction(&mut self, detail: &str) -> BcmrError {
        #[cfg(not(windows))]
        {
            self.transaction.disable_cleanup(true);
        }
        #[cfg(unix)]
        {
            self.delete_transaction_on_drop = false;
        }
        #[cfg(windows)]
        {
            // A handle-bound rename may already have published the file. Never
            // mark that retained handle for deletion after the namespace
            // result becomes uncertain.
            self.delete_staging_on_drop = false;
        }
        #[cfg(not(windows))]
        let recovery_path = self.transaction.path();
        #[cfg(windows)]
        let recovery_path = self.staging_path.as_path();
        #[cfg(not(windows))]
        {
            BcmrError::InvalidInput(format!(
                "atomic publish state became uncertain for '{}': {detail}; recovery data retained at '{}'",
                self.destination.display(),
                recovery_path.display()
            ))
        }
        #[cfg(windows)]
        {
            BcmrError::InvalidInput(format!(
                "atomic publish state became uncertain for '{}': {detail}; recovery data may be retained at '{}' or at the destination",
                self.destination.display(),
                recovery_path.display()
            ))
        }
    }

    #[cfg(windows)]
    fn publish_missing_windows_with<R>(&mut self, rename: R) -> Result<(), BcmrError>
    where
        R: FnOnce(&File, &BoundDirectory, &OsStr) -> Result<(), BcmrError>,
    {
        let file = self.file.as_ref().ok_or_else(|| {
            BcmrError::InvalidInput("atomic receive lost its retained file handle".into())
        })?;
        // From this point onward the namespace outcome may become
        // indeterminate (notably across SMB). Default to preserving the
        // retained file object even if the call or surrounding code unwinds.
        self.delete_staging_on_drop = false;
        if let Err(error) = rename(file, &self.parent, &self.destination_name) {
            // On SMB and other remote filesystems an error can be reported
            // after the server has already committed the rename. Deleting the
            // retained handle here could therefore delete a successfully
            // published destination. Preserve the file object and require
            // explicit recovery instead.
            return Err(self.preserve_uncertain_transaction(&format!(
                "Windows handle-bound rename returned an error with an indeterminate namespace outcome: {error}"
            )));
        }
        // The retained handle now names the published destination. It must
        // never be marked for deletion even if post-publish revalidation
        // reports an uncertain state.
        Ok(())
    }

    fn publish_missing(&mut self) -> Result<(), BcmrError> {
        #[cfg(windows)]
        self.publish_missing_windows_with(rename_handle_noreplace)?;
        #[cfg(not(windows))]
        rename_noreplace(
            &self.transaction_binding,
            &self.staging_name,
            &self.parent,
            &self.destination_name,
        )?;
        #[cfg(windows)]
        if !self.staging_identity.windows.stable_across_rename() {
            // The legacy 64-bit file index is the compatibility fallback for
            // old Windows and SMB servers. On FAT-family filesystems it may
            // legitimately change during rename. The publication itself was
            // still performed by the retained handle with replace disabled,
            // so a path-based recheck would add incompatibility, not safety.
            return Ok(());
        }
        match snapshot_at(&self.parent, &self.destination_name) {
            Ok(snapshot) if snapshot.identity == self.staging_identity => Ok(()),
            Ok(_) => Err(self.preserve_uncertain_transaction(
                "the published destination no longer names the staged file",
            )),
            Err(error) => Err(self.preserve_uncertain_transaction(&format!(
                "the published destination could not be revalidated: {error}"
            ))),
        }
    }

    fn publish_existing(
        &mut self,
        expected_identity: &EntryIdentity,
        expected_fingerprint: &DestinationFingerprint,
    ) -> Result<(), BcmrError> {
        self.publish_existing_with_hooks(
            expected_identity,
            expected_fingerprint,
            || {},
            || {},
            || {},
        )
    }

    #[cfg(all(test, not(windows)))]
    fn publish_existing_with_hook<F>(
        &mut self,
        expected_identity: &EntryIdentity,
        expected_fingerprint: &DestinationFingerprint,
        after_exchange: F,
    ) -> Result<(), BcmrError>
    where
        F: FnOnce(),
    {
        self.publish_existing_with_hooks(
            expected_identity,
            expected_fingerprint,
            || {},
            after_exchange,
            || {},
        )
    }

    fn publish_existing_with_hooks<B, F, R>(
        &mut self,
        expected_identity: &EntryIdentity,
        expected_fingerprint: &DestinationFingerprint,
        before_exchange: B,
        after_exchange: F,
        before_failure_resolution: R,
    ) -> Result<(), BcmrError>
    where
        B: FnOnce(),
        F: FnOnce(),
        R: FnOnce(),
    {
        before_exchange();
        #[cfg(windows)]
        let staging_directory = &self.parent;
        #[cfg(not(windows))]
        let staging_directory = &self.transaction_binding;
        exchange(
            staging_directory,
            &self.staging_name,
            &self.parent,
            &self.destination_name,
        )?;
        after_exchange();

        let displaced_name = self.staging_name.as_os_str();

        let published_snapshot = snapshot_at(&self.parent, &self.destination_name);
        let displaced_snapshot = snapshot_at(staging_directory, displaced_name);
        let destination_still_names_stage = published_snapshot
            .as_ref()
            .is_ok_and(|snapshot| snapshot.identity == self.staging_identity);
        #[cfg(unix)]
        let displaced_security_matches = match &self.observed {
            DestinationObservation::Existing(existing) => {
                ExistingSecuritySnapshot::capture(&existing.security_source)
                    .is_ok_and(|snapshot| snapshot == existing.security_snapshot)
            }
            DestinationObservation::Missing => false,
        };
        #[cfg(not(unix))]
        let displaced_security_matches = true;
        if destination_still_names_stage
            && displaced_snapshot
                .as_ref()
                .is_ok_and(|snapshot| snapshot.identity == *expected_identity)
            && displaced_snapshot.as_ref().is_ok_and(|snapshot| {
                snapshot
                    .fingerprint
                    .matches_after_namespace_move(expected_fingerprint)
            })
            && displaced_security_matches
        {
            return Ok(());
        }
        if !destination_still_names_stage {
            return Err(self.preserve_uncertain_transaction(
                "the destination stopped naming the staged file after exchange; refusing automatic rollback because it could overwrite a newer competing entry",
            ));
        }
        before_failure_resolution();
        Err(self.preserve_uncertain_transaction(
            "the entry displaced during exchange did not match the preflight destination; automatic rollback is unsafe, so recovery data was retained",
        ))
    }

    #[cfg(not(windows))]
    fn remove_displaced_existing(
        &mut self,
        expected_identity: &EntryIdentity,
        expected_fingerprint: &DestinationFingerprint,
    ) -> Result<(), BcmrError> {
        let displaced = snapshot_at(&self.transaction_binding, &self.staging_name);
        #[cfg(unix)]
        let displaced_security_matches = match &self.observed {
            DestinationObservation::Existing(existing) => {
                ExistingSecuritySnapshot::capture(&existing.security_source)
                    .is_ok_and(|snapshot| snapshot == existing.security_snapshot)
            }
            DestinationObservation::Missing => false,
        };
        #[cfg(not(unix))]
        let displaced_security_matches = true;
        if !displaced.as_ref().is_ok_and(|snapshot| {
            snapshot.identity == *expected_identity
                && snapshot
                    .fingerprint
                    .matches_after_namespace_move(expected_fingerprint)
        }) || !displaced_security_matches
        {
            return Err(self.preserve_uncertain_transaction(
                "the durable displaced destination changed before cleanup; recovery data was retained",
            ));
        }

        #[cfg(unix)]
        let removed = {
            use std::os::fd::AsRawFd;
            let name = c_name(&self.staging_name)?;
            let result = unsafe {
                libc::unlinkat(
                    self.transaction_binding.handle.as_file().as_raw_fd(),
                    name.as_ptr(),
                    0,
                )
            };
            if result == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        };
        #[cfg(not(unix))]
        let removed = std::fs::remove_file(&self.staging_path);
        if let Err(error) = removed {
            return Err(self.preserve_uncertain_transaction(&format!(
                "the displaced destination could not be removed from '{}': {error}",
                self.staging_path.display()
            )));
        }
        Ok(())
    }

    fn commit_with_syncers<F, S, D>(
        mut self,
        sync_before_publish: bool,
        metadata: Option<PortableFileMetadata>,
        file_syncer: S,
        mut directory_syncer: D,
        before_publish: F,
    ) -> Result<(), BcmrError>
    where
        F: FnOnce(),
        S: FnOnce(&File) -> std::io::Result<()>,
        D: FnMut(&BoundDirectory) -> std::io::Result<()>,
    {
        #[cfg(windows)]
        if sync_before_publish {
            return Err(BcmrError::InvalidInput(
                "--sync cannot guarantee durable namespace publication on Windows; refusing before mutation"
                    .into(),
            ));
        }

        #[cfg(not(windows))]
        let staging_directory_matches = self.transaction_binding.matches_path()?;
        #[cfg(windows)]
        let staging_directory_matches = true;
        if !self.parent.matches_path()?
            || !staging_directory_matches
            || !self.stage_matches_path()?
        {
            return Err(self.preserve_uncertain_transaction(
                "the destination parent, transaction directory, or staged entry changed before metadata and durability processing",
            ));
        }
        if !self.observed.matches_path(&self.destination)? {
            return Err(BcmrError::DestinationChanged(self.destination.clone()));
        }

        self.apply_security_metadata()?;
        if let Some(metadata) = metadata {
            metadata.apply_to(self.file.as_ref().ok_or_else(|| {
                BcmrError::InvalidInput("atomic receive lost its retained file handle".into())
            })?)?;
        }
        if sync_before_publish {
            file_syncer(self.file.as_ref().ok_or_else(|| {
                BcmrError::InvalidInput("atomic receive lost its retained file handle".into())
            })?)?;
        }
        if !self.stage_matches_path()? {
            return Err(BcmrError::DestinationChanged(self.destination.clone()));
        }

        before_publish();
        #[cfg(not(windows))]
        let staging_directory_matches = self.transaction_binding.matches_path()?;
        #[cfg(windows)]
        let staging_directory_matches = true;
        if !self.parent.matches_path()?
            || !staging_directory_matches
            || !self.stage_matches_path()?
        {
            return Err(self.preserve_uncertain_transaction(
                "the destination parent, transaction directory, or staged entry changed at the final pre-publish check",
            ));
        }
        if !self.observed.matches_path(&self.destination)? {
            return Err(BcmrError::DestinationChanged(self.destination.clone()));
        }
        let existing_state = match &self.observed {
            DestinationObservation::Missing => None,
            DestinationObservation::Existing(existing) => {
                Some((existing.identity.clone(), existing.fingerprint.clone()))
            }
        };
        let published = match &existing_state {
            None => self.publish_missing(),
            Some((expected_identity, expected_fingerprint)) => {
                // The platform exchange retains the displaced entry. If its
                // identity or stable fingerprint differs from preflight, keep
                // the transaction for explicit recovery instead of attempting
                // another namespace mutation.
                self.publish_existing(expected_identity, expected_fingerprint)
            }
        };
        published?;
        #[cfg(not(windows))]
        {
            if sync_before_publish {
                // First make both sides of the cross-directory namespace
                // mutation durable while the displaced destination is still
                // recoverable.
                if let Err(error) = directory_syncer(&self.parent) {
                    return Err(self.preserve_uncertain_transaction(&format!(
                        "the published destination directory could not be made durable before recovery cleanup: {error}"
                    )));
                }
                if let Err(error) = directory_syncer(&self.transaction_binding) {
                    return Err(self.preserve_uncertain_transaction(&format!(
                        "the recovery transaction directory could not be made durable before cleanup: {error}"
                    )));
                }
            }
            if let Some((expected_identity, expected_fingerprint)) = &existing_state {
                self.remove_displaced_existing(expected_identity, expected_fingerprint)?;
                if sync_before_publish {
                    if let Err(error) = directory_syncer(&self.transaction_binding) {
                        return Err(BcmrError::InvalidInput(format!(
                            "the destination publish is durable, but durability of displaced-file cleanup is uncertain: {error}; the transaction directory was retained"
                        )));
                    }
                }
            }
            if !self.transaction_binding.matches_path()? {
                return Err(self.preserve_uncertain_transaction(
                    "the transaction directory changed before successful cleanup",
                ));
            }
            let transaction_path = self.transaction.path().to_path_buf();
            #[cfg(unix)]
            let removed_transaction = {
                use std::os::fd::AsRawFd;
                let transaction_name = file_name(&transaction_path)?;
                let transaction_name = c_name(transaction_name)?;
                let result = unsafe {
                    libc::unlinkat(
                        self.parent.handle.as_file().as_raw_fd(),
                        transaction_name.as_ptr(),
                        libc::AT_REMOVEDIR,
                    )
                };
                if result == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            };
            #[cfg(not(unix))]
            let removed_transaction = std::fs::remove_dir(&transaction_path);
            if let Err(error) = removed_transaction {
                return Err(self.preserve_uncertain_transaction(&format!(
                    "the completed transaction directory could not be removed: {error}"
                )));
            }
            self.transaction.disable_cleanup(true);
        }
        if sync_before_publish {
            if let Err(error) = directory_syncer(&self.parent) {
                return Err(BcmrError::InvalidInput(format!(
                    "the destination publish is durable, but durability of transaction-directory cleanup is uncertain: {error}"
                )));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn commit_with_hook<F>(
        self,
        sync_before_publish: bool,
        before_publish: F,
    ) -> Result<(), BcmrError>
    where
        F: FnOnce(),
    {
        self.commit_with_syncers(
            sync_before_publish,
            None,
            crate::core::io::durable_sync,
            BoundDirectory::durable_sync,
            before_publish,
        )
    }

    pub(crate) fn commit(self, sync_before_publish: bool) -> Result<(), BcmrError> {
        self.commit_with_syncers(
            sync_before_publish,
            None,
            crate::core::io::durable_sync,
            BoundDirectory::durable_sync,
            || {},
        )
    }

    pub(crate) fn commit_with_metadata(
        self,
        sync_before_publish: bool,
        metadata: PortableFileMetadata,
    ) -> Result<(), BcmrError> {
        self.commit_with_syncers(
            sync_before_publish,
            Some(metadata),
            crate::core::io::durable_sync,
            BoundDirectory::durable_sync,
            || {},
        )
    }
}

#[cfg(not(windows))]
impl Drop for AtomicFile {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;

            if !self.delete_transaction_on_drop || !self.stage_matches_path().unwrap_or(false) {
                return;
            }
            let Ok(staging_name) = c_name(&self.staging_name) else {
                return;
            };
            let removed_stage = unsafe {
                libc::unlinkat(
                    self.transaction_binding.handle.as_file().as_raw_fd(),
                    staging_name.as_ptr(),
                    0,
                )
            };
            if removed_stage != 0 || !parent_namespace_is_private(&self.parent) {
                return;
            }
            if !self.transaction_binding.matches_path().unwrap_or(false) {
                return;
            }
            let Some(transaction_name) = self.transaction.path().file_name() else {
                return;
            };
            let Ok(transaction_name) = c_name(transaction_name) else {
                return;
            };
            // The parent has no group/other writer and no extended access ACL,
            // or it is a sticky namespace. Under that proof, no other UID can
            // swap this owned directory entry between validation and rmdir.
            // AT_REMOVEDIR also refuses non-empty substitutions.
            let _ = unsafe {
                libc::unlinkat(
                    self.parent.handle.as_file().as_raw_fd(),
                    transaction_name.as_ptr(),
                    libc::AT_REMOVEDIR,
                )
            };
        }
    }
}

#[cfg(windows)]
impl Drop for AtomicFile {
    fn drop(&mut self) {
        if self.delete_staging_on_drop {
            if let Some(file) = self.file.as_ref() {
                // Cleanup is tied to the exact staged file object, not to a
                // pathname that may have been concurrently rebound.
                let _ = mark_handle_for_deletion(file);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use super::capture_macos_acl;
    use super::AtomicFile;
    #[cfg(unix)]
    use super::{create_payload_at, validate_private_transaction, BoundDirectory};
    use std::io::Write;

    #[cfg(target_os = "macos")]
    fn macos_extended_acl_entry_count(file: &std::fs::File) -> usize {
        use std::os::fd::AsRawFd;

        type Acl = *mut std::ffi::c_void;
        type AclEntry = *mut std::ffi::c_void;
        const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;
        const ACL_FIRST_ENTRY: libc::c_int = 0;
        const ACL_NEXT_ENTRY: libc::c_int = -1;
        unsafe extern "C" {
            fn acl_get_fd_np(fd: libc::c_int, acl_type: libc::c_int) -> Acl;
            fn acl_get_entry(acl: Acl, entry_id: libc::c_int, entry: *mut AclEntry) -> libc::c_int;
            fn acl_free(object: *mut std::ffi::c_void) -> libc::c_int;
        }

        let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
        if acl.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOENT) {
                return 0;
            }
            panic!("failed to read transaction ACL: {error}");
        }

        let mut count = 0usize;
        let mut entry: AclEntry = std::ptr::null_mut();
        let mut selector = ACL_FIRST_ENTRY;
        loop {
            let result = unsafe { acl_get_entry(acl, selector, &mut entry) };
            if result == 0 {
                count += 1;
                selector = ACL_NEXT_ENTRY;
                continue;
            }
            let error = std::io::Error::last_os_error();
            if result == -1 && error.raw_os_error() == Some(libc::EINVAL) {
                break;
            }
            panic!("acl_get_entry failed: {error}");
        }
        unsafe {
            acl_free(acl);
        }
        count
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn private_transaction_removes_inherited_macos_acl() {
        let parent = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("/bin/chmod")
            .arg("+a")
            .arg(
                "everyone allow list,search,add_file,add_subdirectory,\
                 file_inherit,directory_inherit",
            )
            .arg(parent.path())
            .status()
            .unwrap();
        assert!(status.success(), "failed to install inherited test ACL");
        let parent_handle = std::fs::File::open(parent.path()).unwrap();
        assert_eq!(
            macos_extended_acl_entry_count(&parent_handle),
            1,
            "the test parent must expose one inheritable ACL entry"
        );

        let stage = AtomicFile::new(&parent.path().join("destination.bin")).unwrap();
        assert_eq!(
            macos_extended_acl_entry_count(stage.transaction_binding.handle.as_file()),
            0,
            "the private transaction must not retain an inherited ACL entry"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn new_file_inherits_the_final_parent_macos_acl() {
        let parent = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("/bin/chmod")
            .arg("+a")
            .arg("everyone allow read,file_inherit")
            .arg(parent.path())
            .status()
            .unwrap();
        assert!(status.success(), "failed to install inherited test ACL");

        let reference_path = parent.path().join("reference.bin");
        std::fs::File::create(&reference_path).unwrap();
        let reference = std::fs::File::open(&reference_path).unwrap();
        assert_eq!(
            macos_extended_acl_entry_count(&reference),
            1,
            "the test parent must grant one inherited file ACL entry"
        );

        let destination = parent.path().join("destination.bin");
        let stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"received")
            .unwrap();
        stage.commit(false).unwrap();

        let published = std::fs::File::open(&destination).unwrap();
        assert_eq!(
            macos_extended_acl_entry_count(&published),
            macos_extended_acl_entry_count(&reference),
            "atomic publication must preserve the ACL a direct child would inherit"
        );
        assert_eq!(
            capture_macos_acl(&published).unwrap(),
            capture_macos_acl(&reference).unwrap(),
            "the inherited ACE contents and flags must match direct creation"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn new_file_inherits_the_final_parent_linux_default_acl() {
        use xattr::FileExt;

        let parent = tempfile::tempdir().unwrap();
        let inherited_uid = unsafe { libc::geteuid() }.saturating_add(1);
        let status = match std::process::Command::new("setfacl")
            .args([
                "-m",
                &format!("default:user:{inherited_uid}:r--"),
                parent.path().to_str().unwrap(),
            ])
            .status()
        {
            Ok(status) => status,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => panic!("failed to run setfacl: {error}"),
        };
        assert!(status.success(), "failed to install default test ACL");

        let reference_path = parent.path().join("reference.bin");
        let reference = std::fs::File::create(&reference_path).unwrap();
        let reference_acl = reference
            .get_xattr("system.posix_acl_access")
            .unwrap()
            .expect("the reference file must inherit an access ACL");

        let destination = parent.path().join("destination.bin");
        let stage = AtomicFile::new(&destination).unwrap();
        assert_eq!(
            stage
                .transaction_binding
                .handle
                .as_file()
                .get_xattr("system.posix_acl_default")
                .unwrap(),
            None,
            "the private transaction must drop its default ACL after creating the payload"
        );
        let staged_file = stage.try_clone_file().unwrap();
        assert_eq!(
            staged_file
                .get_xattr("system.posix_acl_access")
                .unwrap()
                .as_deref(),
            Some(reference_acl.as_slice()),
            "the private payload must inherit exactly the access ACL of a direct child"
        );
        staged_file
            .try_clone()
            .unwrap()
            .write_all(b"received")
            .unwrap();
        stage.commit(false).unwrap();

        let published = std::fs::File::open(&destination).unwrap();
        assert_eq!(
            published
                .get_xattr("system.posix_acl_access")
                .unwrap()
                .as_deref(),
            Some(reference_acl.as_slice()),
            "atomic publication must retain the inherited Linux access ACL"
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_transaction_validation_rejects_wrong_owner_or_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let transaction = dir.path().join("transaction");
        std::fs::create_dir(&transaction).unwrap();
        std::fs::set_permissions(&transaction, std::fs::Permissions::from_mode(0o700)).unwrap();
        let binding = BoundDirectory::capture(&transaction).unwrap();
        let uid = unsafe { libc::geteuid() };

        validate_private_transaction(&binding, uid, 0o700).unwrap();
        let other_uid = if uid == u32::MAX { uid - 1 } else { uid + 1 };
        assert!(validate_private_transaction(&binding, other_uid, 0o700).is_err());

        std::fs::set_permissions(&transaction, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(validate_private_transaction(&binding, uid, 0o700).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn payload_creation_is_bound_to_the_pinned_transaction_directory() {
        use std::ffi::OsStr;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let transaction = dir.path().join("transaction");
        let displaced = dir.path().join("displaced-transaction");
        std::fs::create_dir(&transaction).unwrap();
        std::fs::set_permissions(&transaction, std::fs::Permissions::from_mode(0o700)).unwrap();
        let binding = BoundDirectory::capture(&transaction).unwrap();
        validate_private_transaction(&binding, unsafe { libc::geteuid() }, 0o700).unwrap();

        std::fs::rename(&transaction, &displaced).unwrap();
        std::fs::create_dir(&transaction).unwrap();
        std::fs::set_permissions(&transaction, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mut payload = create_payload_at(&binding, OsStr::new("payload"), 0o666).unwrap();
        payload.write_all(b"pinned payload").unwrap();
        drop(payload);

        assert_eq!(
            std::fs::read(displaced.join("payload")).unwrap(),
            b"pinned payload"
        );
        assert!(
            !transaction.join("payload").exists(),
            "payload creation must not re-resolve a substituted transaction pathname"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn commit_refuses_a_replaced_staging_path() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        std::fs::write(&destination, b"original destination").unwrap();

        let stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"received payload")
            .unwrap();
        let staging_path = stage.staging_path();
        let displaced_stage = staging_path.with_file_name("displaced-stage.bin");
        std::fs::rename(&staging_path, &displaced_stage).unwrap();
        std::fs::write(&staging_path, b"attacker replacement").unwrap();

        let result = stage.commit(false);
        assert!(
            result.is_err(),
            "a path that no longer names the retained staging handle must not publish"
        );
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"original destination"
        );
        assert_eq!(
            std::fs::read(&staging_path).unwrap(),
            b"attacker replacement",
            "failed identity validation must not let path cleanup delete a substituted entry"
        );
        assert_eq!(
            std::fs::read(&displaced_stage).unwrap(),
            b"received payload",
            "the owned staged payload must remain available for explicit recovery"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn commit_revalidates_staging_after_the_final_publish_hook() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        std::fs::write(&destination, b"original destination").unwrap();

        let stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"received payload")
            .unwrap();
        let staging_path = stage.staging_path();
        let displaced_stage = staging_path.with_file_name("displaced-stage.bin");

        let result = stage.commit_with_hook(false, || {
            std::fs::rename(&staging_path, &displaced_stage).unwrap();
            std::fs::write(&staging_path, b"attacker replacement").unwrap();
        });

        assert!(result.is_err());
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"original destination",
            "a substituted transaction entry must be rejected before the atomic exchange"
        );
        assert_eq!(
            std::fs::read(&staging_path).unwrap(),
            b"attacker replacement"
        );
        assert_eq!(
            std::fs::read(&displaced_stage).unwrap(),
            b"received payload"
        );
    }

    #[cfg(unix)]
    #[test]
    fn drop_never_deletes_a_substituted_staging_entry() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        let stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"owned payload")
            .unwrap();
        let staging_path = stage.staging_path();
        let displaced_stage = staging_path.with_file_name("displaced-stage.bin");
        std::fs::rename(&staging_path, &displaced_stage).unwrap();
        std::fs::write(&staging_path, b"attacker replacement").unwrap();

        drop(stage);

        assert_eq!(
            std::fs::read(staging_path).unwrap(),
            b"attacker replacement"
        );
        assert_eq!(std::fs::read(displaced_stage).unwrap(), b"owned payload");
    }

    #[cfg(unix)]
    #[test]
    fn drop_never_recursively_deletes_a_substituted_transaction_directory() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        let stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"owned payload")
            .unwrap();
        let transaction_path = stage.transaction.path().to_path_buf();
        let displaced_transaction = dir.path().join("displaced-transaction");
        std::fs::rename(&transaction_path, &displaced_transaction).unwrap();
        std::fs::create_dir(&transaction_path).unwrap();
        std::fs::write(transaction_path.join("attacker.bin"), b"do not delete").unwrap();

        drop(stage);

        assert_eq!(
            std::fs::read(transaction_path.join("attacker.bin")).unwrap(),
            b"do not delete"
        );
        assert!(
            displaced_transaction.exists()
                && !displaced_transaction.join("payload").exists(),
            "Drop may unlink its owned payload through the retained directory handle, but must not recursively remove or follow either transaction pathname"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ordinary_drop_removes_its_empty_transaction_in_a_private_parent() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let destination = dir.path().join("destination.bin");
        let stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"owned payload")
            .unwrap();
        let transaction_path = stage.transaction.path().to_path_buf();

        drop(stage);

        assert!(
            !transaction_path.exists(),
            "an ordinary failed transfer must not leak an empty transaction directory"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn drop_keeps_the_empty_transaction_when_parent_has_an_extended_acl() {
        let parent = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("/bin/chmod")
            .arg("+a")
            .arg("everyone allow list,search,add_file,add_subdirectory")
            .arg(parent.path())
            .status()
            .unwrap();
        assert!(status.success(), "failed to install parent test ACL");

        let stage = AtomicFile::new(&parent.path().join("destination.bin")).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"owned payload")
            .unwrap();
        let transaction_path = stage.transaction.path().to_path_buf();

        drop(stage);

        assert!(
            transaction_path.is_dir(),
            "Drop must not remove a directory entry from an ACL-shared parent namespace"
        );
        assert_eq!(
            std::fs::read_dir(transaction_path).unwrap().count(),
            0,
            "Drop may still unlink its owned payload through the retained transaction handle"
        );
    }

    #[cfg(unix)]
    #[test]
    fn uncertain_transaction_retains_its_payload_for_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        let mut stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"recovery payload")
            .unwrap();
        let transaction_path = stage.transaction.path().to_path_buf();
        let error = stage.preserve_uncertain_transaction("test injected uncertainty");
        assert!(error.to_string().contains("recovery data retained"));

        drop(stage);

        assert_eq!(
            std::fs::read(transaction_path.join("payload")).unwrap(),
            b"recovery payload",
            "an uncertain publish must not destroy the retained recovery payload"
        );
    }

    #[cfg(unix)]
    #[test]
    fn overwrite_does_not_widen_existing_file_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("private.bin");
        std::fs::write(&destination, b"private").unwrap();
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o600)).unwrap();

        let stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"replacement")
            .unwrap();
        stage.commit(false).unwrap();

        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn staging_directory_is_private_and_new_file_mode_matches_standard_create() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("new.bin");
        let reference = dir.path().join("reference.bin");
        std::fs::File::create(&reference).unwrap();
        let stage = AtomicFile::new(&destination).unwrap();
        let staging_path = stage.staging_path();

        assert_eq!(
            std::fs::metadata(staging_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&staging_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            std::fs::metadata(&reference)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            "the private transaction directory protects the staging file, while its eventual visible mode must match normal file creation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn staging_preserves_final_parent_setgid_inheritance() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o2770)).unwrap();
        let reference = dir.path().join("reference.bin");
        std::fs::File::create(&reference).unwrap();
        let destination = dir.path().join("destination.bin");

        let stage = AtomicFile::new(&destination).unwrap();
        let transaction = stage.staging_path().parent().unwrap().to_path_buf();
        let transaction_metadata = std::fs::metadata(&transaction).unwrap();
        let payload_metadata = std::fs::metadata(stage.staging_path()).unwrap();

        assert_eq!(
            transaction_metadata.permissions().mode() & 0o2777,
            0o2700,
            "a private intermediate must retain the parent's setgid inheritance"
        );
        assert_eq!(
            payload_metadata.gid(),
            std::fs::metadata(reference).unwrap().gid(),
            "the staged payload must inherit the same group as a direct child"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn overwrite_preserves_existing_extended_attributes() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("protected.bin");
        std::fs::write(&destination, b"old").unwrap();
        xattr::set(&destination, "user.bcmr-test", b"security-label").unwrap();

        let stage = AtomicFile::new(&destination).unwrap();
        stage.try_clone_file().unwrap().write_all(b"new").unwrap();
        stage.commit(false).unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"new");
        assert_eq!(
            xattr::get(&destination, "user.bcmr-test").unwrap(),
            Some(b"security-label".to_vec())
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn privileged_content_bound_xattrs_are_never_preserved() {
        use std::ffi::OsStr;

        assert!(!super::should_preserve_existing_xattr(OsStr::new(
            "security.capability"
        )));
        assert!(!super::should_preserve_existing_xattr(OsStr::new(
            "security.ima"
        )));
        assert!(!super::should_preserve_existing_xattr(OsStr::new(
            "security.evm"
        )));
        assert!(super::should_preserve_existing_xattr(OsStr::new(
            "user.bcmr-test"
        )));
        assert!(super::should_preserve_existing_xattr(OsStr::new(
            "system.posix_acl_access"
        )));
    }

    #[cfg(unix)]
    #[test]
    fn overwrite_drops_setuid_and_setgid_bits() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("privileged.bin");
        std::fs::write(&destination, b"old").unwrap();
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o6751)).unwrap();

        let stage = AtomicFile::new(&destination).unwrap();
        stage.try_clone_file().unwrap().write_all(b"new").unwrap();
        stage.commit(false).unwrap();

        let mode = std::fs::metadata(&destination)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o6000, 0, "remote bytes must not inherit privilege");
        assert_eq!(mode & 0o777, 0o751, "ordinary access bits are retained");
    }

    #[cfg(not(windows))]
    #[test]
    fn existing_overwrite_succeeds_without_a_concurrent_race() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        std::fs::write(&destination, b"old payload").unwrap();

        let stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"new payload")
            .unwrap();
        stage.commit(false).unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"new payload");
    }

    #[cfg(unix)]
    #[test]
    fn portable_metadata_is_applied_to_the_staging_handle_before_publish() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        let stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"payload")
            .unwrap();
        stage
            .commit_with_metadata(
                false,
                super::PortableFileMetadata {
                    atime_seconds: 1_600_000_000,
                    atime_nanoseconds: 0,
                    mtime_seconds: 1_600_000_123,
                    mtime_nanoseconds: 0,
                    mode: 0o6751,
                },
            )
            .unwrap();

        let metadata = std::fs::metadata(&destination).unwrap();
        assert_eq!(metadata.mtime(), 1_600_000_123);
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o0751);
    }

    #[test]
    fn initially_missing_destination_is_not_clobbered_at_publish() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        let stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"received payload")
            .unwrap();

        let result = stage.commit_with_hook(false, || {
            std::fs::write(&destination, b"concurrent creator").unwrap();
        });

        assert!(result.is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"concurrent creator");
    }

    #[cfg(windows)]
    #[test]
    fn windows_missing_destination_is_published_from_the_retained_handle() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("现代-目标-🛰️.bin");
        let stage = AtomicFile::new(&destination).unwrap();
        assert_eq!(
            stage.staging_path().parent(),
            destination.parent(),
            "Windows must stage as a direct child so handle-bound publication can use a same-directory rename and inherit the final parent's ACL"
        );
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"handle-bound payload")
            .unwrap();

        stage.commit(false).unwrap();

        assert_eq!(std::fs::read(destination).unwrap(), b"handle-bound payload");
    }

    #[test]
    fn windows_handle_rename_retries_only_unsupported_root_relative_requests() {
        for error in [50, 87] {
            assert!(super::windows_root_relative_retry_error(Some(error)));
        }
        for error in [1, 5, 32, 80, 120, 183] {
            assert!(!super::windows_root_relative_retry_error(Some(error)));
        }
        assert!(!super::windows_root_relative_retry_error(None));
    }

    #[test]
    fn windows_absolute_rename_name_preserves_unicode_and_supports_long_paths() {
        let unicode = r"C:\destination\现代-目标-🛰️.bin";
        let unicode_wide: Vec<u16> = unicode.encode_utf16().collect();
        assert_eq!(
            super::windows_absolute_rename_name(&unicode_wide).unwrap(),
            unicode_wide
        );

        let long_path = format!(r"C:\destination\{}\payload.bin", "x".repeat(300));
        let long_wide: Vec<u16> = long_path.encode_utf16().collect();
        assert_eq!(
            String::from_utf16(&super::windows_absolute_rename_name(&long_wide).unwrap()).unwrap(),
            format!(r"\\?\{long_path}")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_indeterminate_rename_error_never_deletes_a_published_handle() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        let mut stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"published despite reply loss")
            .unwrap();
        let staging_path = stage.staging_path();
        let moved_destination = destination.clone();

        let result = stage.publish_missing_windows_with(|_file, _parent, _name| {
            std::fs::rename(&staging_path, &moved_destination).unwrap();
            Err(super::BcmrError::Io(std::io::Error::other(
                "simulated lost SMB rename reply",
            )))
        });

        assert!(result.is_err());
        drop(stage);
        assert_eq!(
            std::fs::read(destination).unwrap(),
            b"published despite reply loss",
            "an indeterminate rename error must not mark the retained handle for deletion"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_abandoned_stage_is_deleted_by_retained_handle() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        let stage = AtomicFile::new(&destination).unwrap();
        let staging_path = stage.staging_path();

        drop(stage);

        assert!(
            !staging_path.exists(),
            "an ordinary failure must clean the exact staged file by handle"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn sync_failure_happens_before_publish() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        std::fs::write(&destination, b"original").unwrap();
        let stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"received payload")
            .unwrap();

        let result = stage.commit_with_syncers(
            true,
            None,
            |_file| Err(std::io::Error::other("injected sync failure")),
            |_directory| panic!("directory sync must not run after file sync failure"),
            || panic!("publish hook must not run after sync failure"),
        );

        assert!(result.is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"original");
    }

    #[cfg(not(windows))]
    #[test]
    fn directory_sync_failure_is_reported_after_atomic_publish() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        std::fs::write(&destination, b"original").unwrap();
        let stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"received payload")
            .unwrap();

        let result = stage.commit_with_syncers(
            true,
            None,
            |_file| Ok(()),
            |_directory| Err(std::io::Error::other("injected directory sync failure")),
            || {},
        );

        assert!(
            result.is_err(),
            "a requested durable publish must report a directory-entry flush failure"
        );
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"received payload",
            "the file publish is already atomic before the directory durability error"
        );
        let recovery = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap())
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".bcmr.receive.")
            })
            .expect("the displaced destination must survive a durability failure");
        assert_eq!(
            std::fs::read(recovery.path().join("payload")).unwrap(),
            b"original"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn successful_sync_durably_publishes_before_removing_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        let stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"payload")
            .unwrap();
        let transaction_path = stage.transaction.path().to_path_buf();
        let calls = std::cell::RefCell::new(Vec::new());

        stage
            .commit_with_syncers(
                true,
                None,
                |_file| Ok(()),
                |directory| {
                    let is_parent = directory.path == dir.path();
                    let call_index = calls.borrow().len();
                    if is_parent {
                        if call_index == 0 {
                            assert!(
                                transaction_path.exists(),
                                "the first parent flush must make publication durable while recovery still exists"
                            );
                        } else {
                            assert!(
                                !transaction_path.exists(),
                                "the final parent flush must make transaction removal durable"
                            );
                        }
                    } else {
                        assert!(
                            transaction_path.exists(),
                            "the transaction directory must still exist while it is flushed"
                        );
                    }
                    calls.borrow_mut().push(directory.path.clone());
                    Ok(())
                },
                || {},
            )
            .unwrap();

        assert_eq!(
            calls.into_inner(),
            vec![
                dir.path().to_path_buf(),
                transaction_path,
                dir.path().to_path_buf()
            ]
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn overwrite_sync_keeps_recovery_until_both_publish_directories_are_durable() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        std::fs::write(&destination, b"original").unwrap();
        let stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"replacement")
            .unwrap();
        let transaction_path = stage.transaction.path().to_path_buf();
        let recovery_path = transaction_path.join("payload");
        let calls = std::cell::RefCell::new(Vec::new());

        stage
            .commit_with_syncers(
                true,
                None,
                |_file| Ok(()),
                |directory| {
                    let call_index = calls.borrow().len();
                    match call_index {
                        0 | 1 => assert_eq!(
                            std::fs::read(&recovery_path).unwrap(),
                            b"original",
                            "the displaced destination must remain recoverable through both initial directory flushes"
                        ),
                        2 => assert!(
                            !recovery_path.exists() && transaction_path.exists(),
                            "the recovery entry removal must be flushed before removing its directory"
                        ),
                        3 => assert!(
                            !transaction_path.exists(),
                            "the final parent flush must occur after transaction removal"
                        ),
                        _ => panic!("unexpected directory sync call"),
                    }
                    calls.borrow_mut().push(directory.path.clone());
                    Ok(())
                },
                || {},
            )
            .unwrap();

        assert_eq!(
            calls.into_inner(),
            vec![
                dir.path().to_path_buf(),
                transaction_path.clone(),
                transaction_path,
                dir.path().to_path_buf()
            ]
        );
        assert_eq!(std::fs::read(destination).unwrap(), b"replacement");
    }

    #[cfg(not(windows))]
    #[test]
    fn existing_destination_replacement_is_refused_before_exchange() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let destination = dir.path().join("destination.bin");
        let replacement = dir.path().join("replacement.bin");
        std::fs::write(&destination, b"original").unwrap();
        std::fs::write(&replacement, b"concurrent replacement").unwrap();
        let stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"received payload")
            .unwrap();

        let result = stage.commit_with_hook(false, || {
            std::fs::rename(&replacement, &destination).unwrap();
        });

        assert!(result.is_err());
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"concurrent replacement"
        );
        assert!(
            std::fs::read_dir(dir.path())
                .unwrap()
                .map(|entry| entry.unwrap())
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".bcmr.receive.")),
            "a pre-publish race with a known outcome should remove the owned payload and its empty transaction"
        );
    }

    #[cfg(unix)]
    #[test]
    fn post_exchange_destination_replacement_is_never_overwritten_by_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        let replacement = dir.path().join("replacement.bin");
        std::fs::write(&destination, b"original").unwrap();
        std::fs::write(&replacement, b"post-publish competitor").unwrap();
        let mut stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"received payload")
            .unwrap();
        let (expected_identity, expected_fingerprint) = match &stage.observed {
            super::DestinationObservation::Existing(existing) => {
                (existing.identity.clone(), existing.fingerprint.clone())
            }
            super::DestinationObservation::Missing => panic!("destination was created above"),
        };

        let result =
            stage.publish_existing_with_hook(&expected_identity, &expected_fingerprint, || {
                std::fs::rename(&replacement, &destination).unwrap();
            });

        assert!(result.is_err());
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"post-publish competitor",
            "rollback must not replace a destination that stopped naming the staged file"
        );
        let recovery = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap())
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".bcmr.receive.")
            })
            .expect("uncertain exchange must retain a recovery transaction");
        assert_eq!(
            std::fs::read(recovery.path().join("payload")).unwrap(),
            b"original",
            "the displaced preflight destination must remain recoverable"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rollback_never_clobbers_a_destination_replaced_after_identity_check() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        let pre_exchange_replacement = dir.path().join("pre-exchange.bin");
        let post_check_replacement = dir.path().join("post-check.bin");
        std::fs::write(&destination, b"original").unwrap();
        std::fs::write(&pre_exchange_replacement, b"first competitor").unwrap();
        std::fs::write(&post_check_replacement, b"last competitor").unwrap();
        let mut stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"received payload")
            .unwrap();
        let (expected_identity, expected_fingerprint) = match &stage.observed {
            super::DestinationObservation::Existing(existing) => {
                (existing.identity.clone(), existing.fingerprint.clone())
            }
            super::DestinationObservation::Missing => panic!("destination was created above"),
        };

        std::fs::rename(&pre_exchange_replacement, &destination).unwrap();
        let result = stage.publish_existing_with_hooks(
            &expected_identity,
            &expected_fingerprint,
            || {},
            || {},
            || {
                std::fs::rename(&post_check_replacement, &destination).unwrap();
            },
        );

        assert!(result.is_err());
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"last competitor",
            "failure recovery must never overwrite a destination that changed after its last identity check"
        );
        let recovery = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap())
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".bcmr.receive.")
            })
            .expect("unsafe rollback must retain the displaced entry");
        assert_eq!(
            std::fs::read(recovery.path().join("payload")).unwrap(),
            b"first competitor"
        );
    }

    #[cfg(unix)]
    #[test]
    fn in_place_destination_mutation_is_retained_instead_of_deleted_after_exchange() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        std::fs::write(&destination, b"original").unwrap();
        let mut stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"received")
            .unwrap();
        let (expected_identity, expected_fingerprint) = match &stage.observed {
            super::DestinationObservation::Existing(existing) => {
                (existing.identity.clone(), existing.fingerprint.clone())
            }
            super::DestinationObservation::Missing => panic!("destination was created above"),
        };
        let original_mode = std::fs::metadata(&destination)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let changed_mode = if original_mode == 0o600 { 0o640 } else { 0o600 };

        let result = stage.publish_existing_with_hooks(
            &expected_identity,
            &expected_fingerprint,
            || {
                std::fs::write(&destination, b"mutation").unwrap();
                std::fs::set_permissions(
                    &destination,
                    std::fs::Permissions::from_mode(changed_mode),
                )
                .unwrap();
            },
            || {},
            || {},
        );

        assert!(
            result.is_err(),
            "same-inode content/metadata mutation must not be accepted as the preflight destination"
        );
        let recovery = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap())
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".bcmr.receive.")
            })
            .expect("the in-place update must be retained in a recovery transaction");
        assert_eq!(
            std::fs::read(recovery.path().join("payload")).unwrap(),
            b"mutation"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn concurrent_security_metadata_update_is_retained_for_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        std::fs::write(&destination, b"original").unwrap();
        xattr::set(&destination, "user.bcmr-test", b"original-label").unwrap();
        let mut stage = AtomicFile::new(&destination).unwrap();
        stage
            .try_clone_file()
            .unwrap()
            .write_all(b"received")
            .unwrap();
        let (expected_identity, expected_fingerprint) = match &stage.observed {
            super::DestinationObservation::Existing(existing) => {
                (existing.identity.clone(), existing.fingerprint.clone())
            }
            super::DestinationObservation::Missing => panic!("destination was created above"),
        };

        let result = stage.publish_existing_with_hooks(
            &expected_identity,
            &expected_fingerprint,
            || {
                xattr::set(&destination, "user.bcmr-test", b"racing-label").unwrap();
            },
            || {},
            || {},
        );

        assert!(
            result.is_err(),
            "a racing ACL/xattr update must never be silently discarded"
        );
        let recovery = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap())
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".bcmr.receive.")
            })
            .expect("the security update must remain in a recovery transaction");
        assert_eq!(
            xattr::get(recovery.path().join("payload"), "user.bcmr-test").unwrap(),
            Some(b"racing-label".to_vec())
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_identity_falls_back_when_modern_file_ids_are_unavailable() {
        let modern_error = super::BcmrError::Io(std::io::Error::from_raw_os_error(50));
        let legacy = super::WindowsFileIdentity::Legacy {
            volume: 7,
            identifier: 11,
        };

        assert_eq!(
            super::select_windows_file_identity(Err(modern_error), || Ok(legacy.clone())).unwrap(),
            legacy
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_existing_destination_is_rejected_before_staging() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        std::fs::write(&destination, b"original").unwrap();

        let error = match AtomicFile::new(&destination) {
            Ok(_) => panic!("Windows overwrite staging must fail closed"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("unavailable on Windows"));
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "the unsupported overwrite must not create a transaction"
        );
    }
}
