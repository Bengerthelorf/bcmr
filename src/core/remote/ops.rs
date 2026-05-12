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
    let escaped = shell_escape(&remote.path);
    let output = ssh_command(&remote.ssh_target())
        .arg(format!(
            "LC_ALL=C stat -c '%F %s' '{0}' 2>/dev/null || LC_ALL=C stat -f '%HT %z' '{0}'",
            escaped
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

    let escaped = shell_escape(&remote.path);
    let output = ssh_command(&remote.ssh_target())
        .arg(format!("LC_ALL=C du -sk '{}' 2>/dev/null", escaped))
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr,
            &format!("Cannot compute size of remote path '{}'", remote),
        )));
    }

    let kbs: u64 = String::from_utf8_lossy(&output.stdout)
        .split_ascii_whitespace()
        .next()
        .and_then(|t| t.parse::<u64>().ok())
        .unwrap_or(0);

    Ok(kbs * 1024)
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

pub async fn remote_list_shallow(
    remote: &RemotePath,
) -> Result<Vec<(String, u64, bool)>, BcmrError> {
    let escaped = shell_escape(&remote.path);
    let cmd = format!(
        "find '{}' -maxdepth 1 -mindepth 1 -printf '%f\\0%s\\0%y\\0' 2>/dev/null; true",
        escaped
    );
    let output = ssh_command(&remote.ssh_target()).arg(cmd).output().await?;
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
    while i + 3 <= fields.len() {
        let name = fields[i].to_string();
        let size: u64 = fields[i + 1].parse().unwrap_or(0);
        let is_dir = fields[i + 2] == "d";
        i += 3;
        if name.is_empty() {
            continue;
        }
        entries.push((name, size, is_dir));
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
    let stderr_pipe = child.stderr.take();

    // Drain stderr in parallel — > ~64 KB of diagnostics blocks the remote
    // writer and deadlocks `child.wait()` if we only read after the loop.
    let stderr_task: tokio::task::JoinHandle<String> = tokio::spawn(async move {
        let mut buf = String::new();
        if let Some(mut pipe) = stderr_pipe {
            let _ = pipe.read_to_string(&mut buf).await;
        }
        buf
    });

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
    let stderr_buf = stderr_task.await.unwrap_or_default();
    if !status.success() {
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr_buf,
            &format!("Failed to hash remote file '{}'", remote),
        )));
    }

    Ok(hasher.finalize().to_hex().to_string())
}

fn split_tilde_prefix(path: &str) -> Option<(&str, &str)> {
    let after_tilde = path.strip_prefix('~')?;
    let (user_spec, suffix) = match after_tilde.find('/') {
        Some(idx) => (&after_tilde[..idx], &after_tilde[idx..]),
        None => (after_tilde, ""),
    };
    Some((user_spec, suffix))
}

pub async fn resolve_remote_home(ssh_target: &str, user_spec: &str) -> Result<String, BcmrError> {
    let probe = if user_spec.is_empty() {
        "printf '%s' \"$HOME\"".to_string()
    } else {
        if !user_spec
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(BcmrError::InvalidInput(format!(
                "remote path: refusing to expand ~{} — only alphanumeric/_/- usernames accepted",
                user_spec
            )));
        }
        format!("printf '%s' ~{}", user_spec)
    };

    let output = ssh_command(ssh_target).arg(&probe).output().await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr,
            &format!("remote ~{} expansion", user_spec),
        )));
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let home = raw.trim().to_string();
    if home.is_empty() || home.starts_with('~') {
        return Err(BcmrError::InvalidInput(format!(
            "remote did not expand ~{} (no such user, or HOME unset)",
            user_spec
        )));
    }
    Ok(home)
}

pub async fn expand_remote_tilde(ssh_target: &str, path: &str) -> Result<String, BcmrError> {
    let Some((user_spec, suffix)) = split_tilde_prefix(path) else {
        return Ok(path.to_string());
    };
    let home = resolve_remote_home(ssh_target, user_spec).await?;
    Ok(format!("{}{}", home, suffix))
}

pub async fn remote_path_is_directory(remote: &RemotePath) -> bool {
    let cmd = format!("test -d '{}'", shell_escape(&remote.path));
    let status = ssh_command(&remote.ssh_target())
        .arg(&cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
    matches!(status, Ok(s) if s.success())
}

pub async fn remote_remove(
    remote: &RemotePath,
    recursive: bool,
    force: bool,
    dir_only: bool,
) -> Result<(), BcmrError> {
    let target = remote.ssh_target();
    let escaped = shell_escape(&remote.path);
    let cmd = if dir_only {
        format!("rmdir -- '{}'", escaped)
    } else {
        let flags = match (recursive, force) {
            (true, true) => " -rf",
            (true, false) => " -r",
            (false, true) => " -f",
            (false, false) => "",
        };
        format!("rm{} -- '{}'", flags, escaped)
    };

    let output = ssh_command(&target).arg(&cmd).output().await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr,
            &format!("Cannot remove remote path '{}'", remote),
        )));
    }
    Ok(())
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

#[cfg(test)]
mod tilde_tests {
    use super::split_tilde_prefix;

    #[test]
    fn split_handles_bare_tilde() {
        assert_eq!(split_tilde_prefix("~"), Some(("", "")));
        assert_eq!(split_tilde_prefix("~/"), Some(("", "/")));
        assert_eq!(split_tilde_prefix("~/foo"), Some(("", "/foo")));
        assert_eq!(split_tilde_prefix("~/foo/bar"), Some(("", "/foo/bar")));
    }

    #[test]
    fn split_handles_user_tilde() {
        assert_eq!(split_tilde_prefix("~alice"), Some(("alice", "")));
        assert_eq!(split_tilde_prefix("~alice/foo"), Some(("alice", "/foo")));
    }

    #[test]
    fn split_returns_none_for_non_tilde() {
        assert_eq!(split_tilde_prefix(""), None);
        assert_eq!(split_tilde_prefix("foo"), None);
        assert_eq!(split_tilde_prefix("/abs/path"), None);
        assert_eq!(split_tilde_prefix("./foo~"), None);
    }
}
