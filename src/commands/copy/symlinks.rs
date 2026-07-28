use crate::cli::{Commands, TestMode};
use crate::core::cleanup::TempFileGuard;
use crate::core::error::BcmrError;
use std::path::Path;

use super::file_copy::destination_parent;
use super::plan::SymlinkKind;

const SYMLINK_STAGE_PREFIX: &str = ".bcmr.symlink.";
const SYMLINK_STAGE_SUFFIX: &str = ".tmp";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SymlinkCommitPolicy {
    NoClobber,
    ReplaceExisting,
    WindowsDirectorySafetyNoClobber,
}

fn symlink_commit_policy(
    is_windows: bool,
    kind: SymlinkKind,
    replace_existing: bool,
) -> SymlinkCommitPolicy {
    if !replace_existing {
        SymlinkCommitPolicy::NoClobber
    } else if is_windows && kind == SymlinkKind::Directory {
        // A Windows directory-link stage has the DIRECTORY attribute.  A
        // replacing rename can therefore replace a real empty directory that
        // appears after preflight.  There is no conditional kernel primitive
        // that both replaces links/files and proves the victim is not a real
        // directory, so force remains atomically no-clobber for this one case.
        SymlinkCommitPolicy::WindowsDirectorySafetyNoClobber
    } else {
        SymlinkCommitPolicy::ReplaceExisting
    }
}

impl SymlinkCommitPolicy {
    fn replaces_existing(self) -> bool {
        matches!(self, Self::ReplaceExisting)
    }

    fn target_exists_error(self, dst: &Path) -> BcmrError {
        if matches!(self, Self::WindowsDirectorySafetyNoClobber) {
            BcmrError::InvalidInput(format!(
                "cannot safely replace existing '{}' with a directory symlink on Windows: \
                 --force intentionally uses an atomic no-clobber commit because Windows has \
                 no conditional rename that can protect a racing real directory; \
                 the existing destination was preserved",
                dst.display()
            ))
        } else {
            BcmrError::TargetExists(dst.to_path_buf())
        }
    }
}

fn symlink_entry_metadata(
    path: &Path,
) -> std::result::Result<Option<std::fs::Metadata>, BcmrError> {
    match path.symlink_metadata() {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Windows maps both a missing leaf and an intermediate regular
            // file to NotFound. Only accept the former when the immediate
            // parent is a directory (possibly reached through a symlink).
            let Some(parent) = path.parent() else {
                return Ok(None);
            };
            match parent.metadata() {
                Ok(metadata) if metadata.is_dir() => Ok(None),
                Ok(_) => Err(BcmrError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotADirectory,
                    format!(
                        "destination parent '{}' is not a directory",
                        parent.display()
                    ),
                ))),
                Err(parent_error) if parent_error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(None)
                }
                Err(parent_error) => Err(BcmrError::Io(parent_error)),
            }
        }
        Err(error) => Err(BcmrError::Io(error)),
    }
}

pub(super) fn check_symlink_overwrite(
    dst: &Path,
    kind: SymlinkKind,
    cli: &Commands,
) -> std::result::Result<(), BcmrError> {
    let Some(md) = symlink_entry_metadata(dst)? else {
        return Ok(());
    };
    // cp -P refuses to clobber a real directory with a symlink even under -f.
    let ft = md.file_type();
    if ft.is_dir() && !ft.is_symlink() {
        return Err(BcmrError::InvalidInput(format!(
            "cannot overwrite directory '{}' with a symlink",
            dst.display()
        )));
    }
    let policy = symlink_commit_policy(cfg!(windows), kind, cli.is_force());
    if !policy.replaces_existing() {
        return Err(policy.target_exists_error(dst));
    }
    Ok(())
}

