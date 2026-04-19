use std::path::Path;

use crate::core::compress;
use crate::core::error::BcmrError;
use crate::core::framing::SendHalf;
use crate::core::protocol::Message;

use super::{decode_hex32, write_file_data_frames, BoxedWriter, FileTransfer, ServeClient};

impl ServeClient {
    pub async fn pipelined_put_files<FChunk, FComplete>(
        &mut self,
        files: Vec<FileTransfer>,
        on_chunk: FChunk,
        on_complete: FComplete,
    ) -> Result<Vec<[u8; 32]>, BcmrError>
    where
        FChunk: Fn(u64) + Send + Sync + 'static,
        FComplete: FnMut(usize, &Path, u64),
    {
        let r = self
            .pipelined_put_files_imp(files, on_chunk, on_complete)
            .await;
        if r.is_err() {
            self.poisoned = true;
        }
        r
    }

    async fn pipelined_put_files_imp<FChunk, FComplete>(
        &mut self,
        files: Vec<FileTransfer>,
        on_chunk: FChunk,
        mut on_complete: FComplete,
    ) -> Result<Vec<[u8; 32]>, BcmrError>
    where
        FChunk: Fn(u64) + Send + Sync + 'static,
        FComplete: FnMut(usize, &Path, u64),
    {
        let n = files.len();
        if n == 0 {
            return Ok(Vec::new());
        }

        let (mut wire, mut tx) = self.take_writer()?;
        let algo = self.algo;
        let progress: Vec<(std::path::PathBuf, u64)> =
            files.iter().map(|f| (f.local.clone(), f.size)).collect();

        let writer_task: tokio::task::JoinHandle<Result<(BoxedWriter, SendHalf), BcmrError>> =
            tokio::spawn(async move {
                for ft in files {
                    tx.write_message(
                        &mut wire,
                        &Message::Put {
                            path: ft.remote,
                            size: ft.size,
                            offset: 0,
                        },
                    )
                    .await?;
                    write_file_data_frames(&mut wire, &mut tx, &ft.local, algo, &on_chunk).await?;
                    tx.write_message(&mut wire, &Message::Done).await?;
                }
                Ok((wire, tx))
            });

        let mut hashes: Vec<[u8; 32]> = Vec::with_capacity(n);
        let mut recv_err: Option<BcmrError> = None;
        for (i, (path, size)) in progress.iter().enumerate() {
            match self.recv().await {
                Ok(Message::Ok { hash: Some(h) }) => match decode_hex32(&h) {
                    Ok(arr) => {
                        on_complete(i, path, *size);
                        hashes.push(arr);
                    }
                    Err(e) => {
                        recv_err = Some(e);
                        break;
                    }
                },
                Ok(Message::Ok { hash: None }) => {
                    recv_err = Some(BcmrError::InvalidInput("put response missing hash".into()));
                    break;
                }
                Ok(Message::Error { message }) => {
                    recv_err = Some(BcmrError::InvalidInput(message));
                    break;
                }
                Ok(other) => {
                    recv_err = Some(BcmrError::InvalidInput(format!(
                        "unexpected put response: {other:?}"
                    )));
                    break;
                }
                Err(e) => {
                    recv_err = Some(e);
                    break;
                }
            }
        }

        self.reap_writer_task(writer_task, recv_err.is_some())
            .await?;

        if let Some(e) = recv_err {
            return Err(e);
        }
        Ok(hashes)
    }

    pub async fn pipelined_get_files<FStart, FChunk>(
        &mut self,
        files: Vec<FileTransfer>,
        sync_after_each: bool,
        on_file_start: FStart,
        on_chunk: FChunk,
    ) -> Result<(), BcmrError>
    where
        FStart: FnMut(usize, &Path, u64),
        FChunk: FnMut(u64),
    {
        let r = self
            .pipelined_get_files_imp(files, sync_after_each, on_file_start, on_chunk)
            .await;
        if r.is_err() {
            self.poisoned = true;
        }
        r
    }

