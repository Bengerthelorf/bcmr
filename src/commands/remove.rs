use crate::cli::{Commands, TestMode};
use crate::ui::progress::CopyProgress;
use crate::ui::display::{print_dry_run, ActionType};
use crate::core::traversal;

use anyhow::{bail, Result};
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

// Helper for synchronous check logic
fn check_removes_sync(
    paths: Vec<PathBuf>,
    recursive: bool,
    dir_only: bool,
    force: bool,
    excludes: Vec<regex::Regex>,
) -> Result<Vec<FileToRemove>> {
    let mut files_to_remove = Vec::new();

    for path in paths {
        if traversal::is_excluded(&path, &excludes) {
            continue;
        }

        if path.is_file() {
            let metadata = path.metadata()?;
            files_to_remove.push(FileToRemove {
                path: path.to_path_buf(),
                is_dir: false,
                size: metadata.len(),
            });
        } else if path.is_dir() {
            if !recursive && !dir_only {
                bail!(
                    "Cannot remove '{}': Is a directory (use -r for recursive removal)",
                    path.display()
                );
            }

            // For directories, check if they're empty when -d is used
            if dir_only {
                // Use read_dir (blocking syscall, acceptable in spawn_blocking)
                let mut read_dir = std::fs::read_dir(&path)?;
                if read_dir.next().is_some() {
                    bail!("Cannot remove '{}': Directory not empty", path.display());
                }
                files_to_remove.push(FileToRemove {
                    path: path.to_path_buf(),
                    is_dir: true,
                    size: 0,
                });
                continue;
            }

            // For recursive removal
            if recursive {
                for entry in traversal::walk(&path, true, true, 0, &excludes) {
                    let entry = entry?;
                    let path = entry.path();
                    // Exclude check handled by traversal::walk

                    let metadata = entry.metadata()?;
                    files_to_remove.push(FileToRemove {
                        path: path.to_path_buf(),
                        is_dir: entry.file_type().is_dir(),
                        size: if entry.file_type().is_file() {
                            metadata.len()
                        } else {
                            0
                        },
                    });
                }
            }
        } else {
            if !force {
                bail!(
                    "Cannot remove '{}': No such file or directory",
                    path.display()
                );
            }
        }
    }

    Ok(files_to_remove)
}

pub async fn check_removes(
    paths: &[PathBuf],
    recursive: bool,
    cli: &Commands,
    excludes: &[regex::Regex],
) -> Result<Vec<FileToRemove>> {
    let paths = paths.to_vec();
    let recursive = recursive;
    let dir_only = cli.is_dir_only();
    let force = cli.is_force();
    let excludes = excludes.to_vec();

    tokio::task::spawn_blocking(move || {
        check_removes_sync(paths, recursive, dir_only, force, excludes)
    })
    .await?
}

fn get_total_size_sync(
    paths: Vec<PathBuf>,
    recursive: bool,
    excludes: Vec<regex::Regex>,
) -> Result<u64> {
    let mut total_size = 0;

    for path in paths {
        if traversal::is_excluded(&path, &excludes) {
            continue;
        }

        if path.is_file() {
            total_size += path.metadata()?.len();
        } else if recursive && path.is_dir() {
            for entry in traversal::walk(&path, true, false, 1, &excludes) {
                let entry = entry?;
                let p = entry.path();
                if p.is_file() {
                    total_size += entry.metadata()?.len();
                }
            }
        }
    }

    Ok(total_size)
}

#[allow(dead_code)]
pub async fn get_total_size(
    paths: &[PathBuf],
    recursive: bool,
    _cli: &Commands,
    excludes: &[regex::Regex],
) -> Result<u64> {
    let paths = paths.to_vec();
    let recursive = recursive;
    let excludes = excludes.to_vec();

    tokio::task::spawn_blocking(move || get_total_size_sync(paths, recursive, excludes)).await?
}

async fn confirm_remove(path: &Path, is_dir: bool) -> Result<bool> {
    use crossterm::{
        cursor::{Hide, Show},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode},
    };
    use std::io::{self, Write};

    // Temporarily restore terminal to normal mode for input
    let mut stdout = io::stdout();
    disable_raw_mode()?;
    execute!(stdout, Show)?; // Show cursor

    print!(
        "Remove {} '{}'? (y/N) ",
        if is_dir { "directory" } else { "file" },
        path.display()
    );
    stdout.flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    // Restore raw mode and hide cursor for progress display
    enable_raw_mode()?;
    execute!(stdout, Hide)?;

    Ok(input.trim().to_lowercase() == "y" || input.trim().to_lowercase() == "yes")
}

