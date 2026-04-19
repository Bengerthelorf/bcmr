use crate::commands;
use crate::config::is_json_mode;
use crate::ui::utils::format_bytes;
use anyhow::Result;
use std::io::{self, Write};

pub(crate) fn prompt_yes_no(message: &str) -> Result<bool> {
    if is_json_mode() {
        return Ok(true);
    }
    print!("{} [y/N] ", message);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().eq_ignore_ascii_case("y"))
}

pub(crate) fn confirm_overwrite(files: &[commands::copy::FileToOverwrite]) -> Result<bool> {
    if is_json_mode() {
        return Ok(true);
    }
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

pub(crate) fn confirm_removal(files: &[commands::remove::FileToRemove]) -> Result<bool> {
    if is_json_mode() {
        return Ok(true);
    }
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

pub(crate) fn first_display_name(paths: &[std::path::PathBuf]) -> Option<String> {
    paths.first().map(|p| {
        p.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    })
}
