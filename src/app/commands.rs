use crate::app::completions::validate_mode;
use crate::app::prompts::{confirm_overwrite, confirm_removal, first_display_name};
use crate::app::runners::{resume_or_new_runner, start_scanning_runner};
use crate::cli::Commands;
use crate::commands;
use crate::commands::remote_copy::{handle_remote_copy, is_plain_mode};
use crate::config::is_json_mode;
use crate::core::error::BcmrError;
use crate::output;
use crate::ui::runner::ProgressRunner;
use crate::ui::utils::format_bytes;
use anyhow::{bail, Result};
use std::sync::Arc;

#[cfg(unix)]
fn validate_source_kind(src: &std::path::Path) -> Result<()> {
    use crate::core::remote::parse_remote_path;
    use std::os::unix::fs::FileTypeExt;

    if parse_remote_path(&src.to_string_lossy()).is_some() {
        return Ok(());
    }
    let md = match src.metadata() {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    let ft = md.file_type();
    if ft.is_file() || ft.is_dir() {
        return Ok(());
    }
    let kind = if ft.is_fifo() {
        "FIFO (named pipe)"
    } else if ft.is_socket() {
        "socket"
    } else if ft.is_block_device() {
        "block device"
    } else if ft.is_char_device() {
        "character device"
    } else {
        "non-regular file"
    };
    let hint = if src.to_str() == Some("/dev/null") {
        "\nTo clear a remote file, use: : > /tmp/empty && bcmr copy /tmp/empty <dest>"
    } else {
        ""
    };
    bail!(
        "Source '{}' is a {}; bcmr copy supports only regular files and directories.{}",
        src.display(),
        kind,
        hint
    );
}

#[cfg(not(unix))]
fn validate_source_kind(_src: &std::path::Path) -> Result<()> {
    Ok(())
}

fn source_basename(src: &std::path::Path) -> Option<String> {
    use crate::core::remote::parse_remote_path;
    let s = src.to_string_lossy();
    if let Some(rsrc) = parse_remote_path(&s) {
        rsrc.path
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    } else {
        src.file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
    }
}

fn check_basename_collisions(sources: &[std::path::PathBuf], force: bool) -> Result<()> {
    use std::collections::BTreeMap;
    if sources.len() < 2 {
        return Ok(());
    }
    let mut groups: BTreeMap<String, Vec<&std::path::Path>> = BTreeMap::new();
    for src in sources {
        if let Some(name) = source_basename(src) {
            groups.entry(name).or_default().push(src.as_path());
        }
    }
    let collisions: Vec<_> = groups.iter().filter(|(_, v)| v.len() > 1).collect();
    if collisions.is_empty() {
        return Ok(());
    }
    let mut msg = String::from(
        "Multiple sources collide on basename at destination — last writer would silently win:\n",
    );
    for (name, paths) in &collisions {
        msg.push_str(&format!("  '{}' from:\n", name));
        for p in *paths {
            msg.push_str(&format!("    {}\n", p.display()));
        }
    }
    if force {
        eprintln!("bcmr: warning: {}", msg.trim_end());
        eprintln!("bcmr: -f set, proceeding (last writer wins).");
        return Ok(());
    }
    msg.push_str("Pass -f/--force to allow the last-writer-wins overwrite, or copy each source to a distinct destination.");
    bail!("{}", msg);
}

pub(crate) async fn handle_copy_command(args: &Commands) -> Result<()> {
    let cm = args
        .copy_move_args()
        .ok_or_else(|| anyhow::anyhow!("copy requires copy/move args"))?;

    if cm.to.is_empty() {
        if cm.paths.len() < 2 {
            bail!("missing destination — pass at least <SRC> <DEST> or use --to <DEST>");
        }
        return handle_copy_one(args, None).await;
    }

    let mut failures = 0usize;
    for dest in &cm.to {
        if !is_json_mode() && !args.is_quiet() {
            eprintln!("→ copying to {}", dest.display());
        }
        if let Err(e) = handle_copy_one(args, Some(dest)).await {
            failures += 1;
            eprintln!("Error copying to {}: {}", dest.display(), e);
        }
    }
    if failures > 0 {
        bail!("{} of {} fan-out targets failed", failures, cm.to.len());
    }
    Ok(())
}

async fn handle_copy_one(args: &Commands, dest_override: Option<&std::path::Path>) -> Result<()> {
    use crate::core::remote::parse_remote_path;

    let excludes = args.compile_excludes()?;
    let (sources, dest_buf): (&[std::path::PathBuf], std::path::PathBuf) = match dest_override {
        Some(d) => {
            let s = args
                .copy_move_args()
                .map(|a| a.paths.as_slice())
                .unwrap_or(&[]);
            (s, d.to_path_buf())
        }
        None => {
            let (s, d) = args.get_sources_and_dest().map_err(anyhow::Error::msg)?;
            (s, d.clone())
        }
    };
    let dest: &std::path::Path = dest_buf.as_path();

    if let Some(mode) = args.get_reflink_mode() {
        validate_mode(&mode, "reflink")?;
    }
    if let Some(mode) = args.get_sparse_mode() {
        validate_mode(&mode, "sparse")?;
    }

    for src in sources {
        validate_source_kind(src)?;
    }

    check_basename_collisions(sources, args.is_force())?;

    let dest_str = dest.to_string_lossy();
    let remote_dest = parse_remote_path(&dest_str);
    let any_remote_source = sources
        .iter()
        .any(|s| parse_remote_path(&s.to_string_lossy()).is_some());

    if remote_dest.is_some() || any_remote_source {
        return handle_remote_copy(args, sources, dest, &excludes).await;
    }

    if !args.is_recursive() {
        for src in sources {
            if let Ok(md) = src.symlink_metadata() {
                if md.is_dir() {
                    bail!(
                        "Source '{}' is a directory. Use -r flag for recursive copy.",
                        src.display()
                    );
                }
            }
        }
    }

    if sources.len() > 1 && (!dest.exists() || !dest.is_dir()) {
        bail!(
            "When copying multiple sources, destination '{}' must be an existing directory",
            dest.display()
        );
    }

    let needs_overwrite_prompt = args.is_force() && args.should_prompt_for_overwrite();

    if needs_overwrite_prompt || args.is_dry_run() {
        let first_display = first_display_name(sources);
        let early = if !args.is_dry_run() {
            start_scanning_runner(args, "Copying", first_display.as_deref())?
        } else {
            None
        };

        let plan = match commands::copy::plan_copy(
            sources,
            dest,
            args.is_recursive(),
            args.is_no_deref(),
            &excludes,
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                if let Some(r) = early {
                    r.finish_with_error(&e.to_string());
                }
                return Err(e.into());
            }
        };

        if args.is_force()
            && !plan.overwrites.is_empty()
            && args.should_prompt_for_overwrite()
            && !confirm_overwrite(&plan.overwrites)?
        {
            if let Some(r) = early {
                r.finish_with_error("cancelled by user");
            }
            return Err(BcmrError::Cancelled.into());
        }

        if args.is_dry_run() {
            if !is_json_mode() {
                println!("DRY RUN MODE: No changes will be made.\n");
                commands::copy::dry_run_plan(&plan, args)?;
                println!(
                    "\nSummary: {} sources, {}",
                    sources.len(),
                    format_bytes(plan.total_size as f64)
                );
            }
            return Ok(());
        }

        let runner = resume_or_new_runner(
            early,
            args,
            "Copying",
            first_display.as_deref(),
            plan.total_size,
            args.is_quiet(),
        )?;

        let result = commands::copy::execute_plan(
            &plan,
            args,
            runner.inc_callback(),
            runner.file_callback(),
            runner.reflink_callback(),
        )
        .await;

        if let Err(e) = result {
            return runner.finish_err(e.to_string());
        }

        runner.finish_ok()
    } else {
        let runner = ProgressRunner::new(
            0,
            is_plain_mode(args),
            args.is_quiet(),
            is_json_mode(),
            commands::copy::cleanup_partial_files,
        )?;

        {
            let mut p = runner.progress().lock();
            p.set_operation_type("Copying");
            p.set_scanning(true);
            p.set_verify_mode(args.is_verify());
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
            args,
            &excludes,
            commands::copy::PipelineCallbacks {
                on_progress: runner.inc_callback(),
                on_new_file: Box::new(runner.file_callback()),
                on_total_update: Box::new(total_cb),
                on_scan_complete: Box::new(scan_done_cb),
                on_file_found: Box::new(files_found_cb),
                on_reflink: Box::new(runner.reflink_callback()),
            },
        )
        .await;

        if let Err(e) = result {
            return runner.finish_err(e.to_string());
        }

        runner.finish_ok()
    }
}