pub(super) async fn create_symlink_replacing(
    dst: &Path,
    target: &Path,
    kind: SymlinkKind,
    replace_existing: bool,
    _test_mode: &TestMode,
) -> std::result::Result<(), BcmrError> {
    #[cfg(feature = "test-support")]
    let fail_create = matches!(_test_mode, TestMode::FailSymlinkCreate);
    #[cfg(not(feature = "test-support"))]
    let fail_create = false;
    #[cfg(feature = "test-support")]
    let fail_commit = matches!(_test_mode, TestMode::FailSymlinkCommit);
    #[cfg(not(feature = "test-support"))]
    let fail_commit = false;
    #[cfg(feature = "test-support")]
    let create_destination_before_commit =
        matches!(_test_mode, TestMode::CreateDestinationBeforeSymlinkCommit);
    #[cfg(not(feature = "test-support"))]
    let create_destination_before_commit = false;

    let dst = dst.to_path_buf();
    let target = target.to_path_buf();
    tokio::task::spawn_blocking(move || {
        create_symlink_replacing_sync(
            &dst,
            &target,
            kind,
            replace_existing,
            fail_create,
            fail_commit,
            create_destination_before_commit,
        )
    })
    .await?
}

fn create_symlink_replacing_sync(
    dst: &Path,
    target: &Path,
    kind: SymlinkKind,
    replace_existing: bool,
    fail_create: bool,
    fail_commit: bool,
    create_destination_before_commit: bool,
) -> std::result::Result<(), BcmrError> {
    let commit_policy = symlink_commit_policy(cfg!(windows), kind, replace_existing);
    check_commit_policy_preflight(dst, commit_policy)?;
    let staging = create_symlink_staging(dst, target, kind, fail_create)?;

    if create_destination_before_commit {
        use std::io::Write;

        let mut competitor = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dst)?;
        competitor.write_all(b"racing destination must survive")?;
        competitor.sync_data()?;
    }

    staging.commit(dst, commit_policy, fail_commit)
}

fn check_commit_policy_preflight(
    dst: &Path,
    policy: SymlinkCommitPolicy,
) -> std::result::Result<(), BcmrError> {
    if matches!(policy, SymlinkCommitPolicy::WindowsDirectorySafetyNoClobber)
        && symlink_entry_metadata(dst)?.is_some()
    {
        return Err(policy.target_exists_error(dst));
    }
    Ok(())
}

fn create_symlink_staging(
    dst: &Path,
    target: &Path,
    kind: SymlinkKind,
    fail_create: bool,
) -> std::result::Result<SymlinkStaging, BcmrError> {
    let mut builder = tempfile::Builder::new();
    builder
        .prefix(SYMLINK_STAGE_PREFIX)
        .suffix(SYMLINK_STAGE_SUFFIX);
    let staged = builder
        .make_in(destination_parent(dst), |stage_path| {
            if fail_create {
                return Err(std::io::Error::other("injected symlink create failure"));
            }
            create_platform_symlink(stage_path, target, kind)
        })
        .map_err(|error| map_symlink_creation_error(dst, error))?;
    let (_, path) = staged.into_parts();
    let guard = TempFileGuard::new(path.to_path_buf());
    Ok(SymlinkStaging {
        guard,
        path: Some(path),
    })
}

#[cfg(unix)]
fn create_platform_symlink(
    stage_path: &Path,
    target: &Path,
    _kind: SymlinkKind,
) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, stage_path)
}

#[cfg(windows)]
fn create_platform_symlink(
    stage_path: &Path,
    target: &Path,
    kind: SymlinkKind,
) -> std::io::Result<()> {
    match kind {
        SymlinkKind::File => std::os::windows::fs::symlink_file(target, stage_path),
        SymlinkKind::Directory => std::os::windows::fs::symlink_dir(target, stage_path),
    }
}

#[cfg(not(any(unix, windows)))]
fn create_platform_symlink(
    stage_path: &Path,
    _target: &Path,
    _kind: SymlinkKind,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!(
            "symlink replication is not supported for '{}'",
            stage_path.display()
        ),
    ))
}

fn injected_symlink_failure(phase: &str) -> BcmrError {
    BcmrError::Io(std::io::Error::other(format!(
        "injected symlink {phase} failure"
    )))
}

fn map_symlink_creation_error(dst: &Path, error: std::io::Error) -> BcmrError {
    #[cfg(windows)]
    {
        const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
        if error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) {
            return BcmrError::InvalidInput(format!(
                "cannot create symlink '{}': enable Windows Developer Mode \
                 or run elevated (symlink creation requires privilege)",
                dst.display()
            ));
        }
    }
    let _ = dst;
    BcmrError::Io(error)
}

