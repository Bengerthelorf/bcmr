use std::path::Path;

use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Child;

use crate::core::compress;
use crate::core::error::BcmrError;
use crate::core::protocol::{
    self, CompressionAlgo, ListEntry, Message, CAP_DEDUP, CAP_LZ4, CAP_ZSTD, PROTOCOL_VERSION,
};
use crate::core::transport::ssh as ssh_transport;

type BoxedReader = Box<dyn AsyncRead + Send + Unpin>;
type BoxedWriter = Box<dyn AsyncWrite + Send + Unpin>;

/// Transport-specific cleanup state. The byte streams themselves live
/// as `reader`/`writer` fields on `ServeClient` (type-erased so every
/// transport goes through the same protocol framing code), but the
/// kill-on-drop handle is transport-specific — SSH needs to terminate
/// the child process; a plain TCP connection would just drop the
/// socket.
enum Transport {
    Ssh { child: Child },
}

/// Default client caps: LZ4 + Zstd + dedup. CAP_FAST is opt-in via
/// `--fast` because the trade-off (no server-side hash) only makes
/// sense when the user has another way to verify integrity.
const CLIENT_CAPS: u8 = CAP_LZ4 | CAP_ZSTD | CAP_DEDUP;

/// Files smaller than this skip the dedup pre-flight: the round-trip
/// cost of HaveBlocks/MissingBlocks dominates for tiny payloads.
const DEDUP_MIN_FILE_SIZE: u64 = 16 * 1024 * 1024;
const DEDUP_BLOCK_SIZE: usize = 4 * 1024 * 1024;

pub struct ServeClient {
    transport: Transport,
    reader: BoxedReader,
    writer: Option<BoxedWriter>,
    algo: CompressionAlgo,
    dedup_enabled: bool,
}

/// One file's worth of transfer metadata. Used as the batch input to
/// `pipelined_put_files` / `pipelined_get_files` so callers get
/// self-documenting field names instead of the `(.0, .1, .2)` mystery
/// that an ad-hoc tuple would force.
#[derive(Debug, Clone)]
pub struct FileTransfer {
    /// Path on the server side. For PUT this is where the server
    /// writes to; for GET it's where the server reads from.
    pub remote: String,
    /// Path on the client side. For PUT this is the source file to
    /// read; for GET it's the destination file to create.
    pub local: std::path::PathBuf,
    /// Size of the file as the client understands it. For PUT this
    /// goes into the `Put { size }` declaration the server uses to
    /// bound incoming writes; for GET this is the server-reported
    /// size the client uses to validate the received stream.
    pub size: u64,
}

impl ServeClient {
    /// Connect with default capabilities (advertise all compression the
    /// client supports). Prefer `connect_with_caps` when the caller has
    /// a user-specified `--compress` value to honour.
    pub async fn connect(ssh_target: &str) -> Result<Self, BcmrError> {
        Self::connect_with_caps(ssh_target, CLIENT_CAPS).await
    }

    pub async fn connect_with_caps(ssh_target: &str, caps: u8) -> Result<Self, BcmrError> {
        let spawn = ssh_transport::spawn_remote(ssh_target).await?;
        let mut client = Self::from_ssh_spawn(spawn);
        client.handshake(caps).await?;
        Ok(client)
    }

    /// Test-only connect that lets the caller dictate the caps byte
    /// (default `connect_local` advertises everything except CAP_FAST).
    #[allow(dead_code)]
    pub async fn connect_local_with_caps(caps: u8) -> Result<Self, BcmrError> {
        let mut client = Self::spawn_local_serve().await?;
        client.handshake(caps).await?;
        Ok(client)
    }

    #[allow(dead_code)] // used by integration tests
    pub async fn connect_local() -> Result<Self, BcmrError> {
        let mut client = Self::spawn_local_serve().await?;
        client.handshake(CLIENT_CAPS).await?;
        Ok(client)
    }

