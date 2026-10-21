//! Bounded payload I/O for composite and client-mediated remote transfers.
use crate::core::atomic_file::AtomicFile;
use crate::core::error::BcmrError;
use crate::core::file_metadata::PortableFileMetadata;
use crate::core::remote::{self, backend, sftp_ops, sftp_stage::SftpStage, RemotePath};
use crate::core::sftp::{flags, Attrs, Handle, SftpClient};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

pub(super) type Result<T> = std::result::Result<T, BcmrError>;
pub(super) type Session = Arc<Mutex<SftpClient>>;

#[derive(Clone, Debug)]
pub(super) enum Endpoint {
    Local(PathBuf),
    Ssh(RemotePath),
    Sftp(RemotePath),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    File,
    Directory,
    Link,
}

pub(super) fn local_metadata(metadata: &std::fs::Metadata) -> PortableFileMetadata {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        PortableFileMetadata {
            atime_seconds: metadata.atime(),
            atime_nanoseconds: metadata.atime_nsec() as u32,
            mtime_seconds: metadata.mtime(),
            mtime_nanoseconds: metadata.mtime_nsec() as u32,
            mode: metadata.mode() & 0o777,
        }
    }
    #[cfg(not(unix))]
    {
        let atime = filetime::FileTime::from_last_access_time(metadata);
        let mtime = filetime::FileTime::from_last_modification_time(metadata);
        PortableFileMetadata {
            atime_seconds: atime.unix_seconds(),
            atime_nanoseconds: atime.nanoseconds(),
            mtime_seconds: mtime.unix_seconds(),
            mtime_nanoseconds: mtime.nanoseconds(),
            mode: if metadata.is_dir() { 0o755 } else { 0o644 },
        }
    }
}

impl Endpoint {
    pub(super) async fn parse(path: &Path) -> Result<Self> {
        if let Some(mut remote) = remote::parse_remote_path(&path.to_string_lossy()) {
            remote.reject_unsafe()?;
            remote.expand_tilde().await?;
            if backend::is_sftp(&remote.ssh_target()).await? {
                Ok(Self::Sftp(remote))
            } else {
                Ok(Self::Ssh(remote))
            }
        } else {
            Ok(Self::Local(path.to_owned()))
        }
    }

    pub(super) fn display(&self) -> String {
        match self {
            Self::Local(path) => path.display().to_string(),
            Self::Ssh(remote) | Self::Sftp(remote) => remote.display(),
        }
    }

