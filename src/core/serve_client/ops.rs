use std::path::Path;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::core::compress;
use crate::core::error::BcmrError;
use crate::core::framing::SendHalf;
use crate::core::protocol::{ListEntry, Message};

use super::{
    decode_hex32, write_file_data_frames, BoxedWriter, ServeClient, DEDUP_BLOCK_SIZE,
    DEDUP_MIN_FILE_SIZE,
};

impl ServeClient {
    pub async fn stat(&mut self, path: &str) -> Result<(u64, u64, bool), BcmrError> {
        self.send(&Message::Stat {
            path: path.to_owned(),
        })
        .await?;
        match self.recv().await? {
            Message::StatResponse {
                size,
                mtime,
                is_dir,
            } => Ok((size, mtime as u64, is_dir)),
            Message::Error { message } => Err(BcmrError::InvalidInput(message)),
            other => Err(BcmrError::InvalidInput(format!(
                "unexpected stat response: {other:?}"
            ))),
        }
    }

    pub async fn list(&mut self, path: &str) -> Result<Vec<ListEntry>, BcmrError> {
        self.send(&Message::List {
            path: path.to_owned(),
        })
        .await?;
        match self.recv().await? {
            Message::ListResponse { entries } => Ok(entries),
            Message::Error { message } => Err(BcmrError::InvalidInput(message)),
            other => Err(BcmrError::InvalidInput(format!(
                "unexpected list response: {other:?}"
            ))),
        }
    }

    pub async fn hash(
        &mut self,
        path: &str,
        offset: u64,
        limit: Option<u64>,
    ) -> Result<[u8; 32], BcmrError> {
        self.send(&Message::Hash {
            path: path.to_owned(),
            offset,
            limit,
        })
        .await?;
        match self.recv().await? {
            Message::HashResponse { hash } => decode_hex32(&hash),
            Message::Error { message } => Err(BcmrError::InvalidInput(message)),
            other => Err(BcmrError::InvalidInput(format!(
                "unexpected hash response: {other:?}"
            ))),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub async fn get(
        &mut self,
        path: &str,
        offset: u64,
        on_data: impl Fn(&[u8]),
    ) -> Result<Option<[u8; 32]>, BcmrError> {
        self.send(&Message::Get {
            path: path.to_owned(),
            offset,
        })
        .await?;
        loop {
            match self.recv().await? {
                Message::Data { payload } => on_data(&payload),
                Message::DataCompressed {
                    algo,
                    original_size,
                    payload,
                } => {
                    let decoded = compress::decode_block(algo, original_size, &payload)?;
                    on_data(&decoded);
                }
                Message::Ok { hash } => {
                    return match hash {
                        Some(h) => decode_hex32(&h).map(Some),
                        None => Ok(None),
                    };
                }
                Message::Error { message } => return Err(BcmrError::InvalidInput(message)),
                other => {
                    return Err(BcmrError::InvalidInput(format!(
                        "unexpected get response: {other:?}"
                    )))
                }
            }
        }
    }

    pub async fn put(&mut self, path: &str, data: &Path) -> Result<[u8; 32], BcmrError> {
        let metadata = tokio::fs::metadata(data).await?;
        let size = metadata.len();

        self.send(&Message::Put {
            path: path.to_owned(),
            size,
        })
        .await?;

        if self.dedup_enabled && size >= DEDUP_MIN_FILE_SIZE {
            self.put_with_dedup(data, size).await?;
        } else {
            self.put_streaming(data).await?;
        }
        self.send(&Message::Done).await?;

        match self.recv().await? {
            Message::Ok { hash: Some(h) } => decode_hex32(&h),
            Message::Ok { hash: None } => {
                Err(BcmrError::InvalidInput("put response missing hash".into()))
            }
            Message::Error { message } => Err(BcmrError::InvalidInput(message)),
            other => Err(BcmrError::InvalidInput(format!(
                "unexpected put response: {other:?}"
            ))),
        }
    }

    async fn put_streaming(&mut self, data: &Path) -> Result<(), BcmrError> {
        let algo = self.algo;
        let w = self
            .writer
            .as_mut()
            .ok_or_else(|| BcmrError::InvalidInput("writer already taken".into()))?;
        let tx = self
            .tx
            .as_mut()
            .ok_or_else(|| BcmrError::InvalidInput("send framing taken".into()))?;
        write_file_data_frames(w, tx, data, algo, &|_| {}).await
    }

    pub async fn put_chunked(
        &mut self,
        remote: &str,
        local: &Path,
        local_offset: u64,
        length: u64,
    ) -> Result<(), BcmrError> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};

        self.send(&Message::PutChunked {
            path: remote.to_owned(),
            offset: local_offset,
            length,
        })
        .await?;

        let algo = self.algo;
        let w = self
            .writer
            .as_mut()
            .ok_or_else(|| BcmrError::InvalidInput("writer already taken".into()))?;
        let tx = self
            .tx
            .as_mut()
            .ok_or_else(|| BcmrError::InvalidInput("send framing taken".into()))?;

        let mut file = File::open(local).await?;
        file.seek(std::io::SeekFrom::Start(local_offset)).await?;
        let mut remaining = length;
        let mut buf = vec![0u8; DEDUP_BLOCK_SIZE];
        while remaining > 0 {
            let want = remaining.min(DEDUP_BLOCK_SIZE as u64) as usize;
            let n = file.read(&mut buf[..want]).await?;
            if n == 0 {
                return Err(BcmrError::InvalidInput(format!(
                    "local source truncated: expected {length} bytes from offset {local_offset}"
                )));
            }
            let frame = compress::encode_block(algo, buf[..n].to_vec());
            tx.write_message(w, &frame).await?;
            remaining -= n as u64;
        }
        tx.write_message(w, &Message::Done).await?;
        w.flush().await?;

        match self.recv().await? {
            Message::Ok { .. } => Ok(()),
            Message::Error { message } => Err(BcmrError::InvalidInput(message)),
            other => Err(BcmrError::InvalidInput(format!(
                "unexpected reply to PutChunked: {other:?}"
            ))),
        }
    }

