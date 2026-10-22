use crate::cli::{Commands, TestMode};
use crate::core::error::BcmrError;
use crate::core::traversal;
use crate::ui::display::{print_dry_run, ActionType};
use crate::ui::runner::ProgressHandle;

use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;

pub struct FileToRemove {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
}

fn validate_removal_path(path: &Path) -> Result<(), BcmrError> {
    // Path::components normalizes terminal '.', so inspect the spelling before
    // any traversal. Removing one of these operands could empty its parent.
    let spelling = path.as_os_str().as_encoded_bytes();
    let last = spelling
        .rsplit(|byte| *byte == b'/' || cfg!(windows) && *byte == b'\\')
        .find(|component| !component.is_empty())
        .unwrap_or_default();
    #[cfg(windows)]
    let last = if last.len() >= 2 && last[1] == b':' {
        &last[2..]
    } else {
        last
    };
    if last == b"." || last == b".." {
        return Err(BcmrError::InvalidInput(format!(
            "Cannot remove '{}': refusing a path ending in '.' or '..'; name the directory itself",
            path.display()
        )));
    }
    #[cfg(windows)]
    if matches!(
        path.components().next(),
        Some(std::path::Component::Prefix(_))
    ) && path.components().count() == 1
    {
        return Err(BcmrError::InvalidInput(format!(
            "Cannot remove '{}': refusing an unnamed drive directory; name the directory itself",
            path.display()
        )));
    }
    if path.has_root()
        && path.components().all(|component| {
            matches!(
                component,
                std::path::Component::RootDir | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(BcmrError::InvalidInput(format!(
            "Cannot remove '{}': refusing to remove a filesystem root",
            path.display()
        )));
    }
    Ok(())
}

fn removal_entry_path(path: &Path) -> PathBuf {
    if !crate::core::destination::has_trailing_separator(path) {
        return path.to_path_buf();
    }
    let Some(name) = path.file_name() else {
        return path.to_path_buf();
    };
    let entry = path.parent().unwrap_or_else(|| Path::new("")).join(name);
    // lstat("link/") follows a directory link, and can even make a dangling
    // link look missing. Unlink the selected link, never traverse its referent.
    if entry
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        entry
    } else {
        path.to_path_buf()
    }
}

fn removal_entries(
    path: &Path,
    excludes: &[regex::Regex],
) -> Result<Vec<walkdir::DirEntry>, BcmrError> {
    // filter_entry can prune directories only before their children are read.
    // Capture the selected top-down inventory, then delete children first.
    let mut entries =
        traversal::walk(path, true, false, 0, excludes).collect::<Result<Vec<_>, _>>()?;
    entries.reverse();
    Ok(entries)
}

fn check_removes_sync(
    paths: Vec<PathBuf>,
    recursive: bool,
    dir_only: bool,
    force: bool,
    excludes: Vec<regex::Regex>,
) -> std::result::Result<Vec<FileToRemove>, BcmrError> {
    let mut files_to_remove = Vec::new();

    for path in &paths {
        if !traversal::is_excluded(path, &excludes) {
            validate_removal_path(path)?;
        }
    }

    for path in paths {
        if traversal::is_excluded(&path, &excludes) {
            continue;
        }
        let path = removal_entry_path(&path);

        let md = match path.symlink_metadata() {
            Ok(m) => m,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && force => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(BcmrError::SourceNotFound(path.to_path_buf()));
            }
            Err(error) => return Err(error.into()),
        };

        if md.is_dir() {
            if !recursive && !dir_only {
                return Err(BcmrError::InvalidInput(format!(
                    "Cannot remove '{}': Is a directory (use -r for recursive removal)",
                    path.display()
                )));
            }

            if dir_only {
                let mut read_dir = std::fs::read_dir(&path)?;
                if read_dir.next().is_some() {
                    return Err(BcmrError::InvalidInput(format!(
                        "Cannot remove '{}': Directory not empty",
                        path.display()
                    )));
                }
                files_to_remove.push(FileToRemove {
                    path: path.to_path_buf(),
                    is_dir: true,
                    size: 0,
                });
                continue;
            }

            if recursive {
                for entry in removal_entries(&path, &excludes)? {
                    let entry_path = entry.path();
                    let ft = entry.file_type();

                    let size = if ft.is_file() {
                        entry.metadata()?.len()
                    } else {
                        0
                    };
                    files_to_remove.push(FileToRemove {
                        path: entry_path.to_path_buf(),
                        is_dir: ft.is_dir(),
                        size,
                    });
                }
            }
        } else {
            files_to_remove.push(FileToRemove {
                path: path.to_path_buf(),
                is_dir: false,
                size: if md.is_file() { md.len() } else { 0 },
            });
        }
    }

    Ok(files_to_remove)
}

