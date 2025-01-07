mod cli;
mod copy;
mod r#move;  // Using raw identifier as 'move' is a keyword
mod remove;  // New module for remove command
mod progress;

use anyhow::Result;
use cli::Commands;
use parking_lot::Mutex;
use progress::CopyProgress;
use std::io::{self, Write};
use std::sync::Arc;
use tokio::signal::ctrl_c;
use tokio::time::Duration;

async fn confirm_overwrite(files: &[copy::FileToOverwrite]) -> Result<bool> {
    println!("\nThe following items will be overwritten:");
    for file in files {
        println!(
            "  {} {}",
            if file.is_dir { "DIR:" } else { "FILE:" },
            file.path.display()
        );
    }

    print!("\nDo you want to proceed? [y/N] ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    Ok(input.trim().to_lowercase() == "y")
}

async fn confirm_removal(files: &[remove::FileToRemove]) -> Result<bool> {
    // Calculate total size and item counts
    let mut total_size = 0u64;
    let mut file_count = 0;
    let mut dir_count = 0;

    for file in files {
        if file.is_dir {
            dir_count += 1;
        } else {
            file_count += 1;
            total_size += file.size;
        }
    }

    println!("\nThe following items will be removed:");
    println!("  Files: {}", file_count);
    println!("  Directories: {}", dir_count);
    if total_size > 0 {
        println!("  Total size: {:.2} MiB", total_size as f64 / 1024.0 / 1024.0);
    }
    
    for file in files {
        println!(
            "  {} {}{}",
            if file.is_dir { "DIR:" } else { "FILE:" },
            file.path.display(),
            if !file.is_dir && file.size > 0 {
                format!(" ({:.2} MiB)", file.size as f64 / 1024.0 / 1024.0)
            } else {
                String::new()
            }
        );
    }

    print!("\nDo you want to proceed? [y/N] ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    Ok(input.trim().to_lowercase() == "y")
}

async fn handle_copy_command(args: &Commands) -> Result<()> {
    let test_mode = args.get_test_mode();

    // If force is specified, check the files to be overwritten
    if args.is_force() {
        let files_to_overwrite = copy::check_overwrites(
            args.get_source(),
            args.get_destination(),
            args.is_recursive(),
            args,
        )
        .await?;

        // If there are files to overwrite and confirmation is needed
        if !files_to_overwrite.is_empty() && args.should_prompt_for_overwrite() {
            if !confirm_overwrite(&files_to_overwrite).await? {
                println!("Operation cancelled.");
                return Ok(());
            }
        }
    }

    // Calculate total size
    let total_size = copy::get_total_size(args.get_source(), args.is_recursive(), args).await?;
    let progress = Arc::new(Mutex::new(CopyProgress::new(total_size)?));

    // Set initial file/directory name
    let display_name = args
        .get_source()
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    progress.lock().set_current_file(&display_name, total_size);

    // Create clones for callbacks
    let progress_for_inc = Arc::clone(&progress);
    let progress_for_file = Arc::clone(&progress);

    // Modify signal handling logic
    let progress_for_signal = Arc::clone(&progress);
    tokio::spawn(async move {
        if let Ok(()) = ctrl_c().await {
            let _ = progress_for_signal.lock().finish();
            std::process::exit(0);
        }
    });

    // Start the copy operation with exclude patterns
    let result = copy::copy_path(
        args.get_source(),
        args.get_destination(),
        args.is_recursive(),
        args.is_preserve(),
        test_mode,
        args,
        move |n| progress_for_inc.lock().inc_current(n),
        move |name, size| progress_for_file.lock().set_current_file(name, size),
    )
    .await;

    // Ensure proper cleanup upon completion or error
    let mut progress = progress.lock();
    if let Err(e) = result {
        progress.finish()?;
        return Err(e);
    }
    progress.finish()?;

    tokio::time::sleep(Duration::from_secs(1)).await;
    Ok(())
}

async fn handle_move_command(args: &Commands) -> Result<()> {
    let test_mode = args.get_test_mode();

    if args.is_force() {
        let files_to_overwrite = r#move::check_overwrites(
            args.get_source(),
            args.get_destination(),
            args.is_recursive(),
            args,
        )
        .await?;

        if !files_to_overwrite.is_empty() && args.should_prompt_for_overwrite() {
            if !confirm_overwrite(&files_to_overwrite).await? {
                println!("Operation cancelled.");
                return Ok(());
            }
        }
    }

    let total_size = r#move::get_total_size(args.get_source(), args.is_recursive(), args).await?;
    let progress = Arc::new(Mutex::new(CopyProgress::new(total_size)?));

    let display_name = args
        .get_source()
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    {
        let mut progress_guard = progress.lock();
        progress_guard.set_current_file(&display_name, total_size);
        progress_guard.set_operation_type("Moving");
    }

    let progress_for_inc = Arc::clone(&progress);
    let progress_for_file = Arc::clone(&progress);
    let progress_for_signal = Arc::clone(&progress);

    tokio::spawn(async move {
        if let Ok(()) = ctrl_c().await {
            let _ = progress_for_signal.lock().finish();
            std::process::exit(0);
        }
    });

    let result = r#move::move_path(
        args.get_source(),
        args.get_destination(),
        args.is_recursive(),
        args.is_preserve(),
        test_mode,
        args,
        move |n| progress_for_inc.lock().inc_current(n),
        move |name, size| progress_for_file.lock().set_current_file(name, size),
    )
    .await;

    let mut progress = progress.lock();
    if let Err(e) = result {
        progress.finish()?;
        return Err(e);
    }
    progress.finish()?;

    tokio::time::sleep(Duration::from_secs(1)).await;
    Ok(())
}

