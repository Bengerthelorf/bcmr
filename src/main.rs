mod cli;
mod config;
mod core;
mod commands;
mod ui;

use crate::config::{CONFIG, UpdateCheck};
use crate::core::error::BcmrError;
use anyhow::{bail, Result};
use cli::Commands;
use parking_lot::Mutex;
use ui::progress::{self, ProgressRenderer};
use ui::utils::format_bytes;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::mpsc;

use tokio::signal::ctrl_c;
use tokio::time::Duration;

fn is_plain_mode(args: &Commands) -> bool {
    args.is_tui_mode() || CONFIG.progress.style.eq_ignore_ascii_case("plain")
}

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
        println!("  Total size: {}", format_bytes(total_size as f64));
    }

    for file in files {
        println!(
            "  {} {}{}",
            if file.is_dir { "DIR:" } else { "FILE:" },
            file.path.display(),
            if !file.is_dir && file.size > 0 {
                format!(" ({})", format_bytes(file.size as f64))
            } else {
                String::new()
            }
        );
    }

    prompt_yes_no("\nDo you want to proceed?")
}

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
                commands::copy::cleanup_partial_files();
                let _ = signal.lock().finish();
                std::process::exit(130);
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

async fn handle_copy_command(args: &Commands) -> Result<()> {
    use crate::core::remote::parse_remote_path;

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

    let dest_str = dest.to_string_lossy();
    let remote_dest = parse_remote_path(&dest_str);
    let any_remote_source = sources
        .iter()
        .any(|s| parse_remote_path(&s.to_string_lossy()).is_some());

    if remote_dest.is_some() || any_remote_source {
        return handle_remote_copy(args, sources, dest, &excludes).await;
    }

    if sources.len() > 1 && (!dest.exists() || !dest.is_dir()) {
        bail!(
            "When copying multiple sources, destination '{}' must be an existing directory",
            dest.display()
        );
    }

    let needs_overwrite_prompt = args.is_force() && args.should_prompt_for_overwrite();

    if needs_overwrite_prompt || args.is_dry_run() {
        let plan = commands::copy::plan_copy(sources, dest, args.is_recursive(), &excludes).await?;

        if args.is_force()
            && !plan.overwrites.is_empty()
            && args.should_prompt_for_overwrite()
            && !confirm_overwrite(&plan.overwrites).await?
        {
            return Err(BcmrError::Cancelled.into());
        }

        if args.is_dry_run() {
            println!("DRY RUN MODE: No changes will be made.\n");
            commands::copy::dry_run_plan(&plan, args)?;
            println!("\nSummary: {} sources, {}", sources.len(), format_bytes(plan.total_size as f64));
            return Ok(());
        }

        let runner = ProgressRunner::new(plan.total_size, is_plain_mode(args), false)?;

        {
            let mut p = runner.progress().lock();
            p.set_operation_type("Copying");
            if let Some(first) = sources.first() {
                let display_name = first.file_name().unwrap_or_default().to_string_lossy();
                p.set_current_file(&display_name, plan.total_size);
            }
        }

        let result = commands::copy::execute_plan(
            &plan,
            args.is_preserve(),
            test_mode,
            args,
            runner.inc_callback(),
            runner.file_callback(),
        ).await;

        if let Err(e) = result {
            return runner.finish_err(e.to_string());
        }

        runner.finish_ok()
    } else {
        let runner = ProgressRunner::new(0, is_plain_mode(args), false)?;

        {
            let mut p = runner.progress().lock();
            p.set_operation_type("Copying");
            p.set_scanning(true);
            if let Some(first) = sources.first() {
                let display_name = first.file_name().unwrap_or_default().to_string_lossy();
                p.set_current_file(&display_name, 0);
            }
        }

        let total_cb = {
            let p = Arc::clone(runner.progress());
            move |total: u64| p.lock().set_total_bytes(total)
        };
        let scan_done_cb = {
            let p = Arc::clone(runner.progress());
            move || p.lock().set_scanning(false)
        };
        let files_found_cb = {
            let p = Arc::clone(runner.progress());
            move |count: u64| p.lock().set_files_found(count)
        };

        let result = commands::copy::pipeline_copy(
            sources,
            dest,
            args.is_recursive(),
            &excludes,
            args.is_preserve(),
            test_mode,
            args,
            runner.inc_callback(),
            runner.file_callback(),
            total_cb,
            scan_done_cb,
            files_found_cb,
        ).await;

        if let Err(e) = result {
            return runner.finish_err(e.to_string());
        }

        runner.finish_ok()
    }
}

