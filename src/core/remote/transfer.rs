use super::attrs::{
    apply_remote_attrs_locally, get_remote_attrs, preserve_remote_attrs, verify_remote_file,
};
use super::ops::{remote_file_hash, remote_file_size};
use super::resume::check_resume_state;
use super::ssh_cmd::{shell_escape, ssh_command, ssh_error_message};
use super::{RemotePath, TransferCallbacks, TransferOptions};
use crate::core::atomic_file::AtomicFile;
use crate::core::error::BcmrError;
use std::path::Path;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub async fn download_file(
    remote: &RemotePath,
    local_dst: &Path,
    cb: TransferCallbacks<'_>,
    file_size: u64,
    opts: &TransferOptions,
) -> Result<(), BcmrError> {
    #[cfg(not(unix))]
    if opts.sync {
        return Err(BcmrError::InvalidInput(
            "--sync cannot guarantee durable local namespace publication on this platform; refusing before transfer".into(),
        ));
    }
    (cb.on_new_file)(remote.file_name(), file_size);

    if let Some(parent) = local_dst
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        if opts.sync {
            crate::core::io::create_dir_all_durable_async(parent).await?;
        } else if !parent.exists() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }

    let existing_size = if local_dst.exists() {
        Some(local_dst.metadata()?.len())
    } else {
        None
    };
    let allow_overwrite = opts.force || opts.resume || opts.strict || opts.append;
    if existing_size.is_some() && !allow_overwrite {
        return Err(BcmrError::TargetExists(local_dst.to_path_buf()));
    }
    let metadata = if opts.preserve {
        Some(get_remote_attrs(remote).await?)
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
    if decision.skip_bytes > file_size {
        return Err(BcmrError::InvalidInput(format!(
            "resume offset {} exceeds remote file size {file_size}",
            decision.skip_bytes
        )));
    }

    let ssh_cmd = if decision.use_append_mode {
        format!(
            "tail -c +{} '{}'",
            decision.skip_bytes + 1,
            shell_escape(&if remote.path.starts_with('/') {
                remote.path.clone()
            } else {
                format!("./{}", remote.path)
            })
        )
    } else {
        format!(
            "cat '{}'",
            shell_escape(&if remote.path.starts_with('/') {
                remote.path.clone()
            } else {
                format!("./{}", remote.path)
            })
        )
    };

    let staging = if decision.use_append_mode {
        None
    } else if allow_overwrite {
        Some(AtomicFile::new(local_dst)?)
    } else {
        Some(AtomicFile::new_no_replace(local_dst)?)
    };
    let transfer_path = staging
        .as_ref()
        .map(|stage| stage.staging_path())
        .unwrap_or_else(|| local_dst.to_path_buf());
    let mut dst_file = if let Some(stage) = &staging {
        tokio::fs::File::from_std(stage.try_clone_file()?)
    } else {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(local_dst)
            .await?
    };

    let mut child = ssh_command(&remote.ssh_target())
        .arg(ssh_cmd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| BcmrError::InvalidInput("Failed to capture SSH stdout".to_string()))?;
    let mut stderr_pipe = child.stderr.take();

    let io_result = copy_download_stream(
        &mut stdout,
        &mut dst_file,
        decision.skip_bytes,
        file_size,
        cb.on_progress,
    )
    .await;

    if let Err(e) = io_result {
        let _ = child.kill().await;
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
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr_buf,
            &format!("Download failed for '{}'", remote),
        )));
    }

    dst_file.flush().await?;
    if opts.sync {
        crate::core::io::durable_sync_async(&dst_file).await?;
    }
    if opts.verify {
        let local_path = transfer_path.clone();
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

    crate::core::interrupt::check()?;
    if let Some(stage) = staging {
        drop(dst_file);
        if let Some(metadata) = metadata {
            stage.commit_with_metadata(opts.sync, metadata)?;
        } else {
            stage.commit(opts.sync)?;
        }
    } else if let Some(metadata) = metadata {
        let file = dst_file.into_std().await;
        metadata.apply_to(&file)?;
        if opts.sync {
            crate::core::io::durable_sync(&file)?;
        }
    }

    Ok(())
}

