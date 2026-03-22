mod cli;
mod commands;
mod config;
mod core;
mod ui;

use crate::commands::remote_copy::{handle_remote_copy, is_plain_mode, ProgressRunner};
use crate::config::{UpdateCheck, CONFIG};
use crate::core::error::BcmrError;
use anyhow::{bail, Result};
use cli::Commands;
use std::io::{self, Write};
use std::sync::mpsc;
use std::sync::Arc;
use ui::utils::format_bytes;

fn prompt_yes_no(message: &str) -> Result<bool> {
    print!("{} [y/N] ", message);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().eq_ignore_ascii_case("y"))
}

fn confirm_overwrite(files: &[commands::copy::FileToOverwrite]) -> Result<bool> {
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

fn confirm_removal(files: &[commands::remove::FileToRemove]) -> Result<bool> {
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

async fn handle_copy_command(args: &Commands) -> Result<()> {
    use crate::core::remote::parse_remote_path;

    let test_mode = args.get_test_mode();
    let excludes = args.compile_excludes()?;
    let (sources, dest) = args.get_sources_and_dest().map_err(anyhow::Error::msg)?;

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
            && !confirm_overwrite(&plan.overwrites)?
        {
            return Err(BcmrError::Cancelled.into());
        }

        if args.is_dry_run() {
            println!("DRY RUN MODE: No changes will be made.\n");
            commands::copy::dry_run_plan(&plan, args)?;
            println!(
                "\nSummary: {} sources, {}",
                sources.len(),
                format_bytes(plan.total_size as f64)
            );
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
        )
        .await;

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
        )
        .await;

        if let Err(e) = result {
            return runner.finish_err(e.to_string());
        }

        runner.finish_ok()
    }
}

async fn handle_move_command(args: &Commands) -> Result<()> {
    let test_mode = args.get_test_mode();
    let excludes = args.compile_excludes()?;
    let (sources, dest) = args.get_sources_and_dest().map_err(anyhow::Error::msg)?;

    if sources.len() > 1 && (!dest.exists() || !dest.is_dir()) {
        bail!(
            "When moving multiple sources, destination '{}' must be an existing directory",
            dest.display()
        );
    }

    if args.is_force() {
        let files_to_overwrite =
            commands::r#move::check_overwrites(sources, dest, args.is_recursive(), args, &excludes)
                .await?;

        if !files_to_overwrite.is_empty()
            && args.should_prompt_for_overwrite()
            && !confirm_overwrite(&files_to_overwrite)?
        {
            return Err(BcmrError::Cancelled.into());
        }
    }

    let total_size =
        commands::r#move::get_total_size(sources, args.is_recursive(), args, &excludes).await?;

    if args.is_dry_run() {
        println!("DRY RUN MODE: No changes will be made.\n");

        for src in sources {
            commands::r#move::move_path(
                src,
                dest,
                args.is_recursive(),
                args.is_preserve(),
                test_mode.clone(),
                args,
                &excludes,
                |_| {},
                |_, _| {},
            )
            .await?;
        }

        println!(
            "\nSummary: {} sources, {}",
            sources.len(),
            format_bytes(total_size as f64)
        );
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
            src,
            dest,
            args.is_recursive(),
            args.is_preserve(),
            test_mode.clone(),
            args,
            &excludes,
            runner.inc_callback(),
            runner.file_callback(),
        )
        .await;

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
        let result = commands::remove::remove_paths(
            paths,
            test_mode,
            args,
            &excludes,
            Arc::clone(runner.progress()),
            runner.inc_callback(),
            Box::new(runner.file_callback()),
            files_to_remove.len(),
        )
        .await;

        runner.finish_ok()?;

        result?;

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
        && !confirm_removal(&files_to_remove)?
    {
        return Err(BcmrError::Cancelled.into());
    }

    let total_size = files_to_remove.iter().map(|f| f.size).sum();
    let runner = ProgressRunner::new(total_size, is_plain_mode(args), args.is_dry_run())?;

    runner.progress().lock().set_operation_type("Removing");

    if let Some(first_path) = paths.first() {
        let display_name = first_path.file_name().unwrap_or_default().to_string_lossy();
        runner
            .progress()
            .lock()
            .set_current_file(&display_name, total_size);
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
    )
    .await?;

    runner.finish_ok()
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
            let script = commands::init::generate_init_script(
                shell,
                cmd.as_deref().unwrap_or(""),
                prefix.as_deref(),
                suffix.as_deref(),
                path.as_ref(),
                *no_cmd,
            );
            print!("{}", script);
            Ok(())
        }
        _ => unreachable!(),
    }
}