async fn handle_remote_copy(
    args: &Commands,
    sources: &[std::path::PathBuf],
    dest: &std::path::Path,
    _excludes: &[regex::Regex],
) -> Result<()> {
    use crate::core::remote::{self, parse_remote_path, RemotePath};

    let dest_str = dest.to_string_lossy();
    let remote_dest = parse_remote_path(&dest_str);

    let check_target = if let Some(ref rd) = remote_dest {
        rd.clone()
    } else {
        let src_str = sources[0].to_string_lossy();
        parse_remote_path(&src_str).ok_or_else(|| anyhow::anyhow!("No remote path found"))?
    };
    remote::validate_ssh_connection(&check_target).await?;

    // Determine direction
    if let Some(ref rdest) = remote_dest {
        let mut total_size = 0u64;
        for src in sources {
            if parse_remote_path(&src.to_string_lossy()).is_some() {
                bail!("Cannot copy between two remote hosts. Use local as intermediary.");
            }
            if src.is_file() {
                total_size += src.metadata()?.len();
            } else if src.is_dir() && args.is_recursive() {
                total_size += commands::copy::get_total_size(
                    std::slice::from_ref(src), true, args, &[],
                ).await?;
            } else if src.is_dir() {
                bail!(
                    "Source '{}' is a directory. Use -r flag for recursive copy.",
                    src.display()
                );
            } else {
                bail!("Source '{}' not found", src.display());
            }
        }

        let runner = ProgressRunner::new(total_size, is_plain_mode(args), false)?;
        runner.progress().lock().set_operation_type("Uploading");

        for src in sources {
            if src.is_file() {
                let file_remote = if sources.len() > 1 || rdest.path == "." {
                    // Multiple sources -> dest is dir, append filename
                    let fname = src.file_name().unwrap_or_default().to_string_lossy();
                    RemotePath {
                        user: rdest.user.clone(),
                        host: rdest.host.clone(),
                        path: format!("{}/{}", rdest.path, fname),
                    }
                } else {
                    rdest.clone()
                };
                let inc = runner.inc_callback();
                let file_cb = runner.file_callback();
                remote::upload_file(src, &file_remote, &inc, &file_cb).await?;
            } else if src.is_dir() && args.is_recursive() {
                let dir_name = src.file_name().unwrap_or_default().to_string_lossy();
                let dir_remote = RemotePath {
                    user: rdest.user.clone(),
                    host: rdest.host.clone(),
                    path: format!("{}/{}", rdest.path, dir_name),
                };
                let inc = runner.inc_callback();
                let file_cb = runner.file_callback();
                remote::upload_directory(src, &dir_remote, &inc, &file_cb).await?;
            }
        }

        runner.finish_ok()
    } else {
        let dest_local = dest;

        let mut remote_sources: Vec<(RemotePath, u64)> = Vec::new();
        for src in sources {
            let src_str = src.to_string_lossy();
            let rsrc = parse_remote_path(&src_str).ok_or_else(|| {
                anyhow::anyhow!("Mixed local/remote sources without remote destination")
            })?;
            let size = remote::remote_total_size(&rsrc, args.is_recursive()).await?;
            remote_sources.push((rsrc, size));
        }

        let total_size: u64 = remote_sources.iter().map(|(_, s)| *s).sum();
        let runner = ProgressRunner::new(total_size, is_plain_mode(args), false)?;
        runner.progress().lock().set_operation_type("Downloading");

        for (rsrc, _size) in &remote_sources {
            let info = remote::remote_stat(rsrc).await?;
            let inc = runner.inc_callback();
            let file_cb = runner.file_callback();

            if info.is_dir {
                if !args.is_recursive() {
                    bail!(
                        "Remote source '{}' is a directory. Use -r flag for recursive copy.",
                        rsrc
                    );
                }
                let dir_name = rsrc.path.rsplit('/').next().unwrap_or(&rsrc.path);
                let local_dir = if dest_local.is_dir() {
                    dest_local.join(dir_name)
                } else {
                    dest_local.to_path_buf()
                };
                if !local_dir.exists() {
                    tokio::fs::create_dir_all(&local_dir).await?;
                }
                remote::download_directory(rsrc, &local_dir, &inc, &file_cb).await?;
            } else {
                let local_path = if dest_local.is_dir() {
                    let fname = rsrc.path.rsplit('/').next().unwrap_or(&rsrc.path);
                    dest_local.join(fname)
                } else {
                    dest_local.to_path_buf()
                };
                remote::download_file(rsrc, &local_path, &inc, &file_cb, info.size).await?;
            }
        }

        runner.finish_ok()
    }
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
            return Err(BcmrError::Cancelled.into());
        }
    }

    let total_size = commands::r#move::get_total_size(sources, args.is_recursive(), args, &excludes).await?;

    if args.is_dry_run() {
        println!("DRY RUN MODE: No changes will be made.\n");

        for src in sources {
            let _ = commands::r#move::move_path(
                src, dest,
                args.is_recursive(), args.is_preserve(),
                test_mode.clone(), args, &excludes,
                |_| {}, |_, _| {},
            ).await;
        }

        println!("\nSummary: {} sources, {}", sources.len(), format_bytes(total_size as f64));
        return Ok(());
    }

    let runner = ProgressRunner::new(total_size, is_plain_mode(args), false)?;

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
        println!("DRY RUN MODE: No changes will be made.\n");

        let total_size: u64 = files_to_remove.iter().map(|f| f.size).sum();
        let file_count = files_to_remove.iter().filter(|f| !f.is_dir).count();
        let dir_count = files_to_remove.iter().filter(|f| f.is_dir).count();

        let runner = ProgressRunner::new(total_size, is_plain_mode(args), true)?;
        commands::remove::remove_paths(
            paths, test_mode, args, &excludes,
            Arc::clone(runner.progress()),
            runner.inc_callback(),
            Box::new(runner.file_callback()),
            files_to_remove.len(),
        ).await?;

        print!("\nSummary: {} files", file_count);
        if dir_count > 0 {
            print!(", {} directories", dir_count);
        }
        println!(", {}", format_bytes(total_size as f64));
        return Ok(());
    }

    if !files_to_remove.is_empty()
        && !args.is_force()
        && !args.is_yes()
        && (!args.is_interactive() || files_to_remove.len() > 1)
        && !confirm_removal(&files_to_remove).await?
    {
        return Err(BcmrError::Cancelled.into());
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
        files_to_remove.len(),
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

fn background_update_check(command: &Commands) -> Option<mpsc::Receiver<Option<String>>> {
    if matches!(command, Commands::Update | Commands::Completions { .. }) {
        return None;
    }
    if CONFIG.update_check != UpdateCheck::Notify {
        return None;
    }

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(commands::update::check_for_update());
    });
    Some(rx)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::parse_args();

    let update_rx = background_update_check(&cli.command);

    match &cli.command {
        Commands::Copy { .. } => handle_copy_command(&cli.command).await?,
        Commands::Move { .. } => handle_move_command(&cli.command).await?,
        Commands::Remove { .. } => handle_remove_command(&cli.command).await?,
        Commands::Init { .. } => handle_init_command(&cli.command)?,
        Commands::Update => commands::update::run()?,
        Commands::Completions { shell } => {
            let mut cmd = <cli::Cli as clap::CommandFactory>::command();
            clap_complete::generate(*shell, &mut cmd, "bcmr", &mut std::io::stdout());
        }
    }

    if let Some(rx) = update_rx {
        if let Ok(Some(version)) = rx.try_recv() {
            eprintln!(
                "\x1b[33m↑ Update available: v{} → v{} (run `bcmr update`)\x1b[0m",
                env!("CARGO_PKG_VERSION"),
                version
            );
        }
    }

    Ok(())
}