async fn copy_download_stream<R, W>(
    reader: &mut R,
    writer: &mut W,
    skip_bytes: u64,
    file_size: u64,
    on_progress: &(impl Fn(u64) + ?Sized),
) -> Result<(), BcmrError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if skip_bytes > file_size {
        return Err(BcmrError::InvalidInput(format!(
            "resume offset {skip_bytes} exceeds remote file size {file_size}"
        )));
    }

    let mut transferred = skip_bytes;
    let mut buffer = vec![0u8; (4 * 1024 * 1024).min(crate::core::bandwidth::chunk_size())];
    loop {
        let n = crate::core::interrupt::read_only(async { Ok(reader.read(&mut buffer).await?) })
            .await?;
        if n == 0 {
            break;
        }
        let next = crate::core::protocol::checked_transfer_total(transferred, n, file_size)?;
        crate::core::interrupt::read_only(async {
            crate::core::bandwidth::acquire(n).await;
            Ok(())
        })
        .await?;
        writer.write_all(&buffer[..n]).await?;
        crate::core::interrupt::check()?;
        transferred = next;
        on_progress(n as u64);
    }
    if transferred != file_size {
        return Err(BcmrError::InvalidInput(format!(
            "download ended at {transferred} bytes, expected {file_size}"
        )));
    }
    Ok(())
}

pub async fn upload_file(
    local_src: &Path,
    remote: &RemotePath,
    cb: TransferCallbacks<'_>,
    opts: &TransferOptions,
) -> Result<(), BcmrError> {
    if opts.sync {
        return Err(BcmrError::InvalidInput(
            "--sync requires native serve publication; legacy SSH uploads cannot guarantee durable namespace publication".into(),
        ));
    }
    // Freeze -p before source reads, including resume/verification hashing.
    let metadata = if opts.preserve {
        Some(super::attrs::local_attrs(local_src)?)
    } else {
        None
    };
    let file_size = local_src.metadata()?.len();
    let file_name = local_src
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    (cb.on_new_file)(&file_name, file_size);

    if let Some(parent) = remote.path.rsplit_once('/') {
        if !parent.0.is_empty() {
            let mkdir_out = ssh_command(&remote.ssh_target())
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

    // Prefix-validated appends intentionally remain in place. Every fresh or
    // restarted upload instead requires an explicit, later publication step.
    let allow_overwrite =
        opts.force || (existing_size.is_some() && (opts.resume || opts.strict || opts.append));
    let mut staging = if decision.use_append_mode {
        None
    } else {
        Some(super::staged_upload::StagedUpload::create(remote, allow_overwrite).await?)
    };
    let transfer_remote = staging
        .as_ref()
        .map(|stage| stage.remote())
        .unwrap_or(remote)
        .clone();
    let result: Result<(), BcmrError> = async {
        let mut src_file = tokio::fs::File::open(local_src).await?;
        if decision.skip_bytes > 0 {
            use tokio::io::AsyncSeekExt;
            src_file
                .seek(std::io::SeekFrom::Start(decision.skip_bytes))
                .await?;
        }
        let cat_op = if decision.use_append_mode { ">>" } else { ">" };
        let mut child = ssh_command(&remote.ssh_target())
            .arg(format!(
                "cat {} '{}'",
                cat_op,
                shell_escape(&transfer_remote.path)
            ))
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| BcmrError::InvalidInput("Failed to capture SSH stdin".into()))?;
        let stderr_task = child.stderr.take().map(|mut pipe| {
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let _ = pipe.read_to_end(&mut bytes).await;
                bytes
            })
        });
        let io_result = copy_upload_stream(
            &mut src_file,
            &mut stdin,
            decision.skip_bytes,
            file_size,
            cb.on_progress,
        )
        .await;
        drop(stdin);
        if let Err(error) = io_result {
            let _ = child.kill().await;
            if let Some(task) = stderr_task {
                let _ = task.await;
            }
            return Err(error);
        }
        let status = child.wait().await?;
        let stderr = if let Some(task) = stderr_task {
            task.await.unwrap_or_default()
        } else {
            Vec::new()
        };
        if !status.success() {
            return Err(BcmrError::InvalidInput(ssh_error_message(
                &String::from_utf8_lossy(&stderr),
                &format!("Upload failed for '{}' -> {}", local_src.display(), remote),
            )));
        }
        crate::core::interrupt::check()?;
        if let Some(stage) = staging.as_mut() {
            stage.prepare().await?;
        }
        if opts.verify && !verify_remote_file(local_src, &transfer_remote).await? {
            return Err(BcmrError::InvalidInput(format!(
                "Verification failed: '{}' -> {}",
                local_src.display(),
                remote
            )));
        }
        if let Some(metadata) = metadata {
            super::attrs::set_remote_attrs(&transfer_remote, metadata).await?;
        }
        crate::core::interrupt::check()?;
        if let Some(stage) = staging.as_mut() {
            stage.commit().await?;
        }
        Ok(())
    }
    .await;
    if let Some(stage) = staging.as_ref() {
        stage.cleanup().await;
    }
    result
}