pub(crate) async fn handle_move_command(args: &Commands) -> Result<()> {
    use crate::core::remote::{parse_remote_path, remote_remove};

    let cm = args
        .copy_move_args()
        .ok_or_else(|| anyhow::anyhow!("move requires copy/move args"))?;

    if cm.to.is_empty() {
        if cm.paths.len() < 2 {
            bail!("missing destination — pass at least <SRC> <DEST> or use --to <DEST>");
        }
        return handle_move_one(args).await;
    }

    let mut failures = 0usize;
    for dest in &cm.to {
        if !is_json_mode() && !args.is_quiet() {
            eprintln!("→ moving to {}", dest.display());
        }
        if let Err(e) = handle_copy_one(args, Some(dest)).await {
            failures += 1;
            eprintln!("Error moving to {}: {}", dest.display(), e);
        }
    }
    if failures > 0 {
        bail!(
            "{} of {} fan-out targets failed; sources preserved",
            failures,
            cm.to.len()
        );
    }

    let recursive = args.is_recursive();
    let verbose = args.is_verbose() && !is_json_mode();
    for src in &cm.paths {
        let s = src.to_string_lossy();
        if let Some(remote_src) = parse_remote_path(&s) {
            remote_remove(&remote_src, recursive, false, false).await?;
            if verbose {
                println!("removed source {}", remote_src.display());
            }
        } else {
            let md = src.symlink_metadata()?;
            if md.is_dir() {
                tokio::fs::remove_dir_all(src).await?;
            } else {
                tokio::fs::remove_file(src).await?;
            }
            if verbose {
                println!("removed source '{}'", src.display());
            }
        }
    }
    Ok(())
}