struct SymlinkStaging {
    // The registry guard is declared first so Windows directory-link cleanup
    // runs before TempPath's file-only fallback.
    guard: TempFileGuard,
    path: Option<tempfile::TempPath>,
}

impl SymlinkStaging {
    fn commit(
        mut self,
        dst: &Path,
        policy: SymlinkCommitPolicy,
        fail_commit: bool,
    ) -> std::result::Result<(), BcmrError> {
        let path = self.path.take().ok_or_else(|| {
            BcmrError::InvalidInput("symlink staging path was already consumed".into())
        })?;
        if fail_commit {
            return Err(injected_symlink_failure("commit"));
        }

        #[cfg(windows)]
        {
            let mut path = path;
            match persist_windows_symlink(path.as_ref(), dst, policy) {
                Ok(()) => {
                    path.disable_cleanup(true);
                    self.guard.disarm();
                    Ok(())
                }
                Err(BcmrError::Io(error)) if !policy.replaces_existing() => {
                    let observed_exists =
                        matches!(symlink_entry_metadata(dst), Ok(Some(_metadata)));
                    let map_to_exists = should_map_noclobber_error_to_target_exists(
                        is_target_exists_error(&error),
                        observed_exists,
                    );
                    drop(path);
                    if map_to_exists {
                        Err(policy.target_exists_error(dst))
                    } else {
                        Err(BcmrError::Io(error))
                    }
                }
                Err(error) => {
                    drop(path);
                    Err(error)
                }
            }
        }

        #[cfg(not(windows))]
        {
            let persisted = if policy.replaces_existing() {
                path.persist(dst)
            } else {
                path.persist_noclobber(dst)
            };
            match persisted {
                Ok(()) => {
                    self.guard.disarm();
                    Ok(())
                }
                Err(error) => {
                    let target_exists = is_target_exists_error(&error.error);
                    drop(error.path);
                    if !policy.replaces_existing() && target_exists {
                        Err(policy.target_exists_error(dst))
                    } else {
                        Err(BcmrError::Io(error.error))
                    }
                }
            }
        }
    }
}

#[cfg(any(windows, test))]
fn should_map_noclobber_error_to_target_exists(
    raw_error_is_target_exists: bool,
    destination_was_observed: bool,
) -> bool {
    raw_error_is_target_exists || destination_was_observed
}

fn is_target_exists_error(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        return true;
    }
    #[cfg(windows)]
    {
        const ERROR_FILE_EXISTS: i32 = 80;
        const ERROR_ALREADY_EXISTS: i32 = 183;
        return matches!(
            error.raw_os_error(),
            Some(ERROR_FILE_EXISTS) | Some(ERROR_ALREADY_EXISTS)
        );
    }
    #[cfg(not(windows))]
    false
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowsSymlinkCommitDispatch {
    HandleNoClobber,
    HandleReplace,
}

#[cfg(any(windows, test))]
fn windows_symlink_commit_dispatch(policy: SymlinkCommitPolicy) -> WindowsSymlinkCommitDispatch {
    if policy.replaces_existing() {
        WindowsSymlinkCommitDispatch::HandleReplace
    } else {
        WindowsSymlinkCommitDispatch::HandleNoClobber
    }
}

#[cfg(any(windows, test))]
const WINDOWS_FILE_RENAME_INFO_CLASS: i32 = 3;
#[cfg(any(windows, test))]
const WINDOWS_FILE_RENAME_INFO_EX_CLASS: i32 = 22;
#[cfg(any(windows, test))]
const WINDOWS_FILE_RENAME_FLAG_REPLACE_IF_EXISTS: u32 = 1;
#[cfg(any(windows, test))]
const WINDOWS_FILE_RENAME_FLAG_POSIX_SEMANTICS: u32 = 2;

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WindowsRenameOperation {
    information_class: i32,
    flags: u32,
}