    #[allow(dead_code)]
    async fn spawn_local_serve() -> Result<Self, BcmrError> {
        // Find the bcmr binary in the same directory as the test binary.
        // current_exe() returns the test binary itself, not bcmr.
        let exe = std::env::current_exe()?;
        let bin_dir = exe
            .parent()
            .ok_or_else(|| BcmrError::InvalidInput("cannot find binary directory".into()))?;

        let bin_name = if cfg!(windows) { "bcmr.exe" } else { "bcmr" };

        let candidates = [
            bin_dir.join(bin_name),
            bin_dir
                .parent()
                .map(|p| p.join(bin_name))
                .unwrap_or_default(),
        ];
        let bcmr_path = candidates
            .iter()
            .find(|p| p.exists())
            .ok_or_else(|| {
                BcmrError::InvalidInput(format!(
                    "bcmr binary not found at {} or {}",
                    candidates[0].display(),
                    candidates[1].display()
                ))
            })?
            .clone();

        let spawn = ssh_transport::spawn_local(&bcmr_path).await?;
        Ok(Self::from_ssh_spawn(spawn))
    }

    fn from_ssh_spawn(spawn: ssh_transport::SshSpawn) -> Self {
        Self {
            transport: Transport::Ssh { child: spawn.child },
            reader: Box::new(spawn.stdout),
            writer: Some(Box::new(spawn.stdin)),
            algo: CompressionAlgo::None,
            dedup_enabled: false,
        }
    }

    async fn handshake(&mut self, caps: u8) -> Result<(), BcmrError> {
        self.send(&Message::Hello {
            version: PROTOCOL_VERSION,
            caps,
        })
        .await?;
        match self.recv().await? {
            Message::Welcome {
                caps: server_caps, ..
            } => {
                self.algo = CompressionAlgo::negotiate(caps, server_caps);
                self.dedup_enabled = (caps & server_caps & CAP_DEDUP) != 0;
                Ok(())
            }
            Message::Error { message } => Err(BcmrError::InvalidInput(message)),
            other => Err(BcmrError::InvalidInput(format!(
                "unexpected handshake response: {other:?}"
            ))),
        }
    }

    /// Override the caps the client will advertise on the next handshake
    /// (takes effect only if called before `connect*`, which currently
    /// isn't exposed — kept as an internal knob for tests).
    #[allow(dead_code)]
    pub fn negotiated_algo(&self) -> CompressionAlgo {
        self.algo
    }

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

    #[allow(dead_code)] // used by integration tests
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