async fn handle_move_one(args: &Commands) -> Result<()> {
    use crate::core::remote::parse_remote_path;

    let excludes = args.compile_excludes()?;
    let (sources, dest) = args.get_sources_and_dest().map_err(anyhow::Error::msg)?;

    if args.is_no_deref() {
        bail!(
            "--no-deref is not yet supported for bcmr move; use bcmr copy --no-deref \
             then bcmr remove on the originals, or scp -p / rsync."
        );
    }

    for src in sources {
        validate_source_kind(src)?;
    }

    let dest_str = dest.to_string_lossy();
    let remote_dest = parse_remote_path(&dest_str);
    let any_remote_source = sources
        .iter()
        .any(|s| parse_remote_path(&s.to_string_lossy()).is_some());

    if remote_dest.is_some() || any_remote_source {
        return handle_remote_move(args, sources, dest, &excludes).await;
    }

    if sources.len() > 1 && (!dest.exists() || !dest.is_dir()) {
        bail!(
            "When moving multiple sources, destination '{}' must be an existing directory",
            dest.display()
        );
    }

    let first_display = first_display_name(sources);
    let early = if !args.is_dry_run() {
        start_scanning_runner(args, "Moving", first_display.as_deref())?
    } else {
        None
    };

    let bail_early = |early: Option<ProgressRunner>, e: anyhow::Error| -> Result<()> {
        if let Some(r) = early {
            r.finish_with_error(&e.to_string());
        }
        Err(e)
    };

    if args.is_force() {
        let files_to_overwrite = match commands::r#move::check_overwrites(
            sources,
            dest,
            args.is_recursive(),
            args,
            &excludes,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => return bail_early(early, e.into()),
        };

        if !files_to_overwrite.is_empty()
            && args.should_prompt_for_overwrite()
            && !confirm_overwrite(&files_to_overwrite)?
        {
            return bail_early(early, BcmrError::Cancelled.into());
        }
    }

    let total_size =
        match commands::r#move::get_total_size(sources, args.is_recursive(), args, &excludes).await
        {
            Ok(v) => v,
            Err(e) => return bail_early(early, e.into()),
        };

    if args.is_dry_run() {
        if !is_json_mode() {
            println!("DRY RUN MODE: No changes will be made.\n");
        }

        for src in sources {
            commands::r#move::move_path(src, dest, args, &excludes, |_| {}, |_, _| {}, || {})
                .await?;
        }

        if !is_json_mode() {
            println!(
                "\nSummary: {} sources, {}",
                sources.len(),
                format_bytes(total_size as f64)
            );
        }
        return Ok(());
    }

    let runner = resume_or_new_runner(
        early,
        args,
        "Moving",
        first_display.as_deref(),
        total_size,
        args.is_quiet(),
    )?;

    for src in sources {
        let result = commands::r#move::move_path(
            src,
            dest,
            args,
            &excludes,
            runner.inc_callback(),
            runner.file_callback(),
            runner.reflink_callback(),
        )
        .await;

        if let Err(e) = result {
            if !is_json_mode() {
                eprintln!("Error moving '{}': {}", src.display(), e);
            }
            return runner.finish_err(format!("Error moving '{}': {}", src.display(), e));
        }
    }

    runner.finish_ok()
}