pub async fn remove_path(
    path: &Path,
    is_dir: bool,
    test_mode: TestMode,
    cli: &Commands,
    excludes: &[regex::Regex],
    progress_state: Arc<Mutex<ProgressState>>,
    progress_callback: impl Fn(u64) + Send + Sync,
    on_new_file: impl Fn(&str, u64) + Send + Sync,
) -> Result<()> {
    if traversal::is_excluded(path, excludes) {
        return Ok(());
    }

    // Handle interactive mode
    if cli.is_interactive() && !cli.is_force() {
        if !confirm_remove(path, is_dir).await? {
            return Ok(());
        }
    }

    let file_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    if path.is_dir() && (cli.is_recursive() || cli.is_dir_only()) {
        // REMOVED: Duplicate printing of "Would remove directory"
        
        on_new_file(&file_name, 0);

        // First, collect all entries
        // Use unified traversal with contents_first=true
        let mut entries = Vec::new();
        // min_depth=0 to include the dir itself? Original code used WalkDir::new(path).
        // WalkDir includes root. So min_depth=0.
        for entry in traversal::walk(path, true, true, 0, excludes) {
            entries.push(entry?);
        }

        // Sort in reverse order to handle deepest paths first
        // Wait, contents_first=true already does post-order traversal (children first).
        // Original code:
        // .contents_first(true)
        // .collect()
        // entries.sort_by_key(|entry| Reverse(entry.depth()))
        // WalkDir contents_first guarantees children before parents. The sort seems redundant if WalkDir works as expected,
        // BUT WalkDir yields siblings in some order. Sorting by depth ensures strict depth order?
        // Actually, contents_first is usually enough for deletion.
        // But original code had explicit sort. I should keep it to be safe, or rely on WalkDir order.
        // WalkDir with contents_first=true yields leaves first.
        // Let's keep the sort logic if it was there to be 100% sure about depth.
        entries.sort_by(|a, b| {
            b.path()
                .components()
                .count()
                .cmp(&a.path().components().count())
        });

        // Process all entries
        for entry in entries {
            let entry_path = entry.path();

            // Exclude check handled by traversal::walk!

            let size = if entry.file_type().is_file() {
                let metadata = entry.metadata()?;
                metadata.len()
            } else {
                0
            };

            // Interactive confirmation for each entry if needed
            if cli.is_interactive() && !cli.is_force() {
                // Don't ask again for the root if we already asked above?
                // The original code asked for 'path' at the beginning.
                // Then inside the loop, it iterated ALL entries.
                // The WalkDir includes 'path'.
                // So it would ask TWICE for the root dir?
                // Original code:
                // 1. confirm_remove(path, is_dir) [lines 207-209]
                // 2. Loop entries -> confirm_remove(entry_path, ...) [lines 255-257]
                // If entry_path == path, it asks again!
                // This seems like a bug or feature of original code. I will preserve it or fix it?
                // If I keep it, I preserve behavior.
                if !confirm_remove(entry_path, entry.file_type().is_dir()).await? {
                    continue;
                }
            }

            let entry_name = entry_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            on_new_file(&entry_name, size);

            // Handle test mode for files
            if entry.file_type().is_file() {
                match test_mode {
                    TestMode::Delay(ms) => {
                        progress_callback(size);
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                    }
                    TestMode::SpeedLimit(bps) => {
                        let chunks = size / bps + 1;
                        for _ in 0..chunks {
                            progress_callback(bps.min(size));
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                    TestMode::None => {
                        progress_callback(size);
                    }
                }
            }

            // Remove the entry
            if !cli.is_dry_run() {
                if entry.file_type().is_file() {
                    fs::remove_file(entry_path).await?;
                } else if entry.file_type().is_dir() {
                    fs::remove_dir(entry_path).await?;
                }
            } else {
                // Use new display logic
                print_dry_run(
                    ActionType::Remove, 
                    &entry_path.to_string_lossy(),
                    None
                );
            }

            // Update progress only for actual entries (not the root directory)
            if entry_path != path {
                progress_state.lock().inc_processed();
            }

            if cli.is_verbose() && !cli.is_dry_run() {
                println!("removed {}", entry_path.display());
            }
        }
    } else if path.is_file() {
        if cli.is_dry_run() {
             print_dry_run(
                ActionType::Remove, 
                &path.to_string_lossy(), 
                None
            );
            return Ok(());
        }

        let size = path.metadata()?.len();
        on_new_file(&file_name, size);

        // Simulate progress for test mode
        match test_mode {
            TestMode::Delay(ms) => {
                if size > 0 {
                    progress_callback(size);
                }
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
            TestMode::SpeedLimit(bps) => {
                if size > 0 {
                    let chunks = size / bps + 1;
                    for _ in 0..chunks {
                        progress_callback(bps.min(size));
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
            TestMode::None => {
                if size > 0 {
                    progress_callback(size);
                }
            }
        }

        // Remove the file
        fs::remove_file(path).await?;
        progress_state.lock().inc_processed();

        if cli.is_verbose() {
            println!("removed {}", path.display());
        }
    } else {
        bail!(
            "Cannot remove '{}': No such file or directory",
            path.display()
        );
    }

    Ok(())
}

pub struct ProgressState {
    processed_items: usize,
    progress: Arc<Mutex<CopyProgress>>,
}

impl ProgressState {
    pub fn new(total_items: usize, progress: Arc<Mutex<CopyProgress>>) -> Self {
        progress.lock().set_total_items(total_items);
        Self {
            processed_items: 0,
            progress,
        }
    }

    pub fn inc_processed(&mut self) {
        self.processed_items += 1;
        self.progress.lock().inc_items_processed();
    }
}

pub async fn remove_paths(
    paths: &[PathBuf],
    test_mode: TestMode,
    cli: &Commands,
    excludes: &[regex::Regex],
    progress: Arc<Mutex<CopyProgress>>,
    progress_callback: impl Fn(u64) + Send + Sync + Clone + 'static,
    on_new_file: Box<dyn Fn(&str, u64) + Send + Sync>,
) -> Result<()> {
    // First, calculate total number of items to process
    let files_to_remove = check_removes(paths, cli.is_recursive(), cli, excludes).await?;

    // Set up progress state
    let progress_state = Arc::new(Mutex::new(ProgressState::new(
        files_to_remove.len(),
        Arc::clone(&progress),
    )));

    // Process each path
    for path in paths {
        remove_path(
            path,
            path.is_dir(),
            test_mode.clone(),
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
