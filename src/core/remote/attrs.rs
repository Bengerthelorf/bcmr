use super::ops::remote_file_hash;
use super::ssh_cmd::{shell_escape, ssh_command, ssh_error_message};
use super::RemotePath;
use crate::core::error::BcmrError;
use std::path::Path;

pub async fn preserve_remote_attrs(local_src: &Path, remote: &RemotePath) -> Result<(), BcmrError> {
    let meta = local_src.metadata()?;

    let mode = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o7777
        }
        #[cfg(not(unix))]
        {
            0o644u32
        }
    };

    let mtime_secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let atime_secs = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            meta.atime() as u64
        }
        #[cfg(not(unix))]
        {
            mtime_secs
        }
    };

    let mtime_ts = unix_to_touch_ts(mtime_secs as i64);
    let atime_ts = unix_to_touch_ts(atime_secs as i64);
    let cmd = format!(
        "TZ=UTC touch -m -t '{}' '{}'; TZ=UTC touch -a -t '{}' '{}'; chmod {:o} '{}'",
        mtime_ts,
        shell_escape(&remote.path),
        atime_ts,
        shell_escape(&remote.path),
        mode,
        shell_escape(&remote.path)
    );
    let attr_out = ssh_command(&remote.ssh_target()).arg(cmd).output().await?;
    if !attr_out.status.success() {
        let stderr = String::from_utf8_lossy(&attr_out.stderr);
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr,
            &format!("Failed to set attributes on '{}'", remote),
        )));
    }
    Ok(())
}

async fn get_remote_attrs(remote: &RemotePath) -> Result<(i64, i64, u32), BcmrError> {
    let output = ssh_command(&remote.ssh_target())
        .arg(format!(
            "stat -c '%X %Y %a' '{}' 2>/dev/null || stat -f '%a %m %Lp' '{}'",
            shell_escape(&remote.path),
            shell_escape(&remote.path)
        ))
        .output()
        .await?;

    if !output.status.success() {
        return Err(BcmrError::InvalidInput(format!(
            "Cannot stat remote file '{}'",
            remote
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.split_whitespace().collect();
    let atime_secs: i64 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mtime_secs: i64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let mode: u32 = parts
        .get(2)
        .and_then(|s| u32::from_str_radix(s, 8).ok())
        .unwrap_or(0o644);

    Ok((atime_secs, mtime_secs, mode))
}

fn apply_local_attrs(
    local_path: &Path,
    atime_secs: i64,
    mtime_secs: i64,
    _mode: u32,
) -> Result<(), BcmrError> {
    let atime = filetime::FileTime::from_unix_time(atime_secs, 0);
    let mtime = filetime::FileTime::from_unix_time(mtime_secs, 0);
    filetime::set_file_times(local_path, atime, mtime)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(local_path, std::fs::Permissions::from_mode(_mode))?;
    }

    Ok(())
}

pub async fn apply_remote_attrs_locally(
    remote: &RemotePath,
    local_path: &Path,
) -> Result<(), BcmrError> {
    let (atime_secs, mtime_secs, mode) = get_remote_attrs(remote).await?;
    apply_local_attrs(local_path, atime_secs, mtime_secs, mode)?;
    Ok(())
}

pub async fn verify_remote_file(local_src: &Path, remote: &RemotePath) -> Result<bool, BcmrError> {
    use crate::core::checksum;

    let local_path = local_src.to_path_buf();
    let local_hash = tokio::task::spawn_blocking(move || checksum::calculate_hash(&local_path))
        .await
        .map_err(|e| BcmrError::InvalidInput(e.to_string()))??;

    let remote_hash = remote_file_hash(remote, None).await?;

    Ok(local_hash == remote_hash)
}

fn unix_to_touch_ts(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let hours = rem / 3600;
    let minutes = (rem % 3600) / 60;
    let seconds = rem % 60;

    let z = days as i32 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i32 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}{:02}{:02}{:02}{:02}.{:02}",
        y, m, d, hours, minutes, seconds
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unix_to_touch_ts_epoch() {
        assert_eq!(unix_to_touch_ts(0), "197001010000.00");
    }

    #[test]
    fn test_unix_to_touch_ts_known_date() {
        let ts = unix_to_touch_ts(1577880000);
        assert_eq!(ts, "202001011200.00");
    }

    #[test]
    fn test_unix_to_touch_ts_with_seconds() {
        let ts = unix_to_touch_ts(1592210445);
        assert!(ts.ends_with(".45"));
    }
}
