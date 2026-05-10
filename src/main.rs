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

    let _ = commands::jobs::cleanup_old_jobs(commands::jobs::DEFAULT_GC_RETENTION_SECS);

    Ok(true)
}

const NO_ARGS_HINT: &str = "\
Better Copy Move Remove (BCMR) — modern cp/mv/scp/rm with progress, resume, and remote support.

Common usage:
  bcmr copy -r src/ host:dst/        # SSH copy, recursive, with progress
  bcmr copy -V file host:dst/        # full BLAKE3 verify
  bcmr move -r old/ archive/
  bcmr check -r src/ host:dst/       # compare without copying

  bcmr deploy <host>                 # install bcmr remotely for fast-path
  bcmr init zsh                      # set up shell integration (bcp/bmv/brm)
  bcmr completions install zsh       # one-shot completion install
  bcmr doctor                        # diagnose ssh / config / PATH issues

Type `bcmr help <command>` or `bcmr <command> --help` for full options.
Configuration: ~/.config/bcmr/config.toml
";

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::args().len() == 1 {
        print!("{}", NO_ARGS_HINT);
        return Ok(());
    }

    let mut cli = cli::parse_args();

    if let Some(ref path) = cli.config {
        std::env::set_var("BCMR_CONFIG", path);
    }

    expand_path_bookmarks(&mut cli.command)?;

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
        Commands::Status {
            job_id,
            rm,
            all,
            gc,
        } => {
            handle_status_command(job_id, *rm, *all, *gc);
        }
        Commands::Init { .. } => handle_init_command(&cli.command)?,
        Commands::Update { check } => {
            let check = *check;
            tokio::task::spawn_blocking(move || commands::update::run(check)).await??;
        }
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
        Commands::Deploy { target, path, sudo } => {
            let remote_path = path.as_deref().unwrap_or("~/.local/bin/bcmr");
            commands::deploy::run(target, remote_path, *sudo).await?;
        }
        Commands::Doctor { hosts } => {
            commands::doctor::run(hosts, is_json_mode()).await?;
        }
        Commands::CompleteRemote { partial } => {
            for entry in crate::core::remote::complete_remote_path(partial).await {
                println!("{}", entry);
            }
        }
        Commands::Completions {
            shell,
            install,
            print,
            uninstall,
        } => {
            let mut cmd = build_completion_command();
            let mut buf = Vec::new();
            clap_complete::generate(*shell, &mut cmd, "bcmr", &mut buf);
            let base = String::from_utf8(buf).expect("clap generated invalid UTF-8");

            let script = if *shell == clap_complete::Shell::PowerShell {
                base.replacen(
                    "param($wordToComplete, $commandAst, $cursorPosition)\n",
                    &format!(
                        "param($wordToComplete, $commandAst, $cursorPosition)\n{}\n",
                        POWERSHELL_REMOTE_INJECT
                    ),
                    1,
                )
            } else {
                format!("{}{}", base, remote_completion_script(shell))
            };

            if *install {
                commands::completions::run_install(*shell, &script, *print, *uninstall)?;
            } else {
                print!("{}", script);
            }
        }
    }

    if !is_json_mode() {
        show_update_hint(update_rx);
    }

    Ok(())
}

fn expand_path_bookmarks(cmd: &mut Commands) -> Result<()> {
    use std::path::PathBuf;
    let paths = match cmd {
        Commands::Copy { args, .. } | Commands::Move { args, .. } => &mut args.paths,
        Commands::Check { paths, .. } => paths,
        Commands::Remove { paths, .. } => paths,
        _ => return Ok(()),
    };
    let table = &config::CONFIG.paths;
    for p in paths.iter_mut() {
        let s = p.to_string_lossy();
        let Some((name, _suffix)) = config::parse_alias_token(&s) else {
            continue;
        };
        if !config::is_valid_alias_name(name) {
            anyhow::bail!(
                "invalid alias name '@{name}'; alias names must match [A-Za-z_][A-Za-z0-9_-]*. \
                 To copy a literal file whose name starts with '@', use './{}'",
                &s
            );
        }
        match config::resolve_path_alias(&s, table) {
            Some(resolved) => *p = PathBuf::from(resolved),
            None => {
                let suggestion = nearest_alias(name, table.keys());
                let known: Vec<&str> = table.keys().map(String::as_str).collect();
                let mut msg = format!("unknown path alias '@{name}'");
                if let Some(near) = suggestion {
                    msg.push_str(&format!(" — did you mean '@{near}'?"));
                }
                if known.is_empty() {
                    msg.push_str(
                        "\nNo aliases configured. Add a [paths] table to ~/.config/bcmr/config.toml.",
                    );
                } else {
                    let mut sorted = known;
                    sorted.sort_unstable();
                    msg.push_str(&format!("\nKnown aliases: {}", sorted.join(", ")));
                }
                msg.push_str(&format!(
                    "\nTo refer to a literal file named '@{name}', use './{}'",
                    &s
                ));
                anyhow::bail!("{msg}");
            }
        }
    }
    Ok(())
}

fn nearest_alias<'a>(
    target: &str,
    candidates: impl Iterator<Item = &'a String>,
) -> Option<&'a str> {
    candidates
        .map(|c| (levenshtein(target, c), c.as_str()))
        .filter(|(d, c)| *d <= (c.len() / 2 + 1).max(2))
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, &ac) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &bc) in b.iter().enumerate() {
            let cost = if ac == bc { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
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