pub async fn check_removes(
    paths: &[PathBuf],
    recursive: bool,
    cli: &Commands,
    excludes: &[regex::Regex],
) -> std::result::Result<Vec<FileToRemove>, BcmrError> {
    let paths = paths.to_vec();
    let dir_only = cli.is_dir_only();
    let force = cli.is_force();
    let excludes = excludes.to_vec();

    tokio::task::spawn_blocking(move || {
        check_removes_sync(paths, recursive, dir_only, force, excludes)
    })
    .await?
}

async fn confirm_remove(
    path: &Path,
    is_dir: bool,
    restore_raw: bool,
) -> std::result::Result<bool, BcmrError> {
    use crossterm::{
        cursor::{Hide, Show},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode},
    };
    use std::io::{self, IsTerminal, Write};

    if !io::stdin().is_terminal() {
        return Err(BcmrError::InvalidInput(
            "-i requires a terminal for prompts; stdin is not a TTY (use -y or -f instead)".into(),
        ));
    }

    let mut stdout = io::stdout();
    if restore_raw {
        let _ = disable_raw_mode();
        let _ = execute!(stdout, Show);
    }

    print!(
        "Remove {} '{}'? (y/N) ",
        if is_dir { "directory" } else { "file" },
        path.display()
    );
    stdout.flush()?;

    let mut input = String::new();
    let result = io::stdin().read_line(&mut input);

    if restore_raw {
        let _ = enable_raw_mode();
        let _ = execute!(stdout, Hide);
    }

    result?;
    Ok(input.trim().eq_ignore_ascii_case("y") || input.trim().eq_ignore_ascii_case("yes"))
}

