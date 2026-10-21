use super::{
    collect_download_plan, collect_upload_plan, preserve_directories, progress_mode,
    run_parallel_transfers, transfer_options_from_cli,
};
use crate::cli::Commands;
use crate::core::remote::{self, RemotePath};
use crate::ui::runner::ProgressRunner;
use crate::ui::utils::format_bytes;
use anyhow::{bail, Result};

pub(super) async fn handle_remote_upload(
    args: &Commands,
    sources: &[std::path::PathBuf],
    rdest: &RemotePath,
    parallel: usize,
    excludes: &[regex::Regex],
) -> Result<()> {
    let plan = collect_upload_plan(sources, rdest, args.is_recursive(), excludes)?;
    crate::core::interrupt::check()?;
    if args.is_dry_run() {
        println!(
            "Dry-run: would upload {} to {}",
            format_bytes(plan.total_size as f64),
            rdest
        );
        for directory in &plan.directories {
            println!(
                "  {} -> {} (DIR)",
                directory.local_path.display(),
                directory.remote
            );
        }
        for item in &plan.files {
            println!("  {} -> {}", item.local_path.display(), item.remote);
        }
        return Ok(());
    }
    if plan.files.is_empty() && plan.directories.is_empty() {
        return Ok(());
    }
    if args.is_sync() {
        bail!("--sync cannot guarantee durable remote namespace publication through legacy SSH; refusing before transfer");
    }

    let opts = transfer_options_from_cli(args);
    crate::core::interrupt::check()?;
    let runner = ProgressRunner::new(
        plan.total_size,
        progress_mode(args),
        args.is_quiet(),
        crate::config::is_json_mode(),
        crate::core::cleanup::cleanup_partial_files,
    )?;
    runner.set_operation_type("Uploading");
    runner.set_verify_mode(args.is_verify());

    let directories: Vec<_> = plan.directories.iter().map(|d| d.remote.clone()).collect();
    remote::ensure_remote_directories(&directories).await?;
    if parallel > 1 {
        runner.set_parallel_mode(parallel);
        run_parallel_transfers(plan.files, parallel, runner.handle(), &opts).await?;
    } else {
        for item in &plan.files {
            remote::upload_file(
                &item.local_path,
                &item.remote,
                remote::TransferCallbacks {
                    on_progress: &runner.inc_callback(),
                    on_skip: &runner.skip_callback(),
                    on_new_file: &runner.file_callback(),
                },
                &opts,
            )
            .await?;
        }
    }
    if args.is_preserve() {
        preserve_directories(&plan.directories, true, args.is_sync()).await?;
    }
    runner.finish_ok()
}

pub(super) async fn handle_remote_download(
    args: &Commands,
    sources: &[std::path::PathBuf],
    dest_local: &std::path::Path,
    parallel: usize,
    excludes: &[regex::Regex],
) -> Result<()> {
    let plan = collect_download_plan(sources, dest_local, args.is_recursive(), excludes).await?;
    crate::core::interrupt::check()?;
    if args.is_dry_run() {
        println!(
            "Dry-run: would download {} to {}",
            format_bytes(plan.total_size as f64),
            dest_local.display()
        );
        for directory in &plan.directories {
            println!(
                "  {} -> {} (DIR)",
                directory.remote,
                directory.local_path.display()
            );
        }
        for item in &plan.files {
            println!("  {} -> {}", item.remote, item.local_path.display());
        }
        return Ok(());
    }
    if plan.files.is_empty() && plan.directories.is_empty() {
        return Ok(());
    }
    #[cfg(not(unix))]
    if args.is_sync() {
        bail!("--sync cannot guarantee durable local namespace publication on this platform; refusing before transfer");
    }

    let opts = transfer_options_from_cli(args);
    crate::core::interrupt::check()?;
    let runner = ProgressRunner::new(
        plan.total_size,
        progress_mode(args),
        args.is_quiet(),
        crate::config::is_json_mode(),
        crate::core::cleanup::cleanup_partial_files,
    )?;
    runner.set_operation_type("Downloading");
    runner.set_verify_mode(args.is_verify());

    for directory in &plan.directories {
        if args.is_sync() {
            crate::core::io::create_dir_all_durable_async(&directory.local_path).await?;
        } else {
            tokio::fs::create_dir_all(&directory.local_path).await?;
        }
    }
    if parallel > 1 {
        runner.set_parallel_mode(parallel);
        run_parallel_transfers(plan.files, parallel, runner.handle(), &opts).await?;
    } else {
        for item in &plan.files {
            remote::download_file(
                &item.remote,
                &item.local_path,
                remote::TransferCallbacks {
                    on_progress: &runner.inc_callback(),
                    on_skip: &runner.skip_callback(),
                    on_new_file: &runner.file_callback(),
                },
                item.size,
                &opts,
            )
            .await?;
        }
    }
    if args.is_preserve() {
        preserve_directories(&plan.directories, false, args.is_sync()).await?;
    }
    runner.finish_ok()
}
