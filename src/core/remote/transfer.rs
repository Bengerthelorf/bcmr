use super::attrs::{apply_remote_attrs_locally, preserve_remote_attrs, verify_remote_file};
use super::ops::{remote_file_hash, remote_file_size, remote_list_files};
use super::resume::check_resume_state;
use super::ssh_cmd::{make_ssh_cmd, shell_escape, ssh_command, ssh_error_message};
use super::{RemotePath, RemoteTransferOptions, TransferCallbacks};
use crate::core::error::BcmrError;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub async fn download_file(
    remote: &RemotePath,
    local_dst: &Path,
    cb: TransferCallbacks<'_>,
    file_size: u64,
    opts: &RemoteTransferOptions,
    worker_id: Option<usize>,
) -> Result<(), BcmrError> {
    let file_name = remote.path.rsplit('/').next().unwrap_or(&remote.path);
    (cb.on_new_file)(file_name, file_size);

    if let Some(parent) = local_dst.parent() {
        if !parent.exists() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }

    let existing_size = if local_dst.exists() {
        Some(local_dst.metadata()?.len())
    } else {
        None
    };
    let local_path_for_hash = local_dst.to_path_buf();
    let remote_for_hash = remote.clone();
    let remote_for_partial = remote.clone();

    let decision = check_resume_state(
        opts,
        existing_size,
        file_size,
        async move || {
            tokio::task::spawn_blocking(move || {
                crate::core::checksum::calculate_hash(&local_path_for_hash)
            })
            .await
            .map_err(|e| BcmrError::InvalidInput(e.to_string()))?
            .map_err(BcmrError::Io)
        },
        async move || remote_file_hash(&remote_for_hash, None).await,
        async move |limit| remote_file_hash(&remote_for_partial, Some(limit)).await,
    )
    .await?;

    if decision.skip_entirely {
        (cb.on_skip)(file_size);
        return Ok(());
    }

    if decision.skip_bytes > 0 {
        (cb.on_skip)(decision.skip_bytes);
    }

    let ssh_cmd = if decision.use_append_mode {
        format!(
            "tail -c +{} '{}'",
            decision.skip_bytes + 1,
            shell_escape(&remote.path)
        )
    } else {
        format!("cat '{}'", shell_escape(&remote.path))
    };

    let mut child = make_ssh_cmd(&remote.ssh_target(), worker_id)
        .arg(ssh_cmd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| BcmrError::InvalidInput("Failed to capture SSH stdout".to_string()))?;
    let mut stderr_pipe = child.stderr.take();

    let mut dst_file = if decision.use_append_mode {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(local_dst)
            .await?
    } else {
        tokio::fs::File::create(local_dst).await?
    };
    let mut buffer = vec![0u8; 4 * 1024 * 1024];

    let io_result: Result<(), BcmrError> = async {
        loop {
            let n = stdout.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            dst_file.write_all(&buffer[..n]).await?;
            (cb.on_progress)(n as u64);
        }
        Ok(())
    }
    .await;

    if let Err(e) = io_result {
        let _ = child.kill().await;
        if !decision.use_append_mode {
            let _ = tokio::fs::remove_file(local_dst).await;
        }
        return Err(e);
    }

    let status = child.wait().await?;
    if !status.success() {
        let mut stderr_buf = String::new();
        if let Some(ref mut pipe) = stderr_pipe {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf).await;
            stderr_buf = String::from_utf8_lossy(&buf).to_string();
        }
        if !decision.use_append_mode {
            let _ = tokio::fs::remove_file(local_dst).await;
        }
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr_buf,
            &format!("Download failed for '{}'", remote),
        )));
    }

    if opts.sync {
        dst_file.flush().await?;
        crate::core::io::durable_sync_async(&dst_file).await?;
    }
    drop(dst_file);

    if opts.verify {
        let local_path = local_dst.to_path_buf();
        let local_hash =
            tokio::task::spawn_blocking(move || crate::core::checksum::calculate_hash(&local_path))
                .await
                .map_err(|e| BcmrError::InvalidInput(e.to_string()))??;
        let remote_hash = remote_file_hash(remote, None).await?;
        if local_hash != remote_hash {
            return Err(BcmrError::InvalidInput(format!(
                "Verification failed: {} -> '{}'",
                remote,
                local_dst.display()
            )));
        }
    }

    if opts.preserve {
        apply_remote_attrs_locally(remote, local_dst).await?;
    }

    Ok(())
}