    pub(super) fn basename(&self) -> Result<String> {
        let name = match self {
            Self::Local(path) => path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned),
            Self::Ssh(remote) | Self::Sftp(remote) => Some(remote.file_name().to_owned()),
        };
        name.filter(|name| !name.is_empty() && name != "." && name != "..")
            .ok_or_else(|| {
                backend::invalid(format!(
                    "source '{}' requires a named basename",
                    self.display()
                ))
            })
    }

    pub(super) fn join(&self, relative: &str) -> Result<Self> {
        Ok(match self {
            Self::Local(path) => {
                Self::Local(crate::core::destination::remote_entry_path(path, relative)?)
            }
            Self::Ssh(remote) => Self::Ssh(remote.join(relative)),
            Self::Sftp(remote) => Self::Sftp(remote.join(relative)),
        })
    }

    pub(super) fn parent(&self) -> Self {
        match self {
            Self::Local(path) => Self::Local(
                path.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."))
                    .to_owned(),
            ),
            Self::Ssh(remote) | Self::Sftp(remote) => {
                let parent = remote
                    .path
                    .rsplit_once('/')
                    .map(|(p, _)| if p.is_empty() { "/" } else { p })
                    .unwrap_or(".");
                let path = RemotePath {
                    path: parent.to_owned(),
                    ..remote.clone()
                };
                if matches!(self, Self::Sftp(_)) {
                    Self::Sftp(path)
                } else {
                    Self::Ssh(path)
                }
            }
        }
    }

    pub(super) async fn kind(&self) -> Result<Option<Kind>> {
        match self {
            Self::Local(path) => match path.symlink_metadata() {
                Ok(md) => Ok(Some(if md.is_symlink() {
                    Kind::Link
                } else if md.is_dir() {
                    Kind::Directory
                } else if md.is_file() {
                    Kind::File
                } else {
                    return Err(backend::invalid("destination is a non-regular file"));
                })),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.into()),
            },
            Self::Sftp(remote) => match sftp_ops::attrs(remote, false).await {
                Ok(md) => Ok(Some(if md.is_symlink() {
                    Kind::Link
                } else if md.is_dir() {
                    Kind::Directory
                } else if md.is_file() {
                    Kind::File
                } else {
                    return Err(backend::invalid("SFTP destination is a non-regular file"));
                })),
                Err(error) if backend::missing(&error) => Ok(None),
                Err(error) => Err(error),
            },
            Self::Ssh(remote) => {
                let path = remote.path.replace('\'', "'\\''");
                let output = remote::metadata_command(&remote.ssh_target(), format!("if [ -L '{path}' ]; then printf l; elif [ -d '{path}' ]; then printf d; elif [ -f '{path}' ]; then printf f; elif [ -e '{path}' ]; then exit 75; fi")).await?;
                match output.as_slice() {
                    b"" => Ok(None),
                    b"l" => Ok(Some(Kind::Link)),
                    b"d" => Ok(Some(Kind::Directory)),
                    b"f" => Ok(Some(Kind::File)),
                    _ => Err(backend::invalid("invalid destination metadata")),
                }
            }
        }
    }

    pub(super) async fn is_container(&self) -> Result<bool> {
        match self {
            Self::Local(path) => crate::core::destination::local_is_directory(path),
            Self::Ssh(remote) | Self::Sftp(remote) => {
                if remote.path.ends_with('/') || remote.path == "." {
                    return Ok(true);
                }
                if self.kind().await? == Some(Kind::Directory) {
                    return Ok(true);
                }
                if self.kind().await? == Some(Kind::Link) {
                    return Ok(remote::remote_path_is_directory(remote).await);
                }
                Ok(false)
            }
        }
    }

    pub(super) async fn ensure_dir(&self, sync: bool) -> Result<()> {
        match self {
            Self::Local(path) => {
                if sync {
                    crate::core::io::create_dir_all_durable_async(path).await?;
                } else {
                    tokio::fs::create_dir_all(path).await?;
                }
            }
            Self::Sftp(remote) => sftp_ops::mkdir_all(remote).await?,
            Self::Ssh(remote) => {
                remote::ensure_remote_directories(std::slice::from_ref(remote)).await?
            }
        }
        Ok(())
    }

    pub(super) async fn metadata(&self) -> Result<PortableFileMetadata> {
        match self {
            Self::Local(path) => Ok(local_metadata(&path.metadata()?)),
            Self::Ssh(remote) | Self::Sftp(remote) => remote::get_remote_attrs(remote).await,
        }
    }

    pub(super) async fn disconnect(&self) {
        if let Self::Sftp(remote) = self {
            backend::forget(&remote.ssh_target()).await;
        }
    }

    pub(super) async fn set_metadata(
        &self,
        metadata: PortableFileMetadata,
        sync: bool,
    ) -> Result<()> {
        match self {
            Self::Local(path) => {
                let file = std::fs::File::open(path)?;
                metadata.apply_to(&file)?;
                if sync {
                    if file.metadata()?.is_dir() {
                        #[cfg(unix)]
                        crate::core::io::durable_sync_directory_handle_strict(&file)?;
                    } else {
                        crate::core::io::durable_sync(&file)?;
                    }
                }
            }
            Self::Sftp(remote) => {
                backend::session(&remote.ssh_target())
                    .await?
                    .lock()
                    .await
                    .setstat(&remote.path, &sftp_ops::wire_attrs(metadata)?)
                    .await?;
            }
            Self::Ssh(remote) => remote::set_remote_attrs(remote, metadata).await?,
        }
        Ok(())
    }

    pub(super) async fn hash(&self, limit: Option<u64>) -> Result<String> {
        let mut reader = Reader::open(self, 0, limit).await?;
        let result = async {
            let mut hasher = blake3::Hasher::new();
            let mut count = 0u64;
            loop {
                let max = limit
                    .unwrap_or(u64::MAX)
                    .saturating_sub(count)
                    .min(64 * 1024) as usize;
                if max == 0 {
                    break;
                }
                let data = reader.read(max).await?;
                if data.is_empty() {
                    break;
                }
                count += data.len() as u64;
                hasher.update(&data);
            }
            if limit.is_some_and(|limit| count != limit) {
                return Err(backend::invalid(
                    "file shortened during prefix verification",
                ));
            }
            Ok(hasher.finalize().to_hex().to_string())
        }
        .await;
        reader.finish().await?;
        result
    }
}

