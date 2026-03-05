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
use ui::progress::{self, ProgressRenderer};
use std::io::{self, Write};
use std::sync::Arc;

use tokio::signal::ctrl_c;
use tokio::time::Duration;

fn is_plain_mode(args: &Commands) -> bool {
    args.is_tui_mode() || CONFIG.progress.style.eq_ignore_ascii_case("plain")
}

// --- Confirmation dialogs ---

fn prompt_yes_no(message: &str) -> Result<bool> {
    print!("{} [y/N] ", message);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().eq_ignore_ascii_case("y"))
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
    prompt_yes_no("\nDo you want to proceed?")
}

async fn confirm_removal(files: &[commands::remove::FileToRemove]) -> Result<bool> {
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

    prompt_yes_no("\nDo you want to proceed?")
}

// --- Progress runner: shared boilerplate for all commands ---

struct ProgressRunner {
    progress: Arc<Mutex<Box<dyn ProgressRenderer>>>,
    ticker_handle: tokio::task::JoinHandle<()>,
}

impl ProgressRunner {
    fn new(total_size: u64, plain: bool, silent: bool) -> io::Result<Self> {
        let renderer = progress::create_renderer(total_size, plain, silent)?;
        let progress = Arc::new(Mutex::new(renderer));

        let ticker = Arc::clone(&progress);
        let ticker_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            loop {
                interval.tick().await;
                ticker.lock().tick();
            }
        });

        let signal = Arc::clone(&progress);
        tokio::spawn(async move {
            if let Ok(()) = ctrl_c().await {
                let _ = signal.lock().finish();
                std::process::exit(0);
            }
        });

        Ok(Self {
            progress,
            ticker_handle,
        })
    }

    fn progress(&self) -> &Arc<Mutex<Box<dyn ProgressRenderer>>> {
        &self.progress
    }

    fn inc_callback(&self) -> impl Fn(u64) + Send + Sync + Clone + 'static {
        let p = Arc::clone(&self.progress);
        move |n| p.lock().inc_current(n)
    }

    fn file_callback(&self) -> impl Fn(&str, u64) + Send + Sync + Clone + 'static {
        let p = Arc::clone(&self.progress);
        move |name, size| p.lock().set_current_file(name, size)
    }

    fn finish_ok(self) -> Result<()> {
        self.ticker_handle.abort();
        self.progress.lock().finish()?;
        Ok(())
    }

    fn finish_err(self, msg: String) -> Result<()> {
        self.ticker_handle.abort();
        let _ = self.progress.lock().finish();
        bail!("{}", msg);
    }
}

// --- Command handlers ---

async fn handle_copy_command(args: &Commands) -> Result<()> {
    let test_mode = args.get_test_mode();
    let excludes = args.compile_excludes()?;
    let (sources, dest) = args
        .get_sources_and_dest()
        .map_err(anyhow::Error::msg)?;

    if let Some(mode) = args.get_reflink_mode() {
        validate_mode(&mode, "reflink")?;
    }
    if let Some(mode) = args.get_sparse_mode() {
        validate_mode(&mode, "sparse")?;
    }

    if sources.len() > 1 && (!dest.exists() || !dest.is_dir()) {
        bail!(
            "When copying multiple sources, destination '{}' must be an existing directory",
            dest.display()
        );
    }

    if args.is_force() {
        let files_to_overwrite =
            commands::copy::check_overwrites(sources, dest, args.is_recursive(), args, &excludes).await?;

        if !files_to_overwrite.is_empty()
            && args.should_prompt_for_overwrite()
            && !confirm_overwrite(&files_to_overwrite).await?
        {
            return Err(BcmrError::Cancelled.into());
        }
    }

    if args.is_dry_run() {
        println!("DRY RUN MODE: No changes will be made.");
    }

    let total_size = commands::copy::get_total_size(sources, args.is_recursive(), args, &excludes).await?;
    let runner = ProgressRunner::new(total_size, is_plain_mode(args), args.is_dry_run())?;

    if let Some(first) = sources.first() {
        let display_name = first.file_name().unwrap_or_default().to_string_lossy();
        runner.progress().lock().set_current_file(&display_name, total_size);
    }

    for src in sources {
        let result = commands::copy::copy_path(
            src, dest,
            args.is_recursive(), args.is_preserve(),
            test_mode.clone(), args, &excludes,
            runner.inc_callback(), runner.file_callback(),
        ).await;

        if let Err(e) = result {
            eprintln!("Error copying '{}': {}", src.display(), e);
            return runner.finish_err("Copy operation encountered errors.".into());
        }
    }

    runner.finish_ok()
}

