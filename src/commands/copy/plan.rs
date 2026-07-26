use crate::cli::Commands;
use crate::core::error::BcmrError;
use crate::core::traversal;
use crate::ui::display::{print_dry_run, ActionType};

use std::path::{Path, PathBuf};

use super::overwrite::{determine_dry_run_action, FileToOverwrite};

#[cfg_attr(test, derive(Debug))]
pub enum PlanEntry {
    CreateDir {
        src: PathBuf,
        dst: PathBuf,
    },
    CopyFile {
        src: PathBuf,
        dst: PathBuf,
    },
    Symlink {
        src: PathBuf,
        dst: PathBuf,
        target: PathBuf,
        kind: SymlinkKind,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymlinkKind {
    File,
    Directory,
}

fn symlink_entry_metadata(
    path: &Path,
) -> std::result::Result<Option<std::fs::Metadata>, BcmrError> {
    match path.symlink_metadata() {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(BcmrError::Io(error)),
    }
}

#[cfg(windows)]
fn scanned_symlink_kind(path: &Path, metadata: &std::fs::Metadata) -> SymlinkKind {
    use std::os::windows::fs::MetadataExt;

    let _ = path;
    windows_symlink_kind_from_attributes(metadata.file_attributes())
}

#[cfg(not(windows))]
fn scanned_symlink_kind(path: &Path, _metadata: &std::fs::Metadata) -> SymlinkKind {
    match path.metadata() {
        Ok(metadata) if metadata.is_dir() => SymlinkKind::Directory,
        _ => SymlinkKind::File,
    }
}

#[cfg(any(windows, test))]
fn windows_symlink_kind_from_attributes(attributes: u32) -> SymlinkKind {
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        SymlinkKind::Directory
    } else {
        SymlinkKind::File
    }
}

pub struct CopyPlan {
    pub entries: Vec<PlanEntry>,
    pub total_size: u64,
    pub overwrites: Vec<FileToOverwrite>,
}

fn is_dst_self_or_under_src(src: &Path, dst: &Path) -> std::io::Result<bool> {
    let src_canon = src.canonicalize()?;
    let mut probe = dst.to_path_buf();
    while !probe.exists() {
        if !probe.pop() {
            return Ok(false);
        }
    }
    // probe was produced by popping components from dst, so it's a lexical prefix.
    let suffix = dst.strip_prefix(&probe).unwrap();
    let dst_canon = probe.canonicalize()?.join(suffix);
    Ok(dst_canon == src_canon || dst_canon.starts_with(&src_canon))
}

pub(super) fn scan_sources(
    sources: &[PathBuf],
    dst: &Path,
    recursive: bool,
    no_deref: bool,
    excludes: &[regex::Regex],
    mut on_entry: impl FnMut(PlanEntry, u64) -> std::result::Result<(), BcmrError>,
) -> std::result::Result<(), BcmrError> {
    let dst_is_dir = dst.exists() && dst.is_dir();

    for src in sources {
        if traversal::is_excluded(src, excludes) {
            continue;
        }

        let source_entry_metadata = if no_deref {
            symlink_entry_metadata(src)?
        } else {
            None
        };
        if let Some(metadata) = source_entry_metadata
            .as_ref()
            .filter(|metadata| metadata.file_type().is_symlink())
        {
            let target = std::fs::read_link(src).map_err(BcmrError::Io)?;
            let kind = scanned_symlink_kind(src, metadata);
            let dst_path = if dst_is_dir {
                dst.join(
                    src.file_name()
                        .ok_or_else(BcmrError::invalid_source_file_name)?,
                )
            } else {
                dst.to_path_buf()
            };
            on_entry(
                PlanEntry::Symlink {
                    src: src.clone(),
                    dst: dst_path,
                    target,
                    kind,
                },
                0,
            )?;
            continue;
        }

        if src.is_file() {
            let dst_path = if dst_is_dir {
                dst.join(
                    src.file_name()
                        .ok_or_else(BcmrError::invalid_source_file_name)?,
                )
            } else {
                dst.to_path_buf()
            };

            let size = src.metadata()?.len();
            on_entry(
                PlanEntry::CopyFile {
                    src: src.clone(),
                    dst: dst_path,
                },
                size,
            )?;
        } else if recursive && src.is_dir() {
            let src_name = src
                .file_name()
                .ok_or_else(BcmrError::invalid_source_dir_name)?;
            let new_dst = if dst_is_dir {
                dst.join(src_name)
            } else {
                dst.to_path_buf()
            };

            if is_dst_self_or_under_src(src, &new_dst)? {
                return Err(BcmrError::InvalidInput(format!(
                    "cannot copy directory '{}' into itself ('{}')",
                    src.display(),
                    new_dst.display()
                )));
            }

            on_entry(
                PlanEntry::CreateDir {
                    src: src.clone(),
                    dst: new_dst.clone(),
                },
                0,
            )?;

            for entry in traversal::walk(src, true, false, 1, excludes) {
                let entry = entry?;
                let path = entry.path();
                let relative = path.strip_prefix(src)?;
                let target = new_dst.join(relative);

                let entry_metadata = if no_deref {
                    symlink_entry_metadata(path)?
                } else {
                    None
                };
                if let Some(metadata) = entry_metadata
                    .as_ref()
                    .filter(|metadata| metadata.file_type().is_symlink())
                {
                    let link_target = std::fs::read_link(path).map_err(BcmrError::Io)?;
                    on_entry(
                        PlanEntry::Symlink {
                            src: path.to_path_buf(),
                            dst: target,
                            target: link_target,
                            kind: scanned_symlink_kind(path, metadata),
                        },
                        0,
                    )?;
                } else if path.is_dir() {
                    on_entry(
                        PlanEntry::CreateDir {
                            src: path.to_path_buf(),
                            dst: target,
                        },
                        0,
                    )?;
                } else if path.is_file() {
                    let size = entry.metadata()?.len();
                    on_entry(
                        PlanEntry::CopyFile {
                            src: path.to_path_buf(),
                            dst: target,
                        },
                        size,
                    )?;
                }
            }
        } else if src.is_dir() {
            return Err(BcmrError::InvalidInput(format!(
                "Source '{}' is a directory. Use -r flag for recursive copy.",
                src.display()
            )));
        } else {
            return Err(BcmrError::SourceNotFound(src.clone()));
        }
    }

    Ok(())
}

fn plan_copy_sync(
    sources: Vec<PathBuf>,
    dst: PathBuf,
    recursive: bool,
    no_deref: bool,
    excludes: Vec<regex::Regex>,
) -> std::result::Result<CopyPlan, BcmrError> {
    let mut entries = Vec::new();
    let mut total_size = 0u64;
    let mut overwrites = Vec::new();

    scan_sources(
        &sources,
        &dst,
        recursive,
        no_deref,
        &excludes,
        |entry, size| {
            total_size += size;

            let target = match &entry {
                PlanEntry::CopyFile { dst, .. } if dst.exists() => Some((dst.clone(), false)),
                PlanEntry::CopyFile { .. } => None,
                PlanEntry::Symlink { dst, .. } if symlink_entry_metadata(dst)?.is_some() => {
                    Some((dst.clone(), false))
                }
                PlanEntry::Symlink { .. } => None,
                PlanEntry::CreateDir { dst, .. } if dst.exists() => Some((dst.clone(), true)),
                PlanEntry::CreateDir { .. } => None,
            };
            if let Some((path, is_dir)) = target {
                if !traversal::is_excluded(&path, &excludes) {
                    overwrites.push(FileToOverwrite { path, is_dir });
                }
            }

            entries.push(entry);
            Ok(())
        },
    )?;

    Ok(CopyPlan {
        entries,
        total_size,
        overwrites,
    })
}

pub async fn plan_copy(
    sources: &[PathBuf],
    dst: &Path,
    recursive: bool,
    no_deref: bool,
    excludes: &[regex::Regex],
) -> std::result::Result<CopyPlan, BcmrError> {
    let sources = sources.to_vec();
    let dst = dst.to_path_buf();
    let excludes = excludes.to_vec();
    tokio::task::spawn_blocking(move || plan_copy_sync(sources, dst, recursive, no_deref, excludes))
        .await?
}

pub fn dry_run_plan(plan: &CopyPlan, cli: &Commands) -> std::result::Result<(), BcmrError> {
    for entry in &plan.entries {
        match entry {
            PlanEntry::CreateDir { src, dst } => {
                if !dst.exists() {
                    print_dry_run(
                        ActionType::Add,
                        &src.to_string_lossy(),
                        Some(&format!("(DIR) -> {}", dst.display())),
                    );
                }
            }
            PlanEntry::CopyFile { src, dst } => {
                let action = determine_dry_run_action(src, dst, cli)?;
                print_dry_run(action, &src.to_string_lossy(), Some(&dst.to_string_lossy()));
            }
            PlanEntry::Symlink {
                src,
                dst,
                target,
                kind,
            } => {
                let action = if symlink_entry_metadata(dst)?.is_some() {
                    super::symlinks::check_symlink_overwrite(dst, *kind, cli)?;
                    ActionType::Overwrite
                } else {
                    ActionType::Add
                };
                print_dry_run(
                    action,
                    &src.to_string_lossy(),
                    Some(&format!(
                        "(SYMLINK -> {}) -> {}",
                        target.display(),
                        dst.display()
                    )),
                );
            }
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod scan_tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn collect(src: &PathBuf, dst: &Path, recursive: bool, no_deref: bool) -> Vec<PlanEntry> {
        let mut out = Vec::new();
        scan_sources(
            std::slice::from_ref(src),
            dst,
            recursive,
            no_deref,
            &[],
            |entry, _| {
                out.push(entry);
                Ok(())
            },
        )
        .unwrap();
        out
    }

    fn write(p: &Path, body: &[u8]) {
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn scan_emits_symlink_entry_for_top_level_link() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("target.txt"), b"x");
        let link = dir.path().join("link.txt");
        symlink("target.txt", &link).unwrap();
        let dst = dir.path().join("dst");
        std::fs::create_dir(&dst).unwrap();

        let entries = collect(&link, &dst, false, true);
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            PlanEntry::Symlink {
                src,
                dst: d,
                target,
                kind,
            } => {
                assert_eq!(src, &link);
                assert_eq!(d, &dst.join("link.txt"));
                assert_eq!(target, std::path::Path::new("target.txt"));
                assert_eq!(*kind, SymlinkKind::File);
            }
            other => panic!("expected Symlink, got {other:?}"),
        }
    }

    #[test]
    fn scan_without_no_deref_emits_copyfile_for_link() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("target.txt"), b"x");
        let link = dir.path().join("link.txt");
        symlink("target.txt", &link).unwrap();
        let dst = dir.path().join("dst");
        std::fs::create_dir(&dst).unwrap();

