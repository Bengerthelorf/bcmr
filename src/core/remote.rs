use crate::core::error::BcmrError;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// Parsed remote path: `[user@]host:path`
#[derive(Debug, Clone)]
pub struct RemotePath {
    pub user: Option<String>,
    pub host: String,
    pub path: String,
}

impl RemotePath {
    /// Format as `[user@]host` for SSH command target.
    pub fn ssh_target(&self) -> String {
        match &self.user {
            Some(user) => format!("{}@{}", user, self.host),
            None => self.host.clone(),
        }
    }

    /// Format as display string `[user@]host:path`.
    pub fn display(&self) -> String {
        format!("{}:{}", self.ssh_target(), self.path)
    }
}

impl std::fmt::Display for RemotePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display())
    }
}

/// Parse a path string as a remote path (`[user@]host:path`).
/// Returns `None` if it's a local path.
///
/// Avoids false positives with:
/// - Windows drive letters (e.g., `C:\...`)
/// - Paths starting with `/`, `./`, `..`, or `~`
pub fn parse_remote_path(s: &str) -> Option<RemotePath> {
    // Local path indicators
    if s.starts_with('/')
        || s.starts_with("./")
        || s.starts_with("../")
        || s.starts_with('~')
        || s == "."
        || s == ".."
    {
        return None;
    }

    // Windows drive letter: single letter followed by `:`
    if s.len() >= 2 && s.as_bytes()[0].is_ascii_alphabetic() && s.as_bytes()[1] == b':' {
        // Only skip if the part before `:` is exactly one character (drive letter)
        let colon_pos = s.find(':')?;
        if colon_pos == 1 {
            return None;
        }
    }

    let colon_pos = s.find(':')?;
    if colon_pos == 0 {
        return None;
    }

    let host_part = &s[..colon_pos];
    let path_part = &s[colon_pos + 1..];

    // host_part must look like a hostname or user@hostname
    // It should not contain `/` or spaces
    if host_part.contains('/') || host_part.contains(' ') {
        return None;
    }

    let (user, host) = if let Some(at_pos) = host_part.find('@') {
        let user = &host_part[..at_pos];
        let host = &host_part[at_pos + 1..];
        if user.is_empty() || host.is_empty() {
            return None;
        }
        (Some(user.to_string()), host.to_string())
    } else {
        (None, host_part.to_string())
    };

    // Default remote path to home directory if empty
    let path = if path_part.is_empty() {
        ".".to_string()
    } else {
        path_part.to_string()
    };

    Some(RemotePath { user, host, path })
}

/// Represents file info from the remote side.
#[derive(Debug)]
pub struct RemoteFileInfo {
    pub is_dir: bool,
    pub size: u64,
}

