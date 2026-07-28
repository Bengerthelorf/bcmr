use crate::cli::{Commands, SparseMode, TestMode};
use crate::core::error::BcmrError;

use std::fs::File as StdFile;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempPath;
use tokio::fs::{self, File};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

use super::exec::ProgressCallback;
use crate::core::cleanup::TempFileGuard;

#[derive(Debug, Eq, PartialEq)]
struct DestinationFingerprint {
    len: u64,
    modified: Option<std::time::SystemTime>,
    kind: u8,
    readonly: bool,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    mtime_seconds: i64,
    #[cfg(unix)]
    mtime_nanoseconds: i64,
    #[cfg(unix)]
    ctime_seconds: i64,
    #[cfg(unix)]
    ctime_nanoseconds: i64,
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

        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            kind,
            readonly: metadata.permissions().readonly(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            mode: metadata.mode(),
            #[cfg(unix)]
            mtime_seconds: metadata.mtime(),
            #[cfg(unix)]
            mtime_nanoseconds: metadata.mtime_nsec(),
            #[cfg(unix)]
            ctime_seconds: metadata.ctime(),
            #[cfg(unix)]
            ctime_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug)]
struct DestinationTargetObservation {
    identity: same_file::Handle,
    fingerprint: DestinationFingerprint,
    link_count: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct DestinationObservation {
    entry: Option<DestinationFingerprint>,
    target: Option<DestinationTargetObservation>,
    observe_target: bool,
}

impl DestinationObservation {
    fn capture(path: &Path, observe_target: bool) -> Result<Self, BcmrError> {
        let entry = match std::fs::symlink_metadata(path) {
            Ok(metadata) => Some(DestinationFingerprint::from_metadata(&metadata)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(BcmrError::Io(error)),
        };
        let target = if observe_target
            && entry
                .as_ref()
                .is_some_and(|fingerprint| fingerprint.kind == 1)
        {
            match open_destination_for_observation(path) {
                Ok(file) => {
                    let metadata = file.metadata()?;
                    let fingerprint = DestinationFingerprint::from_metadata(&metadata);
                    if entry.as_ref() != Some(&fingerprint) {
                        return Err(BcmrError::DestinationChanged(path.to_path_buf()));
                    }
                    let link_count = file_link_count(&file)?;
                    let identity = same_file::Handle::from_file(file)?;
                    Some(DestinationTargetObservation {
                        identity,
                        fingerprint,
                        link_count,
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(BcmrError::Io(error)),
            }
        } else {
            None
        };
        Ok(Self {
            entry,
            target,
            observe_target,
        })
    }

    fn matches_path(&self, path: &Path) -> Result<bool, BcmrError> {
        let current = Self::capture(path, self.observe_target)?;
        if self.entry != current.entry {
            return Ok(false);
        }
        Ok(match (&self.target, &current.target) {
            (None, None) => true,
            (Some(expected), Some(current)) => {
                expected.identity == current.identity
                    && expected.fingerprint == current.fingerprint
                    && observed_link_count_matches(expected.link_count, current.link_count)
            }
            _ => false,
        })
    }

    fn matches_file(&self, file: &StdFile) -> Result<bool, BcmrError> {
        let Some(expected) = self.target.as_ref() else {
            return Ok(false);
        };
        let fingerprint = DestinationFingerprint::from_metadata(&file.metadata()?);
        let link_count = file_link_count(file)?;
        let identity = same_file::Handle::from_file(file.try_clone()?)?;
        Ok(expected.identity == identity
            && expected.fingerprint == fingerprint
            && observed_link_count_matches(expected.link_count, link_count))
    }

    fn try_clone_target_file(&self) -> Result<Option<StdFile>, BcmrError> {
        self.target
            .as_ref()
            .map(|target| target.identity.as_file().try_clone().map_err(BcmrError::Io))
            .transpose()
    }

    fn target_has_multiple_links(&self) -> bool {
        self.target
            .as_ref()
            .and_then(|target| target.link_count)
            .is_some_and(|link_count| link_count > 1)
    }
}

fn observed_link_count_matches(expected: Option<u64>, current: Option<u64>) -> bool {
    expected == current
}

#[cfg(unix)]
fn file_link_count(file: &StdFile) -> Result<Option<u64>, BcmrError> {
    use std::os::unix::fs::MetadataExt;
    Ok(Some(file.metadata()?.nlink()))
}

#[cfg(windows)]
fn file_link_count(file: &StdFile) -> Result<Option<u64>, BcmrError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut information = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    let result = unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
    };
    if result == 0 {
        return Err(BcmrError::Io(std::io::Error::last_os_error()));
    }
    let information = unsafe { information.assume_init() };
    Ok(Some(information.nNumberOfLinks as u64))
}

#[cfg(not(any(unix, windows)))]
fn file_link_count(_file: &StdFile) -> Result<Option<u64>, BcmrError> {
    Ok(None)
}

fn open_destination_for_observation(path: &Path) -> std::io::Result<StdFile> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        // Observe the directory entry itself if it becomes a reparse point
        // during the metadata-to-open race; never follow a newly swapped link.
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options
            .share_mode(FILE_SHARE_DELETE | FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

pub(crate) fn destination_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn destination_entry_metadata(path: &Path) -> Result<Option<std::fs::Metadata>, BcmrError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(BcmrError::Io(error)),
    }
}

pub(crate) fn validate_direct_destination(
    dst: &Path,
    metadata: Option<&std::fs::Metadata>,
    replace_existing: bool,
) -> Result<bool, BcmrError> {
    let Some(metadata) = metadata else {
        return Ok(false);
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        if replace_existing {
            // Forced direct modes intentionally replace the directory entry
            // and therefore must not inspect or follow the symlink target.
            return Ok(true);
        }
        return Err(BcmrError::InvalidInput(format!(
            "direct copy destination '{}' is a symbolic link; use -f to replace the link entry",
            dst.display()
        )));
    }
    if !file_type.is_file() {
        return Err(BcmrError::InvalidInput(format!(
            "direct copy destination '{}' must be a regular file",
            dst.display()
        )));
    }
    if !replace_existing && has_multiple_hard_links(dst, metadata)? {
        return Err(BcmrError::InvalidInput(format!(
            "direct copy destination '{}' has multiple hard links; use -f to replace only this path",
            dst.display()
        )));
    }
    Ok(false)
}

#[cfg(unix)]
fn has_multiple_hard_links(_path: &Path, metadata: &std::fs::Metadata) -> Result<bool, BcmrError> {
    use std::os::unix::fs::MetadataExt;
    Ok(metadata.nlink() > 1)
}

#[cfg(windows)]
fn has_multiple_hard_links(path: &Path, _metadata: &std::fs::Metadata) -> Result<bool, BcmrError> {
    let file = open_destination_for_observation(path)?;
    Ok(file_link_count(&file)?.is_some_and(|link_count| link_count > 1))
}

#[cfg(not(any(unix, windows)))]
fn has_multiple_hard_links(_path: &Path, _metadata: &std::fs::Metadata) -> Result<bool, BcmrError> {
    Ok(false)
}

pub(crate) enum CommitPolicy {
    NoClobber,
    ReplaceObserved(Arc<DestinationObservation>),
    ReplaceAny,
}

#[cfg(any(windows, test))]
const WINDOWS_FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
#[cfg(any(windows, test))]
const WINDOWS_FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
#[cfg(test)]
const WINDOWS_FILE_ATTRIBUTE_TEMPORARY: u32 = 0x0000_0100;
#[cfg(any(windows, test))]
const WINDOWS_SETTABLE_SPECIFIC_ATTRIBUTES: u32 = WINDOWS_FILE_ATTRIBUTE_READONLY
        | 0x0000_0002 // HIDDEN
        | 0x0000_0004 // SYSTEM
        | 0x0000_0020 // ARCHIVE
        | 0x0000_1000 // OFFLINE
        | 0x0000_2000; // NOT_CONTENT_INDEXED

#[cfg(any(windows, test))]
fn windows_attributes_after_persist(attributes: u32, preserved_readonly: Option<bool>) -> u32 {
    // SetFileAttributesW rejects status bits such as COMPRESSED, SPARSE_FILE,
    // ENCRYPTED, and REPARSE_POINT. Passing only its documented settable bits
    // leaves those filesystem-managed states unchanged.
    let mut specific = attributes & WINDOWS_SETTABLE_SPECIFIC_ATTRIBUTES;
    match preserved_readonly {
        Some(true) => specific |= WINDOWS_FILE_ATTRIBUTE_READONLY,
        Some(false) => specific &= !WINDOWS_FILE_ATTRIBUTE_READONLY,
        None => {}
    }
    if specific == 0 {
        WINDOWS_FILE_ATTRIBUTE_NORMAL
    } else {
        specific
    }
}

#[cfg(any(windows, test))]
fn windows_attributes_for_failed_stage_cleanup(attributes: u32) -> u32 {
    let specific =
        attributes & (WINDOWS_SETTABLE_SPECIFIC_ATTRIBUTES & !WINDOWS_FILE_ATTRIBUTE_READONLY);
    if specific == 0 {
        WINDOWS_FILE_ATTRIBUTE_NORMAL
    } else {
        specific
    }
}

#[cfg(windows)]
fn persist_windows_stage(
    stage: &Path,
    dst: &Path,
    replace_existing: bool,
    preserved_readonly: Option<bool>,
) -> Result<(), BcmrError> {
    use std::iter;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileAttributesW, SetFileAttributesW, INVALID_FILE_ATTRIBUTES,
    };