        let entries = collect(&link, &dst, false, false);
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0], PlanEntry::CopyFile { .. }));
    }

    #[test]
    fn windows_symlink_attributes_preserve_scanned_kind() {
        assert_eq!(windows_symlink_kind_from_attributes(0), SymlinkKind::File);
        assert_eq!(
            windows_symlink_kind_from_attributes(0x10),
            SymlinkKind::Directory
        );
    }

    #[test]
    fn scan_recursive_emits_symlink_inside_tree() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("tree");
        std::fs::create_dir(&src).unwrap();
        write(&src.join("a.txt"), b"a");
        symlink("a.txt", src.join("rel.lnk")).unwrap();
        let dst = dir.path().join("dst");
        std::fs::create_dir(&dst).unwrap();

        let entries = collect(&src, &dst, true, true);
        let symlink_entries: Vec<_> = entries
            .iter()
            .filter(|e| matches!(e, PlanEntry::Symlink { .. }))
            .collect();
        assert_eq!(
            symlink_entries.len(),
            1,
            "expected exactly one Symlink entry"
        );
        let copy_entries: Vec<_> = entries
            .iter()
            .filter(|e| matches!(e, PlanEntry::CopyFile { .. }))
            .collect();
        assert_eq!(copy_entries.len(), 1);
    }

    #[test]
    fn scan_propagates_non_not_found_symlink_metadata_errors() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("regular-file");
        write(&blocker, b"x");
        let impossible = blocker.join("child");

        let error = scan_sources(
            std::slice::from_ref(&impossible),
            &dir.path().join("dst"),
            false,
            true,
            &[],
            |_, _| Ok(()),
        )
        .unwrap_err();
        match error {
            BcmrError::Io(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::NotADirectory);
            }
            other => panic!("expected propagated IO error, got {other:?}"),
        }
    }

    #[test]
    fn scan_recursive_rejects_dst_equal_to_src() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("over");
        std::fs::create_dir(&src).unwrap();
        write(&src.join("file.txt"), b"data");

        let err = scan_sources(
            std::slice::from_ref(&src),
            &src,
            true,
            false,
            &[],
            |_, _| Ok(()),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("into itself"), "got: {err}");
    }

    #[test]
    fn scan_recursive_rejects_dst_under_src() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("over");
        std::fs::create_dir(&src).unwrap();
        let dst = src.join("sub");

        let err = scan_sources(
            std::slice::from_ref(&src),
            &dst,
            true,
            false,
            &[],
            |_, _| Ok(()),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("into itself"), "got: {err}");
    }

    #[test]
    fn scan_recursive_allows_sibling_dst() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("over");
        std::fs::create_dir(&src).unwrap();
        write(&src.join("file.txt"), b"data");
        let dst = dir.path().join("elsewhere");
        std::fs::create_dir(&dst).unwrap();

        let entries = collect(&src, &dst, true, false);
        assert!(!entries.is_empty());
    }

    #[test]
    fn scan_emits_symlink_for_dangling_link() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("broken.lnk");
        symlink("nonexistent", &link).unwrap();
        let dst = dir.path().join("dst");
        std::fs::create_dir(&dst).unwrap();

        let entries = collect(&link, &dst, false, true);
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            PlanEntry::Symlink { target, .. } => {
                assert_eq!(target, std::path::Path::new("nonexistent"));
            }
            other => panic!("expected Symlink, got {other:?}"),
        }
    }
}