async fn copy_upload_stream<R, W>(
    reader: &mut R,
    writer: &mut W,
    skip_bytes: u64,
    file_size: u64,
    on_progress: &(impl Fn(u64) + ?Sized),
) -> Result<(), BcmrError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if skip_bytes > file_size {
        return Err(BcmrError::InvalidInput(
            "upload resume offset exceeds source size".into(),
        ));
    }
    let mut transferred = skip_bytes;
    let mut buffer = vec![0u8; (4 * 1024 * 1024).min(crate::core::bandwidth::chunk_size())];
    loop {
        let n = crate::core::interrupt::read_only(async { Ok(reader.read(&mut buffer).await?) })
            .await?;
        if n == 0 {
            break;
        }
        let next = crate::core::protocol::checked_transfer_total(transferred, n, file_size)?;
        crate::core::interrupt::read_only(async {
            crate::core::bandwidth::acquire(n).await;
            Ok(())
        })
        .await?;
        writer.write_all(&buffer[..n]).await?;
        crate::core::interrupt::check()?;
        transferred = next;
        on_progress(n as u64);
    }
    if transferred != file_size {
        return Err(BcmrError::InvalidInput(format!(
            "upload ended at {transferred} bytes, expected {file_size}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod upload_stream_tests {
    use super::copy_upload_stream;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn upload_rejects_short_source_before_publication() {
        let mut source: &[u8] = b"ab";
        let (mut stage, mut staged_bytes) = tokio::io::duplex(64);
        let result = copy_upload_stream(&mut source, &mut stage, 0, 8, &|_| {}).await;
        assert!(result.unwrap_err().to_string().contains("expected 8"));
        drop(stage);
        let mut bytes = Vec::new();
        staged_bytes.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"ab");
    }

    #[tokio::test]
    async fn upload_rejects_excess_before_writing_the_excess_chunk() {
        let mut source: &[u8] = b"too long";
        let (mut stage, mut staged_bytes) = tokio::io::duplex(64);
        assert!(copy_upload_stream(&mut source, &mut stage, 0, 3, &|_| {})
            .await
            .is_err());
        drop(stage);
        let mut bytes = Vec::new();
        staged_bytes.read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.is_empty());
    }

    #[tokio::test]
    async fn upload_append_counts_the_validated_prefix() {
        let mut source: &[u8] = b"def";
        let (mut stage, mut staged_bytes) = tokio::io::duplex(64);
        copy_upload_stream(&mut source, &mut stage, 3, 6, &|_| {})
            .await
            .unwrap();
        drop(stage);
        let mut bytes = Vec::new();
        staged_bytes.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"def");
    }
}