fn validate_mode(mode: &str, name: &str) -> Result<()> {
    match mode.to_lowercase().as_str() {
        "force" | "disable" | "never" | "auto" => Ok(()),
        other => Err(BcmrError::InvalidInput(format!(
            "Invalid {} mode '{}'. Supported modes: force, disable, never, auto.",
            name, other
        ))
        .into()),
    }
}

const POWERSHELL_REMOTE_INJECT: &str = r#"    $tokens = $commandAst.ToString() -split '\s+'
    if ($wordToComplete -match '.+:.+' -and $tokens.Count -ge 2 -and ($tokens[1] -eq 'copy' -or $tokens[1] -eq 'move')) {
        $results = bcmr __complete-remote $wordToComplete 2>$null
        if ($results) {
            $results | ForEach-Object {
                [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
            }
            return
        }
    }"#;

fn build_completion_command() -> clap::Command {
    let full = <cli::Cli as clap::CommandFactory>::command();
    let visible: Vec<clap::Command> = full
        .get_subcommands()
        .filter(|s| !s.is_hide_set())
        .cloned()
        .collect();
    let mut cmd = clap::Command::new("bcmr");
    for sub in visible {
        cmd = cmd.subcommand(sub);
    }
    cmd
}

fn remote_completion_script(shell: &clap_complete::Shell) -> &'static str {
    use clap_complete::Shell;
    match shell {
        Shell::Zsh => {
            r#"

_bcmr_with_remote() {
    local cur="${words[CURRENT]}"
    if [[ "$cur" == *:* ]] && [[ "${words[2]}" == "copy" || "${words[2]}" == "move" ]]; then
        local -a results
        results=("${(@f)$(bcmr __complete-remote "$cur" 2>/dev/null)}")
        if [[ ${#results[@]} -gt 0 && -n "${results[1]}" ]]; then
            compadd -U -S '' -- "${results[@]}"
            return
        fi
    fi
    _bcmr "$@"
}
compdef _bcmr_with_remote bcmr
"#
        }
        Shell::Bash => {
            r#"

_bcmr_with_remote() {
    local cur="${COMP_WORDS[COMP_CWORD]}"
    local cmd="${COMP_WORDS[1]}"
    if [[ "$cur" == *:* ]] && [[ "$cmd" == "copy" || "$cmd" == "move" ]]; then
        local IFS=$'\n'
        COMPREPLY=($(bcmr __complete-remote "$cur" 2>/dev/null))
        if [[ ${#COMPREPLY[@]} -gt 0 ]]; then
            compopt -o nospace
            return
        fi
    fi
    _bcmr "$@"
}
complete -F _bcmr_with_remote bcmr
"#
        }
        Shell::Fish => {
            r#"

complete -c bcmr -n '__fish_seen_subcommand_from copy move; and string match -q "*:*" -- (commandline -ct)' -f -a '(bcmr __complete-remote (commandline -ct) 2>/dev/null)'
"#
        }
        Shell::PowerShell => "", // handled via injection into clap-generated script
        _ => "",
    }
}

fn background_update_check(command: &Commands) -> Option<mpsc::Receiver<Option<String>>> {
    if matches!(
        command,
        Commands::Update | Commands::Completions { .. } | Commands::CompleteRemote { .. }
    ) {
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
        Commands::CompleteRemote { partial } => {
            for entry in crate::core::remote::complete_remote_path(partial).await {
                println!("{}", entry);
            }
        }
        Commands::Completions { shell } => {
            let mut cmd = build_completion_command();
            let mut buf = Vec::new();
            clap_complete::generate(*shell, &mut cmd, "bcmr", &mut buf);
            let base = String::from_utf8(buf).expect("clap generated invalid UTF-8");

            if *shell == clap_complete::Shell::PowerShell {
                // Inject remote path check into the clap-generated script block
                // so there's only ONE Register-ArgumentCompleter
                let injected = base.replacen(
                    "param($wordToComplete, $commandAst, $cursorPosition)\n",
                    &format!(
                        "param($wordToComplete, $commandAst, $cursorPosition)\n{}\n",
                        POWERSHELL_REMOTE_INJECT
                    ),
                    1,
                );
                print!("{}", injected);
            } else {
                print!("{}", base);
                print!("{}", remote_completion_script(shell));
            }
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
