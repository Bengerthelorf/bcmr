use crate::config::is_json_mode;
use crate::core::error::BcmrError;
use crate::core::remote::{
    parse_remote_path, remote_file_hash, remote_list_shallow, remote_stat, remote_total_size,
    RemotePath,
};
use crate::ui::utils::format_bytes;
use anyhow::{anyhow, Result};
use serde_json::json;
use std::path::Path;

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
    let info = remote_stat(&rp).await.map_err(into_anyhow)?;
    if !info.is_dir {
        if is_json_mode() {
            println!(
                "{}",
                json!({
                    "path": rp.display(),
                    "entries": [{"type": "file", "size": info.size, "name": rp.file_name()}],
                })
            );
        } else {
            println!("- {}  {}", format_bytes(info.size as f64), rp.file_name());
        }
        return Ok(());
    }
    let mut entries = remote_list_shallow(&rp).await.map_err(into_anyhow)?;
    entries.sort();
    if is_json_mode() {
        let entries_json: Vec<_> = entries
            .iter()
            .map(|(name, size, is_dir)| {
                json!({
                    "type": if *is_dir { "directory" } else { "file" },
                    "size": size,
                    "name": name,
                })
            })
            .collect();
        println!("{}", json!({"path": rp.display(), "entries": entries_json}));
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

pub async fn stat(path: &Path) -> Result<()> {
    let rp = resolved(path).await?;
    let info = remote_stat(&rp).await.map_err(into_anyhow)?;
    let kind = if info.is_dir { "directory" } else { "file" };
    if is_json_mode() {
        println!(
            "{}",
            json!({"path": rp.display(), "type": kind, "size": info.size})
        );
        return Ok(());
    }
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
    if is_json_mode() {
        println!(
            "{}",
            json!({
                "path": rp.display(),
                "bytes": total,
                "human": format_bytes(total as f64),
            })
        );
    } else {
        println!("{}\t{}", format_bytes(total as f64), rp.display());
    }
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
    if is_json_mode() {
        println!(
            "{}",
            json!({"path": rp.display(), "hash": hex, "algo": "blake3"})
        );
    } else {
        println!("{}  {}", hex, rp.display());
    }
    Ok(())
}

fn into_anyhow(e: BcmrError) -> anyhow::Error {
    anyhow::anyhow!("{}", e)
}
