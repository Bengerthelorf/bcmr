//! Native no-replace moves and non-following, captured source cleanup.
use crate::core::error::BcmrError;
use crate::core::traversal;
#[cfg(unix)]
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Atomically rename an entry while preserving any existing destination.
pub fn rename_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let source_name = std::ffi::CString::new(source.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "move source contains NUL")
        })?;
        let destination_name =
            std::ffi::CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "move destination contains NUL",
                )
            })?;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                source_name.as_ptr(),
                libc::AT_FDCWD,
                destination_name.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "watchos",
            target_os = "visionos"
        ))]
        let result = unsafe {
            libc::renameatx_np(
                libc::AT_FDCWD,
                source_name.as_ptr(),
                libc::AT_FDCWD,
                destination_name.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(not(any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "watchos",
            target_os = "visionos"
        )))]
        {
            let _ = (source_name, destination_name);
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "atomic no-replace move is unavailable on this platform",
            ));
        }
        #[cfg(any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "watchos",
            target_os = "visionos"
        ))]
        if result != 0 {
            return Err(std::io::Error::last_os_error());
        }
        #[cfg(any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "watchos",
            target_os = "visionos"
        ))]
        return Ok(());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide = |path: &Path| -> std::io::Result<Vec<u16>> {
            let mut name: Vec<_> = path.as_os_str().encode_wide().collect();
            if name.contains(&0) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "move path contains NUL",
                ));
            }
            name.push(0);
            Ok(name)
        };
        let source = wide(source)?;
        let destination = wide(destination)?;
        // No REPLACE_EXISTING and no COPY_ALLOWED: both collision and EXDEV
        // remain failures, including for directories and reparse-point links.
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                0,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (source, destination);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "atomic no-replace move is unavailable on this platform",
        ))
    }
}

pub fn is_target_exists(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::AlreadyExists
        || cfg!(windows) && matches!(error.raw_os_error(), Some(80 | 183))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryKind {
    File,
    Directory,
    Symlink,
}

#[cfg(windows)]
fn entry_handle(path: &Path) -> std::io::Result<same_file::Handle> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let file = std::fs::OpenOptions::new()
        .access_mode(0)
        .share_mode(FILE_SHARE_DELETE | FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    same_file::Handle::from_file(file)
}

#[derive(Debug)]
struct SourceStamp {
    kind: EntryKind,
    len: u64,
    modified: Option<std::time::SystemTime>,
    readonly: bool,
    #[cfg(unix)]
    identity: (u64, u64),
    #[cfg(unix)]
    security: (u32, u32, u32),
    #[cfg(unix)]
    changed: (i64, i64),
    #[cfg(windows)]
    identity: same_file::Handle,
}

impl SourceStamp {
    fn capture(path: &Path) -> Result<Self, BcmrError> {
        let metadata = std::fs::symlink_metadata(path)?;
        let kind = if metadata.file_type().is_symlink() {
            EntryKind::Symlink
        } else if metadata.is_dir() {
            EntryKind::Directory
        } else if metadata.is_file() {
            EntryKind::File
        } else {
            return Err(BcmrError::InvalidInput(format!(
                "cannot safely move special source '{}'",
                path.display()
            )));
        };
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            kind,
            len: metadata.len(),
            modified: metadata.modified().ok(),
            readonly: metadata.permissions().readonly(),
            #[cfg(unix)]
            identity: (metadata.dev(), metadata.ino()),
            #[cfg(unix)]
            security: (metadata.mode(), metadata.uid(), metadata.gid()),
            #[cfg(unix)]
            changed: (metadata.ctime(), metadata.ctime_nsec()),
            #[cfg(windows)]
            identity: entry_handle(path)?,
        })
    }

    fn same_identity(&self, actual: &Self) -> bool {
        if self.kind != actual.kind {
            return false;
        }
        #[cfg(any(unix, windows))]
        {
            self.identity == actual.identity
        }
        #[cfg(not(any(unix, windows)))]
        false
    }

    fn matches_after_rename(&self, actual: &Self) -> bool {
        if !self.same_identity(actual) || self.readonly != actual.readonly {
            return false;
        }
        #[cfg(unix)]
        if self.security != actual.security {
            return false;
        }
        // Own child removals change directory size and timestamps. Keep the
        // identity and security checks; directory removal is nonrecursive.
        if self.kind == EntryKind::Directory {
            return true;
        }
        // A quarantine rename changes Unix ctime, so compare stable fields
        // here and enforce the full ctime stamp before that rename instead.
        self.len == actual.len && self.modified == actual.modified
    }
}