#[cfg(any(windows, test))]
fn windows_rename_operation(dispatch: WindowsSymlinkCommitDispatch) -> WindowsRenameOperation {
    match dispatch {
        WindowsSymlinkCommitDispatch::HandleNoClobber => WindowsRenameOperation {
            information_class: WINDOWS_FILE_RENAME_INFO_CLASS,
            flags: 0,
        },
        WindowsSymlinkCommitDispatch::HandleReplace => WindowsRenameOperation {
            information_class: WINDOWS_FILE_RENAME_INFO_EX_CLASS,
            flags: WINDOWS_FILE_RENAME_FLAG_REPLACE_IF_EXISTS
                | WINDOWS_FILE_RENAME_FLAG_POSIX_SEMANTICS,
        },
    }
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
fn windows_extended_rename_unavailable(raw_os_error: Option<i32>) -> bool {
    const ERROR_INVALID_FUNCTION: i32 = 1;
    const ERROR_NOT_SUPPORTED: i32 = 50;
    const ERROR_INVALID_PARAMETER: i32 = 87;
    const ERROR_CALL_NOT_IMPLEMENTED: i32 = 120;
    matches!(
        raw_os_error,
        Some(ERROR_INVALID_FUNCTION)
            | Some(ERROR_NOT_SUPPORTED)
            | Some(ERROR_INVALID_PARAMETER)
            | Some(ERROR_CALL_NOT_IMPLEMENTED)
    )
}

#[cfg(any(windows, test))]
fn windows_rename_parent_access_mode() -> u32 {
    const FILE_TRAVERSE: u32 = 0x20;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    FILE_TRAVERSE | FILE_READ_ATTRIBUTES
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
    // Rust std uses 248 here because some legacy APIs have a lower limit than
    // the better-known 260-unit MAX_PATH.
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

// Matches the layout of FILE_RENAME_INFO while remaining testable on non-Windows
// hosts (windows-sys is a target-specific dependency).  The first field is the
// four-byte union containing ReplaceIfExists/Flags; zero means no replacement.
#[cfg(any(windows, test))]
#[repr(C)]
struct WindowsFileRenameInfoLayout {
    replace_or_flags: u32,
    root_directory: usize,
    file_name_length: u32,
    file_name: [u16; 1],
}

#[cfg(any(windows, test))]
struct WindowsRenameInfoBuffer {
    storage: Vec<usize>,
    buffer_size: u32,
}

#[cfg(any(windows, test))]
impl WindowsRenameInfoBuffer {
    fn new(root_directory: usize, file_name: &[u16], flags: u32) -> std::io::Result<Self> {
        use std::mem::{offset_of, size_of};

        if file_name.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Windows rename destination name is empty",
            ));
        }
        if file_name.contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Windows rename destination contains an interior NUL",
            ));
        }

        let file_name_bytes = file_name
            .len()
            .checked_mul(size_of::<u16>())
            .ok_or_else(|| std::io::Error::other("Windows rename destination is too long"))?;
        // Keep a trailing NUL in the allocation for defensive interoperability.
        // FileNameLength remains authoritative and excludes this terminator.
        let buffer_size = offset_of!(WindowsFileRenameInfoLayout, file_name)
            .checked_add(file_name_bytes)
            .and_then(|size| size.checked_add(size_of::<u16>()))
            .ok_or_else(|| std::io::Error::other("Windows rename buffer size overflow"))?;
        let buffer_size_u32 = u32::try_from(buffer_size)
            .map_err(|_| std::io::Error::other("Windows rename destination is too long"))?;
        let storage_words = buffer_size.div_ceil(size_of::<usize>());
        let mut storage = vec![0usize; storage_words];
        let info = storage.as_mut_ptr().cast::<WindowsFileRenameInfoLayout>();

        unsafe {
            std::ptr::addr_of_mut!((*info).replace_or_flags).write(flags);
            std::ptr::addr_of_mut!((*info).root_directory).write(root_directory);
            std::ptr::addr_of_mut!((*info).file_name_length)
                .write(u32::try_from(file_name_bytes).map_err(|_| {
                    std::io::Error::other("Windows rename destination is too long")
                })?);
            file_name.as_ptr().copy_to_nonoverlapping(
                std::ptr::addr_of_mut!((*info).file_name).cast::<u16>(),
                file_name.len(),
            );
        }

        Ok(Self {
            storage,
            buffer_size: buffer_size_u32,
        })
    }

    #[cfg(windows)]
    fn as_ptr(&self) -> *const std::ffi::c_void {
        self.storage.as_ptr().cast()
    }

    #[cfg(windows)]
    fn buffer_size(&self) -> u32 {
        self.buffer_size
    }

    #[cfg(test)]
    fn flags_for_test(&self) -> u32 {
        let info = self.storage.as_ptr().cast::<WindowsFileRenameInfoLayout>();
        unsafe { (*info).replace_or_flags }
    }

    #[cfg(test)]
    fn root_directory_for_test(&self) -> usize {
        let info = self.storage.as_ptr().cast::<WindowsFileRenameInfoLayout>();
        unsafe { (*info).root_directory }
    }

    #[cfg(test)]
    fn file_name_length_for_test(&self) -> u32 {
        let info = self.storage.as_ptr().cast::<WindowsFileRenameInfoLayout>();
        unsafe { (*info).file_name_length }
    }

    #[cfg(test)]
    fn buffer_size_for_test(&self) -> u32 {
        self.buffer_size
    }

    #[cfg(test)]
    fn file_name_for_test(&self) -> Vec<u16> {
        let info = self.storage.as_ptr().cast::<WindowsFileRenameInfoLayout>();
        let units = unsafe { (*info).file_name_length as usize } / std::mem::size_of::<u16>();
        unsafe {
            std::slice::from_raw_parts(std::ptr::addr_of!((*info).file_name).cast::<u16>(), units)
                .to_vec()
        }
    }
}