/// Query remote file info via SSH + stat.
pub async fn remote_stat(remote: &RemotePath) -> Result<RemoteFileInfo, BcmrError> {
    let output = Command::new("ssh")
        .arg("-o").arg("BatchMode=yes")
        .arg(remote.ssh_target())
        .arg(format!(
            "stat -c '%F %s' '{}' 2>/dev/null || stat -f '%HT %z' '{}'",
            remote.path, remote.path
        ))
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BcmrError::InvalidInput(format!(
            "Cannot stat remote path '{}': {}",
            remote, stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.trim();

    // Linux stat -c '%F %s': "regular file 12345" or "directory 4096"
    // macOS stat -f '%HT %z': "Regular File 12345" or "Directory 4096"
    let is_dir = line.to_lowercase().starts_with("directory");
    let size: u64 = line
        .rsplit_once(' ')
        .and_then(|(_, s)| s.parse().ok())
        .unwrap_or(0);

    Ok(RemoteFileInfo { is_dir, size })
}

/// Get total size of remote path (file or directory recursively).
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

    // Use find + stat to sum file sizes
    let output = Command::new("ssh")
        .arg("-o").arg("BatchMode=yes")
        .arg(remote.ssh_target())
        .arg(format!(
            "find '{}' -type f -exec stat -c '%s' {{}} + 2>/dev/null || find '{}' -type f -exec stat -f '%z' {{}} +",
            remote.path, remote.path
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

/// List files in a remote directory (returns relative paths with sizes).
pub async fn remote_list_files(
    remote: &RemotePath,
) -> Result<Vec<(String, u64, bool)>, BcmrError> {
    // Returns (relative_path, size, is_dir) tuples
    let output = Command::new("ssh")
        .arg("-o").arg("BatchMode=yes")
        .arg(remote.ssh_target())
        .arg(format!(
            "find '{}' -printf '%P\\t%s\\t%y\\n' 2>/dev/null || find '{}' -exec stat -f '%N\\t%z\\t%HT' {{}} +",
            remote.path, remote.path
        ))
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BcmrError::InvalidInput(format!(
            "Cannot list remote directory '{}': {}",
            remote, stderr.trim()
        )));
    }

    let mut entries = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let rel_path = parts[0].to_string();
        if rel_path.is_empty() {
            continue; // skip root dir itself
        }
        let size: u64 = parts[1].parse().unwrap_or(0);
        let is_dir = parts[2] == "d" || parts[2].to_lowercase().contains("directory");
        entries.push((rel_path, size, is_dir));
    }

    Ok(entries)
}

/// Download a single file from remote to local with progress callback.
pub async fn download_file(
    remote: &RemotePath,
    local_dst: &Path,
    progress_callback: &impl Fn(u64),
    on_new_file: &impl Fn(&str, u64),
    file_size: u64,
) -> Result<(), BcmrError> {
    let file_name = remote
        .path
        .rsplit('/')
        .next()
        .unwrap_or(&remote.path);
    on_new_file(file_name, file_size);

    // Create parent directory if needed
    if let Some(parent) = local_dst.parent() {
        if !parent.exists() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }

    let mut child = Command::new("ssh")
        .arg("-o").arg("BatchMode=yes")
        .arg(remote.ssh_target())
        .arg(format!("cat '{}'", remote.path))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut stdout = child.stdout.take().ok_or_else(|| {
        BcmrError::InvalidInput("Failed to capture SSH stdout".to_string())
    })?;

    let mut dst_file = tokio::fs::File::create(local_dst).await?;
    let mut buffer = vec![0u8; 1024 * 1024];

    loop {
        let n = stdout.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        dst_file.write_all(&buffer[..n]).await?;
        progress_callback(n as u64);
    }

    let status = child.wait().await?;
    if !status.success() {
        return Err(BcmrError::InvalidInput(format!(
            "SSH download failed for '{}'",
            remote
        )));
    }

    Ok(())
}

/// Upload a single file from local to remote with progress callback.
pub async fn upload_file(
    local_src: &Path,
    remote: &RemotePath,
    progress_callback: &impl Fn(u64),
    on_new_file: &impl Fn(&str, u64),
) -> Result<(), BcmrError> {
    let file_size = local_src.metadata()?.len();
    let file_name = local_src
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    on_new_file(&file_name, file_size);

    let mut child = Command::new("ssh")
        .arg("-o").arg("BatchMode=yes")
        .arg(remote.ssh_target())
        .arg(format!("cat > '{}'", remote.path))
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut stdin = child.stdin.take().ok_or_else(|| {
        BcmrError::InvalidInput("Failed to capture SSH stdin".to_string())
    })?;

    let mut src_file = tokio::fs::File::open(local_src).await?;
    let mut buffer = vec![0u8; 1024 * 1024];

    loop {
        let n = src_file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        stdin.write_all(&buffer[..n]).await?;
        progress_callback(n as u64);
    }

    drop(stdin); // Close stdin to signal EOF
    let status = child.wait().await?;
    if !status.success() {
        return Err(BcmrError::InvalidInput(format!(
            "SSH upload failed for '{}' -> {}",
            local_src.display(),
            remote
        )));
    }

    Ok(())
}

/// Download a directory recursively from remote.
pub async fn download_directory(
    remote: &RemotePath,
    local_dst: &Path,
    progress_callback: &impl Fn(u64),
    on_new_file: &impl Fn(&str, u64),
) -> Result<(), BcmrError> {
    // List all entries
    let entries = remote_list_files(remote).await?;

    // Create directories first
    for (rel_path, _, is_dir) in &entries {
        if *is_dir {
            let dir_path = local_dst.join(rel_path);
            if !dir_path.exists() {
                tokio::fs::create_dir_all(&dir_path).await?;
            }
        }
    }

    // Download files
    for (rel_path, size, is_dir) in &entries {
        if *is_dir {
            continue;
        }
        let file_remote = RemotePath {
            user: remote.user.clone(),
            host: remote.host.clone(),
            path: format!("{}/{}", remote.path, rel_path),
        };
        let local_file = local_dst.join(rel_path);
        download_file(&file_remote, &local_file, progress_callback, on_new_file, *size).await?;
    }

    Ok(())
}

/// Upload a directory recursively to remote.
pub async fn upload_directory(
    local_src: &Path,
    remote: &RemotePath,
    progress_callback: &impl Fn(u64),
    on_new_file: &impl Fn(&str, u64),
) -> Result<(), BcmrError> {
    use crate::core::traversal;

    // Ensure remote base directory exists
    let output = Command::new("ssh")
        .arg("-o").arg("BatchMode=yes")
        .arg(remote.ssh_target())
        .arg(format!("mkdir -p '{}'", remote.path))
        .output()
        .await?;

    if !output.status.success() {
        return Err(BcmrError::InvalidInput(format!(
            "Failed to create remote directory '{}'",
            remote
        )));
    }

    // Walk local directory
    let excludes: Vec<regex::Regex> = Vec::new();
    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in traversal::walk(local_src, true, false, 1, &excludes) {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(local_src)?;

        if path.is_dir() {
            dirs.push(rel.to_path_buf());
        } else if path.is_file() {
            files.push((path.to_path_buf(), rel.to_path_buf()));
        }
    }

    // Create remote directories
    if !dirs.is_empty() {
        let mkdir_cmd = dirs
            .iter()
            .map(|d| format!("'{}/{}'", remote.path, d.display()))
            .collect::<Vec<_>>()
            .join(" ");

        Command::new("ssh")
            .arg("-o").arg("BatchMode=yes")
            .arg(remote.ssh_target())
            .arg(format!("mkdir -p {}", mkdir_cmd))
            .output()
            .await?;
    }

    // Upload files
    for (local_path, rel_path) in &files {
        let file_remote = RemotePath {
            user: remote.user.clone(),
            host: remote.host.clone(),
            path: format!("{}/{}", remote.path, rel_path.display()),
        };
        upload_file(local_path, &file_remote, progress_callback, on_new_file).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_remote_path() {
        // Valid remote paths
        let r = parse_remote_path("user@host:/path/to/file").unwrap();
        assert_eq!(r.user, Some("user".to_string()));
        assert_eq!(r.host, "host");
        assert_eq!(r.path, "/path/to/file");

        let r = parse_remote_path("host:file.txt").unwrap();
        assert_eq!(r.user, None);
        assert_eq!(r.host, "host");
        assert_eq!(r.path, "file.txt");

        let r = parse_remote_path("user@192.168.1.1:").unwrap();
        assert_eq!(r.path, ".");

        // Local paths (should return None)
        assert!(parse_remote_path("/absolute/path").is_none());
        assert!(parse_remote_path("./relative/path").is_none());
        assert!(parse_remote_path("../parent/path").is_none());
        assert!(parse_remote_path("~/home/path").is_none());
        assert!(parse_remote_path(".").is_none());
        assert!(parse_remote_path("..").is_none());

        // Windows drive letters
        assert!(parse_remote_path("C:\\Users\\file").is_none());
        assert!(parse_remote_path("D:file").is_none());

        // Invalid
        assert!(parse_remote_path(":path").is_none());
        assert!(parse_remote_path("@host:path").is_none());
        assert!(parse_remote_path("user@:path").is_none());
    }
}