/// Compare the source entries themselves, including links rather than targets.
pub fn same_entry(source: &Path, destination: &Path) -> Result<bool, BcmrError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let source = std::fs::symlink_metadata(source)?;
        let destination = std::fs::symlink_metadata(destination)?;
        Ok(source.dev() == destination.dev() && source.ino() == destination.ino())
    }
    #[cfg(windows)]
    {
        Ok(entry_handle(source)? == entry_handle(destination)?)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Ok(std::path::absolute(source)? == std::path::absolute(destination)?)
    }
}

#[derive(Debug)]
struct CleanupEntry {
    path: PathBuf,
    stamp: SourceStamp,
}

/// A preorder, non-following selection captured before copying begins.
/// Cleanup never discovers additional source paths and never recursively deletes.
#[derive(Debug)]
pub struct LocalCleanupPlan {
    entries: Vec<CleanupEntry>,
}

impl LocalCleanupPlan {
    pub fn capture(paths: &[PathBuf], excludes: &[regex::Regex]) -> Result<Self, BcmrError> {
        crate::core::interrupt::check()?;
        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        for source in paths {
            if traversal::is_copy_excluded(source, source, excludes) {
                continue;
            }
            // Explicit no_deref keeps both root and descendant links as leaves;
            // preorder pruning happens before reading excluded directories.
            for entry in traversal::copy_walk(source, true, 0, true, excludes) {
                crate::core::interrupt::check()?;
                let entry = entry?;
                let path = std::path::absolute(entry.path())?;
                if seen.insert(path.clone()) {
                    entries.push(CleanupEntry {
                        stamp: SourceStamp::capture(&path)?,
                        path,
                    });
                }
            }
        }
        Ok(Self { entries })
    }

    pub async fn remove(&self) -> Result<(), BcmrError> {
        self.remove_with_hooks_async(|_| {}, |_, _| {}).await
    }

    #[cfg(test)]
    fn remove_with_hook(&self, before_quarantine: impl FnMut(&Path)) -> Result<(), BcmrError> {
        self.remove_with_hooks(before_quarantine, |_, _| {})
    }

    #[cfg(test)]
    fn remove_with_hooks(
        &self,
        before_quarantine: impl FnMut(&Path),
        after_quarantine: impl FnMut(&Path, &Path),
    ) -> Result<(), BcmrError> {
        futures::executor::block_on(
            self.remove_with_hooks_async(before_quarantine, after_quarantine),
        )
    }