    /// Single-file GET. Kept for the e2e test surface (each test still
    /// drives one file end-to-end through the protocol). Production
    /// download path uses `pipelined_get_files`, which sends all GET
    /// requests up-front so request and reply streams overlap.
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
        // Single-file put() has no batch-level progress channel; skip
        // reporting here and let the caller drive progress via their own
        // means (the single-file CLI path already owns a per-byte meter).
        write_file_data_frames(w, data, algo, &|_| {}).await
    }

    async fn put_with_dedup(&mut self, data: &Path, size: u64) -> Result<(), BcmrError> {
        // Compute one hash per block. We re-read the file later for the
        // actual transfer of missing blocks; the alternative (hash and
        // buffer everything in memory) caps file size at RAM.
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
            // tokio's read() may return short; loop until we have a full
            // block or hit EOF (last block can be partial).
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

    /// Take ownership of the write half for a pipelined operation.
    /// Pairs with `reap_writer_task`, which puts it back (or surfaces
    /// the writer's failure).
    fn take_writer(&mut self) -> Result<BoxedWriter, BcmrError> {
        self.writer
            .take()
            .ok_or_else(|| BcmrError::InvalidInput("writer already taken (concurrent op?)".into()))
    }

    /// Common tail for the two pipelined methods. The reader task is the
    /// caller; the writer task was spawned by the caller right after
    /// `take_stdin`. This helper handles the three-way join:
    ///
    /// - if the reader bailed mid-batch (`reader_errored`), the writer
    ///   may still be pushing frames into a connection whose reply side
    ///   is full — abort it before awaiting to avoid a deadlock where
    ///   the writer blocks on stdin (because SSH socket buffers are full
    ///   because the server is blocked on stdout because nobody is
    ///   reading the replies anymore).
    /// - on clean success both sides finish, we reclaim stdin, caller
    ///   can reuse the client.
    /// - on writer-only failure (reader clean), propagate the writer
    ///   error — the reader saw the EOF but didn't flag it because the
    ///   happy-path recv got the EOF *after* collecting all expected
    ///   replies, so recv_err is `None` even though something went
    ///   wrong upstream.
    async fn reap_writer_task(
        &mut self,
        writer: tokio::task::JoinHandle<Result<BoxedWriter, BcmrError>>,
        reader_errored: bool,
    ) -> Result<(), BcmrError> {
        if reader_errored {
            writer.abort();
        }
        match writer.await {
            Ok(Ok(writer_back)) => {
                self.writer = Some(writer_back);
                Ok(())
            }
            Ok(Err(writer_err)) => {
                if reader_errored {
                    // reader's error is the "proximate cause"; the writer
                    // I/O error is almost certainly the same wire closing
                    // under it. Return Ok here and let the caller surface
                    // the reader error.
                    Ok(())
                } else {
                    Err(writer_err)
                }
            }
            Err(join_err) if !join_err.is_cancelled() => Err(BcmrError::InvalidInput(format!(
                "writer task join failed: {join_err}"
            ))),
            // Cancelled by our abort() — expected when reader_errored.
            Err(_) => Ok(()),
        }
    }

    /// Stream-pipeline many small file PUTs over the same connection.
    ///
    /// The single-file `put()` is strict request-response: send Put +
    /// Data\* + Done, await Ok, then start the next file. On a 10000-file
    /// batch each file pays one full client→server→client round trip
    /// regardless of how cheap the actual fdatasync is on the wire ---
    /// SSH transport buffers don't help when the *client* is the one
    /// blocking. SFTP closes the same gap for `scp -r` by keeping a
    /// window of in-flight requests; our protocol's dispatch loop is
    /// already FIFO and out-of-order-safe for replies, so we just need
    /// to stop waiting on the client side.
    ///
    /// Implementation: spawn a writer task that owns `stdin` and emits
    /// `Put / Data* / Done` for every file in send order. The reader
    /// (this task) reads `Ok / Error` frames in matching FIFO order.
    /// The OS pipe between client stdin and the SSH child plus SSH's
    /// own send window provide natural backpressure when the server
    /// hasn't drained yet — no explicit channel needed.
    ///
    /// Skips the dedup pre-flight unconditionally (dedup needs a
    /// HaveBlocks/MissingBlocks round trip per file, which would
    /// re-introduce the very serialization we're trying to remove).
    /// Caller should keep large files (where dedup wins) on the
    /// per-file `put()` path.
    ///
    /// Bails on the first error (writer-side I/O failure, server `Error`
    /// frame, or unexpected message). After a failure the connection is
    /// left in an indeterminate state — the writer task may have
    /// half-flushed a frame, the server may have queued unread replies —
    /// so the caller should drop the client rather than try another op.
    ///
    /// `on_chunk(chunk_bytes)` fires once per Data frame the writer
    /// emits, so a long-running upload can update a progress bar in
    /// real time instead of jumping at file completion. `on_complete`
    /// fires exactly once per file after its Ok is received. Both
    /// callbacks must be `Send + Sync + 'static` because `on_chunk` is
    /// moved into the writer task.
    pub async fn pipelined_put_files<FChunk, FComplete>(
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

        let mut wire = self.take_writer()?;
        let algo = self.algo;
        // Reader-side views of the input (just what on_complete needs).
        // The writer task consumes `files` so we snapshot here.
        let progress: Vec<(std::path::PathBuf, u64)> =
            files.iter().map(|f| (f.local.clone(), f.size)).collect();

        let writer_task: tokio::task::JoinHandle<Result<BoxedWriter, BcmrError>> =
            tokio::spawn(async move {
                for ft in files {
                    protocol::write_message(
                        &mut wire,
                        &Message::Put {
                            path: ft.remote,
                            size: ft.size,
                        },
                    )
                    .await?;
                    write_file_data_frames(&mut wire, &ft.local, algo, &on_chunk).await?;
                    protocol::write_message(&mut wire, &Message::Done).await?;
                }
                Ok(wire)
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

    /// Stream-pipeline many GET requests over the same connection.
    ///
    /// Mirror image of `pipelined_put_files`: spawn a writer task that
    /// emits all `Get { path }` requests in send order while this task
    /// (the reader) consumes the resulting `Data* / Ok` streams and
    /// writes each file's bytes to its destination. Server's dispatch
    /// loop is FIFO so reply ordering matches send ordering — no
    /// per-message correlation id needed.
    ///
    /// `on_file_start(idx, path, declared_size)` fires just before each
    /// file's Data stream begins (after the dst is created). `on_chunk`
    /// fires per Data frame with the decoded byte count — pair these to
    /// drive a progress bar that doesn't stall on large files.
    /// `sync_after_each` gates `sync_all()` on the dst before moving to
    /// the next file; matches the per-PUT fsync behavior under
    /// `CAP_SYNC`.
    ///
    /// **Validates `ft.size` against the received stream**: if the
    /// server sends more bytes than declared, or `Ok` arrives before
    /// enough bytes did, the call bails with an error rather than
    /// silently truncating the dst. This catches server misbehavior and
    /// inadvertent truncation at the protocol boundary.
    ///
    /// Bails on the first `Error` frame or I/O failure. Partial state
    /// survives on disk: files completed before the bail are intact,
    /// the file in progress is left half-written at its final path
    /// (no `.tmp` + rename dance here — inherited behavior from the
    /// pre-pipelining loop). The signal-handler-driven
    /// `cleanup_partial_files` hook only knows about `TempFileGuard`
    /// registrations, so Ctrl+C mid-batch leaves the current file
    /// truncated until a retry overwrites it. Worth fixing when an
    /// actual user reports it.
    pub async fn pipelined_get_files<FStart, FChunk>(
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

        let mut wire = self.take_writer()?;

        let request_paths: Vec<String> = files.iter().map(|f| f.remote.clone()).collect();

        // Writer task: send all N Get requests in order. Each request is
        // ~30 bytes; even at N=10000 the cumulative ~300 KiB fits in the
        // OS pipe + SSH socket buffer with room to spare. If the buffer
        // *did* fill, write_message().await would naturally backpressure,
        // so we don't bother with an explicit channel.
        let writer_task: tokio::task::JoinHandle<Result<BoxedWriter, BcmrError>> =
            tokio::spawn(async move {
                for path in request_paths {
                    protocol::write_message(&mut wire, &Message::Get { path, offset: 0 }).await?;
                }
                Ok(wire)
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
            // Intentional: sync `std::fs::File` + sync `write_all` inside
            // an async fn. `tokio::fs` would wrap every read/write in its
            // own spawn_blocking — for 4 MiB chunks on NVMe that's thread
            // bounces dominating the actual I/O (same anti-pattern as
            // local-perf Exp 13). Keeping the writes sync in-task is
            // measurably faster here.
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
                        break; // next file
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

    #[allow(dead_code)] // used by integration tests
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
        // Drop stdin to send EOF → server exits cleanly.
        // After wait(), the Drop impl's start_kill() is a harmless no-op
        // on an already-exited child.
        self.writer.take();
        self.transport.wait().await;
        Ok(())
    }

    /// Hook for the pool's `close`: drop writer + wait without consuming
    /// self, so the pool can drive the same shutdown across N clients
    /// via `&mut self` iteration instead of by-value consumption.
    async fn close_in_place(&mut self) -> Result<(), BcmrError> {
        self.writer.take();
        self.transport.wait().await;
        Ok(())
    }

    async fn send(&mut self, msg: &Message) -> Result<(), BcmrError> {
        let w = self
            .writer
            .as_mut()
            .ok_or_else(|| BcmrError::InvalidInput("connection closed".into()))?;
        protocol::write_message(w, msg).await?;
        w.flush().await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Message, BcmrError> {
        protocol::read_message(&mut self.reader)
            .await?
            .ok_or_else(|| BcmrError::InvalidInput("server closed connection unexpectedly".into()))
    }
}

impl Transport {
    async fn wait(&mut self) {
        match self {
            Transport::Ssh { child } => {
                let _ = child.wait().await;
            }
        }
    }

    fn start_kill(&mut self) {
        match self {
            Transport::Ssh { child } => {
                let _ = child.start_kill();
            }
        }
    }
}

/// Kill child process on drop to prevent orphaned bcmr serve processes.
impl Drop for ServeClient {
    fn drop(&mut self) {
        self.transport.start_kill();
    }
}

/// A pool of N parallel `ServeClient` connections to the same remote.
///
/// Rationale: SSH gives us one cipher stream per TCP connection. AES-NI
/// tops out at ~500 MB/s/core; ChaCha20 at ~200-500. A single
/// `ServeClient` is therefore wall-clocked by a single crypto thread on
/// the server and another on the client. Opening N connections
/// side-by-side gives us N independent cipher streams → up to N× the
/// crypto ceiling, until the NIC, disk, or per-file syscall overhead
/// takes over. This is the `mscp`-style "just open more ssh" trick, no
/// protocol change required.
///
/// The pool exposes "striped" variants of the pipelined batch methods:
/// the input file list is partitioned round-robin across the N clients
/// and each client's work runs concurrently via `futures::try_join_all`
/// on the caller's task (no `tokio::spawn` here — the individual
/// clients' writer tasks already use spawn internally).
///
/// Callbacks given to the pool must be `Fn + Send + Sync + Clone +
/// 'static` because they're cloned into each client's call and invoked
/// from multiple tasks. The ordering guarantee from single-client
/// pipelining (on_complete fires in input-index order) is **dropped**
/// for the pool: completions now arrive in whatever order the N clients
/// finish. PUT hashes are still returned in input order — the pool
/// re-assembles them from each bucket's original indices.
pub struct ServeClientPool {
    clients: Vec<ServeClient>,
}

impl ServeClientPool {
    /// Open N parallel SSH connections to the same target. All N
    /// connections do their own SSH handshake in parallel; total
    /// handshake latency ≈ single-connection (not N× because they
    /// concurrent). `caps` is the same byte on every connection — no
    /// per-connection capability splitting.
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

    /// Test helper: N parallel local `bcmr serve` subprocesses. Same
    /// pattern as `ServeClient::connect_local` but for the pool.
    #[allow(dead_code)]
    pub async fn connect_local(n: usize) -> Result<Self, BcmrError> {
        if n == 0 {
            return Err(BcmrError::InvalidInput("pool size must be >= 1".into()));
        }
        let futures: Vec<_> = (0..n).map(|_| ServeClient::connect_local()).collect();
        let clients = futures::future::try_join_all(futures).await?;
        Ok(Self { clients })
    }

    /// Number of active connections. Effective pool size = min(N,
    /// files.len()) on a given batch; see the `_striped` methods.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.clients.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    /// Access the first (always present) client. Useful for ops that
    /// don't benefit from striping: single-file `put()` with dedup
    /// pre-flight, `stat`, `list`, `mkdir` — all one-shot round trips
    /// where running them on multiple connections just wastes
    /// handshakes. The pool always has at least one client (we check
    /// in `connect_with_caps`).
    pub fn first_mut(&mut self) -> &mut ServeClient {
        &mut self.clients[0]
    }

    /// Directory creation stays on client[0] because ordering between
    /// parent mkdir and child mkdir matters; round-tripping across N
    /// connections would add complexity for work that's already cheap
    /// (one mkdir round trip per dir, ~2 ms on LAN).
    pub async fn mkdir(&mut self, path: &str) -> Result<(), BcmrError> {
        self.clients[0].mkdir(path).await
    }

    /// Stripe `files` round-robin across the pool's clients and run
    /// each client's `pipelined_put_files` concurrently. Returns PUT
    /// hashes in *input* order (not completion order — we re-assemble
    /// from each bucket's saved indices).
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

        // Round-robin partition. Each bucket keeps the original indices
        // so hashes can be re-scattered into input order at the end.
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

    /// Mirror of `pipelined_put_files_striped` for the download direction.
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

    /// Cleanly close all N connections.
    pub async fn close(mut self) -> Result<(), BcmrError> {
        let futs = self.clients.iter_mut().map(|c| c.close_in_place());
        let _ = futures::future::join_all(futs).await;
        self.clients.clear();
        Ok(())
    }
}

/// Read `data` and emit it as a sequence of `Data` (or compressed) frames
/// to `writer`. Used by both the single-file `put_streaming` path and the
/// many-file `pipelined_put_files` writer task; extracting it here keeps
/// the encode-loop in one place so a change to chunking, compression
/// negotiation, or framing can't drift between the two callers.
///
/// `on_chunk(uncompressed_bytes)` fires once per Data frame so callers
/// can drive a progress bar. Passing a `|_| {}` no-op skips reporting.
async fn write_file_data_frames<W>(
    writer: &mut W,
    data: &Path,
    algo: CompressionAlgo,
    on_chunk: &(impl Fn(u64) + ?Sized),
) -> Result<(), BcmrError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut file = File::open(data).await?;
    let mut buf = vec![0u8; DEDUP_BLOCK_SIZE];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let frame = compress::encode_block(algo, buf[..n].to_vec());
        protocol::write_message(writer, &frame).await?;
        on_chunk(n as u64);
    }
    Ok(())
}

fn decode_hex32(hex: &str) -> Result<[u8; 32], BcmrError> {
    if hex.len() != 64 {
        return Err(BcmrError::InvalidInput(format!(
            "expected 64-char hex hash, got {} chars",
            hex.len()
        )));
    }
    let mut out = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
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

fn hex_nibble(b: u8) -> Result<u8, BcmrError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(BcmrError::InvalidInput(format!(
            "invalid hex character: {}",
            b as char
        ))),
    }
}
