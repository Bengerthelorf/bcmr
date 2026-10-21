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

// Probe extensions before producing any listing bytes: falling back after a
// partially successful find could duplicate entries or conceal traversal errors.
// Paths are positional arguments throughout, never shell programs. BSD stat
// does not interpret \\0 in its format, so printf owns the NUL framing.
const REMOTE_LIST_SCRIPT: &str = r#"
case "$1" in /*) root=$1 ;; *) root=./$1 ;; esac
CDPATH= cd "$root" || exit 1
if find . -maxdepth 0 -printf '' >/dev/null 2>&1; then
    find . -mindepth 1 -printf '%P\0%s\0%y\0'
else
    if LC_ALL=C stat -c '%s %F' . >/dev/null 2>&1; then
        style=gnu
    elif LC_ALL=C stat -f '%z %HT' . >/dev/null 2>&1; then
        style=bsd
    else
        printf '%s\n' 'Cannot read directory metadata with remote stat' >&2
        exit 1
    fi
    find . ! -path . -exec sh -c '
        style=$1
        shift
        for path in "$@"; do
            if [ "$style" = gnu ]; then
                metadata=$(LC_ALL=C stat -c "%s %F" "$path") || exit 1
            else
                metadata=$(LC_ALL=C stat -f "%z %HT" "$path") || exit 1
            fi
            size=${metadata%% *}
            kind=${metadata#* }
            case "$kind" in directory|Directory) kind=d ;; *) kind=f ;; esac
            printf "%s\000%s\000%s\000" "${path#./}" "$size" "$kind" || exit 1
        done
    ' bcmr-list "$style" {} +
fi
"#;

fn parse_remote_listing(raw: &[u8]) -> Result<Vec<(String, u64, bool)>, &'static str> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    if !raw.ends_with(&[0]) {
        return Err("listing is missing its final NUL delimiter");
    }
    let fields: Vec<&[u8]> = raw[..raw.len() - 1].split(|byte| *byte == 0).collect();
    if !fields.len().is_multiple_of(3) {
        return Err("listing contains an incomplete entry");
    }
    let mut entries = Vec::with_capacity(fields.len() / 3);
    for entry in fields.chunks_exact(3) {
        let path = std::str::from_utf8(entry[0]).map_err(|_| "listing path is not UTF-8")?;
        // Remote names use '/' even when the client runs on Windows. Native
        // Path components would mistake valid remote backslashes/colons for
        // Windows path syntax.
        if path.split('/').any(|part| matches!(part, "" | "." | "..")) {
            return Err("listing contains an invalid relative path");
        }
        let size = std::str::from_utf8(entry[1])
            .map_err(|_| "listing size is not UTF-8")?
            .parse::<u64>()
            .map_err(|_| "listing contains an invalid file size")?;
        let is_dir = match entry[2] {
            b"d" => true,
            b"f" | b"l" | b"b" | b"c" | b"p" | b"s" | b"D" => false,
            _ => return Err("listing contains an invalid entry type"),
        };
        entries.push((path.to_string(), size, is_dir));
    }
    Ok(entries)
}

pub async fn remote_list_files(remote: &RemotePath) -> Result<Vec<(String, u64, bool)>, BcmrError> {
    let command = format!(
        "sh -c '{}' bcmr-list '{}'",
        shell_escape(REMOTE_LIST_SCRIPT),
        shell_escape(&remote.path)
    );
    let output = ssh_command(&remote.ssh_target())
        .arg(command)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr,
            &format!("Cannot list remote directory '{}'", remote),
        )));
    }

    parse_remote_listing(&output.stdout).map_err(|detail| {
        BcmrError::InvalidInput(format!("Invalid remote listing for '{}': {detail}", remote))
    })
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
            &format!("remote ~{} expansion on '{}'", user_spec, ssh_target),
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

pub async fn complete_remote_path(partial: &str) -> Result<Vec<String>, BcmrError> {
    let remote =
        parse_remote_path(partial).ok_or_else(|| BcmrError::invalid_remote_path(partial))?;

    let (dir, prefix) = if remote.path.ends_with('/') || remote.path == "." {
        (remote.path.clone(), String::new())
    } else if let Some(pos) = remote.path.rfind('/') {
        (
            remote.path[..=pos].to_string(),
            remote.path[pos + 1..].to_string(),
        )
    } else if remote.path.starts_with('~') {
        (format!("{}/", remote.path), String::new())
    } else {
        (".".to_string(), remote.path.clone())
    };

    let target = remote.ssh_target();
    // Resolve only the directory used for I/O. Candidates retain the user's
    // spelling (~, ~user, and relative paths), and no path is evaluated as code.
    let expanded_dir = expand_remote_tilde(&target, &dir).await?;
    let output = ssh_command(&target)
        .arg(format!(
            "LC_ALL=C ls -1ap -- '{}'",
            shell_escape(&expanded_dir)
        ))
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let context = format!("Cannot complete remote path '{}'", partial);
        let diagnostic = if output.status.code() == Some(255) {
            ssh_error_message(&stderr, &context)
        } else {
            // An authenticated remote ls can fail with "Permission denied".
            // Preserve that diagnostic instead of calling it an SSH auth error.
            format!(
                "{}: remote directory listing failed ({}): {}",
                context,
                output.status,
                stderr.trim()
            )
        };
        return Err(BcmrError::InvalidInput(diagnostic));
    }

    let base = if dir == "." { String::new() } else { dir };
    let mut display_remote = remote;
    display_remote.path = String::new();
    let display_target = display_remote.display();

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| *l != "./" && *l != "../")
        .filter(|l| prefix.is_empty() || l.starts_with(&prefix))
        .map(|l| format!("{}{}{}", display_target, base, l))
        .collect())
}

#[cfg(test)]
mod listing_tests {
    use super::parse_remote_listing;

    #[test]
    fn parses_relative_files_directories_and_literal_shell_characters() {
        let entries = parse_remote_listing(
            concat!(
                "nested dir\0",
                "4096\0d\0nested dir/quote'\n$(command)\\name:part\0",
                "7\0f\0"
            )
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(
            entries,
            vec![
                ("nested dir".to_string(), 4096, true),
                (
                    "nested dir/quote'\n$(command)\\name:part".to_string(),
                    7,
                    false
                ),
            ]
        );
        assert!(parse_remote_listing(b"").unwrap().is_empty());
    }

    #[test]
    fn refuses_malformed_framing_metadata_and_unsafe_paths() {
        for raw in [
            b"literal\\0stat\\0output".as_slice(),
            b"file\0\x31\0f",
            b"file\0\x31\0",
            b"file\0no-size\0f\0",
            b"file\0\x31\0unknown\0",
            b"/absolute\0\x31\0f\0",
            b"../outside\0\x31\0f\0",
            b"nested/../outside\0\x31\0f\0",
            b"\xff\0\x31\0f\0",
        ] {
            assert!(parse_remote_listing(raw).is_err(), "accepted {raw:?}");
        }
    }
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