async fn handle_move_command(args: &Commands) -> Result<()> {
    let test_mode = args.get_test_mode();
    let excludes = args.compile_excludes()?;
    let (sources, dest) = args
        .get_sources_and_dest()
        .map_err(anyhow::Error::msg)?;

    if sources.len() > 1 && (!dest.exists() || !dest.is_dir()) {
        bail!(
            "When moving multiple sources, destination '{}' must be an existing directory",
            dest.display()
        );
    }

    if args.is_force() {
        let files_to_overwrite =
            commands::r#move::check_overwrites(sources, dest, args.is_recursive(), args, &excludes).await?;

        if !files_to_overwrite.is_empty()
            && args.should_prompt_for_overwrite()
            && !confirm_overwrite(&files_to_overwrite).await?
        {
            println!("Operation cancelled.");
            return Ok(());
        }
    }

    if args.is_dry_run() {
        println!("DRY RUN MODE: No changes will be made.");
    }

    let total_size = commands::r#move::get_total_size(sources, args.is_recursive(), args, &excludes).await?;
    let runner = ProgressRunner::new(total_size, is_plain_mode(args), args.is_dry_run())?;

    if let Some(first) = sources.first() {
        let display_name = first.file_name().unwrap_or_default().to_string_lossy();
        let mut p = runner.progress().lock();
        p.set_current_file(&display_name, total_size);
        p.set_operation_type("Moving");
    }

    for src in sources {
        let result = commands::r#move::move_path(
            src, dest,
            args.is_recursive(), args.is_preserve(),
            test_mode.clone(), args, &excludes,
            runner.inc_callback(), runner.file_callback(),
        ).await;

        if let Err(e) = result {
            eprintln!("Error moving '{}': {}", src.display(), e);
            return runner.finish_err("Move operation encountered errors.".into());
        }
    }

    runner.finish_ok()
}

async fn handle_remove_command(args: &Commands) -> Result<()> {
    let test_mode = args.get_test_mode();
    let excludes = args.compile_excludes()?;
    let paths = args.get_remove_paths().map_err(anyhow::Error::msg)?;

    let files_to_remove =
        commands::remove::check_removes(paths, args.is_recursive(), args, &excludes).await?;

    if args.is_dry_run() {
        println!("DRY RUN MODE: No changes will be made.");
    }

    if !args.is_dry_run()
        && !files_to_remove.is_empty()
        && !args.is_force()
        && (!args.is_interactive() || files_to_remove.len() > 1)
        && !confirm_removal(&files_to_remove).await?
    {
        println!("Operation cancelled.");
        return Ok(());
    }

    let total_size = files_to_remove.iter().map(|f| f.size).sum();
    let runner = ProgressRunner::new(total_size, is_plain_mode(args), args.is_dry_run())?;

    runner.progress().lock().set_operation_type("Removing");

    if let Some(first_path) = paths.first() {
        let display_name = first_path.file_name().unwrap_or_default().to_string_lossy();
        runner.progress().lock().set_current_file(&display_name, total_size);
    }

    commands::remove::remove_paths(
        paths,
        test_mode,
        args,
        &excludes,
        Arc::clone(runner.progress()),
        runner.inc_callback(),
        Box::new(runner.file_callback()),
    ).await?;

    runner.finish_ok()
}

fn handle_init_command(args: &Commands) -> Result<()> {
    match args {
        Commands::Init {
            shell, cmd, prefix, suffix, path, no_cmd,
        } => {
            let script = commands::init::generate_init_script(
                shell,
                cmd.as_deref().unwrap_or(""),
                prefix.as_deref(),
                suffix.as_deref(),
                path.as_ref(),
                *no_cmd
            );
            print!("{}", script);
            Ok(())
        }
        _ => unreachable!(),
    }
}

fn validate_mode(mode: &str, name: &str) -> Result<()> {
    match mode.to_lowercase().as_str() {
        "force" | "disable" | "auto" => Ok(()),
        other => Err(BcmrError::InvalidInput(
            format!("Invalid {} mode '{}'. Supported modes: force, disable, auto.", name, other)
        ).into()),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::parse_args();

    match &cli.command {
        Commands::Copy { .. } => handle_copy_command(&cli.command).await?,
        Commands::Move { .. } => handle_move_command(&cli.command).await?,
        Commands::Remove { .. } => handle_remove_command(&cli.command).await?,
        Commands::Init { .. } => handle_init_command(&cli.command)?,
    }

    Ok(())
}
