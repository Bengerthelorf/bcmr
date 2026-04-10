use crate::cli::{Commands, TestMode};
use crate::core::error::BcmrError;
use crate::core::traversal;
use crate::ui::display::{print_dry_run, ActionType};
use crate::ui::progress::ProgressRenderer;

use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;

pub struct FileToRemove {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
}

fn check_removes_sync(
    paths: Vec<PathBuf>,
    recursive: bool,
    dir_only: bool,
    force: bool,
    excludes: Vec<regex::Regex>,
) -> std::result::Result<Vec<FileToRemove>, BcmrError> {
    let mut files_to_remove = Vec::new();

    for path in paths {
        if traversal::is_excluded(&path, &excludes) {
            continue;
        }

        // symlink_metadata() doesn't follow symlinks, so dangling symlinks
        // report Ok and broken-target files aren't accidentally hidden as
        // "missing".
        let md = match path.symlink_metadata() {
            Ok(m) => m,
            Err(_) if force => continue,
            Err(_) => return Err(BcmrError::SourceNotFound(path.to_path_buf())),
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
                for entry in traversal::walk(&path, true, true, 0, &excludes) {
                    let entry = entry?;
                    let entry_path = entry.path();
                    let ft = entry.file_type();

                    // Only regular files contribute bytes to the total.
                    // Symlinks, fifos, sockets etc. all count as items to
                    // remove but have size 0.
                    let size = if ft.is_file() {
                        entry.metadata().map(|m| m.len()).unwrap_or(0)
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
            // Regular file, symlink (valid or dangling), or any other
            // non-directory entry. All of these are unlinked the same way.
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
    use std::io::{self, Write};

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
        TestMode::None => {
            callback(size);
        }
    }
}

pub async fn remove_path(
    path: &Path,
    is_dir: bool,
    cli: &Commands,
    excludes: &[regex::Regex],
    progress_state: Arc<Mutex<ProgressState>>,
    progress_callback: impl Fn(u64) + Send + Sync,
    on_new_file: impl Fn(&str, u64) + Send + Sync,
) -> std::result::Result<(), BcmrError> {
    let test_mode = cli.get_test_mode();
    if traversal::is_excluded(path, excludes) {
        return Ok(());
    }

    let is_tui = cli.is_tui_mode();
    if cli.is_interactive() && !cli.is_force() && !confirm_remove(path, is_dir, is_tui).await? {
        return Ok(());
    }

    let file_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // symlink_metadata so we see the link itself, not what it points at.
    let md = match path.symlink_metadata() {
        Ok(m) => m,
        Err(_) if cli.is_force() => return Ok(()),
        Err(e) => return Err(e.into()),
    };

    if md.is_dir() && (cli.is_recursive() || cli.is_dir_only()) {
        on_new_file(&file_name, 0);

        for entry in traversal::walk(path, true, true, 0, excludes) {
            let entry = entry?;
            let entry_path = entry.path();
            let ft = entry.file_type();

            let size = if ft.is_file() {
                entry.metadata()?.len()
            } else {
                0
            };

            if cli.is_interactive()
                && !cli.is_force()
                && !confirm_remove(entry_path, ft.is_dir(), is_tui).await?
            {
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
                    fs::remove_dir(entry_path).await?;
                } else {
                    // Files, symlinks (valid or dangling), fifos, sockets:
                    // unlink() handles all non-directories uniformly.
                    fs::remove_file(entry_path).await?;
                }
            } else {
                print_dry_run(ActionType::Remove, &entry_path.to_string_lossy(), None);
            }

            if ft.is_file() {
                report_progress(size, &test_mode, &progress_callback).await;
            }

            // Skip root item itself
            if entry_path != path {
                progress_state.lock().inc_processed();
            }

            if cli.is_verbose() && !cli.is_dry_run() {
                println!("removed {}", entry_path.display());
            }
        }
    } else if md.is_dir() {
        // Non-recursive directory: the earlier check in check_removes_sync
        // would have rejected this for normal remove; dir_only / rmdir is
        // handled by the caller's dry-run branch via remove_paths.
        return Err(BcmrError::InvalidInput(format!(
            "Cannot remove '{}': Is a directory (use -r for recursive removal)",
            path.display()
        )));
    } else {
        // Regular file or symlink (any kind).
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
        progress_state.lock().inc_processed();

        if cli.is_verbose() {
            println!("removed {}", path.display());
        }
    }

    Ok(())
}

pub struct ProgressState {
    progress: Arc<Mutex<Box<dyn ProgressRenderer>>>,
}

impl ProgressState {
    pub fn new(total_items: usize, progress: Arc<Mutex<Box<dyn ProgressRenderer>>>) -> Self {
        progress.lock().set_total_items(total_items);
        Self { progress }
    }

    pub fn inc_processed(&mut self) {
        self.progress.lock().inc_items_processed();
    }
}

type FileCallback = Box<dyn Fn(&str, u64) + Send + Sync>;

pub async fn remove_paths(
    paths: &[PathBuf],
    cli: &Commands,
    excludes: &[regex::Regex],
    progress: Arc<Mutex<Box<dyn ProgressRenderer>>>,
    progress_callback: impl Fn(u64) + Send + Sync + Clone + 'static,
    on_new_file: FileCallback,
    total_items: usize,
) -> std::result::Result<(), BcmrError> {
    let progress_state = Arc::new(Mutex::new(ProgressState::new(
        total_items,
        Arc::clone(&progress),
    )));

    for path in paths {
        remove_path(
            path,
            path.is_dir(),
            cli,
            excludes,
            Arc::clone(&progress_state),
            progress_callback.clone(),
            &*on_new_file,
        )
        .await?;
    }

    Ok(())
}