#[allow(dead_code)] // Core library API; the CLI uses one shared transfer inventory.
pub async fn download_directory(
    remote: &RemotePath,
    local_dst: &Path,
    cb: TransferCallbacks<'_>,
    excludes: &[regex::Regex],
    opts: &TransferOptions,
) -> Result<(), BcmrError> {
    let entries = super::inventory_remote(remote, false, excludes).await?;
    if entries.is_empty() {
        return Ok(());
    }
    if !entries[0].is_dir {
        return Err(BcmrError::InvalidInput(format!(
            "Remote source '{}' is not a directory",
            remote
        )));
    }
    let mut directories = Vec::new();
    for entry in &entries {
        if entry.is_dir {
            let local = if entry.path.is_empty() {
                local_dst.to_path_buf()
            } else {
                crate::core::destination::remote_entry_path(local_dst, &entry.path)?
            };
            if opts.sync {
                crate::core::io::create_dir_all_durable_async(&local).await?;
            } else {
                tokio::fs::create_dir_all(&local).await?;
            }
            let source = if entry.path.is_empty() {
                remote.clone()
            } else {
                remote.join(&entry.path)
            };
            directories.push((source, local));
        }
    }
    for entry in &entries {
        if entry.is_dir {
            continue;
        }
        if entry.link_target.is_some() {
            return Err(BcmrError::InvalidInput(
                "dereferenced inventory contains a symbolic link".into(),
            ));
        }
        let local = crate::core::destination::remote_entry_path(local_dst, &entry.path)?;
        download_file(
            &remote.join(&entry.path),
            &local,
            TransferCallbacks {
                on_progress: cb.on_progress,
                on_skip: cb.on_skip,
                on_new_file: cb.on_new_file,
            },
            entry.size,
            opts,
        )
        .await?;
    }
    if opts.preserve {
        for (source, local) in directories.iter().rev() {
            #[cfg(not(unix))]
            if opts.sync {
                return Err(BcmrError::InvalidInput(
                    "--sync cannot guarantee durable local directory metadata on this platform"
                        .into(),
                ));
            }
            #[cfg(unix)]
            let handle = if opts.sync {
                Some(tokio::fs::File::open(local).await?.into_std().await)
            } else {
                None
            };
            apply_remote_attrs_locally(source, local).await?;
            #[cfg(unix)]
            if let Some(handle) = handle {
                tokio::task::spawn_blocking(move || {
                    crate::core::io::durable_sync_directory_handle_strict(&handle)
                })
                .await??;
            }
        }
    }
    Ok(())
}

