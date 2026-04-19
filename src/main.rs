mod app;
mod cli;
mod commands;
mod config;
mod core;
mod output;
mod ui;

use crate::app::commands::{
    handle_check_command, handle_copy_command, handle_init_command, handle_move_command,
    handle_remove_command,
};
use crate::app::completions::{
    build_completion_command, remote_completion_script, POWERSHELL_REMOTE_INJECT,
};
use crate::app::status::handle_status_command;
use crate::app::updates::background_update_check;
use crate::config::{is_json_mode, set_json_mode};
use anyhow::Result;
use cli::Commands;
use std::sync::mpsc;

fn maybe_detach(cli: &cli::Cli) -> Result<bool> {
    let is_operation = matches!(
        cli.command,
        Commands::Copy { .. } | Commands::Move { .. } | Commands::Remove { .. }
    );

    if !cli.json || !is_operation {
        return Ok(false);
    }

    if let Some(ref job_id) = cli._bg {
        let log_path = commands::jobs::log_path(job_id);
        config::set_log_file(log_path);
        return Ok(false);
    }

    commands::jobs::ensure_jobs_dir()?;
    let job_id = commands::jobs::new_job_id();
    let log_path = commands::jobs::log_path(&job_id);

    let exe = std::env::current_exe()?;
    let original_args: Vec<String> = std::env::args().skip(1).collect();
    let mut args = vec!["--_bg".to_string(), job_id.clone()];
    args.extend(original_args);

    let child = std::process::Command::new(exe)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    let job_info = commands::jobs::JobInfo {
        job_id: job_id.clone(),
        pid: child.id(),
        log: log_path.to_string_lossy().to_string(),
    };
    let mut f = std::fs::File::create(&log_path)?;
    serde_json::to_writer(&mut f, &job_info)?;
    use std::io::Write;
    f.write_all(b"\n")?;

    println!("{}", serde_json::to_string(&job_info)?);

    commands::jobs::cleanup_old_jobs(7 * 24 * 3600);

    Ok(true)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::parse_args();

    if maybe_detach(&cli)? {
        return Ok(());
    }

    set_json_mode(cli.json || cli._bg.is_some());

    let update_rx = background_update_check(&cli.command);

    match &cli.command {
        Commands::Copy { .. } => handle_copy_command(&cli.command).await?,
        Commands::Move { .. } => handle_move_command(&cli.command).await?,
        Commands::Remove { .. } => handle_remove_command(&cli.command).await?,
        Commands::Check { .. } => {
            let result = handle_check_command(&cli.command).await;
            match result {
                Ok(r) => {
                    if is_json_mode() {
                        let out = output::CommandOutput::Check(r);
                        println!("{}", out.to_json());
                        let code = out.exit_code();
                        if code != 0 {
                            std::process::exit(code);
                        }
                    } else {
                        let in_sync = r.in_sync;
                        output::print_check_human(&r);
                        if !in_sync {
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    if is_json_mode() {
                        let out = output::error_output("check", &e);
                        println!("{}", out.to_json());
                        std::process::exit(2);
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        Commands::Status { job_id } => {
            handle_status_command(job_id);
        }
        Commands::Init { .. } => handle_init_command(&cli.command)?,
        Commands::Update => commands::update::run()?,
        Commands::Serve { root, listen } => {
            if let Some(addr) = listen {
                let parsed: std::net::SocketAddr = addr.parse().map_err(|e| {
                    anyhow::anyhow!("bcmr serve --listen: invalid address '{addr}': {e}")
                })?;
                commands::serve::run_listen(root.clone(), parsed).await?;
            } else {
                commands::serve::run(root.clone()).await?;
            }
        }
        Commands::Deploy { target, path } => {
            let remote_path = path.as_deref().unwrap_or("~/.local/bin/bcmr");
            commands::deploy::run(target, remote_path).await?;
        }
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

    if !is_json_mode() {
        show_update_hint(update_rx);
    }

    Ok(())
}

fn show_update_hint(update_rx: Option<mpsc::Receiver<Option<String>>>) {
    if let Some(rx) = update_rx {
        if let Ok(Some(version)) = rx.try_recv() {
            eprintln!(
                "\x1b[33m↑ Update available: v{} → v{} (run `bcmr update`)\x1b[0m",
                env!("CARGO_PKG_VERSION"),
                version
            );
        }
    }
}