pub async fn upload_file(
    local_src: &Path,
    remote: &RemotePath,
    cb: TransferCallbacks<'_>,
    opts: &RemoteTransferOptions,
    worker_id: Option<usize>,
) -> Result<(), BcmrError> {
    let file_size = local_src.metadata()?.len();
    let file_name = local_src
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    (cb.on_new_file)(&file_name, file_size);

    if let Some(parent) = remote.path.rsplit_once('/') {
        if !parent.0.is_empty() {
            let mkdir_out = make_ssh_cmd(&remote.ssh_target(), worker_id)
                .arg(format!("mkdir -p '{}'", shell_escape(parent.0)))
                .output()
                .await?;
            if !mkdir_out.status.success() {
                let stderr = String::from_utf8_lossy(&mkdir_out.stderr);
                return Err(BcmrError::InvalidInput(ssh_error_message(
                    &stderr,
                    &format!("Failed to create remote directory '{}'", parent.0),
                )));
            }
        }
    }

    let existing_size = remote_file_size(remote).await.ok().flatten();
    let remote_for_hash = remote.clone();
    let local_path_for_hash = local_src.to_path_buf();
    let local_path_for_partial = local_src.to_path_buf();

    let decision = check_resume_state(
        opts,
        existing_size,
        file_size,
        async move || remote_file_hash(&remote_for_hash, None).await,
        async move || {
            tokio::task::spawn_blocking(move || {
                crate::core::checksum::calculate_hash(&local_path_for_hash)
            })
            .await
            .map_err(|e| BcmrError::InvalidInput(e.to_string()))?
            .map_err(BcmrError::Io)
        },
        async move |limit| {
            tokio::task::spawn_blocking(move || {
                crate::core::checksum::calculate_partial_hash(&local_path_for_partial, limit)
            })
            .await
            .map_err(|e| BcmrError::InvalidInput(e.to_string()))?
            .map_err(BcmrError::Io)
        },
    )
    .await?;

    if decision.skip_entirely {
        (cb.on_skip)(file_size);
        return Ok(());
    }

    if decision.skip_bytes > 0 {
        (cb.on_skip)(decision.skip_bytes);
    }

    let cat_op = if decision.use_append_mode { ">>" } else { ">" };
    let mut child = make_ssh_cmd(&remote.ssh_target(), worker_id)
        .arg(format!("cat {} '{}'", cat_op, shell_escape(&remote.path)))
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| BcmrError::InvalidInput("Failed to capture SSH stdin".to_string()))?;

    let mut src_file = tokio::fs::File::open(local_src).await?;

    if decision.skip_bytes > 0 {
        use tokio::io::AsyncSeekExt;
        src_file
            .seek(std::io::SeekFrom::Start(decision.skip_bytes))
            .await?;
    }

    let mut buffer = vec![0u8; 4 * 1024 * 1024];

    let io_result: Result<(), BcmrError> = async {
        loop {
            let n = src_file.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            stdin.write_all(&buffer[..n]).await?;
            (cb.on_progress)(n as u64);
        }
        Ok(())
    }
    .await;

    drop(stdin);

    if let Err(e) = io_result {
        let _ = child.kill().await;
        return Err(e);
    }

    let output = child.wait_with_output().await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr,
            &format!("Upload failed for '{}' -> {}", local_src.display(), remote),
        )));
    }

    if opts.verify && !verify_remote_file(local_src, remote).await? {
        return Err(BcmrError::InvalidInput(format!(
            "Verification failed: '{}' -> {}",
            local_src.display(),
            remote
        )));
    }

    if opts.preserve {
        preserve_remote_attrs(local_src, remote).await?;
    }

    Ok(())
}

