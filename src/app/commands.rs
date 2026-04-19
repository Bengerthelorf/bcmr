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

pub(crate) async fn handle_copy_command(args: &Commands) -> Result<()> {
    use crate::core::remote::parse_remote_path;

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
        let first_display = first_display_name(sources);
        let early = if !args.is_dry_run() {
            start_scanning_runner(args, "Copying", first_display.as_deref())?
        } else {
            None
        };

        let plan =
            match commands::copy::plan_copy(sources, dest, args.is_recursive(), &excludes).await {
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
            false,
        )?;

        let result = commands::copy::execute_plan(
            &plan,
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
        let runner = ProgressRunner::new(
            0,
            is_plain_mode(args),
            false,
            is_json_mode(),
            commands::copy::cleanup_partial_files,
        )?;

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
            args,
            &excludes,
            commands::copy::PipelineCallbacks {
                on_progress: runner.inc_callback(),
                on_new_file: Box::new(runner.file_callback()),
                on_total_update: Box::new(total_cb),
                on_scan_complete: Box::new(scan_done_cb),
                on_file_found: Box::new(files_found_cb),
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
    let excludes = args.compile_excludes()?;
    let (sources, dest) = args.get_sources_and_dest().map_err(anyhow::Error::msg)?;

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
            commands::r#move::move_path(src, dest, args, &excludes, |_| {}, |_, _| {}).await?;
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
        false,
    )?;

    for src in sources {
        let result = commands::r#move::move_path(
            src,
            dest,
            args,
            &excludes,
            runner.inc_callback(),
            runner.file_callback(),
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

pub(crate) async fn handle_remove_command(args: &Commands) -> Result<()> {
    let excludes = args.compile_excludes()?;
    let paths = args.get_remove_paths().map_err(anyhow::Error::msg)?;

    let first_display = first_display_name(paths);
    let early = start_scanning_runner(args, "Removing", first_display.as_deref())?;

    let files_to_remove =
        match commands::remove::check_removes(paths, args.is_recursive(), args, &excludes).await {
            Ok(v) => v,
            Err(e) => {
                if let Some(r) = early {
                    r.finish_with_error(&e.to_string());
                }
                return Err(e.into());
            }
        };

    if args.is_dry_run() {
        if !is_json_mode() {
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
            paths,
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
        false,
    )?;

    let result = commands::remove::remove_paths(
        paths,
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
    Ok(commands::check::run(sources, dest, args.is_recursive(), &excludes).await?)
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
