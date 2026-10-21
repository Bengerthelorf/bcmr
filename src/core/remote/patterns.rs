use super::ssh_cmd::{shell_escape, ssh_command, ssh_error_message};
use super::RemotePath;
use crate::core::error::BcmrError;
use crate::core::star::matches_component;
use std::ffi::OsStr;

async fn query(remote: &RemotePath, command: String) -> Result<Vec<u8>, BcmrError> {
    let output = ssh_command(&remote.ssh_target())
        .arg(command)
        .stdin(std::process::Stdio::null())
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let context = format!("Cannot expand remote source '{remote}'");
        let message = if output.status.code() == Some(255) {
            ssh_error_message(&stderr, &context)
        } else {
            format!("{context}: {}", stderr.trim())
        };
        return Err(BcmrError::InvalidInput(message));
    }
    Ok(output.stdout)
}

async fn exists(remote: &RemotePath, directory_only: bool) -> Result<bool, BcmrError> {
    let path = shell_escape(&remote.path);
    let condition = if directory_only {
        format!("test -d '{path}'")
    } else {
        format!("test -e '{path}' || test -L '{path}'")
    };
    let output = query(remote, format!("if {condition}; then printf 1; fi")).await?;
    Ok(output == b"1")
}

async fn names(remote: &RemotePath) -> Result<Vec<String>, BcmrError> {
    // `/.` follows a directory symlink without recursively following children.
    // Prefix relative paths so find cannot interpret a leading '-' as an option.
    let base = if remote.path.starts_with('/') {
        remote.path.clone()
    } else if remote.path.is_empty() {
        ".".to_string()
    } else {
        format!("./{}", remote.path)
    };
    let listing_root = format!("{base}/.");
    let escaped = shell_escape(&listing_root);
    let output = query(
        remote,
        format!(
            "if test -d '{escaped}'; then LC_ALL=C find '{escaped}' ! -name . -prune -exec printf '%s\\0' {{}} +; fi"
        ),
    )
    .await?;
    if output.is_empty() {
        return Ok(Vec::new());
    }
    let text = String::from_utf8(output).map_err(|_| {
        BcmrError::InvalidInput(format!(
            "Remote directory '{remote}' contains non-UTF-8 names"
        ))
    })?;
    if !text.ends_with('\0') {
        return Err(BcmrError::InvalidInput(format!(
            "Invalid directory listing from '{remote}'"
        )));
    }
    let prefix = format!("{listing_root}/");
    let mut result = Vec::new();
    for path in text[..text.len() - 1].split('\0') {
        let name = path.strip_prefix(&prefix).ok_or_else(|| {
            BcmrError::InvalidInput(format!("Unexpected directory entry from '{remote}'"))
        })?;
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(BcmrError::InvalidInput(format!(
                "Invalid directory entry from '{remote}'"
            )));
        }
        result.push(name.to_string());
    }
    result.sort();
    Ok(result)
}

/// Expand only `*`, using quoted directory enumeration instead of shell globbing.
pub async fn expand_remote_star(mut source: RemotePath) -> Result<Vec<RemotePath>, BcmrError> {
    source.reject_unsafe()?;
    source.expand_tilde().await?;
    let directory_only = source.path.ends_with('/');
    let final_component_is_pattern = source
        .path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .is_some_and(|component| component.contains('*'));
    if exists(&source, directory_only).await? {
        return Ok(vec![source]);
    }
    let mut root = source.clone();
    root.path = if source.path.starts_with('/') {
        "/"
    } else {
        "."
    }
    .to_string();
    let mut candidates = vec![root];
    for component in source.path.split('/').filter(|part| !part.is_empty()) {
        if component.contains('*') {
            let mut matched = Vec::new();
            for base in candidates {
                for name in names(&base).await? {
                    if matches_component(OsStr::new(component), OsStr::new(&name)) {
                        matched.push(base.join(&name));
                    }
                }
            }
            candidates = matched;
        } else {
            candidates = candidates
                .into_iter()
                .map(|base| base.join(component))
                .collect();
        }
    }
    let mut matched = Vec::new();
    for candidate in candidates {
        // A final wildcard component already came from a successful listing.
        // Avoid an extra SSH process per file for common large match sets.
        if (!directory_only && final_component_is_pattern)
            || exists(&candidate, directory_only).await?
        {
            candidate.reject_unsafe()?;
            matched.push(candidate);
        }
    }
    matched.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(matched)
}