    pub async fn get_chunked(
        &mut self,
        remote: &str,
        local: &Path,
        remote_offset: u64,
        local_offset: u64,
        length: u64,
    ) -> Result<(), BcmrError> {
        use tokio::io::{AsyncSeekExt, AsyncWriteExt as _};

        self.send(&Message::GetChunked {
            path: remote.to_owned(),
            offset: remote_offset,
            length,
        })
        .await?;

        let mut dst = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(local)
            .await?;
        dst.seek(std::io::SeekFrom::Start(local_offset)).await?;

        let mut written = 0u64;
        loop {
            match self.recv().await? {
                Message::Data { payload } => {
                    if written + payload.len() as u64 > length {
                        return Err(BcmrError::InvalidInput(format!(
                            "get_chunked: server sent {} bytes past the requested {}",
                            written + payload.len() as u64 - length,
                            length
                        )));
                    }
                    dst.write_all(&payload).await?;
                    written += payload.len() as u64;
                }
                Message::DataCompressed {
                    algo,
                    original_size,
                    payload,
                } => {
                    let decoded = compress::decode_block(algo, original_size, &payload)?;
                    if written + decoded.len() as u64 > length {
                        return Err(BcmrError::InvalidInput(format!(
                            "get_chunked: server sent {} bytes past the requested {}",
                            written + decoded.len() as u64 - length,
                            length
                        )));
                    }
                    dst.write_all(&decoded).await?;
                    written += decoded.len() as u64;
                }
                Message::Ok { .. } => {
                    if written != length {
                        return Err(BcmrError::InvalidInput(format!(
                            "get_chunked: expected {length} bytes, got {written}"
                        )));
                    }
                    return Ok(());
                }
                Message::Error { message } => return Err(BcmrError::InvalidInput(message)),
                other => {
                    return Err(BcmrError::InvalidInput(format!(
                        "unexpected reply to GetChunked: {other:?}"
                    )))
                }
            }
        }
    }