    let stage_w: Vec<u16> = stage
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    let original_attributes = unsafe { GetFileAttributesW(stage_w.as_ptr()) };
    if original_attributes == INVALID_FILE_ATTRIBUTES {
        return Err(BcmrError::Io(std::io::Error::last_os_error()));
    }
    let persisted_attributes =
        windows_attributes_after_persist(original_attributes, preserved_readonly);
    if unsafe { SetFileAttributesW(stage_w.as_ptr(), persisted_attributes) } == 0 {
        let error = std::io::Error::last_os_error();
        let cleanup_attributes = windows_attributes_for_failed_stage_cleanup(original_attributes);
        let _ = unsafe { SetFileAttributesW(stage_w.as_ptr(), cleanup_attributes) };
        return Err(BcmrError::Io(error));
    }

    let dispatch = if replace_existing {
        super::symlinks::WindowsSymlinkCommitDispatch::HandleReplace
    } else {
        super::symlinks::WindowsSymlinkCommitDispatch::HandleNoClobber
    };
    let operation = super::symlinks::windows_rename_operation(dispatch);
    let error = match super::symlinks::persist_windows_symlink_by_handle(stage, dst, operation) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    // The stage is private and will be deleted after a failed commit. Make it
    // writable so Windows cleanup cannot leak a preserved read-only stage.
    let cleanup_attributes = windows_attributes_for_failed_stage_cleanup(original_attributes);
    let _ = unsafe { SetFileAttributesW(stage_w.as_ptr(), cleanup_attributes) };
    if replace_existing
        && super::symlinks::windows_extended_rename_unavailable(error.raw_os_error())
    {
        Err(BcmrError::InvalidInput(format!(
            "cannot atomically replace '{}' on this Windows version, filesystem, or destination: \
             FileRenameInfoEx with replace-if-exists and POSIX semantics is unavailable; \
             the existing destination was preserved ({error})",
            dst.display()
        )))
    } else {
        Err(BcmrError::Io(error))
    }
}

struct DestinationWriterLock {
    _file: StdFile,
}

impl DestinationWriterLock {
    async fn acquire(dst: &Path) -> Result<Self, BcmrError> {
        let dst = dst.to_path_buf();
        tokio::task::spawn_blocking(move || Self::acquire_sync(&dst)).await?
    }

    fn acquire_sync(dst: &Path) -> Result<Self, BcmrError> {
        let file = open_writer_lock_file(dst)?;
        file.lock()?;
        Ok(Self { _file: file })
    }

    #[cfg(test)]
    fn try_acquire_sync(dst: &Path) -> Result<Option<Self>, BcmrError> {
        let file = open_writer_lock_file(dst)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(BcmrError::Io(error)),
        }
    }
}

