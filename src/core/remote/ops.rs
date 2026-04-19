use super::ssh_cmd::{shell_escape, ssh_command, ssh_error_message};
use super::{parse_remote_path, RemoteFileInfo, RemotePath};
use crate::core::error::BcmrError;
use tokio::io::AsyncReadExt;

pub async fn validate_ssh_connection(remote: &RemotePath) -> Result<(), BcmrError> {
    let target = remote.ssh_target();
    let output = ssh_command(&target).arg("echo ok").output().await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr,
            &format!("Cannot connect to '{}'", target),
        )));
    }
    Ok(())
}

pub async fn remote_file_size(remote: &RemotePath) -> Result<Option<u64>, BcmrError> {
    let output = ssh_command(&remote.ssh_target())
        .arg(format!(
            "stat -c '%s' '{}' 2>/dev/null || stat -f '%z' '{}'",
            shell_escape(&remote.path),
            shell_escape(&remote.path)
        ))
        .output()
        .await?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.trim().parse::<u64>().ok())
}

pub async fn remote_stat(remote: &RemotePath) -> Result<RemoteFileInfo, BcmrError> {
    let output = ssh_command(&remote.ssh_target())
        .arg(format!(
            "stat -c '%F %s' '{}' 2>/dev/null || stat -f '%HT %z' '{}'",
            shell_escape(&remote.path),
            shell_escape(&remote.path)
        ))
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr,
            &format!("Cannot stat remote path '{}'", remote),
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.trim();

    let is_dir = line.to_lowercase().starts_with("directory");
    let size: u64 = line
        .rsplit_once(' ')
        .and_then(|(_, s)| s.parse().ok())
        .unwrap_or(0);

    Ok(RemoteFileInfo { is_dir, size })
}

pub async fn remote_total_size(remote: &RemotePath, recursive: bool) -> Result<u64, BcmrError> {
    let info = remote_stat(remote).await?;

    if !info.is_dir {
        return Ok(info.size);
    }

    if !recursive {
        return Err(BcmrError::InvalidInput(format!(
            "Remote source '{}' is a directory. Use -r flag for recursive copy.",
            remote
        )));
    }

    let output = ssh_command(&remote.ssh_target())
        .arg(format!(
            "find '{}' -type f -exec stat -c '%s' {{}} + 2>/dev/null || find '{}' -type f -exec stat -f '%z' {{}} +",
            shell_escape(&remote.path), shell_escape(&remote.path)
        ))
        .output()
        .await?;

    if !output.status.success() {
        return Ok(0);
    }

    let total: u64 = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| l.trim().parse::<u64>().ok())
        .sum();

    Ok(total)
}

pub async fn remote_list_files(remote: &RemotePath) -> Result<Vec<(String, u64, bool)>, BcmrError> {
    let output = ssh_command(&remote.ssh_target())
        .arg(format!(
            "find '{}' -printf '%P\\0%s\\0%y\\0' 2>/dev/null || find '{}' ! -path '{}' -exec stat -f '%N\\0%z\\0%HT\\0' {{}} +",
            shell_escape(&remote.path), shell_escape(&remote.path), shell_escape(&remote.path)
        ))
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr,
            &format!("Cannot list remote directory '{}'", remote),
        )));
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let fields: Vec<&str> = raw.split('\0').collect();
    let mut entries = Vec::new();

    let mut i = 0;
    while i + 2 < fields.len() {
        let rel_path = fields[i].to_string();
        let size: u64 = fields[i + 1].parse().unwrap_or(0);
        let is_dir = fields[i + 2] == "d" || fields[i + 2].to_lowercase().contains("directory");
        i += 3;

        if rel_path.is_empty() {
            continue;
        }
        entries.push((rel_path, size, is_dir));
    }

    Ok(entries)
}

pub async fn remote_file_hash(
    remote: &RemotePath,
    limit: Option<u64>,
) -> Result<String, BcmrError> {
    let cmd = match limit {
        Some(n) => format!("head -c {} '{}'", n, shell_escape(&remote.path)),
        None => format!("cat '{}'", shell_escape(&remote.path)),
    };

    let mut child = ssh_command(&remote.ssh_target())
        .arg(cmd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| BcmrError::InvalidInput("Failed to capture SSH stdout".to_string()))?;

    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 4 * 1024 * 1024];
    loop {
        let n = stdout.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    let status = child.wait().await?;
    if !status.success() {
        return Err(BcmrError::InvalidInput(format!(
            "Failed to hash remote file '{}'",
            remote
        )));
    }

    Ok(hasher.finalize().to_hex().to_string())
}

pub async fn complete_remote_path(partial: &str) -> Vec<String> {
    let remote = match parse_remote_path(partial) {
        Some(r) => r,
        None => return Vec::new(),
    };

    let (dir, prefix) = if remote.path.ends_with('/') || remote.path == "." {
        (remote.path.clone(), String::new())
    } else if let Some(pos) = remote.path.rfind('/') {
        (
            remote.path[..=pos].to_string(),
            remote.path[pos + 1..].to_string(),
        )
    } else {
        (".".to_string(), remote.path.clone())
    };

    let target = remote.ssh_target();
    let output = match ssh_command(&target)
        .arg(format!("ls -1ap '{}' 2>/dev/null", shell_escape(&dir)))
        .output()
        .await
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let base = if dir == "." { String::new() } else { dir };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| *l != "./" && *l != "../")
        .filter(|l| prefix.is_empty() || l.starts_with(&prefix))
        .map(|l| format!("{}:{}{}", target, base, l))
        .collect()
}