    async fn remove_with_hooks_async(
        &self,
        mut before_quarantine: impl FnMut(&Path),
        mut after_quarantine: impl FnMut(&Path, &Path),
    ) -> Result<(), BcmrError> {
        #[cfg(unix)]
        let (mut aliases, mut expected_changes) = {
            let mut aliases: HashMap<_, Vec<&CleanupEntry>> = HashMap::new();
            let mut expected_changes = HashMap::new();
            for entry in &self.entries {
                if entry.stamp.kind == EntryKind::Directory {
                    continue;
                }
                let group = aliases.entry(entry.stamp.identity).or_default();
                if let Some(previous) = group.first() {
                    // Do not normalize inconsistent captures of the same inode
                    // when accounting for our own hardlink cleanup changes.
                    if !previous.stamp.matches_after_rename(&entry.stamp)
                        || previous.stamp.changed != entry.stamp.changed
                    {
                        return Err(BcmrError::DestinationChanged(entry.path.clone()));
                    }
                }
                group.push(entry);
                expected_changes.insert(entry.stamp.identity, entry.stamp.changed);
            }
            (aliases, expected_changes)
        };
        for entry in self.entries.iter().rev() {
            crate::core::interrupt::check()?;
            #[cfg(unix)]
            let next_alias = aliases.get_mut(&entry.stamp.identity).and_then(|group| {
                group.pop();
                group.last().copied()
            });
            let actual = match SourceStamp::capture(&entry.path) {
                Ok(actual) => actual,
                Err(BcmrError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    continue
                }
                Err(error) => return Err(error),
            };
            if !entry.stamp.matches_after_rename(&actual) {
                return Err(BcmrError::DestinationChanged(entry.path.clone()));
            }
            #[cfg(unix)]
            if entry.stamp.kind != EntryKind::Directory
                && expected_changes.get(&entry.stamp.identity) != Some(&actual.changed)
            {
                return Err(BcmrError::DestinationChanged(entry.path.clone()));
            }
            if entry.stamp.kind == EntryKind::Directory {
                // Surviving parents can contain excluded or newly created
                // children. Never move those subtrees out of place.
                if std::fs::read_dir(&entry.path)?
                    .next()
                    .transpose()?
                    .is_some()
                {
                    continue;
                }
            }
            let parent = entry
                .path
                .parent()
                .ok_or_else(|| BcmrError::InvalidInput("cleanup source has no parent".into()))?;
            crate::core::interrupt::check()?;
            let mut quarantine = tempfile::Builder::new()
                .prefix(".bcmr.move.")
                .tempdir_in(parent)?;
            // This directory may contain an uncopied racing entry after rename.
            // Never let TempDir recursively delete it, even on an unwind.
            quarantine.disable_cleanup(true);
            let private_path = quarantine.path().join("entry");
            before_quarantine(&entry.path);
            if let Err(error) = crate::core::interrupt::check() {
                let _ = std::fs::remove_dir(quarantine.path());
                return Err(error);
            }
            if let Err(error) = rename_no_replace(&entry.path, &private_path) {
                let _ = std::fs::remove_dir(quarantine.path());
                return Err(error.into());
            }
            after_quarantine(&entry.path, &private_path);
            let moved = SourceStamp::capture(&private_path);
            if !moved
                .as_ref()
                .is_ok_and(|actual| entry.stamp.matches_after_rename(actual))
            {
                let restored = rename_no_replace(&private_path, &entry.path);
                if restored.is_ok() {
                    let _ = std::fs::remove_dir(quarantine.path());
                }
                return Err(BcmrError::InvalidInput(format!(
                    "source '{}' changed before cleanup; {}",
                    entry.path.display(),
                    if restored.is_ok() {
                        "uncopied entry restored".to_string()
                    } else {
                        format!(
                            "uncopied recovery data retained at '{}'",
                            private_path.display()
                        )
                    }
                )));
            }
            let validation = async {
                crate::core::interrupt::check()?;
                if entry.stamp.kind != EntryKind::Directory
                    && crate::core::remote::move_delivery::has_local(&entry.path)
                {
                    // The original name is now absent. A destination that is
                    // merely another spelling of that entry cannot validate.
                    // Keep quarantine alive until the read finishes or cancels.
                    crate::core::interrupt::read_only(
                        crate::core::remote::move_delivery::validate_local(&entry.path),
                    )
                    .await?;
                    let actual = SourceStamp::capture(&private_path)?;
                    if !entry.stamp.matches_after_rename(&actual) {
                        return Err(BcmrError::DestinationChanged(entry.path.clone()));
                    }
                }
                crate::core::interrupt::check()
            }
            .await;
            if let Err(error) = validation {
                if rename_no_replace(&private_path, &entry.path).is_ok() {
                    let _ = std::fs::remove_dir(quarantine.path());
                    return Err(error);
                }
                let message = format!(
                    "{error}; source recovery data retained at '{}'",
                    private_path.display()
                );
                return Err(match error {
                    BcmrError::Io(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                        BcmrError::Io(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            message,
                        ))
                    }
                    _ => BcmrError::InvalidInput(message),
                });
            }
            if let Err(error) = remove_leaf(&private_path, entry.stamp.kind) {
                // A child may arrive after the empty-directory precheck.
                if entry.stamp.kind == EntryKind::Directory
                    && error.kind() == std::io::ErrorKind::DirectoryNotEmpty
                    && rename_no_replace(&private_path, &entry.path).is_ok()
                {
                    std::fs::remove_dir(quarantine.path())?;
                    continue;
                }
                return Err(BcmrError::InvalidInput(format!(
                    "copied source cleanup failed: {error}; recovery data retained at '{}'",
                    private_path.display()
                )));
            }
            std::fs::remove_dir(quarantine.path())?;
            #[cfg(unix)]
            if let Some(next) = next_alias {
                let updated = SourceStamp::capture(&next.path)?;
                if !entry.stamp.matches_after_rename(&updated)
                    || !next.stamp.matches_after_rename(&updated)
                {
                    return Err(BcmrError::DestinationChanged(next.path.clone()));
                }
                // Rename and unlink change every hardlink's shared ctime.
                // Refresh only that field after our verified own operation;
                // the next alias still has a strict full-stamp precheck.
                // External writes during the operation-to-restat window are
                // not a filesystem transaction and can remain undetectable.
                expected_changes.insert(entry.stamp.identity, updated.changed);
            }
        }
        crate::core::interrupt::check()?;
        Ok(())
    }
}

