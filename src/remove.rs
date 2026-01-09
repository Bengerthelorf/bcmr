use crate::cli::{Commands, TestMode};
use crate::progress::CopyProgress;
use anyhow::{bail, Result};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;
use walkdir::WalkDir;

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
        let path_str = path.to_string_lossy();
        if excludes.iter().any(|re| re.is_match(&path_str)) {
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
                for entry in WalkDir::new(path).contents_first(true) {
                    let entry = entry?;
                    let path = entry.path();
                    let path_str = path.to_string_lossy();

                    if excludes.iter().any(|re| re.is_match(&path_str)) {
                        continue;
                    }

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
        let path_str = path.to_string_lossy();
        if excludes.iter().any(|re| re.is_match(&path_str)) {
            continue;
        }

        if path.is_file() {
            total_size += path.metadata()?.len();
        } else if recursive && path.is_dir() {
            for entry in WalkDir::new(path).min_depth(1) {
                let entry = entry?;
                let p = entry.path();
                let p_str = p.to_string_lossy();
                if p.is_file() && !excludes.iter().any(|re| re.is_match(&p_str)) {
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
    if cli.should_exclude(&path.to_string_lossy(), excludes) {
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
        if cli.is_dry_run() {
            println!("Would remove directory '{}' and contents", path.display());
        }

        on_new_file(&file_name, 0);

        // First, collect all entries
        let mut entries: Vec<_> = WalkDir::new(path)
            .contents_first(true) // This ensures we process contents before containing directory
            .into_iter()
            .collect::<std::result::Result<_, _>>()?;

        // Sort in reverse order to handle deepest paths first
        entries.sort_by(|a, b| {
            b.path()
                .components()
                .count()
                .cmp(&a.path().components().count())
        });

        // Process all entries
        for entry in entries {
            let entry_path = entry.path();

            if cli.should_exclude(&entry_path.to_string_lossy(), excludes) {
                continue;
            }

            let size = if entry.file_type().is_file() {
                let metadata = entry.metadata()?;
                metadata.len()
            } else {
                0
            };

            // Interactive confirmation for each entry if needed
            if cli.is_interactive() && !cli.is_force() {
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
                println!("Would remove '{}'", entry_path.display());
            }

            // Update progress only for actual entries (not the root directory)
            if entry_path != path {
                progress_state.lock().inc_processed();
            }

            if cli.is_verbose() {
                println!("removed {}", entry_path.display());
            }
        }
    } else if path.is_file() {
        if cli.is_dry_run() {
            println!("Would remove file '{}'", path.display());
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