fn open_writer_lock_file(dst: &Path) -> Result<StdFile, BcmrError> {
    let parent = destination_parent(dst);
    let canonical_parent = std::fs::canonicalize(parent)?;
    let normalized = match dst.file_name() {
        Some(file_name) => canonical_parent.join(file_name),
        None => canonical_parent,
    };
    let mut hasher = blake3::Hasher::new();
    hash_path(&mut hasher, &normalized);
    let key = hasher.finalize().to_hex();

    let lock_dir = writer_lock_dir()?;
    std::fs::create_dir_all(&lock_dir)?;
    let lock_dir_metadata = std::fs::symlink_metadata(&lock_dir)?;
    if lock_dir_metadata.file_type().is_symlink() || !lock_dir_metadata.is_dir() {
        return Err(BcmrError::InvalidInput(format!(
            "writer lock path is not a private directory: {}",
            lock_dir.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if lock_dir_metadata.uid() != unsafe { libc::geteuid() } {
            return Err(BcmrError::InvalidInput(format!(
                "writer lock directory is not owned by the current user: {}",
                lock_dir.display()
            )));
        }
        std::fs::set_permissions(&lock_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let lock_path = lock_dir.join(format!("{}.lock", &key[..32]));
    if let Ok(metadata) = std::fs::symlink_metadata(&lock_path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(BcmrError::InvalidInput(format!(
                "writer lock path is not a regular file: {}",
                lock_path.display()
            )));
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(lock_path)?;
    if !file.metadata()?.is_file() {
        return Err(BcmrError::InvalidInput(
            "writer lock handle is not a regular file".into(),
        ));
    }
    Ok(file)
}

fn writer_lock_dir() -> Result<std::path::PathBuf, BcmrError> {
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        return Ok(std::path::PathBuf::from(data_home)
            .join("bcmr")
            .join("writer-locks"));
    }
    directories::ProjectDirs::from("", "", "bcmr")
        .map(|directories| directories.data_local_dir().join("writer-locks"))
        .ok_or_else(|| {
            BcmrError::InvalidInput(
                "could not determine a private data directory for writer locks".into(),
            )
        })
}

#[cfg(unix)]
fn hash_path(hasher: &mut blake3::Hasher, path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    hasher.update(path.as_os_str().as_bytes());
}

#[cfg(windows)]
fn hash_path(hasher: &mut blake3::Hasher, path: &Path) {
    use std::os::windows::ffi::OsStrExt;
    for code_unit in path.as_os_str().encode_wide() {
        hasher.update(&code_unit.to_le_bytes());
    }
}

#[cfg(not(any(unix, windows)))]
fn hash_path(hasher: &mut blake3::Hasher, path: &Path) {
    hasher.update(path.to_string_lossy().as_bytes());
}

pub(crate) struct AtomicStaging {
    file: Option<StdFile>,
    path: TempPath,
    guard: TempFileGuard,
}

impl AtomicStaging {
    pub(crate) fn path(&self) -> &Path {
        self.path.as_ref()
    }

    fn try_clone_file(&self) -> Result<StdFile, BcmrError> {
        self.file
            .as_ref()
            .ok_or_else(|| {
                BcmrError::InvalidInput("atomic staging lost its retained file handle".into())
            })?
            .try_clone()
            .map_err(BcmrError::Io)
    }

    pub(crate) fn commit(
        self,
        dst: &Path,
        policy: &CommitPolicy,
        preserved_readonly: Option<bool>,
    ) -> Result<(), BcmrError> {
        let AtomicStaging {
            file,
            path,
            mut guard,
        } = self;
        // Windows cannot reliably rename a file while arbitrary handles remain
        // open.  Close the retained create-new handle before persisting.
        drop(file);
        if let CommitPolicy::ReplaceObserved(observed) = policy {
            if !observed.matches_path(dst)? {
                return Err(BcmrError::DestinationChanged(dst.to_path_buf()));
            }
        }
        #[cfg(windows)]
        {
            let mut path = path;
            let replace_existing = !matches!(policy, CommitPolicy::NoClobber);
            match persist_windows_stage(path.as_ref(), dst, replace_existing, preserved_readonly) {
                Ok(()) => {
                    path.disable_cleanup(true);
                    guard.disarm();
                    Ok(())
                }
                Err(error) => {
                    drop(path);
                    if !replace_existing
                        && matches!(
                            &error,
                            BcmrError::Io(error)
                                if super::symlinks::is_target_exists_error(error)
                        )
                    {
                        return Err(BcmrError::TargetExists(dst.to_path_buf()));
                    }
                    Err(error)
                }
            }
        }
        #[cfg(not(windows))]
        let _ = preserved_readonly;
        #[cfg(not(windows))]
        let persisted = match policy {
            CommitPolicy::NoClobber => path.persist_noclobber(dst),
            CommitPolicy::ReplaceObserved(_) | CommitPolicy::ReplaceAny => path.persist(dst),
        };
        #[cfg(not(windows))]
        match persisted {
            Ok(()) => {
                guard.disarm();
                Ok(())
            }
            Err(error) => {
                let is_target_exists = error.error.kind() == std::io::ErrorKind::AlreadyExists;
                drop(error.path);
                if matches!(policy, CommitPolicy::NoClobber) && is_target_exists {
                    Err(BcmrError::TargetExists(dst.to_path_buf()))
                } else {
                    Err(BcmrError::Io(error.error))
                }
            }
        }
    }

    #[cfg(any(target_os = "macos", test))]
    fn relinquish_cleanup(&mut self) {
        self.guard.disarm();
        self.path.disable_cleanup(true);
    }

    #[cfg(any(not(any(target_os = "linux", target_os = "macos")), test))]
    fn finish_unsupported_reflink(self, fail_on_error: bool) -> Result<(Self, bool), BcmrError> {
        if fail_on_error {
            Err(BcmrError::Reflink(
                "forced reflink is unsupported on this platform".into(),
            ))
        } else {
            Ok((self, false))
        }
    }

    #[cfg(any(target_os = "linux", test))]
    fn try_reflink_retained<F>(
        self,
        file_size: u64,
        fail_on_error: bool,
        operation: F,
    ) -> Result<(Self, bool), BcmrError>
    where
        F: FnOnce(&StdFile) -> std::io::Result<()>,
    {
        if file_size == 0 {
            return Ok((self, true));
        }

        let stage_file = self.file.as_ref().ok_or_else(|| {
            BcmrError::InvalidInput("atomic staging lost its retained file handle".into())
        })?;
        match operation(stage_file) {
            Ok(()) => Ok((self, true)),
            Err(error) if fail_on_error => Err(BcmrError::Reflink(format!(
                "Reflink failed (forced): {error}"
            ))),
            Err(_) => {
                self.file
                    .as_ref()
                    .expect("retained staging file checked above")
                    .set_len(0)?;
                Ok((self, false))
            }
        }
    }

    #[cfg(any(target_os = "macos", test))]
    fn try_reflink_create_new<F>(
        mut self,
        fail_on_error: bool,
        operation: F,
    ) -> Result<(Self, bool), BcmrError>
    where
        F: FnOnce(&Path) -> std::io::Result<()>,
    {
        // clonefile requires an absent destination.  Close the original
        // reservation before unlinking it so finalize never carries a stale
        // live handle into the final rename.
        drop(self.file.take());
        std::fs::remove_file(self.path())?;
        match operation(self.path()) {
            Ok(()) => Ok((self, true)),
            Err(error) if fail_on_error => {
                // macOS clonefile creates the destination atomically.  On
                // failure, an EEXIST path may belong to a competing creator.
                self.relinquish_cleanup();
                Err(BcmrError::Reflink(format!(
                    "Reflink failed (forced): {error}"
                )))
            }
            Err(_) => match create_new_stage_file(self.path()) {
                Ok(file) => {
                    self.file = Some(file);
                    Ok((self, false))
                }
                Err(error) => {
                    self.relinquish_cleanup();
                    Err(BcmrError::Io(error))
                }
            },
        }
    }
}

#[cfg(any(target_os = "macos", test))]
fn create_new_stage_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o666);
    }
    options.open(path)
}

#[cfg(any(target_os = "linux", test))]
fn reset_copy_file_range_stage_for_fallback(file: &std::fs::File) -> std::io::Result<()> {
    file.set_len(0)
}

pub(crate) fn create_staging(dst: &Path) -> Result<AtomicStaging, BcmrError> {
    let parent = destination_parent(dst);
    let mut builder = tempfile::Builder::new();
    // Keep this prefix short: a legal 255-byte final name still needs room
    // for tempfile's random suffix on filesystems with NAME_MAX=255.
    builder.prefix(".bcmr.stage.").suffix(".tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // tempfile defaults to 0600.  Local copy historically creates files as
        // 0666 filtered by umask, so preserve that ordinary-copy behavior.
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    let file = builder.tempfile_in(parent)?;
    let (file, path) = file.into_parts();
    let guard = TempFileGuard::new(path.to_path_buf());
    Ok(AtomicStaging {
        file: Some(file),
        path,
        guard,
    })
}

async fn seed_staging_prefix(
    dst: &Path,
    staging: &AtomicStaging,
    start_offset: u64,
    observed: Arc<DestinationObservation>,
) -> Result<(), BcmrError> {
    let dst = dst.to_path_buf();
    let mut stage_file = staging.try_clone_file()?;
    tokio::task::spawn_blocking(move || {
        use std::io::{Read, Seek, SeekFrom, Write};

        stage_file.set_len(0)?;
        stage_file.seek(SeekFrom::Start(0))?;
        if start_offset == 0 {
            return Ok(());
        }

        let mut source = open_destination_for_observation(&dst)?;
        if !observed.matches_file(&source)? {
            return Err(BcmrError::DestinationChanged(dst));
        }
        source.seek(SeekFrom::Start(0))?;

        let mut remaining = start_offset;
        let mut buffer = vec![0u8; crate::core::session::COPY_BLOCK_SIZE as usize];
        while remaining > 0 {
            let read_limit = remaining.min(buffer.len() as u64) as usize;
            let read = source.read(&mut buffer[..read_limit])?;
            if read == 0 {
                return Err(BcmrError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("destination prefix ended with {remaining} bytes left to seed"),
                )));
            }
            stage_file.write_all(&buffer[..read])?;
            remaining -= read as u64;
        }
        stage_file.set_len(start_offset)?;
        if !observed.matches_path(&dst)? {
            return Err(BcmrError::DestinationChanged(dst));
        }
        Ok(())
    })
    .await?
}

#[cfg(feature = "test-support")]
fn inject_destination_replacement(dst: &Path, bytes: &[u8]) -> Result<(), BcmrError> {
    use std::io::Write;

    let replacement = create_staging(dst)?;
    let mut file = replacement.try_clone_file()?;
    file.write_all(bytes)?;
    file.sync_data()?;
    drop(file);
    replacement.commit(dst, &CommitPolicy::ReplaceAny, None)
}

#[cfg(all(feature = "test-support", unix))]
fn inject_destination_fifo(dst: &Path) -> Result<(), BcmrError> {
    use std::os::unix::ffi::OsStrExt;

    std::fs::remove_file(dst)?;
    let dst = std::ffi::CString::new(dst.as_os_str().as_bytes()).map_err(|_| {
        BcmrError::InvalidInput("test destination path contains an interior NUL".into())
    })?;
    if unsafe { libc::mkfifo(dst.as_ptr(), 0o600) } != 0 {
        return Err(BcmrError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(all(feature = "test-support", not(unix)))]
fn inject_destination_fifo(_dst: &Path) -> Result<(), BcmrError> {
    Err(BcmrError::InvalidInput(
        "FIFO destination injection is only supported on Unix".into(),
    ))
}

#[cfg(feature = "test-support")]
fn inject_destination_hardlink(dst: &Path) -> Result<(), BcmrError> {
    std::fs::hard_link(dst, dst.with_extension("bcmr-test-hardlink"))?;
    Ok(())
}

#[cfg(any(target_os = "linux", test))]
fn try_linux_reflink(
    src: &Path,
    staging: AtomicStaging,
    file_size: u64,
    fail_on_error: bool,
) -> Result<(AtomicStaging, bool), BcmrError> {
    let src_file = StdFile::open(src)?;
    staging.try_reflink_retained(file_size, fail_on_error, |dst_file| {
        let length = std::num::NonZeroU64::new(file_size)
            .expect("empty files return before invoking the reflink backend");
        reflink_copy::ReflinkBlockBuilder::new(&src_file, dst_file, length).reflink_block()
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn try_atomic_reflink(
    src: &Path,
    staging: AtomicStaging,
    file_size: u64,
    try_reflink: bool,
    fail_on_error: bool,
    sparse_mode: &SparseMode,
    callback: &impl Fn(u64),
) -> Result<(AtomicStaging, bool), BcmrError> {
    if !try_reflink {
        return Ok((staging, false));
    }

    if matches!(sparse_mode, SparseMode::Always) {
        return Ok((staging, false));
    }

    #[cfg(target_os = "linux")]
    let (staging, reflinked) = {
        let src = src.to_path_buf();
        tokio::task::spawn_blocking(move || {
            try_linux_reflink(&src, staging, file_size, fail_on_error)
        })
        .await??
    };

    #[cfg(target_os = "macos")]
    let (staging, reflinked) = {
        let src = src.to_path_buf();
        tokio::task::spawn_blocking(move || {
            staging.try_reflink_create_new(fail_on_error, |dst| reflink_copy::reflink(&src, dst))
        })
        .await??
    };

    if reflinked {
        callback(file_size);
    }
    Ok((staging, reflinked))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
async fn try_atomic_reflink(
    _src: &Path,
    staging: AtomicStaging,
    _file_size: u64,
    try_reflink: bool,
    fail_on_error: bool,
    _sparse_mode: &SparseMode,
    _callback: &impl Fn(u64),
) -> Result<(AtomicStaging, bool), BcmrError> {
    if !try_reflink {
        return Ok((staging, false));
    }
    staging.finish_unsupported_reflink(fail_on_error)
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, PartialEq, Eq)]
enum CopyFileRangeLoopOutcome {
    Complete,
    Fallback,
}

#[cfg(any(target_os = "linux", test))]
fn copy_file_range_loop_with<CopyRange>(
    expected_remaining: u64,
    mut copy_range: CopyRange,
) -> Result<CopyFileRangeLoopOutcome, BcmrError>
where
    CopyRange: FnMut(usize) -> std::io::Result<usize>,
{
    const CHUNK: usize = 4 * 1024 * 1024;
    let mut remaining = expected_remaining;

    while remaining > 0 {
        let requested = remaining.min(CHUNK as u64) as usize;
        match copy_range(requested) {
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ENOSYS | libc::EXDEV | libc::EINVAL | libc::EOPNOTSUPP)
                ) =>
            {
                return Ok(CopyFileRangeLoopOutcome::Fallback);
            }
            Err(error) => return Err(BcmrError::Io(error)),
            Ok(0) => {
                return Err(BcmrError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("source ended with {remaining} bytes remaining"),
                )));
            }
            Ok(copied) if copied > requested => {
                return Err(BcmrError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("copy_file_range copied {copied} bytes for a {requested}-byte request"),
                )));
            }
            Ok(copied) => {
                remaining -= copied as u64;
            }
        }
    }

    Ok(CopyFileRangeLoopOutcome::Complete)
}