async fn report_progress(size: u64, test_mode: &TestMode, callback: &(impl Fn(u64) + Send + Sync)) {
    if size == 0 {
        return;
    }
    match test_mode {
        TestMode::Delay(ms) => {
            callback(size);
            tokio::time::sleep(Duration::from_millis(*ms)).await;
        }
        TestMode::SpeedLimit(bps) => {
            let mut remaining = size;
            while remaining > 0 {
                let chunk = remaining.min(*bps);
                callback(chunk);
                remaining -= chunk;
                if remaining > 0 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
        TestMode::None | TestMode::CorruptBeforeFinalize => {
            callback(size);
        }
        #[cfg(feature = "test-support")]
        TestMode::TruncateSourceAfterSnapshot
        | TestMode::TruncateSourceAfterSnapshotDelay
        | TestMode::TruncateSourceAfterSnapshotSpeedLimit
        | TestMode::TruncateSourceAfterStageWrite
        | TestMode::CreateDestinationBeforeFinalize
        | TestMode::ReplaceDestinationBeforeFinalize
        | TestMode::ReplaceDestinationAfterResumeResolution
        | TestMode::ReplaceDestinationWithFifoAfterObservation
        | TestMode::CreateDestinationHardlinkBeforeFinalize
        | TestMode::FailSymlinkCreate
        | TestMode::FailSymlinkCommit
        | TestMode::CreateDestinationBeforeSymlinkCommit => {
            callback(size);
        }
    }
}

pub async fn remove_path(
    path: &Path,
    _is_dir: bool,
    cli: &Commands,
    excludes: &[regex::Regex],
    progress: ProgressHandle,
    progress_callback: impl Fn(u64) + Send + Sync,
    on_new_file: impl Fn(&str, u64) + Send + Sync,
) -> std::result::Result<(), BcmrError> {
    let test_mode = cli.get_test_mode();
    if traversal::is_excluded(path, excludes) {
        return Ok(());
    }
    validate_removal_path(path)?;
    let selected = removal_entry_path(path);
    let path = selected.as_path();
    let interactive = cli.is_interactive() && !cli.is_force() && !cli.is_yes() && !cli.is_dry_run();
    let verbose = cli.is_verbose() && !cli.is_quiet() && !crate::config::is_json_mode();

    let uses_tui = crate::ui::progress::renderer_kind(
        crate::commands::remote_copy::progress_mode(cli),
        cli.is_quiet() || interactive,
        crate::config::is_json_mode(),
    ) == crate::ui::progress::RendererKind::Tui;
    let file_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let md = match path.symlink_metadata() {
        Ok(m) => m,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && cli.is_force() => {
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    if md.is_dir() && cli.is_dir_only() {
        if interactive && !confirm_remove(path, true, uses_tui).await? {
            return Ok(());
        }
        on_new_file(&file_name, 0);
        if cli.is_dry_run() {
            print_dry_run(ActionType::Remove, &path.to_string_lossy(), None);
        } else {
            // A directory can gain children after the empty-directory check or
            // while the user is confirming. -d never authorizes traversing it.
            fs::remove_dir(path).await?;
        }
        progress.inc_items_processed();
        if verbose && !cli.is_dry_run() {
            println!("removed {}", path.display());
        }
    } else if md.is_dir() && cli.is_recursive() {
        on_new_file(&file_name, 0);

        for entry in removal_entries(path, excludes)? {
            let entry_path = entry.path();
            let ft = entry.file_type();

            let size = if ft.is_file() {
                entry.metadata()?.len()
            } else {
                0
            };

            if interactive && !confirm_remove(entry_path, ft.is_dir(), uses_tui).await? {
                continue;
            }

            let entry_name = entry_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            on_new_file(&entry_name, size);

            if !cli.is_dry_run() {
                if ft.is_dir() {
                    if let Err(e) = fs::remove_dir(entry_path).await {
                        // Exclusions and interactive skips intentionally leave
                        // children behind; keeping the parent is the expected
                        // outcome, not an error.
                        if e.kind() != std::io::ErrorKind::DirectoryNotEmpty {
                            return Err(e.into());
                        }
                        progress.inc_items_processed();
                        continue;
                    }
                } else {
                    fs::remove_file(entry_path).await?;
                }
            } else {
                print_dry_run(ActionType::Remove, &entry_path.to_string_lossy(), None);
            }

            if ft.is_file() {
                report_progress(size, &test_mode, &progress_callback).await;
            }

            progress.inc_items_processed();

            if verbose && !cli.is_dry_run() {
                println!("removed {}", entry_path.display());
            }
        }
    } else if md.is_dir() {
        return Err(BcmrError::InvalidInput(format!(
            "Cannot remove '{}': Is a directory (use -r for recursive removal)",
            path.display()
        )));
    } else {
        if interactive && !confirm_remove(path, false, uses_tui).await? {
            return Ok(());
        }
        if cli.is_dry_run() {
            print_dry_run(ActionType::Remove, &path.to_string_lossy(), None);
            return Ok(());
        }

        let size = if md.is_file() { md.len() } else { 0 };
        on_new_file(&file_name, size);

        if size > 0 {
            report_progress(size, &test_mode, &progress_callback).await;
        }

        fs::remove_file(path).await?;
        progress.inc_items_processed();

        if verbose {
            println!("removed {}", path.display());
        }
    }

    Ok(())
}

type FileCallback = Box<dyn Fn(&str, u64) + Send + Sync>;

pub async fn remove_paths(
    paths: &[PathBuf],
    cli: &Commands,
    excludes: &[regex::Regex],
    progress: ProgressHandle,
    progress_callback: impl Fn(u64) + Send + Sync + Clone + 'static,
    on_new_file: FileCallback,
    total_items: usize,
) -> std::result::Result<(), BcmrError> {
    progress.set_total_items(total_items);

    for path in paths {
        remove_path(
            path,
            path.is_dir(),
            cli,
            excludes,
            progress.clone(),
            progress_callback.clone(),
            &*on_new_file,
        )
        .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_removal_path;
    use std::path::Path;

    #[test]
    fn removal_validation_refuses_root_and_dot_operands_without_touching_files() {
        for path in ["/", "////", ".", "..", "tree/.", "tree/.///", "tree/../"] {
            assert!(validate_removal_path(Path::new(path)).is_err(), "{path}");
        }
        #[cfg(windows)]
        for path in [
            "C:",
            "C:\\",
            "C:.",
            "C:..",
            "tree\\.\\",
            "\\\\server\\share\\",
        ] {
            assert!(validate_removal_path(Path::new(path)).is_err(), "{path}");
        }
    }

    #[test]
    fn removal_validation_allows_named_relative_and_absolute_entries() {
        for path in [
            "../named",
            "./named/",
            "tree/../named",
            "/tmp/named/",
            ".hidden",
        ] {
            assert!(validate_removal_path(Path::new(path)).is_ok(), "{path}");
        }
        #[cfg(not(windows))]
        assert!(validate_removal_path(Path::new("C:")).is_ok());
    }
}