#[cfg(windows)]
fn persist_windows_symlink(
    stage: &Path,
    dst: &Path,
    policy: SymlinkCommitPolicy,
) -> std::result::Result<(), BcmrError> {
    let operation = windows_rename_operation(windows_symlink_commit_dispatch(policy));
    persist_windows_symlink_by_handle(stage, dst, operation)
        .map_err(|error| map_windows_handle_commit_error(policy, dst, error))
}

#[cfg(any(windows, test))]
fn map_windows_handle_commit_error(
    policy: SymlinkCommitPolicy,
    dst: &Path,
    error: std::io::Error,
) -> BcmrError {
    if policy.replaces_existing() && windows_extended_rename_unavailable(error.raw_os_error()) {
        BcmrError::InvalidInput(format!(
            "cannot atomically replace '{}' on this Windows version, filesystem, or destination: \
             FileRenameInfoEx with replace-if-exists and POSIX semantics is unavailable; \
             the existing destination was preserved ({error})",
            dst.display()
        ))
    } else {
        BcmrError::Io(error)
    }
}

#[cfg(windows)]
fn persist_windows_symlink_by_handle(
    stage: &Path,
    dst: &Path,
    operation: WindowsRenameOperation,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
    };

    let share_mode = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    let mut stage_options = std::fs::OpenOptions::new();
    stage_options
        .access_mode(DELETE)
        .share_mode(share_mode)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    // Opening through std gives both drive and UNC paths its long-path handling.
    let stage_file = stage_options.open(stage)?;

    let mut parent_options = std::fs::OpenOptions::new();
    debug_assert_eq!(
        windows_rename_parent_access_mode(),
        FILE_TRAVERSE | FILE_READ_ATTRIBUTES
    );
    parent_options
        .access_mode(windows_rename_parent_access_mode())
        .share_mode(share_mode)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    let parent_file = parent_options.open(destination_parent(dst))?;

    let destination_name = dst.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Windows rename destination has no basename",
        )
    })?;
    let destination_name_wide: Vec<u16> = destination_name.encode_wide().collect();
    let root_relative = WindowsRenameInfoBuffer::new(
        parent_file.as_raw_handle() as usize,
        &destination_name_wide,
        operation.flags,
    )?;

    match set_windows_rename_info(&stage_file, &root_relative, operation) {
        Ok(()) => Ok(()),
        Err(error) if windows_root_relative_retry_error(error.raw_os_error()) => {
            // MS-FSCC requires RootDirectory=0 for network rename requests.
            // Retry on the same source handle with an explicit-length absolute
            // name. This retains the requested atomic no-clobber or replacing
            // semantics and does not invoke a MAX_PATH-limited path rename API.
            let absolute_destination = std::path::absolute(dst)?;
            let absolute_wide: Vec<u16> = absolute_destination.as_os_str().encode_wide().collect();
            let absolute_rename_name = windows_absolute_rename_name(&absolute_wide)?;
            let absolute = WindowsRenameInfoBuffer::new(0, &absolute_rename_name, operation.flags)?;
            set_windows_rename_info(&stage_file, &absolute, operation)
        }
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn set_windows_rename_info(
    stage_file: &std::fs::File,
    buffer: &WindowsRenameInfoBuffer,
    operation: WindowsRenameOperation,
) -> std::io::Result<()> {
    use std::mem::{align_of, offset_of};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FileRenameInfo, FileRenameInfoEx, SetFileInformationByHandle, FILE_RENAME_INFO,
    };

    debug_assert!(
        align_of::<usize>() >= align_of::<FILE_RENAME_INFO>(),
        "rename buffer must satisfy FILE_RENAME_INFO alignment"
    );
    debug_assert_eq!(
        offset_of!(WindowsFileRenameInfoLayout, root_directory),
        offset_of!(FILE_RENAME_INFO, RootDirectory)
    );
    debug_assert_eq!(
        offset_of!(WindowsFileRenameInfoLayout, file_name_length),
        offset_of!(FILE_RENAME_INFO, FileNameLength)
    );
    debug_assert_eq!(
        offset_of!(WindowsFileRenameInfoLayout, file_name),
        offset_of!(FILE_RENAME_INFO, FileName)
    );
    debug_assert_eq!(WINDOWS_FILE_RENAME_INFO_CLASS, FileRenameInfo);
    debug_assert_eq!(WINDOWS_FILE_RENAME_INFO_EX_CLASS, FileRenameInfoEx);
    let information_class = match operation.information_class {
        WINDOWS_FILE_RENAME_INFO_CLASS => FileRenameInfo,
        WINDOWS_FILE_RENAME_INFO_EX_CLASS => FileRenameInfoEx,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unsupported Windows rename information class",
            ));
        }
    };

    let result = unsafe {
        SetFileInformationByHandle(
            stage_file.as_raw_handle() as HANDLE,
            information_class,
            buffer.as_ptr(),
            buffer.buffer_size(),
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_metadata_propagates_non_not_found_errors() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("regular-file");
        std::fs::write(&blocker, b"x").unwrap();

        let error = symlink_entry_metadata(&blocker.join("child")).unwrap_err();
        match error {
            BcmrError::Io(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::NotADirectory);
            }
            other => panic!("expected propagated IO error, got {other:?}"),
        }
    }

    #[test]
    fn windows_directory_symlink_force_selects_safety_noclobber() {
        assert_eq!(
            symlink_commit_policy(true, SymlinkKind::Directory, true),
            SymlinkCommitPolicy::WindowsDirectorySafetyNoClobber
        );
        assert_eq!(
            symlink_commit_policy(true, SymlinkKind::File, true),
            SymlinkCommitPolicy::ReplaceExisting
        );
        assert_eq!(
            symlink_commit_policy(false, SymlinkKind::Directory, true),
            SymlinkCommitPolicy::ReplaceExisting
        );
        assert_eq!(
            symlink_commit_policy(true, SymlinkKind::Directory, false),
            SymlinkCommitPolicy::NoClobber
        );
    }

    #[test]
    fn windows_dispatch_uses_handles_for_noclobber_and_force_replace() {
        assert_eq!(
            windows_symlink_commit_dispatch(SymlinkCommitPolicy::NoClobber),
            WindowsSymlinkCommitDispatch::HandleNoClobber
        );
        assert_eq!(
            windows_symlink_commit_dispatch(SymlinkCommitPolicy::WindowsDirectorySafetyNoClobber),
            WindowsSymlinkCommitDispatch::HandleNoClobber
        );
        assert_eq!(
            windows_symlink_commit_dispatch(SymlinkCommitPolicy::ReplaceExisting),
            WindowsSymlinkCommitDispatch::HandleReplace
        );
        assert_eq!(
            windows_rename_operation(WindowsSymlinkCommitDispatch::HandleNoClobber),
            WindowsRenameOperation {
                information_class: 3,
                flags: 0,
            }
        );
        assert_eq!(
            windows_rename_operation(WindowsSymlinkCommitDispatch::HandleReplace),
            WindowsRenameOperation {
                information_class: 22,
                flags: 1 | 2,
            }
        );
        assert_eq!(
            windows_rename_parent_access_mode(),
            0x20 | 0x80,
            "the RootDirectory handle needs FILE_TRAVERSE | FILE_READ_ATTRIBUTES"
        );
    }

    #[test]
    fn windows_noclobber_buffer_is_nonreplacing_and_root_relative() {
        let filename: Vec<u16> = "landed-link".encode_utf16().collect();
        let buffer = WindowsRenameInfoBuffer::new(0x1234, &filename, 0).unwrap();

        assert_eq!(buffer.flags_for_test(), 0);
        assert_eq!(buffer.root_directory_for_test(), 0x1234);
        assert_eq!(buffer.file_name_for_test(), filename);
        assert_eq!(
            buffer.file_name_length_for_test(),
            u32::try_from(filename.len() * std::mem::size_of::<u16>()).unwrap()
        );
        assert!(
            buffer.buffer_size_for_test() > buffer.file_name_length_for_test(),
            "the API buffer must include the fixed FILE_RENAME_INFO fields"
        );
    }

    #[test]
    fn windows_smb_fallback_is_conservative_and_supports_long_absolute_names() {
        for error in [50, 87] {
            assert!(windows_root_relative_retry_error(Some(error)));
        }
        for error in [1, 5, 32, 80, 120, 183] {
            assert!(!windows_root_relative_retry_error(Some(error)));
        }
        assert!(!windows_root_relative_retry_error(None));

        let long_drive_path = format!(r"C:\folder\{}\landed-link", "x".repeat(300));
        let long_drive_wide: Vec<u16> = long_drive_path.encode_utf16().collect();
        let long_absolute_name = windows_absolute_rename_name(&long_drive_wide).unwrap();
        let expected = format!(r"\\?\{long_drive_path}");
        assert_eq!(
            String::from_utf16(&long_absolute_name).unwrap(),
            expected,
            "long drive paths must use a verbatim prefix"
        );

        let replace = windows_rename_operation(WindowsSymlinkCommitDispatch::HandleReplace);
        let buffer = WindowsRenameInfoBuffer::new(0, &long_absolute_name, replace.flags).unwrap();
        assert_eq!(buffer.root_directory_for_test(), 0);
        assert_eq!(buffer.flags_for_test(), 3);
        assert_eq!(buffer.file_name_for_test(), long_absolute_name);
    }

    #[test]
    fn windows_force_replace_fails_safely_when_extended_rename_is_unavailable() {
        let dst = Path::new(r"C:\destination\landed");
        for raw_error in [1, 50, 87, 120] {
            let error = map_windows_handle_commit_error(
                SymlinkCommitPolicy::ReplaceExisting,
                dst,
                std::io::Error::from_raw_os_error(raw_error),
            );
            match error {
                BcmrError::InvalidInput(message) => {
                    assert!(message.contains("FileRenameInfoEx"));
                    assert!(message.contains("preserved"));
                }
                other => panic!("expected typed unsupported error, got {other:?}"),
            }
        }

        let ordinary_io = map_windows_handle_commit_error(
            SymlinkCommitPolicy::NoClobber,
            dst,
            std::io::Error::from_raw_os_error(50),
        );
        assert!(matches!(ordinary_io, BcmrError::Io(_)));
    }

    #[test]
    fn windows_absolute_rename_name_handles_drive_unc_and_existing_prefixes() {
        fn convert(path: &str) -> std::io::Result<String> {
            let wide: Vec<u16> = path.encode_utf16().collect();
            windows_absolute_rename_name(&wide)
                .and_then(|converted| String::from_utf16(&converted).map_err(std::io::Error::other))
        }

        for short in [r"C:\short\landed", r"\\server\share\landed"] {
            assert_eq!(convert(short).unwrap(), short);
        }

        let long_tail = "x".repeat(300);
        let drive = format!(r"C:\folder\{long_tail}\landed");
        assert_eq!(convert(&drive).unwrap(), format!(r"\\?\{drive}"));

        let unc = format!(r"\\server\share\{long_tail}\landed");
        assert_eq!(
            convert(&unc).unwrap(),
            format!(r"\\?\UNC\server\share\{long_tail}\landed")
        );

        let verbatim = format!(r"\\?\C:\folder\{long_tail}\landed");
        assert_eq!(convert(&verbatim).unwrap(), verbatim);
        let nt = format!(r"\??\C:\folder\{long_tail}\landed");
        assert_eq!(convert(&nt).unwrap(), nt);

        let mut nul = "C:\\folder".encode_utf16().collect::<Vec<_>>();
        nul.push(0);
        nul.extend("landed".encode_utf16());
        assert_eq!(
            windows_absolute_rename_name(&nul).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn noclobber_error_mapping_uses_post_failure_destination_observation() {
        assert!(should_map_noclobber_error_to_target_exists(true, false));
        assert!(should_map_noclobber_error_to_target_exists(false, true));
        assert!(!should_map_noclobber_error_to_target_exists(false, false));
    }

    #[test]
    fn windows_directory_safety_preflight_rejects_existing_entries() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("landed");
        std::fs::write(&dst, b"preserve me").unwrap();
        let policy = SymlinkCommitPolicy::WindowsDirectorySafetyNoClobber;

        let error = check_commit_policy_preflight(&dst, policy).unwrap_err();
        assert!(matches!(error, BcmrError::InvalidInput(_)));
        assert_eq!(std::fs::read(&dst).unwrap(), b"preserve me");

        let missing = dir.path().join("missing");
        check_commit_policy_preflight(&missing, policy).unwrap();

        let blocker = dir.path().join("regular");
        std::fs::write(&blocker, b"x").unwrap();
        let error = check_commit_policy_preflight(&blocker.join("child"), policy).unwrap_err();
        assert!(matches!(
            error,
            BcmrError::Io(ref error) if error.kind() == std::io::ErrorKind::NotADirectory
        ));
    }

    #[cfg(unix)]
    #[test]
    fn windows_directory_safety_policy_preserves_racing_real_directory() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let dst = dir.path().join("landed");
        let staging = create_symlink_staging(&dst, &target, SymlinkKind::Directory, false).unwrap();

        // The competitor wins after the caller's preflight but before commit.
        std::fs::create_dir(&dst).unwrap();
        let error = staging
            .commit(
                &dst,
                SymlinkCommitPolicy::WindowsDirectorySafetyNoClobber,
                false,
            )
            .unwrap_err();

        assert!(matches!(error, BcmrError::InvalidInput(_)));
        let metadata = dst.symlink_metadata().unwrap();
        assert!(metadata.is_dir());
        assert!(!metadata.file_type().is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn windows_directory_safety_policy_preserves_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("landed");
        std::os::unix::fs::symlink("old-target", &dst).unwrap();
        let staging =
            create_symlink_staging(&dst, Path::new("new-target"), SymlinkKind::Directory, false)
                .unwrap();

        let error = staging
            .commit(
                &dst,
                SymlinkCommitPolicy::WindowsDirectorySafetyNoClobber,
                false,
            )
            .unwrap_err();

        match error {
            BcmrError::InvalidInput(message) => {
                assert!(message.contains("Windows"));
                assert!(message.contains("no-clobber"));
                assert!(message.contains("preserved"));
            }
            other => panic!("expected the Windows safety limitation, got {other:?}"),
        }
        assert_eq!(std::fs::read_link(&dst).unwrap(), Path::new("old-target"));
    }
}