async fn handle_remote_move(
    args: &Commands,
    sources: &[std::path::PathBuf],
    dest: &std::path::Path,
    excludes: &[regex::Regex],
) -> Result<()> {
    use crate::core::remote::{parse_remote_path, remote_remove};
    use crate::ui::display::{print_dry_run, ActionType};

    handle_remote_copy(args, sources, dest, excludes).await?;

    if args.is_dry_run() {
        if !is_json_mode() {
            for src in sources {
                let label = src.to_string_lossy();
                print_dry_run(ActionType::Remove, &format!("source {}", label), None);
            }
        }
        return Ok(());
    }

    let recursive = args.is_recursive();
    let verbose = args.is_verbose() && !is_json_mode();

    for src in sources {
        let src_str = src.to_string_lossy();
        if let Some(remote_src) = parse_remote_path(&src_str) {
            remote_remove(&remote_src, recursive, false, false).await?;
            if verbose {
                println!("removed source {}", remote_src.display());
            }
        } else {
            let md = match src.symlink_metadata() {
                Ok(m) => m,
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "Source '{}' vanished after copy: {}",
                        src.display(),
                        e
                    ));
                }
            };
            if md.is_dir() {
                tokio::fs::remove_dir_all(src).await?;
            } else {
                tokio::fs::remove_file(src).await?;
            }
            if verbose {
                println!("removed source '{}'", src.display());
            }
        }
    }

    Ok(())
}

