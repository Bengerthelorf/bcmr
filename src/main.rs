mod cli;
mod copy;
mod r#move;
mod remove;
mod progress;
mod init;
mod config;

use anyhow::{Result, bail};
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
    let excludes = args.compile_excludes()?;
    let (sources, dest) = args.get_sources_and_dest();

    if sources.len() > 1 && (!dest.exists() || !dest.is_dir()) {
        bail!("When copying multiple sources, destination '{}' must be an existing directory", dest.display());
    }

    // If force is specified, check the files to be overwritten
    if args.is_force() {
        let files_to_overwrite = copy::check_overwrites(
            sources,
            dest,
            args.is_recursive(),
            args,
            &excludes,
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

    if args.is_dry_run() {
        println!("DRY RUN MODE: No changes will be made.");
    }

    // Calculate total size
    let total_size = copy::get_total_size(sources, args.is_recursive(), args, &excludes).await?;
    let progress = Arc::new(Mutex::new(CopyProgress::new(total_size, !args.is_fancy_progress())?));

    // Initialize progress display
    if let Some(first) = sources.first() {
         let display_name = first
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        progress.lock().set_current_file(&display_name, total_size);
    }

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

    // Loop through sources and copy each
    let mut success = true;
    for src in sources {
        let result = copy::copy_path(
            src,
            dest,
            args.is_recursive(),
            args.is_preserve(),
            test_mode.clone(),
            args,
            &excludes,
            {
                let p = Arc::clone(&progress_for_inc);
                move |n| p.lock().inc_current(n)
            },
            {
                let p = Arc::clone(&progress_for_file);
                move |name, size| p.lock().set_current_file(name, size)
            },
        )
        .await;

        if let Err(e) = result {
            eprintln!("Error copying '{}': {}", src.display(), e);
            success = false;
            // Should we stop or continue? Standard cp continues? No, generally it might stop?
            // Let's stop on error for now.
            break;
        }
    }

    // Ensure proper cleanup upon completion or error
    let mut progress = progress.lock();
    progress.finish()?;

    if !success {
        bail!("Copy operation encountered errors.");
    }
 
    tokio::time::sleep(Duration::from_secs(1)).await;
    Ok(())
}

async fn handle_move_command(args: &Commands) -> Result<()> {
    let test_mode = args.get_test_mode();
    let excludes = args.compile_excludes()?;
    let (sources, dest) = args.get_sources_and_dest();

    if sources.len() > 1 && (!dest.exists() || !dest.is_dir()) {
        bail!("When moving multiple sources, destination '{}' must be an existing directory", dest.display());
    }

    if args.is_force() {
        let files_to_overwrite = r#move::check_overwrites(
            sources,
            dest,
            args.is_recursive(),
            args,
            &excludes,
        )
        .await?;

        if !files_to_overwrite.is_empty() && args.should_prompt_for_overwrite() {
            if !confirm_overwrite(&files_to_overwrite).await? {
                println!("Operation cancelled.");
                return Ok(());
            }
        }
    }

    if args.is_dry_run() {
        println!("DRY RUN MODE: No changes will be made.");
    }

    let total_size = r#move::get_total_size(sources, args.is_recursive(), args, &excludes).await?;
    let progress = Arc::new(Mutex::new(CopyProgress::new(total_size, !args.is_fancy_progress())?));

    if let Some(first) = sources.first() {
         let display_name = first
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        {
            let mut progress_guard = progress.lock();
            progress_guard.set_current_file(&display_name, total_size);
            progress_guard.set_operation_type("Moving");
        }
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

    let mut success = true;
    for src in sources {
        let result = r#move::move_path(
            src,
            dest,
            args.is_recursive(),
            args.is_preserve(),
            test_mode.clone(),
            args,
            &excludes,
            {
                let p = Arc::clone(&progress_for_inc);
                move |n| p.lock().inc_current(n)
            },
            {
                let p = Arc::clone(&progress_for_file);
                move |name, size| p.lock().set_current_file(name, size)
            },
        )
        .await;

        if let Err(e) = result {
            eprintln!("Error moving '{}': {}", src.display(), e);
            success = false;
            break;
        }
    }

    let mut progress = progress.lock();
    progress.finish()?;

    if !success {
        bail!("Move operation encountered errors.");
    }

    tokio::time::sleep(Duration::from_secs(1)).await;
    Ok(())
}

async fn handle_remove_command(args: &Commands) -> Result<()> {
    let test_mode = args.get_test_mode();
    let excludes = args.compile_excludes()?;
    let paths = args.get_remove_paths().unwrap();

    // First check all files that will be removed
    let files_to_remove = remove::check_removes(paths, args.is_recursive(), args, &excludes).await?;

    if args.is_dry_run() {
        println!("DRY RUN MODE: No changes will be made.");
    }

    // Ask for confirmation if needed (not in force mode and either interactive or has items to remove)
    // In dry-run we skip confirmation? Or maybe confirmation confirms that we see what would happen?
    // Usually dry-run skips actual prompts or just shows them.
    // Let's skip prompt if dry_run? Or prompt "Would you like to remove...?"?
    // User wants "no changes with no execution".
    // I will skip confirmation in dry_run because we are just showing info.
    if !args.is_dry_run() && !files_to_remove.is_empty() && !args.is_force() && (!args.is_interactive() || files_to_remove.len() > 1) {
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
    let progress = Arc::new(Mutex::new(CopyProgress::new(total_size, !args.is_fancy_progress())?));
    
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
            &excludes,
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

fn handle_init_command(args: &Commands) -> Result<()> {
    match args {
        Commands::Init { shell, cmd, path, no_cmd } => {
            // 生成初始化脚本
            let script = init::generate_init_script(shell, cmd, path.as_ref(), *no_cmd);
            
            // 直接打印到标准输出，这样就可以被 eval 捕获
            print!("{}", script);
            
            Ok(())
        },
        _ => unreachable!(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::parse_args();
    // Load config if available (implied, handled inside modules or lazily? 
    // Wait, config module is there but main doesn't seem to init it explicitly.
    // It seems config is loaded on demand or static? 
    // `src/config.rs` has `CONFIG` lazy_static. Checked in previous turns.
    // So we don't need to do anything here.)

    match &cli.command {
        Commands::Copy { .. } => handle_copy_command(&cli.command).await?,
        Commands::Move { .. } => handle_move_command(&cli.command).await?,
        Commands::Remove { .. } => handle_remove_command(&cli.command).await?,
        Commands::Init { .. } => handle_init_command(&cli.command)?,
    }

    Ok(())
}