async fn diagnostic(mut pipe: tokio::process::ChildStderr) -> String {
    let mut retained = Vec::new();
    let mut buffer = [0u8; 4096];
    while let Ok(n) = pipe.read(&mut buffer).await {
        if n == 0 {
            break;
        }
        let keep = n.min(8192usize.saturating_sub(retained.len()));
        retained.extend_from_slice(&buffer[..keep]);
    }
    String::from_utf8_lossy(&retained).into_owned()
}

pub(super) enum Reader {
    Local(tokio::fs::File),
    Sftp {
        session: Session,
        handle: Handle,
        offset: u64,
    },
    Ssh {
        child: Child,
        stdout: ChildStdout,
        stderr: tokio::task::JoinHandle<String>,
    },
}

impl Reader {
    pub(super) async fn open(source: &Endpoint, offset: u64, limit: Option<u64>) -> Result<Self> {
        match source {
            Endpoint::Local(path) => {
                let mut file = tokio::fs::File::open(path).await?;
                file.seek(std::io::SeekFrom::Start(offset)).await?;
                Ok(Self::Local(file))
            }
            Endpoint::Sftp(remote) => {
                let session = backend::session(&remote.ssh_target()).await?;
                let handle = session
                    .lock()
                    .await
                    .open(&remote.path, flags::READ, &Attrs::default())
                    .await?;
                Ok(Self::Sftp {
                    session,
                    handle,
                    offset,
                })
            }
            Endpoint::Ssh(remote) => {
                let path = if remote.path.starts_with('/') {
                    remote.path.clone()
                } else {
                    format!("./{}", remote.path)
                };
                let path = path.replace('\'', "'\\''");
                let command = if offset == 0 {
                    format!("cat '{path}'")
                } else {
                    format!(
                        "tail -c +{} '{path}'",
                        offset
                            .checked_add(1)
                            .ok_or_else(|| backend::invalid("offset overflow"))?
                    )
                };
                let command = match limit {
                    Some(limit) => format!("{command} | head -c {limit}"),
                    None => command,
                };
                let mut child = remote::ssh_command(&remote.ssh_target())
                    .arg(command)
                    .kill_on_drop(true)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()?;
                let stdout = child
                    .stdout
                    .take()
                    .ok_or_else(|| backend::invalid("SSH reader stdout unavailable"))?;
                let stderr = tokio::spawn(diagnostic(
                    child
                        .stderr
                        .take()
                        .ok_or_else(|| backend::invalid("SSH reader stderr unavailable"))?,
                ));
                Ok(Self::Ssh {
                    child,
                    stdout,
                    stderr,
                })
            }
        }
    }

    pub(super) async fn read(&mut self, max: usize) -> Result<Vec<u8>> {
        let max = max.clamp(1, 64 * 1024);
        match self {
            Self::Local(file) => {
                let mut data = vec![0; max];
                let n = file.read(&mut data).await?;
                data.truncate(n);
                Ok(data)
            }
            Self::Ssh { stdout, .. } => {
                let mut data = vec![0; max];
                let n = stdout.read(&mut data).await?;
                data.truncate(n);
                Ok(data)
            }
            Self::Sftp {
                session,
                handle,
                offset,
            } => {
                let data = session.lock().await.read(handle, *offset, max).await?;
                *offset = offset
                    .checked_add(data.len() as u64)
                    .ok_or_else(|| backend::invalid("SFTP read offset overflow"))?;
                Ok(data)
            }
        }
    }

