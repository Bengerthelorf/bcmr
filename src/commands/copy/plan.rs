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
    },
}

pub struct CopyPlan {
    pub entries: Vec<PlanEntry>,
    pub total_size: u64,
    pub overwrites: Vec<FileToOverwrite>,
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

    let is_symlink = |p: &Path| -> bool {
        p.symlink_metadata()
            .map(|m| m.is_symlink())
            .unwrap_or(false)
    };

    for src in sources {
        if traversal::is_excluded(src, excludes) {
            continue;
        }

        if no_deref && is_symlink(src) {
            let target = std::fs::read_link(src).map_err(BcmrError::Io)?;
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

                if no_deref && is_symlink(path) {
                    let link_target = std::fs::read_link(path).map_err(BcmrError::Io)?;
                    on_entry(
                        PlanEntry::Symlink {
                            src: path.to_path_buf(),
                            dst: target,
                            target: link_target,
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
                PlanEntry::CopyFile { dst, .. } | PlanEntry::Symlink { dst, .. } => {
                    Some((dst.clone(), false))
                }
                PlanEntry::CreateDir { dst, .. } if dst.exists() => Some((dst.clone(), true)),
                PlanEntry::CreateDir { .. } => None,
            };
            if let Some((path, is_dir)) = target {
                if path.exists() && !traversal::is_excluded(&path, &excludes) {
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
            PlanEntry::Symlink { src, dst, target } => {
                let action = if dst.symlink_metadata().is_ok() {
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
            } => {
                assert_eq!(src, &link);
                assert_eq!(d, &dst.join("link.txt"));
                assert_eq!(target, std::path::Path::new("target.txt"));
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
