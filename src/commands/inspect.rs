use crate::core::error::BcmrError;
use crate::core::remote::{
    parse_remote_path, remote_file_hash, remote_stat, remote_total_size, RemotePath,
};
use crate::ui::utils::format_bytes;
use anyhow::{anyhow, Result};
use std::path::Path;
use tokio::process::Command;

fn require_remote(path: &Path) -> Result<RemotePath> {
    let s = path.to_string_lossy();
    let rp = parse_remote_path(&s)
        .ok_or_else(|| anyhow!("'{}' is not a remote path; use host:path or @bookmark", s))?;
    rp.reject_unsafe()?;
    Ok(rp)
}

async fn resolved(path: &Path) -> Result<RemotePath> {
    let mut rp = require_remote(path)?;
    rp.expand_tilde().await?;
    Ok(rp)
}

pub async fn ls(path: &Path) -> Result<()> {
    let rp = resolved(path).await?;
    let entries = list_shallow(&rp).await?;
    if entries.is_empty() {
        return Ok(());
    }
    let max_size_width = entries
        .iter()
        .map(|(_, size, is_dir)| {
            if *is_dir {
                1
            } else {
                format_bytes(*size as f64).len()
            }
        })
        .max()
        .unwrap_or(0);
    for (name, size, is_dir) in &entries {
        let kind = if *is_dir { "d" } else { "-" };
        let size_str = if *is_dir {
            "-".to_string()
        } else {
            format_bytes(*size as f64)
        };
        println!("{} {:>w$}  {}", kind, size_str, name, w = max_size_width);
    }
    Ok(())
}

async fn list_shallow(rp: &RemotePath) -> Result<Vec<(String, u64, bool)>> {
    // Shallow listing: -maxdepth 1 -mindepth 1, suppress perm errors and
    // ignore find's nonzero exit that triggers when any child is unreadable.
    let escaped = rp.path.replace('\'', "'\\''");
    let cmd = format!(
        "find '{}' -maxdepth 1 -mindepth 1 -printf '%f\\0%s\\0%y\\0' 2>/dev/null; true",
        escaped
    );
    let output = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            &rp.ssh_target(),
            &cmd,
        ])
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "ssh ls failed for '{}': {}",
            rp.display(),
            stderr.trim()
        ));
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let fields: Vec<&str> = raw.split('\0').collect();
    let mut entries = Vec::new();
    let mut i = 0;
    while i + 2 < fields.len() {
        let name = fields[i].to_string();
        let size: u64 = fields[i + 1].parse().unwrap_or(0);
        let is_dir = fields[i + 2] == "d";
        i += 3;
        if name.is_empty() {
            continue;
        }
        entries.push((name, size, is_dir));
    }
    entries.sort();
    Ok(entries)
}

pub async fn stat(path: &Path) -> Result<()> {
    let rp = resolved(path).await?;
    let info = remote_stat(&rp).await.map_err(into_anyhow)?;
    let kind = if info.is_dir { "directory" } else { "file" };
    if info.is_dir {
        println!("{}: {}", rp.display(), kind);
    } else {
        println!(
            "{}: {} ({} bytes, {})",
            rp.display(),
            kind,
            info.size,
            format_bytes(info.size as f64)
        );
    }
    Ok(())
}

pub async fn du(path: &Path) -> Result<()> {
    let rp = resolved(path).await?;
    let total = remote_total_size(&rp, true).await.map_err(into_anyhow)?;
    println!("{}\t{}", format_bytes(total as f64), rp.display());
    Ok(())
}

pub async fn hash(path: &Path) -> Result<()> {
    let rp = resolved(path).await?;
    let info = remote_stat(&rp).await.map_err(into_anyhow)?;
    if info.is_dir {
        return Err(anyhow!(
            "'{}' is a directory; bcmr hash takes a file",
            rp.display()
        ));
    }
    let hex = remote_file_hash(&rp, None).await.map_err(into_anyhow)?;
    println!("{}  {}", hex, rp.display());
    Ok(())
}

fn into_anyhow(e: BcmrError) -> anyhow::Error {
    anyhow::anyhow!("{}", e)
}