    pub(super) async fn finish(self) -> Result<()> {
        match self {
            Self::Local(_) => Ok(()),
            Self::Sftp {
                session, handle, ..
            } => session.lock().await.close(&handle).await,
            Self::Ssh {
                mut child,
                stdout,
                stderr,
            } => {
                drop(stdout);
                let status = child.wait().await?;
                let message = stderr.await.unwrap_or_default();
                if status.success() {
                    Ok(())
                } else {
                    Err(backend::invalid(format!("SSH reader failed: {message}")))
                }
            }
        }
    }
}

pub(super) enum Writer {
    Local {
        transaction: AtomicFile,
        file: tokio::fs::File,
    },
    Sftp {
        transaction: SftpStage,
        session: Session,
        handle: Option<Handle>,
        offset: u64,
    },
    Ssh {
        transaction: remote::staged_upload::StagedUpload,
        child: Child,
        stdin: Option<ChildStdin>,
        stderr: Option<tokio::task::JoinHandle<String>>,
    },
}

impl Writer {
    pub(super) async fn create(target: &Endpoint, overwrite: bool, sync: bool) -> Result<Self> {
        target.parent().ensure_dir(sync).await?;
        match target {
            Endpoint::Local(path) => {
                let transaction = if overwrite {
                    AtomicFile::new(path)?
                } else {
                    AtomicFile::new_no_replace(path)?
                };
                let file = tokio::fs::File::from_std(transaction.try_clone_file()?);
                Ok(Self::Local { transaction, file })
            }
            Endpoint::Sftp(remote) => {
                let transaction = SftpStage::create(remote, overwrite).await?;
                let session = backend::session(&remote.ssh_target()).await?;
                let opened = session
                    .lock()
                    .await
                    .open(
                        &transaction.payload.path,
                        flags::WRITE | flags::CREATE | flags::EXCLUSIVE,
                        &Attrs {
                            permissions: Some(0o600),
                            ..Default::default()
                        },
                    )
                    .await;
                match opened {
                    Ok(handle) => {
                        if let Err(error) = transaction.capture_payload().await {
                            let _ = session.lock().await.close(&handle).await;
                            return match transaction.cleanup().await {
                                Ok(()) => Err(error),
                                Err(cleanup) => Err(backend::invalid(format!(
                                    "{error}; stage cleanup also failed: {cleanup}"
                                ))),
                            };
                        }
                        Ok(Self::Sftp {
                            transaction,
                            session,
                            handle: Some(handle),
                            offset: 0,
                        })
                    }
                    Err(error) => match transaction.cleanup().await {
                        Ok(()) => Err(error),
                        Err(cleanup) => Err(backend::invalid(format!(
                            "{error}; stage cleanup also failed: {cleanup}"
                        ))),
                    },
                }
            }
            Endpoint::Ssh(remote) => {
                let transaction =
                    remote::staged_upload::StagedUpload::create(remote, overwrite).await?;
                let command = format!(
                    "cat > '{}'",
                    transaction.remote().path.replace('\'', "'\\''")
                );
                let spawned = remote::ssh_command(&remote.ssh_target())
                    .arg(command)
                    .kill_on_drop(true)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn();
                match spawned {
                    Ok(mut child) => {
                        let stdin = child.stdin.take();
                        let stderr = child
                            .stderr
                            .take()
                            .map(|pipe| tokio::spawn(diagnostic(pipe)));
                        Ok(Self::Ssh {
                            transaction,
                            child,
                            stdin,
                            stderr,
                        })
                    }
                    Err(error) => {
                        transaction.cleanup().await;
                        Err(error.into())
                    }
                }
            }
        }
    }