    async fn pipelined_get_files_imp<FStart, FChunk>(
        &mut self,
        files: Vec<FileTransfer>,
        sync_after_each: bool,
        mut on_file_start: FStart,
        mut on_chunk: FChunk,
    ) -> Result<(), BcmrError>
    where
        FStart: FnMut(usize, &Path, u64),
        FChunk: FnMut(u64),
    {
        let n = files.len();
        if n == 0 {
            return Ok(());
        }

        let (mut wire, mut tx) = self.take_writer()?;

        let request_paths: Vec<String> = files.iter().map(|f| f.remote.clone()).collect();

        let writer_task: tokio::task::JoinHandle<Result<(BoxedWriter, SendHalf), BcmrError>> =
            tokio::spawn(async move {
                for path in request_paths {
                    tx.write_message(&mut wire, &Message::Get { path, offset: 0 })
                        .await?;
                }
                Ok((wire, tx))
            });

        let mut recv_err: Option<BcmrError> = None;
        'files_loop: for (i, ft) in files.iter().enumerate() {
            if let Some(parent) = ft.local.parent() {
                if !parent.as_os_str().is_empty() && !parent.exists() {
                    if let Err(e) = tokio::fs::create_dir_all(parent).await {
                        recv_err = Some(BcmrError::InvalidInput(format!(
                            "create parent for {}: {e}",
                            ft.local.display()
                        )));
                        break;
                    }
                }
            }
            let mut dst = match std::fs::File::create(&ft.local) {
                Ok(f) => f,
                Err(e) => {
                    recv_err = Some(BcmrError::InvalidInput(format!(
                        "create dst {}: {e}",
                        ft.local.display()
                    )));
                    break;
                }
            };
            on_file_start(i, &ft.local, ft.size);

            let mut received: u64 = 0;
            loop {
                use std::io::Write;
                match self.recv().await {
                    Ok(Message::Data { payload }) => {
                        let n_bytes = payload.len() as u64;
                        if received + n_bytes > ft.size {
                            recv_err = Some(BcmrError::InvalidInput(format!(
                                "server sent {} bytes past declared size {} for {}",
                                received + n_bytes - ft.size,
                                ft.size,
                                ft.local.display()
                            )));
                            break 'files_loop;
                        }
                        if let Err(e) = dst.write_all(&payload) {
                            recv_err = Some(BcmrError::InvalidInput(format!("write dst: {e}")));
                            break 'files_loop;
                        }
                        received += n_bytes;
                        on_chunk(n_bytes);
                    }
                    Ok(Message::DataCompressed {
                        algo,
                        original_size,
                        payload,
                    }) => {
                        let decoded = match compress::decode_block(algo, original_size, &payload) {
                            Ok(d) => d,
                            Err(e) => {
                                recv_err =
                                    Some(BcmrError::InvalidInput(format!("decompress: {e}")));
                                break 'files_loop;
                            }
                        };
                        let n_bytes = decoded.len() as u64;
                        if received + n_bytes > ft.size {
                            recv_err = Some(BcmrError::InvalidInput(format!(
                                "server sent {} bytes past declared size {} for {}",
                                received + n_bytes - ft.size,
                                ft.size,
                                ft.local.display()
                            )));
                            break 'files_loop;
                        }
                        if let Err(e) = dst.write_all(&decoded) {
                            recv_err = Some(BcmrError::InvalidInput(format!("write dst: {e}")));
                            break 'files_loop;
                        }
                        received += n_bytes;
                        on_chunk(n_bytes);
                    }
                    Ok(Message::Ok { .. }) => {
                        if received != ft.size {
                            recv_err = Some(BcmrError::InvalidInput(format!(
                                "short read: got {received} bytes, declared {} for {}",
                                ft.size,
                                ft.local.display()
                            )));
                            break 'files_loop;
                        }
                        if sync_after_each {
                            if let Err(e) = dst.sync_all() {
                                recv_err = Some(BcmrError::InvalidInput(format!("fsync dst: {e}")));
                                break 'files_loop;
                            }
                        }
                        drop(dst);
                        break;
                    }
                    Ok(Message::Error { message }) => {
                        recv_err = Some(BcmrError::InvalidInput(message));
                        break 'files_loop;
                    }
                    Ok(other) => {
                        recv_err = Some(BcmrError::InvalidInput(format!(
                            "unexpected get response: {other:?}"
                        )));
                        break 'files_loop;
                    }
                    Err(e) => {
                        recv_err = Some(e);
                        break 'files_loop;
                    }
                }
            }
        }

        self.reap_writer_task(writer_task, recv_err.is_some())
            .await?;

        if let Some(e) = recv_err {
            return Err(e);
        }
        Ok(())
    }
}