/// Create only the directories selected by a completed transfer inventory.
pub async fn ensure_remote_directories(directories: &[RemotePath]) -> Result<(), BcmrError> {
    let mut by_host: std::collections::BTreeMap<String, Vec<&RemotePath>> =
        std::collections::BTreeMap::new();
    for directory in directories {
        by_host
            .entry(directory.ssh_target())
            .or_default()
            .push(directory);
    }
    for (target, paths) in by_host {
        for chunk in paths.chunks(128) {
            let arguments = chunk
                .iter()
                .map(|directory| {
                    // POSIX mkdir has no portable --; make a relative leading '-'
                    // an ordinary path component before putting it in shell quotes.
                    let path = if directory.path.starts_with('/') {
                        directory.path.clone()
                    } else {
                        format!("./{}", directory.path)
                    };
                    format!("'{}'", shell_escape(&path))
                })
                .collect::<Vec<_>>()
                .join(" ");
            let output = ssh_command(&target)
                .arg(format!("mkdir -p {arguments}"))
                .output()
                .await?;
            if !output.status.success() {
                return Err(BcmrError::InvalidInput(ssh_error_message(
                    &String::from_utf8_lossy(&output.stderr),
                    &format!("Failed to create remote directory tree '{}'", chunk[0]),
                )));
            }
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub async fn ensure_remote_tree(local_src: &Path, remote: &RemotePath) -> Result<(), BcmrError> {
    let mut directories = Vec::new();
    for entry in crate::core::traversal::copy_walk(local_src, true, 0, false, &[]) {
        let entry = entry?;
        if entry.metadata()?.is_dir() {
            let relative = entry.path().strip_prefix(local_src)?;
            let path = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            directories.push(if path.is_empty() {
                remote.clone()
            } else {
                remote.join(&path)
            });
        }
    }
    ensure_remote_directories(&directories).await
}

#[allow(dead_code)] // Core library API; the CLI uses one shared transfer inventory.
pub async fn upload_directory(
    local_src: &Path,
    remote: &RemotePath,
    cb: TransferCallbacks<'_>,
    excludes: &[regex::Regex],
    opts: &TransferOptions,
) -> Result<(), BcmrError> {
    if crate::core::traversal::is_copy_excluded(local_src, local_src, excludes) {
        return Ok(());
    }
    if opts.sync {
        return Err(BcmrError::InvalidInput(
            "--sync cannot guarantee durable remote namespace publication through legacy SSH; refusing before transfer".into(),
        ));
    }
    if !local_src.metadata()?.is_dir() {
        return Err(BcmrError::InvalidInput(format!(
            "Source '{}' is not a directory",
            local_src.display()
        )));
    }
    let mut directories = Vec::new();
    let mut files = Vec::new();
    for entry in crate::core::traversal::copy_walk(local_src, true, 0, false, excludes) {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(local_src)?;
        let relative = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let target = if relative.is_empty() {
            remote.clone()
        } else {
            remote.join(&relative)
        };
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            directories.push((path.to_path_buf(), target));
        } else if metadata.is_file() {
            files.push((path.to_path_buf(), target));
        } else {
            return Err(BcmrError::InvalidInput(format!(
                "Source '{}' is not a regular file or directory",
                path.display()
            )));
        }
    }
    let remote_directories: Vec<_> = directories
        .iter()
        .map(|(_, target)| target.clone())
        .collect();
    ensure_remote_directories(&remote_directories).await?;
    for (source, target) in files {
        upload_file(
            &source,
            &target,
            TransferCallbacks {
                on_progress: cb.on_progress,
                on_skip: cb.on_skip,
                on_new_file: cb.on_new_file,
            },
            opts,
        )
        .await?;
    }
    if opts.preserve {
        for (source, target) in directories.iter().rev() {
            preserve_remote_attrs(source, target).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn source(bytes: Vec<u8>) -> tokio::io::DuplexStream {
        let (mut writer, reader) = tokio::io::duplex(64);
        tokio::spawn(async move {
            writer.write_all(&bytes).await.unwrap();
            writer.shutdown().await.unwrap();
        });
        reader
    }

    async fn sink_bytes(mut reader: tokio::io::DuplexStream) -> Vec<u8> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.unwrap();
        bytes
    }

    #[tokio::test]
    async fn download_stream_rejects_excess_before_writing_it() {
        let mut input = source(vec![1, 2, 3, 4]).await;
        let (mut output, output_reader) = tokio::io::duplex(64);

        assert!(copy_download_stream(&mut input, &mut output, 0, 3, &|_| {})
            .await
            .is_err());
        drop(output);
        assert!(sink_bytes(output_reader).await.is_empty());
    }

    #[tokio::test]
    async fn download_stream_rejects_short_read_at_eof() {
        let mut input = source(vec![1, 2, 3]).await;
        let (mut output, output_reader) = tokio::io::duplex(64);

        assert!(copy_download_stream(&mut input, &mut output, 0, 4, &|_| {})
            .await
            .is_err());
        drop(output);
        assert_eq!(sink_bytes(output_reader).await, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn download_stream_completes_exactly_from_resume_offset() {
        let mut input = source(vec![3, 4]).await;
        let (mut output, output_reader) = tokio::io::duplex(64);

        copy_download_stream(&mut input, &mut output, 2, 4, &|_| {})
            .await
            .unwrap();
        drop(output);
        assert_eq!(sink_bytes(output_reader).await, vec![3, 4]);
    }

    #[tokio::test]
    async fn download_stream_rejects_resume_offset_past_remote_size() {
        let mut input = source(Vec::new()).await;
        let (mut output, output_reader) = tokio::io::duplex(64);

        assert!(copy_download_stream(&mut input, &mut output, 5, 4, &|_| {})
            .await
            .is_err());
        drop(output);
        assert!(sink_bytes(output_reader).await.is_empty());
    }
}