pub(crate) async fn handle_remove_command(args: &Commands) -> Result<()> {
    use crate::core::remote::{parse_remote_path, remote_remove, RemotePath};
    use crate::ui::display::{print_dry_run, ActionType};
    use std::path::PathBuf;

    let excludes = args.compile_excludes()?;
    let paths = args.get_remove_paths().map_err(anyhow::Error::msg)?;

    let mut local_paths: Vec<PathBuf> = Vec::new();
    let mut remote_paths: Vec<RemotePath> = Vec::new();
    for p in paths {
        match parse_remote_path(&p.to_string_lossy()) {
            Some(mut r) => {
                r.reject_unsafe()?;
                r.expand_tilde().await?;
                remote_paths.push(r);
            }
            None => local_paths.push(p.clone()),
        }
    }

    if !remote_paths.is_empty() {
        if args.is_dry_run() {
            if !is_json_mode() {
                println!("DRY RUN MODE: No changes will be made.\n");
            }
            for r in &remote_paths {
                print_dry_run(ActionType::Remove, &r.display(), None);
            }
        } else {
            if !args.is_force()
                && !args.is_yes()
                && !crate::app::prompts::confirm_remote_removal(&remote_paths)?
            {
                return Err(BcmrError::Cancelled.into());
            }
            let recursive = args.is_recursive();
            let force = args.is_force();
            let dir_only = args.is_dir_only();
            for r in &remote_paths {
                remote_remove(r, recursive, force, dir_only).await?;
                if args.is_verbose() && !is_json_mode() {
                    println!("removed {}", r.display());
                }
            }
            if !is_json_mode() {
                println!(
                    "Removed {} remote path{}",
                    remote_paths.len(),
                    if remote_paths.len() == 1 { "" } else { "s" }
                );
            }
        }

        if local_paths.is_empty() {
            return Ok(());
        }
    }

    let first_display = first_display_name(&local_paths);
    let early = start_scanning_runner(args, "Removing", first_display.as_deref())?;

    let files_to_remove =
        match commands::remove::check_removes(&local_paths, args.is_recursive(), args, &excludes)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                if let Some(r) = early {
                    r.finish_with_error(&e.to_string());
                }
                return Err(e.into());
            }
        };

    if args.is_dry_run() {
        if !is_json_mode() && remote_paths.is_empty() {
            println!("DRY RUN MODE: No changes will be made.\n");
        }

        let total_size: u64 = files_to_remove.iter().map(|f| f.size).sum();
        let file_count = files_to_remove.iter().filter(|f| !f.is_dir).count();
        let dir_count = files_to_remove.iter().filter(|f| f.is_dir).count();

        let runner = resume_or_new_runner(
            early,
            args,
            "Removing",
            first_display.as_deref(),
            total_size,
            true,
        )?;
        let result = commands::remove::remove_paths(
            &local_paths,
            args,
            &excludes,
            Arc::clone(runner.progress()),
            runner.inc_callback(),
            Box::new(runner.file_callback()),
            files_to_remove.len(),
        )
        .await;

        match result {
            Ok(()) => {
                runner.finish_ok()?;
            }
            Err(e) => {
                runner.finish_with_error(&e.to_string());
                return Err(e.into());
            }
        }

        if !is_json_mode() {
            print!("\nSummary: {} files", file_count);
            if dir_count > 0 {
                print!(", {} directories", dir_count);
            }
            println!(", {}", format_bytes(total_size as f64));
        }
        return Ok(());
    }

    if !files_to_remove.is_empty()
        && !args.is_force()
        && !args.is_yes()
        && (!args.is_interactive() || files_to_remove.len() > 1)
        && !confirm_removal(&files_to_remove)?
    {
        if let Some(r) = early {
            r.finish_with_error("cancelled by user");
        }
        return Err(BcmrError::Cancelled.into());
    }

    let total_size: u64 = files_to_remove.iter().map(|f| f.size).sum();
    let runner = resume_or_new_runner(
        early,
        args,
        "Removing",
        first_display.as_deref(),
        total_size,
        args.is_quiet(),
    )?;

    let result = commands::remove::remove_paths(
        &local_paths,
        args,
        &excludes,
        Arc::clone(runner.progress()),
        runner.inc_callback(),
        Box::new(runner.file_callback()),
        files_to_remove.len(),
    )
    .await;

    match result {
        Ok(()) => runner.finish_ok(),
        Err(e) => {
            runner.finish_with_error(&e.to_string());
            Err(e.into())
        }
    }
}

pub(crate) async fn handle_check_command(args: &Commands) -> Result<output::CheckResult> {
    let excludes = args.compile_excludes()?;
    let (sources, dest) = args.get_sources_and_dest().map_err(anyhow::Error::msg)?;
    let no_hash = matches!(args, Commands::Check { no_hash: true, .. });
    Ok(commands::check::run(sources, dest, args.is_recursive(), &excludes, no_hash).await?)
}

pub(crate) fn handle_init_command(args: &Commands) -> Result<()> {
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