fn remove_leaf(path: &Path, kind: EntryKind) -> std::io::Result<()> {
    if kind == EntryKind::Directory {
        return std::fs::remove_dir(path);
    }
    #[cfg(windows)]
    if kind == EntryKind::Symlink {
        use std::os::windows::fs::MetadataExt;
        if std::fs::symlink_metadata(path)?.file_attributes() & 0x10 != 0 {
            return std::fs::remove_dir(path);
        }
    }
    let _ = kind;
    std::fs::remove_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_replace_move_preserves_racing_file_and_directory() {
        let dir = tempfile::tempdir().unwrap();
        for directory in [false, true] {
            let source = dir.path().join(if directory {
                "source-dir"
            } else {
                "source-file"
            });
            let destination = dir.path().join(if directory {
                "target-dir"
            } else {
                "target-file"
            });
            if directory {
                std::fs::create_dir(&source).unwrap();
                std::fs::create_dir(&destination).unwrap();
            } else {
                std::fs::write(&source, b"source").unwrap();
                std::fs::write(&destination, b"competitor").unwrap();
            }
            assert!(rename_no_replace(&source, &destination).is_err());
            assert!(source.exists());
            if directory {
                assert!(destination.is_dir());
            } else {
                assert_eq!(std::fs::read(destination).unwrap(), b"competitor");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn no_replace_move_preserves_racing_symlink_and_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        let outside = dir.path().join("outside");
        std::fs::write(&source, b"source").unwrap();
        std::fs::write(&outside, b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, &destination).unwrap();
        assert!(rename_no_replace(&source, &destination).is_err());
        assert_eq!(std::fs::read(source).unwrap(), b"source");
        assert!(destination.is_symlink());
        assert_eq!(std::fs::read(outside).unwrap(), b"outside");
    }

    #[tokio::test]
    async fn captured_cleanup_preserves_new_and_excluded_entries() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir_all(source.join("keep")).unwrap();
        std::fs::write(source.join("keep/private"), b"excluded").unwrap();
        std::fs::write(source.join("selected"), b"copied").unwrap();
        let pattern = regex::Regex::new(&format!(
            "{}$",
            regex::escape(&source.join("keep").to_string_lossy())
        ))
        .unwrap();
        let plan = LocalCleanupPlan::capture(std::slice::from_ref(&source), &[pattern]).unwrap();
        std::fs::write(source.join("new"), b"not copied").unwrap();
        plan.remove().await.unwrap();
        assert!(!source.join("selected").exists());
        assert_eq!(std::fs::read(source.join("new")).unwrap(), b"not copied");
        assert_eq!(
            std::fs::read(source.join("keep/private")).unwrap(),
            b"excluded"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_preserves_same_length_rewrite_with_restored_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::write(&source, b"copied").unwrap();
        let metadata = std::fs::metadata(&source).unwrap();
        let plan = LocalCleanupPlan::capture(std::slice::from_ref(&source), &[]).unwrap();
        std::fs::write(&source, b"newest").unwrap();
        filetime::set_file_mtime(
            &source,
            filetime::FileTime::from_last_modification_time(&metadata),
        )
        .unwrap();
        assert!(plan.remove().await.is_err());
        assert_eq!(std::fs::read(source).unwrap(), b"newest");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_preserves_directory_with_changed_security() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o700)).unwrap();
        let plan = LocalCleanupPlan::capture(std::slice::from_ref(&source), &[]).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert!(plan.remove().await.is_err());
        assert!(source.is_dir());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_removes_captured_hardlinks_despite_own_ctime_changes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        std::fs::write(source.join("first"), b"copied").unwrap();
        std::fs::hard_link(source.join("first"), source.join("nested/second")).unwrap();
        let plan = LocalCleanupPlan::capture(std::slice::from_ref(&source), &[]).unwrap();
        plan.remove().await.unwrap();
        assert!(!source.exists());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_checks_hardlink_ctime_again_after_own_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let remaining = dir.path().join("remaining");
        let between = dir.path().join("between");
        let first = dir.path().join("first");
        std::fs::write(&remaining, b"copied").unwrap();
        std::fs::hard_link(&remaining, &first).unwrap();
        std::fs::write(&between, b"copied").unwrap();
        let metadata = std::fs::metadata(&remaining).unwrap();
        let plan =
            LocalCleanupPlan::capture(&[remaining.clone(), between.clone(), first.clone()], &[])
                .unwrap();
        let result = plan.remove_with_hook(|path| {
            if path == between {
                std::fs::write(&remaining, b"newest").unwrap();
                filetime::set_file_mtime(
                    &remaining,
                    filetime::FileTime::from_last_modification_time(&metadata),
                )
                .unwrap();
            }
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(remaining).unwrap(), b"newest");
        assert!(!first.exists());
    }

    #[test]
    fn cleanup_restores_a_replacement_racing_the_quarantine_rename() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let original = dir.path().join("original");
        std::fs::write(&source, b"copied").unwrap();
        let plan = LocalCleanupPlan::capture(std::slice::from_ref(&source), &[]).unwrap();
        let result = plan.remove_with_hook(|path| {
            std::fs::rename(path, &original).unwrap();
            std::fs::write(path, b"uncopied competitor").unwrap();
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(source).unwrap(), b"uncopied competitor");
        assert_eq!(std::fs::read(original).unwrap(), b"copied");
    }

    #[test]
    fn cleanup_retains_recovery_when_the_original_path_is_recreated() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let original = dir.path().join("original");
        std::fs::write(&source, b"copied").unwrap();
        let plan = LocalCleanupPlan::capture(std::slice::from_ref(&source), &[]).unwrap();
        let result = plan.remove_with_hooks(
            |path| {
                std::fs::rename(path, &original).unwrap();
                std::fs::write(path, b"first uncopied competitor").unwrap();
            },
            |path, _| {
                std::fs::write(path, b"second competitor").unwrap();
            },
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("recovery data retained"));
        assert_eq!(std::fs::read(source).unwrap(), b"second competitor");
        assert_eq!(std::fs::read(original).unwrap(), b"copied");
        let recovery = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".bcmr.move.")
            })
            .unwrap();
        assert_eq!(
            std::fs::read(recovery.join("entry")).unwrap(),
            b"first uncopied competitor"
        );
    }

    #[test]
    fn cleanup_restores_a_directory_that_gains_an_uncopied_child() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let plan = LocalCleanupPlan::capture(std::slice::from_ref(&source), &[]).unwrap();
        plan.remove_with_hooks(
            |_| {},
            |_, private| {
                std::fs::write(private.join("new"), b"new child").unwrap();
            },
        )
        .unwrap();
        assert_eq!(std::fs::read(source.join("new")).unwrap(), b"new child");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_unlinks_directory_links_without_touching_their_targets() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        let source = dir.path().join("source");
        std::fs::create_dir(&outside).unwrap();
        std::fs::create_dir(&source).unwrap();
        std::fs::write(outside.join("private"), b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, source.join("link")).unwrap();
        let plan = LocalCleanupPlan::capture(std::slice::from_ref(&source), &[]).unwrap();
        plan.remove().await.unwrap();
        assert!(!source.exists());
        assert_eq!(std::fs::read(outside.join("private")).unwrap(), b"outside");
    }
}
