use std::path::Path;

use crate::core::atomic_file::AtomicFile;
use crate::core::error::BcmrError;
use crate::core::protocol::Message;

use super::{FileTransfer, ServeClient};

pub struct ServeClientPool {
    clients: Vec<ServeClient>,
}

impl ServeClientPool {
    async fn build<F, Fut>(n: usize, mut connector: F) -> Result<Self, BcmrError>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<ServeClient, BcmrError>>,
    {
        if n == 0 {
            return Err(BcmrError::InvalidInput("pool size must be >= 1".into()));
        }
        let futures: Vec<_> = (0..n).map(|_| connector()).collect();
        let clients = futures::future::try_join_all(futures).await?;
        Ok(Self { clients })
    }

    pub async fn connect_with_caps(
        ssh_target: &str,
        caps: u8,
        n: usize,
    ) -> Result<Self, BcmrError> {
        Self::build(n, || ServeClient::connect_with_caps(ssh_target, caps)).await
    }

    pub async fn connect_direct_with_caps(
        ssh_target: &str,
        caps: u8,
        n: usize,
    ) -> Result<Self, BcmrError> {
        Self::build(n, || {
            ServeClient::connect_direct_with_caps(ssh_target, caps)
        })
        .await
    }

    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub async fn connect_direct_local(n: usize) -> Result<Self, BcmrError> {
        Self::build(n, ServeClient::connect_direct_local).await
    }

    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub async fn connect_local(n: usize) -> Result<Self, BcmrError> {
        Self::build(n, ServeClient::connect_local).await
    }