    pub(super) async fn write(&mut self, data: &[u8]) -> Result<()> {
        match self {
            Self::Local { file, .. } => file.write_all(data).await?,
            Self::Sftp {
                session,
                handle,
                offset,
                ..
            } => {
                session
                    .lock()
                    .await
                    .write(
                        handle
                            .as_ref()
                            .ok_or_else(|| backend::invalid("closed SFTP output"))?,
                        *offset,
                        data,
                    )
                    .await?;
                *offset += data.len() as u64;
            }
            Self::Ssh { stdin, .. } => {
                stdin
                    .as_mut()
                    .ok_or_else(|| backend::invalid("closed SSH output"))?
                    .write_all(data)
                    .await?
            }
        }
        Ok(())
    }

    pub(super) fn staging(&self) -> Endpoint {
        match self {
            Self::Local { transaction, .. } => Endpoint::Local(transaction.staging_path()),
            Self::Sftp { transaction, .. } => Endpoint::Sftp(transaction.payload.clone()),
            Self::Ssh { transaction, .. } => Endpoint::Ssh(transaction.remote().clone()),
        }
    }

    pub(super) async fn staged_hash(&self) -> Result<String> {
        match self {
            Self::Sftp { transaction, .. } => transaction.payload_hash().await,
            _ => self.staging().hash(None).await,
        }
    }

    pub(super) async fn close_stream(&mut self) -> Result<()> {
        match self {
            Self::Local { file, .. } => file.flush().await?,
            Self::Sftp {
                session, handle, ..
            } => {
                if let Some(handle) = handle.take() {
                    session.lock().await.close(&handle).await?;
                }
            }
            Self::Ssh {
                child,
                stdin,
                stderr,
                ..
            } => {
                if let Some(mut stdin) = stdin.take() {
                    stdin.shutdown().await?;
                    drop(stdin);
                }
                let status = child.wait().await?;
                let diagnostic = if let Some(stderr) = stderr.take() {
                    stderr.await.unwrap_or_default()
                } else {
                    String::new()
                };
                if !status.success() {
                    return Err(backend::invalid(format!("SSH writer failed: {diagnostic}")));
                }
            }
        }
        Ok(())
    }

    pub(super) async fn prepare(&mut self) -> Result<()> {
        if let Self::Ssh { transaction, .. } = self {
            transaction.prepare().await?;
        }
        Ok(())
    }

    pub(super) async fn commit(
        mut self,
        metadata: Option<PortableFileMetadata>,
        sync: bool,
    ) -> Result<()> {
        match self {
            Self::Local { transaction, file } => {
                drop(file);
                if let Some(metadata) = metadata {
                    transaction.commit_with_metadata(sync, metadata)
                } else {
                    transaction.commit(sync)
                }
            }
            Self::Sftp {
                ref transaction, ..
            } => {
                let result = transaction.commit(metadata).await;
                let cleanup = transaction.cleanup().await;
                match (result, cleanup) {
                    (Err(error), Err(cleanup)) => Err(backend::invalid(format!(
                        "{error}; stage cleanup also failed: {cleanup}"
                    ))),
                    (Err(error), _) | (_, Err(error)) => Err(error),
                    _ => Ok(()),
                }
            }
            Self::Ssh {
                ref mut transaction,
                ..
            } => {
                let result = transaction.commit().await;
                transaction.cleanup().await;
                result
            }
        }
    }

    pub(super) async fn abort(mut self) -> Result<()> {
        match &mut self {
            Self::Local { .. } => Ok(()),
            Self::Sftp {
                transaction,
                session,
                handle,
                ..
            } => {
                if let Some(handle) = handle.take() {
                    let _ = session.lock().await.close(&handle).await;
                }
                transaction.cleanup().await
            }
            Self::Ssh {
                transaction,
                child,
                stdin,
                ..
            } => {
                stdin.take();
                let _ = child.kill().await;
                let _ = child.wait().await;
                transaction.cleanup().await;
                Ok(())
            }
        }
    }
}