#[cfg(target_os = "linux")]
async fn try_copy_file_range<F>(
    src: &Path,
    dst: &Path,
    file_size: u64,
    callback: &F,
) -> Option<Result<(), BcmrError>>
where
    F: Fn(u64) + Send + Sync + Clone + 'static,
{
    let src = src.to_path_buf();
    let dst = dst.to_path_buf();

    let task = tokio::task::spawn_blocking(move || {
        use std::os::unix::io::AsRawFd;

        let src_file = std::fs::File::open(src).ok()?;
        let dst_file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(dst)
            .ok()?;
        let src_fd = src_file.as_raw_fd();
        let dst_fd = dst_file.as_raw_fd();

        if file_size > 0 {
            unsafe {
                let _ = libc::fallocate(dst_fd, 0, 0, file_size as libc::off_t);
            }
        }

        let outcome = copy_file_range_loop_with(file_size, |requested| {
            let copied = unsafe {
                libc::copy_file_range(
                    src_fd,
                    std::ptr::null_mut(),
                    dst_fd,
                    std::ptr::null_mut(),
                    requested,
                    0,
                )
            };
            if copied < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(copied as usize)
            }
        });

        match outcome {
            Ok(CopyFileRangeLoopOutcome::Complete) => Some(Ok(())),
            Ok(CopyFileRangeLoopOutcome::Fallback) => {
                if let Err(error) = reset_copy_file_range_stage_for_fallback(&dst_file) {
                    Some(Err(BcmrError::Io(error)))
                } else {
                    None
                }
            }
            Err(error) => Some(Err(error)),
        }
    });

    match task.await {
        Ok(Some(Ok(()))) => {
            callback(file_size);
            Some(Ok(()))
        }
        Ok(result) => result,
        Err(error) => Some(Err(BcmrError::Join(error))),
    }
}

#[cfg(unix)]
pub(super) fn copy_xattrs(src: &Path, dst: &Path) -> std::result::Result<(), BcmrError> {
    let names = match xattr::list(src) {
        Ok(n) => n,
        Err(e) if is_unsupported(&e) => return Ok(()),
        Err(e) => return Err(BcmrError::Io(e)),
    };
    for name in names {
        let value = match xattr::get(src, &name) {
            Ok(Some(v)) => v,
            Ok(None) => continue,
            Err(e) if is_unsupported(&e) => continue,
            Err(_) => continue,
        };
        let _ = xattr::set(dst, &name, &value);
    }
    Ok(())
}

#[cfg(unix)]
fn is_unsupported(e: &std::io::Error) -> bool {
    e.raw_os_error()
        .is_some_and(|errno| [libc::ENOTSUP, libc::EOPNOTSUPP].contains(&errno))
}

pub(super) struct CopyFileOptions {
    transfer: crate::core::remote::TransferOptions,
    reflink_arg: Option<String>,
    sparse_arg: Option<String>,
    test_mode: TestMode,
    replace_existing: bool,
}

impl CopyFileOptions {
    pub(super) fn from_cli(cli: &Commands, test_mode: TestMode) -> Self {
        Self {
            transfer: crate::core::remote::TransferOptions {
                preserve: cli.is_preserve(),
                verify: cli.is_verify(),
                resume: cli.is_resume(),
                strict: cli.is_strict(),
                append: cli.is_append(),
                sync: cli.is_sync(),
            },
            reflink_arg: cli.get_reflink_mode(),
            sparse_arg: cli.get_sparse_mode(),
            test_mode,
            replace_existing: cli.is_force(),
        }
    }

    fn effective_reflink_mode(&self) -> Result<(bool, bool), BcmrError> {
        let (try_reflink, fail_on_error) = resolve_reflink_mode(&self.reflink_arg);
        if try_reflink
            && fail_on_error
            && matches!(resolve_sparse_mode(&self.sparse_arg), SparseMode::Always)
        {
            return Err(BcmrError::InvalidInput(
                "--reflink=force is incompatible with --sparse=force".into(),
            ));
        }
        let direct_mode = self.transfer.resume || self.transfer.append || self.transfer.strict;
        if direct_mode && try_reflink {
            if fail_on_error {
                return Err(BcmrError::InvalidInput(
                    "--reflink=force is incompatible with --resume, --append, or --strict".into(),
                ));
            }
            return Ok((false, false));
        }
        Ok((try_reflink, fail_on_error))
    }

    pub(super) fn validate_reflink_compatibility(&self) -> Result<(), BcmrError> {
        self.effective_reflink_mode()?;
        Ok(())
    }
}

fn resolve_reflink_mode(arg: &Option<String>) -> (bool, bool) {
    let mode_str = arg
        .as_deref()
        .unwrap_or(&crate::config::CONFIG.copy.reflink);
    match mode_str.to_lowercase().as_str() {
        "force" => (true, true),
        "disable" | "never" => (false, false),
        _ => (true, false),
    }
}