    async fn put_with_dedup(&mut self, data: &Path, size: u64) -> Result<(), BcmrError> {
        let hashes = compute_block_hashes(data, size).await?;
        let n_blocks = hashes.len();

        self.send(&Message::HaveBlocks {
            block_size: DEDUP_BLOCK_SIZE as u32,
            hashes,
        })
        .await?;

        let bits = match self.recv().await? {
            Message::MissingBlocks { bits } => bits,
            Message::Error { message } => return Err(BcmrError::InvalidInput(message)),
            other => {
                return Err(BcmrError::InvalidInput(format!(
                    "expected MissingBlocks, got {other:?}"
                )))
            }
        };

        let mut file = File::open(data).await?;
        let mut buf = vec![0u8; DEDUP_BLOCK_SIZE];
        for idx in 0..n_blocks {
            let mut filled = 0;
            while filled < DEDUP_BLOCK_SIZE {
                let n = file.read(&mut buf[filled..]).await?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            if filled == 0 {
                break;
            }
            if (bits.get(idx / 8).copied().unwrap_or(0) >> (idx % 8)) & 1 == 1 {
                let frame = compress::encode_block(self.algo, buf[..filled].to_vec());
                self.send(&frame).await?;
            }
        }
        Ok(())
    }

    pub(super) fn take_writer(&mut self) -> Result<(BoxedWriter, SendHalf), BcmrError> {
        let writer = self.writer.take().ok_or_else(|| {
            BcmrError::InvalidInput("writer already taken (concurrent op?)".into())
        })?;
        let tx = self.tx.take().ok_or_else(|| {
            BcmrError::InvalidInput("send framing already taken (concurrent op?)".into())
        })?;
        Ok((writer, tx))
    }

    pub(super) async fn reap_writer_task(
        &mut self,
        writer: tokio::task::JoinHandle<Result<(BoxedWriter, SendHalf), BcmrError>>,
        reader_errored: bool,
    ) -> Result<(), BcmrError> {
        if reader_errored {
            writer.abort();
        }
        match writer.await {
            Ok(Ok((writer_back, tx_back))) => {
                self.writer = Some(writer_back);
                self.tx = Some(tx_back);
                Ok(())
            }
            Ok(Err(writer_err)) => {
                if reader_errored {
                    Ok(())
                } else {
                    Err(writer_err)
                }
            }
            Err(join_err) if !join_err.is_cancelled() => Err(BcmrError::InvalidInput(format!(
                "writer task join failed: {join_err}"
            ))),
            Err(_) => Ok(()),
        }
    }

    pub async fn mkdir(&mut self, path: &str) -> Result<(), BcmrError> {
        self.send(&Message::Mkdir {
            path: path.to_owned(),
        })
        .await?;
        match self.recv().await? {
            Message::Ok { .. } => Ok(()),
            Message::Error { message } => Err(BcmrError::InvalidInput(message)),
            other => Err(BcmrError::InvalidInput(format!(
                "unexpected mkdir response: {other:?}"
            ))),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub async fn resume_check(&mut self, path: &str) -> Result<(u64, Option<[u8; 32]>), BcmrError> {
        self.send(&Message::Resume {
            path: path.to_owned(),
        })
        .await?;
        match self.recv().await? {
            Message::ResumeResponse { size, block_hash } => {
                let hash = match block_hash {
                    Some(h) => Some(decode_hex32(&h)?),
                    None => None,
                };
                Ok((size, hash))
            }
            Message::Error { message } => Err(BcmrError::InvalidInput(message)),
            other => Err(BcmrError::InvalidInput(format!(
                "unexpected resume response: {other:?}"
            ))),
        }
    }

    pub async fn close(mut self) -> Result<(), BcmrError> {
        self.writer.take();
        self.transport.wait().await;
        Ok(())
    }

    pub(super) async fn close_in_place(&mut self) -> Result<(), BcmrError> {
        self.writer.take();
        self.transport.wait().await;
        Ok(())
    }
}

async fn compute_block_hashes(path: &Path, size: u64) -> Result<Vec<[u8; 32]>, BcmrError> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<Vec<[u8; 32]>, std::io::Error> {
        use std::io::Read;
        let mut file = std::fs::File::open(&path)?;
        let n_blocks = (size).div_ceil(DEDUP_BLOCK_SIZE as u64) as usize;
        let mut hashes = Vec::with_capacity(n_blocks);
        let mut buf = vec![0u8; DEDUP_BLOCK_SIZE];
        loop {
            let mut filled = 0;
            while filled < DEDUP_BLOCK_SIZE {
                let n = file.read(&mut buf[filled..])?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            if filled == 0 {
                break;
            }
            let mut h = [0u8; 32];
            h.copy_from_slice(blake3::hash(&buf[..filled]).as_bytes());
            hashes.push(h);
            if filled < DEDUP_BLOCK_SIZE {
                break;
            }
        }
        Ok(hashes)
    })
    .await
    .map_err(|e| BcmrError::InvalidInput(format!("hash task panicked: {}", e)))?
    .map_err(BcmrError::Io)
}
