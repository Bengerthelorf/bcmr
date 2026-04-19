use std::path::Path;

use crate::core::error::BcmrError;
use crate::core::protocol::Message;

use super::{FileTransfer, ServeClient};

pub struct ServeClientPool {
    clients: Vec<ServeClient>,
}

impl ServeClientPool {
    pub async fn connect_with_caps(
        ssh_target: &str,
        caps: u8,
        n: usize,
    ) -> Result<Self, BcmrError> {
        if n == 0 {
            return Err(BcmrError::InvalidInput("pool size must be >= 1".into()));
        }
        let target = ssh_target.to_owned();
        let futures: Vec<_> = (0..n)
            .map(|_| {
                let t = target.clone();
                async move { ServeClient::connect_with_caps(&t, caps).await }
            })
            .collect();
        let clients = futures::future::try_join_all(futures).await?;
        Ok(Self { clients })
    }

    pub async fn connect_direct_with_caps(
        ssh_target: &str,
        caps: u8,
        n: usize,
    ) -> Result<Self, BcmrError> {
        if n == 0 {
            return Err(BcmrError::InvalidInput("pool size must be >= 1".into()));
        }
        let target = ssh_target.to_owned();
        let futures: Vec<_> = (0..n)
            .map(|_| {
                let t = target.clone();
                async move { ServeClient::connect_direct_with_caps(&t, caps).await }
            })
            .collect();
        let clients = futures::future::try_join_all(futures).await?;
        Ok(Self { clients })
    }

    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub async fn connect_direct_local(n: usize) -> Result<Self, BcmrError> {
        if n == 0 {
            return Err(BcmrError::InvalidInput("pool size must be >= 1".into()));
        }
        let futures: Vec<_> = (0..n)
            .map(|_| ServeClient::connect_direct_local())
            .collect();
        let clients = futures::future::try_join_all(futures).await?;
        Ok(Self { clients })
    }

    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub async fn connect_local(n: usize) -> Result<Self, BcmrError> {
        if n == 0 {
            return Err(BcmrError::InvalidInput("pool size must be >= 1".into()));
        }
        let futures: Vec<_> = (0..n).map(|_| ServeClient::connect_local()).collect();
        let clients = futures::future::try_join_all(futures).await?;
        Ok(Self { clients })
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

    pub async fn striped_put_file(
        &mut self,
        local: &Path,
        remote: &str,
    ) -> Result<[u8; 32], BcmrError> {
        if self.clients.is_empty() {
            return Err(BcmrError::InvalidInput("pool is empty".into()));
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

        hash_task
            .await
            .map_err(|e| BcmrError::InvalidInput(format!("hash task join: {e}")))?
    }

    pub async fn striped_get_file(
        &mut self,
        remote: &str,
        local: &Path,
        remote_size: u64,
    ) -> Result<[u8; 32], BcmrError> {
        if self.clients.is_empty() {
            return Err(BcmrError::InvalidInput("pool is empty".into()));
        }
        let f = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(local)
            .await?;
        f.set_len(remote_size).await?;
        drop(f);

        let local_owned = local.to_path_buf();
        let remote_owned = remote.to_owned();
        let ranges = divide_ranges(remote_size, self.clients.len());
        let futs: Vec<_> = self
            .clients
            .iter_mut()
            .zip(ranges)
            .filter(|(_, (_, length))| *length > 0)
            .map(|(client, (offset, length))| {
                let local = local_owned.clone();
                let remote = remote_owned.clone();
                async move {
                    client
                        .get_chunked(&remote, &local, offset, offset, length)
                        .await
                }
            })
            .collect();
        futures::future::try_join_all(futs).await?;

        spawn_blake3_file(local.to_path_buf())
            .await
            .map_err(|e| BcmrError::InvalidInput(format!("hash task join: {e}")))?
    }

    async fn request_truncate(&mut self, remote: &str, size: u64) -> Result<(), BcmrError> {
        self.clients[0]
            .send(&Message::Truncate {
                path: remote.to_owned(),
                size,
            })
            .await?;
        match self.clients[0].recv().await? {
            Message::Ok { .. } => Ok(()),
            Message::Error { message } => Err(BcmrError::InvalidInput(message)),
            other => Err(BcmrError::InvalidInput(format!(
                "unexpected reply to Truncate: {other:?}"
            ))),
        }
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

fn spawn_blake3_file(
    path: std::path::PathBuf,
) -> tokio::task::JoinHandle<Result<[u8; 32], BcmrError>> {
    const READ_CHUNK: usize = 4 * 1024 * 1024;
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut f = std::fs::File::open(&path)?;
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(*hasher.finalize().as_bytes())
    })
}