pub async fn download_directory(
    remote: &RemotePath,
    local_dst: &Path,
    cb: TransferCallbacks<'_>,
    excludes: &[regex::Regex],
    opts: &RemoteTransferOptions,
) -> Result<(), BcmrError> {
    let entries = remote_list_files(remote).await?;

    let entries: Vec<_> = entries
        .into_iter()
        .filter(|(rel_path, _, _)| {
            !crate::core::traversal::is_excluded(std::path::Path::new(rel_path), excludes)
        })
        .collect();

    for (rel_path, _, is_dir) in &entries {
        if *is_dir {
            let dir_path = local_dst.join(rel_path);
            if !dir_path.exists() {
                tokio::fs::create_dir_all(&dir_path).await?;
            }
        }
    }

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
        download_file(
            &file_remote,
            &local_file,
            TransferCallbacks {
                on_progress: cb.on_progress,
                on_skip: cb.on_skip,
                on_new_file: cb.on_new_file,
            },
            *size,
            opts,
            None,
        )
        .await?;
    }

    Ok(())
}

pub async fn ensure_remote_tree(local_src: &Path, remote: &RemotePath) -> Result<(), BcmrError> {
    use crate::core::traversal;

    ssh_command(&remote.ssh_target())
        .arg(format!("mkdir -p '{}'", shell_escape(&remote.path)))
        .output()
        .await?;

    let excludes: Vec<regex::Regex> = Vec::new();
    let mut dirs = Vec::new();

    for entry in traversal::walk(local_src, true, false, 1, &excludes) {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let rel = path.strip_prefix(local_src)?;
            dirs.push(rel.to_path_buf());
        }
    }

    if !dirs.is_empty() {
        let mkdir_cmd = dirs
            .iter()
            .map(|d| {
                format!(
                    "'{}/{}'",
                    shell_escape(&remote.path),
                    shell_escape(&d.display().to_string())
                )
            })
            .collect::<Vec<_>>()
            .join(" ");

        ssh_command(&remote.ssh_target())
            .arg(format!("mkdir -p {}", mkdir_cmd))
            .output()
            .await?;
    }

    Ok(())
}

pub async fn upload_directory(
    local_src: &Path,
    remote: &RemotePath,
    cb: TransferCallbacks<'_>,
    excludes: &[regex::Regex],
    opts: &RemoteTransferOptions,
) -> Result<(), BcmrError> {
    use crate::core::traversal;

    let output = ssh_command(&remote.ssh_target())
        .arg(format!("mkdir -p '{}'", shell_escape(&remote.path)))
        .output()
        .await?;

    if !output.status.success() {
        return Err(BcmrError::InvalidInput(format!(
            "Failed to create remote directory '{}'",
            remote
        )));
    }

    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in traversal::walk(local_src, true, false, 1, excludes) {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(local_src)?;

        if path.is_dir() {
            dirs.push(rel.to_path_buf());
        } else if path.is_file() {
            files.push((path.to_path_buf(), rel.to_path_buf()));
        }
    }

    if !dirs.is_empty() {
        let mkdir_cmd = dirs
            .iter()
            .map(|d| {
                format!(
                    "'{}/{}'",
                    shell_escape(&remote.path),
                    shell_escape(&d.display().to_string())
                )
            })
            .collect::<Vec<_>>()
            .join(" ");

        ssh_command(&remote.ssh_target())
            .arg(format!("mkdir -p {}", mkdir_cmd))
            .output()
            .await?;
    }

    for (local_path, rel_path) in &files {
        let file_remote = RemotePath {
            user: remote.user.clone(),
            host: remote.host.clone(),
            path: format!("{}/{}", remote.path, rel_path.display()),
        };
        upload_file(
            local_path,
            &file_remote,
            TransferCallbacks {
                on_progress: cb.on_progress,
                on_skip: cb.on_skip,
                on_new_file: cb.on_new_file,
            },
            opts,
            None,
        )
        .await?;
    }

    Ok(())
}
