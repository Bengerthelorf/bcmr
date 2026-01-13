mod cli;
mod config;
mod core;
mod commands;
mod ui;

use crate::config::CONFIG;
use crate::core::error::BcmrError;
use anyhow::{bail, Result};
use cli::Commands;
use parking_lot::Mutex;
use ui::progress::CopyProgress;
use std::io::{self, Write};
use std::sync::Arc;

use tokio::signal::ctrl_c;
use tokio::time::Duration;

fn is_plain_mode_enabled(args: &Commands) -> bool {
    args.is_tui_mode() || CONFIG.progress.style.eq_ignore_ascii_case("plain")
}

async fn confirm_overwrite(files: &[commands::copy::FileToOverwrite]) -> Result<bool> {
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

async fn confirm_removal(files: &[commands::remove::FileToRemove]) -> Result<bool> {
    // Calc stats
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
        println!(
            "  Total size: {:.2} MiB",
            total_size as f64 / 1024.0 / 1024.0
        );
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

    // Validation
    if let Some(mode) = args.get_reflink_mode() {
         match mode.to_lowercase().as_str() {
             "force" => {},
             "disable" => {},
             "auto" => {},
             other => {
                 return Err(BcmrError::InvalidInput(format!("Invalid reflink mode '{}'. Supported modes: force, disable, auto.", other)).into());
             }
         }
    }

    if let Some(mode) = args.get_sparse_mode() {
         match mode.to_lowercase().as_str() {
             "force" => {},
             "disable" => {},
             "auto" => {},
             other => {
                 return Err(BcmrError::InvalidInput(format!("Invalid sparse mode '{}'. Supported modes: force, disable, auto.", other)).into());
             }
         }
    }

    if sources.len() > 1 && (!dest.exists() || !dest.is_dir()) {
        bail!(
            "When copying multiple sources, destination '{}' must be an existing directory",
            dest.display()
        );
    }

    if args.is_force() {
    // Force -> Check overwrites
        let files_to_overwrite =
        commands::copy::check_overwrites(sources, dest, args.is_recursive(), args, &excludes).await?;

        if !files_to_overwrite.is_empty() && args.should_prompt_for_overwrite() {
            if !confirm_overwrite(&files_to_overwrite).await? {
                return Err(BcmrError::Cancelled.into());
            }
        }
    }

    if args.is_dry_run() {
        println!("DRY RUN MODE: No changes will be made.");
    }

    // Total size
    let total_size = commands::copy::get_total_size(sources, args.is_recursive(), args, &excludes).await?;
    let progress = Arc::new(Mutex::new(CopyProgress::new(
        total_size,
        is_plain_mode_enabled(args),
        args.is_dry_run(),
    )?));

    // Init display
    if let Some(first) = sources.first() {
        let display_name = first.file_name().unwrap_or_default().to_string_lossy();
        progress.lock().set_current_file(&display_name, total_size);
    }

    // Clones
    let progress_for_inc = Arc::clone(&progress);
    let progress_for_file = Arc::clone(&progress);

    let progress_for_signal = Arc::clone(&progress);

    let progress_ticker = Arc::clone(&progress);
    // Spawn ticker
    let ticker_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            interval.tick().await;
            progress_ticker.lock().tick();
        }
    });

    // Ctrl+C handler
    tokio::spawn(async move {
        if let Ok(()) = ctrl_c().await {
            let _ = progress_for_signal.lock().finish();
            std::process::exit(0);
        }
    });

    // Process sources
    let mut success = true;
    for src in sources {
        let result = commands::copy::copy_path(
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
            // Stop TUI, print error
            ticker_handle.abort();
            let mut p = progress.lock();
            let _ = p.finish(); // Ignore error during finish if any, to ensure we print the actual error
            drop(p);

            eprintln!("Error copying '{}': {}", src.display(), e);
            success = false;
            // Stop on error
            break;
        }
    }
    
    // Success? Finish
    if success {
        ticker_handle.abort();
        let mut progress = progress.lock();
        progress.finish()?;
    }

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
        bail!(
            "When moving multiple sources, destination '{}' must be an existing directory",
            dest.display()
        );
    }

    if args.is_force() {
        let files_to_overwrite =
            commands::r#move::check_overwrites(sources, dest, args.is_recursive(), args, &excludes).await?;

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

    let total_size = commands::r#move::get_total_size(sources, args.is_recursive(), args, &excludes).await?;
    let progress = Arc::new(Mutex::new(CopyProgress::new(
        total_size,
        is_plain_mode_enabled(args),
        args.is_dry_run(),
    )?));

    if let Some(first) = sources.first() {
        let display_name = first.file_name().unwrap_or_default().to_string_lossy();
        {
            let mut progress_guard = progress.lock();
            progress_guard.set_current_file(&display_name, total_size);
            progress_guard.set_operation_type("Moving");
        }
    }

    let progress_for_inc = Arc::clone(&progress);
    let progress_for_file = Arc::clone(&progress);
    let progress_for_signal = Arc::clone(&progress);

    // Spawn ticker
    let progress_ticker = Arc::clone(&progress);
    let ticker_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            interval.tick().await;
            progress_ticker.lock().tick();
        }
    });

    tokio::spawn(async move {
        if let Ok(()) = ctrl_c().await {
            let _ = progress_for_signal.lock().finish();
            std::process::exit(0);
        }
    });

    let mut success = true;
    for src in sources {
        let result = commands::r#move::move_path(
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
            ticker_handle.abort();
            let mut p = progress.lock();
            let _ = p.finish();
            drop(p);
            
            eprintln!("Error moving '{}': {}", src.display(), e);
            success = false;
            break;
        }
    }

    if success {
         ticker_handle.abort();
         let mut progress = progress.lock();
         progress.finish()?;
    }

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

    let files_to_remove =
        commands::remove::check_removes(paths, args.is_recursive(), args, &excludes).await?;

    if args.is_dry_run() {
        println!("DRY RUN MODE: No changes will be made.");
    }

    // Confirm? (skip in dry-run)
    if !args.is_dry_run()
        && !files_to_remove.is_empty()
        && !args.is_force()
        && (!args.is_interactive() || files_to_remove.len() > 1)
    {
        if !confirm_removal(&files_to_remove).await? {
            println!("Operation cancelled.");
            return Ok(());
        }
    }

    // Total size
    let total_size = files_to_remove.iter().map(|f| f.size).sum();

    // Init display
    let progress = Arc::new(Mutex::new(CopyProgress::new(
        total_size,
        is_plain_mode_enabled(args),
        args.is_dry_run(),
    )?));

    progress.lock().set_operation_type("Removing");

    // Initial display
    if let Some(first_path) = paths.first() {
        let display_name = first_path.file_name().unwrap_or_default().to_string_lossy();
        progress.lock().set_current_file(&display_name, total_size);
    }

    // Clones
    let progress_for_inc = Arc::clone(&progress);
    let progress_for_file = Arc::clone(&progress);

    // Ctrl+C handler
    let progress_for_signal = Arc::clone(&progress);
    #[allow(unused_mut)]
    let (tx, mut rx) = tokio::sync::oneshot::channel();

    // Spawn ticker
    let progress_ticker = Arc::clone(&progress);
    let ticker_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            interval.tick().await;
            progress_ticker.lock().tick();
        }
    });

    tokio::spawn(async move {
        if let Ok(()) = ctrl_c().await {
            let _ = progress_for_signal.lock().finish();
            let _ = tx.send(());
        }
    });

    // Callbacks
    let inc_callback = move |n| progress_for_inc.lock().inc_current(n);
    let file_callback = Box::new(move |name: &str, size: u64| {
        progress_for_file.lock().set_current_file(name, size);
    });

    // Run | Ctrl+C
    tokio::select! {
        result = commands::remove::remove_paths(
            paths,
            test_mode,
            args,
            &excludes,
            Arc::clone(&progress),
            inc_callback,
            file_callback,
        ) => {
            // Cleanup
            let mut progress = progress.lock();
            if let Err(e) = result {
                progress.finish()?;
                return Err(e.into());
            }
            progress.finish()?;
        }
        _ = rx => {
            ticker_handle.abort();
            println!("\nOperation cancelled by user.");
            return Ok(());
        }
    }

    // Wait for final status
    tokio::time::sleep(Duration::from_secs(1)).await;
    Ok(())
}

fn handle_init_command(args: &Commands) -> Result<()> {
    match args {
        Commands::Init {
            shell,
            cmd,
            prefix,
            suffix,
            path,
            no_cmd,
        } => {
            // Script generation
            let script = commands::init::generate_init_script(
                shell,
                cmd.as_deref().unwrap_or(""),
                prefix.as_deref(),
                suffix.as_deref(),
                path.as_ref(),
                *no_cmd
            );

            // Print script for eval
            print!("{}", script);

            Ok(())
        }
        _ => unreachable!(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::parse_args();
    // Load config (lazy static)

    match &cli.command {
        Commands::Copy { .. } => handle_copy_command(&cli.command).await?,
        Commands::Move { .. } => handle_move_command(&cli.command).await?,
        Commands::Remove { .. } => handle_remove_command(&cli.command).await?,
        Commands::Init { .. } => handle_init_command(&cli.command)?,
    }

    Ok(())
}