    pub fn len(&self) -> usize {
        self.clients.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    pub fn first_mut(&mut self) -> &mut ServeClient {
        &mut self.clients[0]
    }

    pub async fn mkdir(&mut self, path: &str) -> Result<(), BcmrError> {
        self.clients[0].mkdir(path).await
    }

    pub async fn pipelined_put_files_striped<FChunk, FComplete>(
        &mut self,
        files: Vec<FileTransfer>,
        overwrite: bool,
        on_chunk: FChunk,
        on_complete: FComplete,
    ) -> Result<Vec<[u8; 32]>, BcmrError>
    where
        FChunk: Fn(u64) + Send + Sync + Clone + 'static,
        FComplete: Fn(usize, &Path, u64) + Send + Sync + Clone + 'static,
    {
        let n_files = files.len();
        if n_files == 0 {
            return Ok(Vec::new());
        }
        let n_clients = self.clients.len().min(n_files);

        let mut buckets: Vec<(Vec<usize>, Vec<FileTransfer>)> =
            (0..n_clients).map(|_| (Vec::new(), Vec::new())).collect();
        for (i, ft) in files.into_iter().enumerate() {
            let b = &mut buckets[i % n_clients];
            b.0.push(i);
            b.1.push(ft);
        }

        let futs = self.clients.iter_mut().take(n_clients).zip(buckets).map(
            |(client, (indices, bucket_files))| {
                let on_chunk_c = on_chunk.clone();
                let on_complete_c = on_complete.clone();
                let indices_for_cb = indices.clone();
                async move {
                    let hashes = client
                        .pipelined_put_files(
                            bucket_files,
                            overwrite,
                            on_chunk_c,
                            move |local_idx, path, size| {
                                let orig_idx = indices_for_cb[local_idx];
                                on_complete_c(orig_idx, path, size);
                            },
                        )
                        .await?;
                    Ok::<(Vec<usize>, Vec<[u8; 32]>), BcmrError>((indices, hashes))
                }
            },
        );

        let results = futures::future::try_join_all(futs).await?;

        let mut out: Vec<Option<[u8; 32]>> = (0..n_files).map(|_| None).collect();
        for (indices, hashes) in results {
            for (idx, hash) in indices.into_iter().zip(hashes) {
                out[idx] = Some(hash);
            }
        }
        Ok(out
            .into_iter()
            .map(|h| h.expect("every slot filled"))
            .collect())
    }

    pub async fn pipelined_get_files_striped<FStart, FChunk>(
        &mut self,
        files: Vec<FileTransfer>,
        sync_after_each: bool,
        verify_before_publish: bool,
        on_file_start: FStart,
        on_chunk: FChunk,
    ) -> Result<(), BcmrError>
    where
        FStart: Fn(usize, &Path, u64) + Send + Sync + Clone + 'static,
        FChunk: Fn(u64) + Send + Sync + Clone + 'static,
    {
        let n_files = files.len();
        if n_files == 0 {
            return Ok(());
        }
        let n_clients = self.clients.len().min(n_files);

        let mut buckets: Vec<(Vec<usize>, Vec<FileTransfer>)> =
            (0..n_clients).map(|_| (Vec::new(), Vec::new())).collect();
        for (i, ft) in files.into_iter().enumerate() {
            let b = &mut buckets[i % n_clients];
            b.0.push(i);
            b.1.push(ft);
        }

        let futs = self.clients.iter_mut().take(n_clients).zip(buckets).map(
            |(client, (indices, bucket_files))| {
                let on_start_c = on_file_start.clone();
                let on_chunk_c = on_chunk.clone();
                async move {
                    client
                        .pipelined_get_files(
                            bucket_files,
                            sync_after_each,
                            verify_before_publish,
                            move |local_idx, path, size| {
                                let orig_idx = indices[local_idx];
                                on_start_c(orig_idx, path, size);
                            },
                            on_chunk_c,
                        )
                        .await
                }
            },
        );

        futures::future::try_join_all(futs).await?;
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn striped_put_file(
        &mut self,
        local: &Path,
        remote: &str,
    ) -> Result<[u8; 32], BcmrError> {
        if self.clients.is_empty() {
            return Err(BcmrError::pool_empty());
        }
        let file_size = tokio::fs::metadata(local).await?.len();
        self.request_truncate(remote, file_size).await?;
        if file_size == 0 {
            return Ok(*blake3::hash(b"").as_bytes());
        }

        let hash_task = spawn_blake3_file(local.to_path_buf());

        let local_owned = local.to_path_buf();
        let remote_owned = remote.to_owned();
        let ranges = divide_ranges(file_size, self.clients.len());
        let futs: Vec<_> = self
            .clients
            .iter_mut()
            .zip(ranges)
            .filter(|(_, (_, length))| *length > 0)
            .map(|(client, (offset, length))| {
                let local = local_owned.clone();
                let remote = remote_owned.clone();
                async move { client.put_chunked(&remote, &local, offset, length).await }
            })
            .collect();
        futures::future::try_join_all(futs).await?;

        hash_task.await.map_err(BcmrError::hash_task_join_failed)?
    }

    #[allow(dead_code)]
    pub async fn striped_get_file(
        &mut self,
        remote: &str,
        local: &Path,
        remote_size: u64,
    ) -> Result<[u8; 32], BcmrError> {
        self.striped_get_file_synced(remote, local, remote_size, false)
            .await
    }

    pub async fn striped_get_file_synced(
        &mut self,
        remote: &str,
        local: &Path,
        remote_size: u64,
        sync_before_publish: bool,
    ) -> Result<[u8; 32], BcmrError> {
        self.striped_get_file_synced_with_metadata(
            remote,
            local,
            remote_size,
            sync_before_publish,
            None,
        )
        .await
    }

    pub async fn striped_get_file_synced_with_metadata(
        &mut self,
        remote: &str,
        local: &Path,
        remote_size: u64,
        sync_before_publish: bool,
        metadata: Option<crate::core::file_metadata::PortableFileMetadata>,
    ) -> Result<[u8; 32], BcmrError> {
        self.striped_get_file_impl(
            remote,
            local,
            remote_size,
            sync_before_publish,
            metadata,
            |_staging_path| {},
        )
        .await
    }

    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub async fn striped_get_file_with_stage_hook<F>(
        &mut self,
        remote: &str,
        local: &Path,
        remote_size: u64,
        before_transfer: F,
    ) -> Result<[u8; 32], BcmrError>
    where
        F: FnOnce(&Path),
    {
        self.striped_get_file_impl(remote, local, remote_size, false, None, before_transfer)
            .await
    }

    async fn striped_get_file_impl<F>(
        &mut self,
        remote: &str,
        local: &Path,
        remote_size: u64,
        sync_before_publish: bool,
        metadata: Option<crate::core::file_metadata::PortableFileMetadata>,
        before_transfer: F,
    ) -> Result<[u8; 32], BcmrError>
    where
        F: FnOnce(&Path),
    {
        if self.clients.is_empty() {
            return Err(BcmrError::pool_empty());
        }
        let staging = AtomicFile::new(local)?;
        let f = staging.try_clone_file()?;
        f.set_len(remote_size)?;
        drop(f);

        let staging_path = staging.staging_path();
        before_transfer(&staging_path);

        let destination = std::sync::Arc::new(staging.try_clone_file()?);
        let remote_owned = remote.to_owned();
        let ranges = divide_ranges(remote_size, self.clients.len());
        let futs: Vec<_> = self
            .clients
            .iter_mut()
            .zip(ranges)
            .enumerate()
            .filter(|(index, (_, (_, length)))| *length > 0 || (remote_size == 0 && *index == 0))
            .map(|(_, (client, (offset, length)))| {
                let destination = std::sync::Arc::clone(&destination);
                let remote = remote_owned.clone();
                async move {
                    client
                        .get_chunked_to_file(&remote, destination, offset, offset, length)
                        .await
                }
            })
            .collect();
        futures::future::try_join_all(futs).await?;
        drop(destination);

        let hash = spawn_blake3_file_handle(staging.try_clone_file()?)
            .await
            .map_err(BcmrError::hash_task_join_failed)??;
        if let Some(metadata) = metadata {
            staging.commit_with_metadata(sync_before_publish, metadata)?;
        } else {
            staging.commit(sync_before_publish)?;
        }
        Ok(hash)
    }

    #[allow(dead_code)]
    async fn request_truncate(&mut self, remote: &str, size: u64) -> Result<(), BcmrError> {
        self.clients[0]
            .request_one(
                "Truncate",
                &Message::Truncate {
                    path: remote.to_owned(),
                    size,
                },
                |m| match m {
                    Message::Ok { .. } => Ok(()),
                    other => Err(other),
                },
            )
            .await
    }

    pub async fn close(mut self) -> Result<(), BcmrError> {
        let futs = self.clients.iter_mut().map(|c| c.close_in_place());
        let _ = futures::future::join_all(futs).await;
        self.clients.clear();
        Ok(())
    }
}

fn divide_ranges(total: u64, n: usize) -> Vec<(u64, u64)> {
    let mut ranges = Vec::with_capacity(n);
    let chunk = total.div_ceil(n as u64);
    let mut offset = 0u64;
    for _ in 0..n {
        let length = chunk.min(total.saturating_sub(offset));
        ranges.push((offset, length));
        offset += length;
    }
    ranges
}

#[allow(dead_code)]
fn spawn_blake3_file(
    path: std::path::PathBuf,
) -> tokio::task::JoinHandle<Result<[u8; 32], BcmrError>> {
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&path)?;
        calculate_blake3_file(file)
    })
}

fn spawn_blake3_file_handle(
    file: std::fs::File,
) -> tokio::task::JoinHandle<Result<[u8; 32], BcmrError>> {
    tokio::task::spawn_blocking(move || calculate_blake3_file(file))
}

fn calculate_blake3_file(mut file: std::fs::File) -> Result<[u8; 32], BcmrError> {
    const READ_CHUNK: usize = 4 * 1024 * 1024;
    use std::io::{Read, Seek};
    file.rewind()?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(*hasher.finalize().as_bytes())
}
