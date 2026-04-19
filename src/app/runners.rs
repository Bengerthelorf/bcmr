use crate::cli::Commands;
use crate::commands;
use crate::commands::remote_copy::is_plain_mode;
use crate::config::is_json_mode;
use crate::ui::runner::ProgressRunner;
use anyhow::Result;

pub(crate) fn start_scanning_runner(
    args: &Commands,
    operation: &str,
    first_display: Option<&str>,
) -> Result<Option<ProgressRunner>> {
    if !is_json_mode() {
        return Ok(None);
    }
    let runner = ProgressRunner::new(
        0,
        is_plain_mode(args),
        false,
        true,
        commands::copy::cleanup_partial_files,
    )?;
    {
        let mut p = runner.progress().lock();
        p.set_operation_type(operation);
        p.set_scanning(true);
        if let Some(name) = first_display {
            p.set_current_file(name, 0);
        }
    }
    Ok(Some(runner))
}

pub(crate) fn resume_or_new_runner(
    early: Option<ProgressRunner>,
    args: &Commands,
    operation: &str,
    first_display: Option<&str>,
    total_size: u64,
    silent: bool,
) -> Result<ProgressRunner> {
    if let Some(r) = early {
        {
            let mut p = r.progress().lock();
            p.set_total_bytes(total_size);
            p.set_scanning(false);
            if let Some(name) = first_display {
                p.set_current_file(name, total_size);
            }
        }
        return Ok(r);
    }
    let r = ProgressRunner::new(
        total_size,
        is_plain_mode(args),
        silent,
        is_json_mode(),
        commands::copy::cleanup_partial_files,
    )?;
    {
        let mut p = r.progress().lock();
        p.set_operation_type(operation);
        if let Some(name) = first_display {
            p.set_current_file(name, total_size);
        }
    }
    Ok(r)
}