async fn handle_remove_command(args: &Commands) -> Result<()> {
    let test_mode = args.get_test_mode();
    let paths = args.get_remove_paths().unwrap();

    // First check all files that will be removed
    let files_to_remove = remove::check_removes(paths, args.is_recursive(), args).await?;

    // Ask for confirmation if needed (not in force mode and either interactive or has items to remove)
    if !files_to_remove.is_empty() && !args.is_force() && (!args.is_interactive() || files_to_remove.len() > 1) {
        if !confirm_removal(&files_to_remove).await? {
            println!("Operation cancelled.");
            return Ok(());
        }
    }

    // Calculate total size for progress bar
    let total_size = files_to_remove.iter()
        .map(|f| f.size)
        .sum();

    // Initialize progress display
    let progress = Arc::new(Mutex::new(CopyProgress::new(total_size)?));
    
    // Set operation type
    progress.lock().set_operation_type("Removing");

    // Set initial display using the first path
    if let Some(first_path) = paths.first() {
        let display_name = first_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        progress.lock().set_current_file(&display_name, total_size);
    }

    // Create clones for callbacks
    let progress_for_inc = Arc::clone(&progress);
    let progress_for_file = Arc::clone(&progress);

    // Set up improved Ctrl+C handler
    let progress_for_signal = Arc::clone(&progress);
    #[allow(unused_mut)]
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    
    tokio::spawn(async move {
        if let Ok(()) = ctrl_c().await {
            let _ = progress_for_signal.lock().finish();
            let _ = tx.send(());
        }
    });

    // Prepare the callbacks
    let inc_callback = move |n| progress_for_inc.lock().inc_current(n);
    let file_callback = Box::new(move |name: &str, size: u64| {
        progress_for_file.lock().set_current_file(name, size);
    });

    // Use tokio::select! to handle both the remove operation and ctrl+c
    tokio::select! {
        result = remove::remove_paths(
            paths,
            test_mode,
            args,
            Arc::clone(&progress),
            inc_callback,
            file_callback,
        ) => {
            // Clean up and handle any errors
            let mut progress = progress.lock();
            if let Err(e) = result {
                progress.finish()?;
                return Err(e);
            }
            progress.finish()?;
        }
        _ = rx => {
            println!("\nOperation cancelled by user.");
            return Ok(());
        }
    }

    // Give the user time to see final status
    tokio::time::sleep(Duration::from_secs(1)).await;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::parse_args();

    match &cli.command {
        Commands::Copy { .. } => handle_copy_command(&cli.command).await?,
        Commands::Move { .. } => handle_move_command(&cli.command).await?,
        Commands::Remove { .. } => handle_remove_command(&cli.command).await?,
    }

    Ok(())
}