fn resolve_sparse_mode(arg: &Option<String>) -> SparseMode {
    let mode_str = arg.as_deref().unwrap_or(&crate::config::CONFIG.copy.sparse);
    match mode_str.to_lowercase().as_str() {
        "force" => SparseMode::Always,
        "disable" | "never" => SparseMode::Never,
        _ => SparseMode::Auto,
    }
}

fn revalidate_source_snapshot(src: &Path, expected_len: u64) -> Result<(), BcmrError> {
    let current_len = src.metadata()?.len();
    if current_len < expected_len {
        return Err(BcmrError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!("source ended at {current_len} bytes; snapshot was {expected_len} bytes"),
        )));
    }
    if current_len > expected_len {
        return Err(BcmrError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("source grew to {current_len} bytes; snapshot was {expected_len} bytes"),
        )));
    }
    Ok(())
}

type FinalizeCtx<'a> = super::super::copy_strategies::FinalizeParams<'a>;

async fn run_finalize(
    ctx: FinalizeCtx<'_>,
    dst_file: fs::File,
) -> std::result::Result<(), BcmrError> {
    super::super::copy_strategies::finalize(dst_file, ctx).await
}

pub(super) async fn copy_file<F>(
    src: &Path,
    dst: &Path,
    opts: CopyFileOptions,
    callback: &ProgressCallback<F>,
) -> std::result::Result<(), BcmrError>
where
    F: Fn(u64) + Send + Sync + Clone + 'static,
{
    let (try_reflink, fail_on_error) = opts.effective_reflink_mode()?;
    let CopyFileOptions {
        transfer,
        reflink_arg: _,
        ref sparse_arg,
        test_mode,
        replace_existing,
    } = opts;
    #[cfg(feature = "test-support")]
    let truncate_source_after_snapshot = matches!(
        test_mode,
        TestMode::TruncateSourceAfterSnapshot
            | TestMode::TruncateSourceAfterSnapshotDelay
            | TestMode::TruncateSourceAfterSnapshotSpeedLimit
    );
    #[cfg(feature = "test-support")]
    let create_destination_before_finalize =
        matches!(test_mode, TestMode::CreateDestinationBeforeFinalize);
    #[cfg(feature = "test-support")]
    let replace_destination_before_finalize =
        matches!(test_mode, TestMode::ReplaceDestinationBeforeFinalize);
    #[cfg(feature = "test-support")]
    let replace_destination_after_resume_resolution =
        matches!(test_mode, TestMode::ReplaceDestinationAfterResumeResolution);
    #[cfg(feature = "test-support")]
    let replace_destination_with_fifo_after_observation = matches!(
        test_mode,
        TestMode::ReplaceDestinationWithFifoAfterObservation
    );
    #[cfg(feature = "test-support")]
    let create_destination_hardlink_before_finalize =
        matches!(test_mode, TestMode::CreateDestinationHardlinkBeforeFinalize);
    #[cfg(feature = "test-support")]
    let test_mode = match test_mode {
        TestMode::TruncateSourceAfterSnapshot => TestMode::None,
        TestMode::TruncateSourceAfterSnapshotDelay => TestMode::Delay(0),
        TestMode::TruncateSourceAfterSnapshotSpeedLimit => TestMode::SpeedLimit(u64::MAX),
        TestMode::CreateDestinationBeforeFinalize
        | TestMode::ReplaceDestinationBeforeFinalize
        | TestMode::ReplaceDestinationAfterResumeResolution
        | TestMode::ReplaceDestinationWithFifoAfterObservation
        | TestMode::CreateDestinationHardlinkBeforeFinalize
        | TestMode::FailSymlinkCreate
        | TestMode::FailSymlinkCommit
        | TestMode::CreateDestinationBeforeSymlinkCommit => TestMode::None,
        other => other,
    };
    let crate::core::remote::TransferOptions {
        preserve,
        verify,
        resume,
        strict,
        append,
        sync,
    } = transfer;

    let file_size = src.metadata()?.len();
    #[cfg(feature = "test-support")]
    if truncate_source_after_snapshot {
        let truncated = fs::OpenOptions::new().write(true).open(src).await?;
        truncated.set_len(file_size.saturating_sub(1)).await?;
        drop(truncated);
    }
    let file_name = src
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    (*callback.on_new_file)(&file_name, file_size);

    let sparse_mode = resolve_sparse_mode(sparse_arg);

    let parent = destination_parent(dst);
    if !parent.exists() {
        fs::create_dir_all(parent).await?;
    }

    let _writer_lock = DestinationWriterLock::acquire(dst).await?;
    let direct_mode = resume || append || strict;
    let entry_metadata = destination_entry_metadata(dst)?;
    let initial_entry = entry_metadata
        .as_ref()
        .map(DestinationFingerprint::from_metadata);
    let force_direct_fresh = if direct_mode {
        validate_direct_destination(dst, entry_metadata.as_ref(), replace_existing)?
    } else {
        false
    };
    let observe_target = direct_mode && entry_metadata.as_ref().is_some_and(|m| m.is_file());
    let observed_destination = Arc::new(DestinationObservation::capture(dst, observe_target)?);
    if initial_entry != observed_destination.entry {
        return Err(BcmrError::DestinationChanged(dst.to_path_buf()));
    }
    if observe_target && observed_destination.target.is_none() {
        return Err(BcmrError::DestinationChanged(dst.to_path_buf()));
    }
    if direct_mode && !replace_existing && observed_destination.target_has_multiple_links() {
        return Err(BcmrError::InvalidInput(format!(
            "direct copy destination '{}' has multiple hard links; use -f to replace only this path",
            dst.display()
        )));
    }
    let commit_policy = if replace_existing {
        CommitPolicy::ReplaceAny
    } else if direct_mode && entry_metadata.is_some() {
        CommitPolicy::ReplaceObserved(Arc::clone(&observed_destination))
    } else {
        CommitPolicy::NoClobber
    };
    let corrupt_before_verify = matches!(test_mode, TestMode::CorruptBeforeFinalize);
    #[cfg(feature = "test-support")]
    if replace_destination_with_fifo_after_observation {
        inject_destination_fifo(dst)?;
    }

    // Defer resume progress publication until the selected source snapshot has
    // been validated. In particular, size-only append completion must not
    // report success after the source shrinks.
    let defer_resume_progress = |_: u64| {};
    let mut direct_resume_state = if direct_mode {
        let destination = observed_destination.try_clone_target_file()?;
        let state = match (force_direct_fresh, destination) {
            (false, Some(destination)) => {
                crate::core::resume::resolve_observed_file(
                    crate::core::resume::ObservedResumeRequest {
                        src,
                        dst,
                        file_size,
                        resume,
                        strict,
                        append,
                        destination,
                    },
                    &defer_resume_progress,
                )
                .await?
            }
            _ => crate::core::resume::ResumeState {
                start_offset: 0,
                already_complete: false,
                loaded_session: None,
                truncate_tail: false,
            },
        };
        #[cfg(feature = "test-support")]
        if replace_destination_after_resume_resolution {
            inject_destination_replacement(dst, b"post-resolution replacement must survive")?;
        }
        revalidate_source_snapshot(src, file_size)?;
        Some(state)
    } else {
        None
    };

    if direct_resume_state
        .as_ref()
        .is_some_and(|state| state.already_complete)
    {
        if !observed_destination.matches_path(dst)? {
            return Err(BcmrError::DestinationChanged(dst.to_path_buf()));
        }
        let confirmed_destination = observed_destination
            .try_clone_target_file()?
            .ok_or_else(|| BcmrError::DestinationChanged(dst.to_path_buf()))?;
        let confirmed = crate::core::resume::resolve_observed_file(
            crate::core::resume::ObservedResumeRequest {
                src,
                dst,
                file_size,
                resume,
                strict,
                append,
                destination: confirmed_destination,
            },
            &defer_resume_progress,
        )
        .await?;
        revalidate_source_snapshot(src, file_size)?;
        if !confirmed.already_complete || !observed_destination.matches_path(dst)? {
            return Err(BcmrError::DestinationChanged(dst.to_path_buf()));
        }
        (callback.callback)(file_size);
        crate::core::session::Session::remove(src, dst);
        return Ok(());
    }

    let stage = create_staging(dst)?;
    let write_target = stage.path().to_path_buf();
    let mut staging = Some(stage);

    let reflinked = if let Some(stage) = staging.take() {
        let (stage, reflinked) = try_atomic_reflink(
            src,
            stage,
            file_size,
            try_reflink,
            fail_on_error,
            &sparse_mode,
            &callback.callback,
        )
        .await?;
        staging = Some(stage);
        reflinked
    } else {
        false
    };

    if reflinked {
        (callback.on_reflink)();
        let ctx = FinalizeCtx {
            write_target: &write_target,
            dst,
            src,
            expected_file_size: file_size,
            staging: staging.take(),
            commit_policy: &commit_policy,
            sync,
            preserve,
            verify,
            inline_src_hash: None,
            corrupt_before_verify,
        };
        return run_finalize(ctx, fs::File::open(&write_target).await?).await;
    }

    #[cfg(target_os = "linux")]
    if !direct_mode
        && matches!(test_mode, TestMode::None)
        && matches!(sparse_mode, SparseMode::Never)
    {
        match try_copy_file_range(src, &write_target, file_size, &callback.callback).await {
            Some(Ok(())) => {
                let ctx = FinalizeCtx {
                    write_target: &write_target,
                    dst,
                    src,
                    expected_file_size: file_size,
                    staging: staging.take(),
                    commit_policy: &commit_policy,
                    sync,
                    preserve,
                    verify,
                    inline_src_hash: None,
                    corrupt_before_verify,
                };
                return run_finalize(ctx, fs::File::open(&write_target).await?).await;
            }
            Some(Err(e)) => return Err(e),
            None => {}
        }
    }

    let resume_state = match direct_resume_state.take() {
        Some(state) => state,
        None => crate::core::resume::ResumeState {
            start_offset: 0,
            already_complete: false,
            loaded_session: None,
            truncate_tail: false,
        },
    };

    if resume_state.already_complete {
        return Err(BcmrError::InvalidInput(
            "an already-complete direct copy reached staging unexpectedly".into(),
        ));
    }

    let start_offset = resume_state.start_offset;
    let _loaded_session = resume_state.loaded_session;
    let _truncate_tail = resume_state.truncate_tail;
    seed_staging_prefix(
        dst,
        staging
            .as_ref()
            .expect("every local transfer owns an atomic stage"),
        start_offset,
        Arc::clone(&observed_destination),
    )
    .await?;
    if start_offset > 0 {
        (callback.callback)(start_offset);
    }
    let expected_remaining = file_size.checked_sub(start_offset).ok_or_else(|| {
        BcmrError::InvalidInput("resume offset exceeds the source size snapshot".into())
    })?;

    let mut file_flags = fs::OpenOptions::new();
    file_flags.write(true);

    let mut src_file = File::open(src).await?;
    let mut dst_file = file_flags.open(&write_target).await?;

    if start_offset > 0 {
        src_file.seek(SeekFrom::Start(start_offset)).await?;
        dst_file.seek(SeekFrom::Start(start_offset)).await?;
    }

    // A final-key session may prove only bytes in the final destination.
    // Private stage progress cannot safely extend that proof because the stage
    // is discarded on failure.
    let mut session: Option<crate::core::session::Session> = None;

    let inline_src_hash = match test_mode {
        TestMode::Delay(ms) => {
            let mut buffer = vec![0u8; crate::core::session::COPY_BLOCK_SIZE as usize];
            let mut remaining = expected_remaining;
            while remaining > 0 {
                let read_limit = remaining.min(buffer.len() as u64) as usize;
                let n = src_file.read(&mut buffer[..read_limit]).await?;
                if n == 0 {
                    return Err(BcmrError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!("source ended with {remaining} bytes remaining"),
                    )));
                }
                remaining -= n as u64;
                dst_file.write_all(&buffer[..n]).await?;
                (callback.callback)(n as u64);
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
            None
        }
        TestMode::SpeedLimit(bps) => {
            let mut buffer = vec![0u8; crate::core::session::COPY_BLOCK_SIZE as usize];
            let chunk_size = bps.min(buffer.len() as u64);
            let mut start_time = Instant::now();
            let mut remaining = expected_remaining;
            while remaining > 0 {
                let read_limit = remaining.min(chunk_size) as usize;
                let n = src_file.read(&mut buffer[..read_limit]).await?;
                if n == 0 {
                    return Err(BcmrError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!("source ended with {remaining} bytes remaining"),
                    )));
                }
                remaining -= n as u64;
                dst_file.write_all(&buffer[..n]).await?;
                let elapsed = start_time.elapsed();
                let target = Duration::from_secs_f64(n as f64 / bps as f64);
                if elapsed < target {
                    tokio::time::sleep(target - elapsed).await;
                    start_time = Instant::now();
                }
                (callback.callback)(n as u64);
            }
            None
        }
        TestMode::None | TestMode::CorruptBeforeFinalize => {
            let need_src_hash = verify || session.is_some();
            super::super::copy_strategies::streaming_copy(
                &mut src_file,
                &mut dst_file,
                &mut session,
                super::super::copy_strategies::StreamingCopyParams {
                    sparse_mode: sparse_mode.clone(),
                    start_offset,
                    expected_remaining,
                    need_src_hash,
                },
                &callback.callback,
            )
            .await?
        }
        #[cfg(feature = "test-support")]
        TestMode::TruncateSourceAfterStageWrite => {
            let mut buffer = vec![0u8; crate::core::session::COPY_BLOCK_SIZE as usize];
            let mut remaining = expected_remaining;
            let mut injected = false;
            while remaining > 0 {
                let read_limit = remaining.min(buffer.len() as u64) as usize;
                let n = src_file.read(&mut buffer[..read_limit]).await?;
                if n == 0 {
                    return Err(BcmrError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!("source ended with {remaining} bytes remaining"),
                    )));
                }
                remaining -= n as u64;
                dst_file.write_all(&buffer[..n]).await?;
                (callback.callback)(n as u64);

                if !injected {
                    let truncate_at = start_offset + n as u64;
                    let truncated = fs::OpenOptions::new().write(true).open(src).await?;
                    truncated.set_len(truncate_at).await?;
                    drop(truncated);
                    injected = true;
                }
            }
            None
        }
        #[cfg(feature = "test-support")]
        TestMode::TruncateSourceAfterSnapshot
        | TestMode::TruncateSourceAfterSnapshotDelay
        | TestMode::TruncateSourceAfterSnapshotSpeedLimit => {
            unreachable!("truncate test modes are normalized before transfer")
        }
        #[cfg(feature = "test-support")]
        TestMode::CreateDestinationBeforeFinalize
        | TestMode::ReplaceDestinationBeforeFinalize
        | TestMode::ReplaceDestinationAfterResumeResolution
        | TestMode::ReplaceDestinationWithFifoAfterObservation
        | TestMode::CreateDestinationHardlinkBeforeFinalize => {
            unreachable!("destination race modes are normalized before transfer")
        }
        #[cfg(feature = "test-support")]
        TestMode::FailSymlinkCreate
        | TestMode::FailSymlinkCommit
        | TestMode::CreateDestinationBeforeSymlinkCommit => {
            unreachable!("symlink test modes are normalized before file transfer")
        }
    };

    #[cfg(feature = "test-support")]
    if create_destination_before_finalize {
        let mut competitor = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dst)
            .await?;
        competitor
            .write_all(b"racing destination must survive")
            .await?;
        competitor.sync_data().await?;
    }
    #[cfg(feature = "test-support")]
    if replace_destination_before_finalize {
        inject_destination_replacement(dst, b"external replacement must survive")?;
    }
    #[cfg(feature = "test-support")]
    if create_destination_hardlink_before_finalize {
        inject_destination_hardlink(dst)?;
    }

    let ctx = FinalizeCtx {
        write_target: &write_target,
        dst,
        src,
        expected_file_size: file_size,
        staging: staging.take(),
        commit_policy: &commit_policy,
        sync,
        preserve,
        verify,
        inline_src_hash,
        corrupt_before_verify,
    };
    run_finalize(ctx, dst_file).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::{Arc, Barrier};

    #[test]
    fn windows_persist_policy_preserves_readonly_and_clears_only_temporary() {
        let hidden = 0x0000_0002;
        let compressed = 0x0000_0800;
        let sparse = 0x0000_0200;
        let encrypted = 0x0000_4000;
        let reparse_point = 0x0000_0400;
        assert_eq!(
            windows_attributes_after_persist(
                WINDOWS_FILE_ATTRIBUTE_READONLY
                    | WINDOWS_FILE_ATTRIBUTE_TEMPORARY
                    | hidden
                    | compressed
                    | sparse
                    | encrypted
                    | reparse_point,
                None,
            ),
            WINDOWS_FILE_ATTRIBUTE_READONLY | hidden
        );
        assert_eq!(
            windows_attributes_after_persist(WINDOWS_FILE_ATTRIBUTE_TEMPORARY, None),
            WINDOWS_FILE_ATTRIBUTE_NORMAL
        );
        assert_eq!(
            windows_attributes_after_persist(WINDOWS_FILE_ATTRIBUTE_TEMPORARY | hidden, Some(true),),
            WINDOWS_FILE_ATTRIBUTE_READONLY | hidden
        );
        assert_eq!(
            windows_attributes_after_persist(
                WINDOWS_FILE_ATTRIBUTE_READONLY | WINDOWS_FILE_ATTRIBUTE_TEMPORARY | hidden,
                Some(false),
            ),
            hidden
        );
        assert_eq!(
            windows_attributes_for_failed_stage_cleanup(
                WINDOWS_FILE_ATTRIBUTE_READONLY | WINDOWS_FILE_ATTRIBUTE_TEMPORARY | hidden
            ),
            hidden
        );
    }

    #[test]
    fn destination_link_count_is_part_of_the_observation_proof() {
        assert!(observed_link_count_matches(Some(1), Some(1)));
        assert!(observed_link_count_matches(None, None));
        assert!(!observed_link_count_matches(Some(1), Some(2)));
        assert!(!observed_link_count_matches(Some(1), None));
    }

    #[test]
    fn writer_locks_serialize_one_destination_without_blocking_another() {
        let dir = tempfile::tempdir().unwrap();
        let first_destination = dir.path().join("first.bin");
        let other_destination = dir.path().join("other.bin");

        let first = DestinationWriterLock::acquire_sync(&first_destination).unwrap();
        assert!(
            DestinationWriterLock::try_acquire_sync(&first_destination)
                .unwrap()
                .is_none(),
            "a second BCMR writer must not acquire the same destination lock"
        );
        let other = DestinationWriterLock::try_acquire_sync(&other_destination)
            .unwrap()
            .expect("a different destination must have an independent lock");
        drop(other);
        drop(first);
        assert!(
            DestinationWriterLock::try_acquire_sync(&first_destination)
                .unwrap()
                .is_some(),
            "the persistent lock file must be reusable after the holder closes"
        );

        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
            assert!(
                writer_lock_dir()
                    .unwrap()
                    .starts_with(std::path::PathBuf::from(data_home)),
                "tests must keep writer-lock artifacts under the external XDG data root"
            );
        }
    }

    #[test]
    fn unique_sibling_staging_isolated_from_other_staging_files() {
        let dir = tempfile::tempdir().unwrap();
        let dst = Arc::new(dir.path().join("destination.bin"));
        let preoccupied = dir.path().join(".bcmr.stage.preoccupied.tmp");

        #[cfg(unix)]
        std::os::unix::fs::symlink("nowhere", &preoccupied).unwrap();
        #[cfg(not(unix))]
        std::fs::write(&preoccupied, b"occupied").unwrap();

        let barrier = Arc::new(Barrier::new(16));
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let dst = Arc::clone(&dst);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    create_staging(&dst).unwrap()
                })
            })
            .collect();
        let mut stages: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let paths: HashSet<_> = stages
            .iter()
            .map(|stage| stage.path().to_path_buf())
            .collect();
        assert_eq!(paths.len(), 16, "every concurrent copy needs its own stage");
        for path in &paths {
            assert_eq!(path.parent(), Some(dir.path()));
            assert!(
                path.is_file(),
                "stage must be a regular file: {}",
                path.display()
            );
            assert!(!path.is_symlink(), "stage must not follow a symlink");
        }
        assert!(
            preoccupied.symlink_metadata().is_ok(),
            "preoccupied path changed"
        );

        let removed = stages.pop().unwrap();
        let removed_path = removed.path().to_path_buf();
        drop(removed);
        assert!(!removed_path.exists());
        for stage in stages {
            assert!(
                stage.path().is_file(),
                "one cleanup must not remove another stage"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn staging_uses_the_normal_create_mode_before_umask() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let control = dir.path().join("ordinary-create.bin");
        std::fs::File::create(&control).unwrap();
        let stage = create_staging(&dir.path().join("destination.bin")).unwrap();
        let mode = std::fs::metadata(stage.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let control_mode = std::fs::metadata(&control).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, control_mode,
            "staging must honor the active umask like File::create"
        );
    }

    #[test]
    fn staging_allows_a_maximum_length_final_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("d".repeat(255));
        let stage = create_staging(&dst).unwrap();
        assert_eq!(stage.path().parent(), Some(dir.path()));
    }

    #[test]
    fn no_replace_commit_rejects_a_target_created_after_preflight() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("destination.bin");
        let stage = create_staging(&dst).unwrap();
        std::fs::write(stage.path(), b"new bytes").unwrap();

        // This models a concurrent creator winning after the UI preflight but
        // before the actual commit syscall.
        let old_bytes = b"racing writer wins";
        std::fs::write(&dst, old_bytes).unwrap();
        let error = stage
            .commit(&dst, &CommitPolicy::NoClobber, None)
            .unwrap_err();

        assert!(matches!(error, BcmrError::TargetExists(path) if path == dst));
        assert_eq!(std::fs::read(&dst).unwrap(), old_bytes);
        let remaining_stages: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".bcmr.stage.")
            })
            .collect();
        assert!(remaining_stages.is_empty());
    }

    fn copy_options_with_direct_mode(mode: &str, reflink: &str) -> CopyFileOptions {
        CopyFileOptions {
            transfer: crate::core::remote::TransferOptions {
                preserve: false,
                verify: false,
                resume: mode == "resume",
                strict: mode == "strict",
                append: mode == "append",
                sync: false,
            },
            reflink_arg: Some(reflink.into()),
            sparse_arg: Some("auto".into()),
            test_mode: TestMode::None,
            replace_existing: false,
        }
    }

    #[test]
    fn automatic_reflink_is_disabled_for_every_direct_mode() {
        for mode in ["resume", "append", "strict"] {
            let options = copy_options_with_direct_mode(mode, "auto");
            assert_eq!(
                options.effective_reflink_mode().unwrap(),
                (false, false),
                "{mode} must bypass the reflink backend"
            );
        }
    }

    #[test]
    fn unsupported_platform_auto_reflink_keeps_the_stage_reserved() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();

        let (stage, reflinked) = stage.finish_unsupported_reflink(false).unwrap();

        assert!(!reflinked);
        assert_eq!(stage.path(), stage_path);
        let create_error = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stage.path())
            .unwrap_err();
        assert_eq!(create_error.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn unsupported_platform_forced_reflink_cleans_the_owned_stage() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();

        let result = stage.finish_unsupported_reflink(true);

        assert!(matches!(result, Err(BcmrError::Reflink(_))));
        assert!(!stage_path.exists());
    }

    #[test]
    fn retained_reflink_keeps_the_exclusive_stage_name_reserved() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();

        let (stage, reflinked) = stage
            .try_reflink_retained(15, false, |file| {
                let create_error = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&stage_path)
                    .unwrap_err();
                assert_eq!(create_error.kind(), std::io::ErrorKind::AlreadyExists);

                file.set_len(0)?;
                let mut file = file;
                file.write_all(b"retained result")
            })
            .unwrap();

        assert!(reflinked);
        assert_eq!(std::fs::read(stage.path()).unwrap(), b"retained result");
    }

    #[test]
    fn retained_reflink_auto_failure_resets_the_owned_stage_for_streaming() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();

        let (stage, reflinked) = stage
            .try_reflink_retained(13, false, |file| {
                let mut file = file;
                file.write_all(b"partial clone")?;
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "simulated unsupported reflink",
                ))
            })
            .unwrap();

        assert!(!reflinked);
        assert_eq!(stage.path(), stage_path);
        assert_eq!(std::fs::metadata(stage.path()).unwrap().len(), 0);
        let create_error = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stage.path())
            .unwrap_err();
        assert_eq!(create_error.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn empty_retained_reflink_succeeds_without_invoking_the_backend() {
        use std::cell::Cell;

        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        let stage = create_staging(&final_path).unwrap();
        let called = Cell::new(false);

        let (stage, reflinked) = stage
            .try_reflink_retained(0, true, |_| {
                called.set(true);
                Err(std::io::Error::other("backend must not run"))
            })
            .unwrap();

        assert!(reflinked);
        assert!(!called.get());
        assert_eq!(std::fs::metadata(stage.path()).unwrap().len(), 0);
    }

    #[test]
    fn linux_reflink_helper_accepts_an_empty_source_without_releasing_the_stage() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("source.bin");
        let final_path = dir.path().join("destination.bin");
        std::fs::write(&src, b"").unwrap();
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();

        let (stage, reflinked) = try_linux_reflink(&src, stage, 0, true).unwrap();

        assert!(reflinked);
        assert_eq!(stage.path(), stage_path);
        assert_eq!(std::fs::metadata(stage.path()).unwrap().len(), 0);
    }

    #[test]
    fn retained_reflink_force_failure_cleans_its_owned_stage() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();

        let result = stage.try_reflink_retained(1, true, |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "simulated forced reflink failure",
            ))
        });

        assert!(matches!(result, Err(BcmrError::Reflink(_))));
        assert!(!stage_path.exists());
    }

    #[test]
    fn macos_clonefile_helper_gets_an_absent_path_and_stage_still_commits() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();

        let (stage, reflinked) = stage
            .try_reflink_create_new(false, |path| {
                assert!(!path.exists(), "reflink must receive an absent path");
                let mut cloned = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)?;
                cloned.write_all(b"reflink result")
            })
            .unwrap();

        assert!(reflinked);
        assert_eq!(stage.path(), stage_path);
        assert_eq!(std::fs::read(stage.path()).unwrap(), b"reflink result");
        stage
            .commit(&final_path, &CommitPolicy::ReplaceAny, None)
            .unwrap();
        assert_eq!(std::fs::read(final_path).unwrap(), b"reflink result");
    }

    #[test]
    fn macos_auto_failure_recreates_the_same_exclusive_reservation() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();

        let (stage, reflinked) = stage
            .try_reflink_create_new(false, |path| {
                assert_eq!(path, stage_path);
                assert!(!path.exists(), "reflink must receive an absent path");
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "simulated unsupported reflink",
                ))
            })
            .unwrap();

        assert!(!reflinked);
        assert_eq!(stage.path(), stage_path);
        assert!(stage.path().is_file(), "fallback must own a regular file");
        assert!(
            stage.file.is_some(),
            "fallback must retain the newly created file handle"
        );
        let error = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stage.path())
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn copy_file_range_fallback_keeps_the_exclusive_stage_reservation() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();
        let mut stage_file = stage.file.as_ref().unwrap();
        stage_file
            .write_all(b"partial copy_file_range bytes")
            .unwrap();

        reset_copy_file_range_stage_for_fallback(stage_file).unwrap();

        assert!(stage_path.is_file());
        assert_eq!(std::fs::metadata(&stage_path).unwrap().len(), 0);
        let error = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage_path)
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn copy_file_range_loop_rejects_zero_progress_before_snapshot_completion() {
        let error = copy_file_range_loop_with(17, |_| Ok(0))
            .expect_err("zero progress before the snapshot boundary must fail");

        assert!(
            matches!(error, BcmrError::Io(ref error) if error.kind() == std::io::ErrorKind::UnexpectedEof)
        );
    }

    #[test]
    fn copy_file_range_loop_caps_the_final_request_at_the_snapshot_boundary() {
        use std::cell::RefCell;

        let chunk = 4 * 1024 * 1024;
        let requests = RefCell::new(Vec::new());

        let outcome = copy_file_range_loop_with(chunk as u64 + 3, |requested| {
            requests.borrow_mut().push(requested);
            Ok(requested)
        })
        .unwrap();

        assert_eq!(outcome, CopyFileRangeLoopOutcome::Complete);
        assert_eq!(requests.into_inner(), vec![chunk, 3]);
    }

    #[test]
    fn copy_file_range_loop_falls_back_only_for_supported_errno_values() {
        for errno in [libc::ENOSYS, libc::EXDEV, libc::EINVAL, libc::EOPNOTSUPP] {
            let outcome =
                copy_file_range_loop_with(1, |_| Err(std::io::Error::from_raw_os_error(errno)))
                    .unwrap();
            assert_eq!(
                outcome,
                CopyFileRangeLoopOutcome::Fallback,
                "errno {errno} must select the reset-and-fallback path"
            );
        }

        let error =
            copy_file_range_loop_with(1, |_| Err(std::io::Error::from_raw_os_error(libc::EIO)))
                .expect_err("unrelated I/O errors must propagate");
        assert!(
            matches!(error, BcmrError::Io(ref error) if error.raw_os_error() == Some(libc::EIO))
        );
    }

    #[test]
    fn macos_auto_reservation_race_does_not_delete_competing_file() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        std::fs::write(&final_path, b"old final bytes").unwrap();
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();
        let sentinel = b"competing writer's bytes";

        let result = stage.try_reflink_create_new(false, |path| {
            assert!(!path.exists(), "reflink must receive an absent path");
            std::fs::write(path, sentinel)?;
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "simulated failed reflink after a competing create",
            ))
        });

        assert!(
            matches!(result, Err(BcmrError::Io(ref error)) if error.kind() == std::io::ErrorKind::AlreadyExists)
        );
        assert_eq!(std::fs::read(&stage_path).unwrap(), sentinel);
        assert_eq!(std::fs::read(&final_path).unwrap(), b"old final bytes");
    }

    #[test]
    fn macos_forced_failure_does_not_delete_competing_file() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        std::fs::write(&final_path, b"old final bytes").unwrap();
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();
        let sentinel = b"competing writer's bytes";

        let result = stage.try_reflink_create_new(true, |path| {
            assert!(!path.exists(), "reflink must receive an absent path");
            std::fs::write(path, sentinel)?;
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "simulated forced reflink failure",
            ))
        });

        assert!(matches!(result, Err(BcmrError::Reflink(_))));
        assert_eq!(std::fs::read(&stage_path).unwrap(), sentinel);
        assert_eq!(std::fs::read(&final_path).unwrap(), b"old final bytes");
    }

    #[cfg(unix)]
    #[test]
    fn macos_auto_reservation_race_does_not_follow_or_delete_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("destination.bin");
        std::fs::write(&final_path, b"old final bytes").unwrap();
        let sentinel_target = dir.path().join("sentinel-target.bin");
        std::fs::write(&sentinel_target, b"sentinel target bytes").unwrap();
        let stage = create_staging(&final_path).unwrap();
        let stage_path = stage.path().to_path_buf();

        let result = stage.try_reflink_create_new(false, |path| {
            assert!(!path.exists(), "reflink must receive an absent path");
            std::os::unix::fs::symlink(&sentinel_target, path)?;
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "simulated failed reflink after a competing symlink",
            ))
        });

        assert!(
            matches!(result, Err(BcmrError::Io(ref error)) if error.kind() == std::io::ErrorKind::AlreadyExists)
        );
        assert!(stage_path
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read(&sentinel_target).unwrap(),
            b"sentinel target bytes"
        );
        assert_eq!(std::fs::read(&final_path).unwrap(), b"old final bytes");
    }
}
