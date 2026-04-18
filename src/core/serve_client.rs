use std::path::Path;

use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Child;

use crate::core::compress;
use crate::core::error::BcmrError;
use crate::core::framing::{self, RecvHalf, SendHalf};
use crate::core::protocol::{
    self, CompressionAlgo, ListEntry, Message, CAP_AEAD, CAP_DEDUP, CAP_DIRECT_TCP, CAP_LZ4,
    CAP_ZSTD, PROTOCOL_VERSION,
};
use crate::core::protocol_aead::Direction;
use crate::core::transport::ssh as ssh_transport;

/// Must match `AUTH_HELLO_TAG` on the server. Domain-separates the
/// rendezvous MAC from any other keyed hash that might share the session key.
const AUTH_HELLO_TAG: &[u8] = b"bcmr-direct-v1";

/// Best-effort diagnostic for direct-TCP dial failures. `ssh -G` can take
/// 50-500 ms (DNS + ssh_config parsing), hence spawn_blocking.
async fn ssh_target_uses_proxyjump(target: &str) -> bool {
    let target = target.to_owned();
    tokio::task::spawn_blocking(move || {
        let Ok(out) = std::process::Command::new("ssh")
            .args(["-G", &target])
            .output()
        else {
            return false;
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        stdout.lines().any(|line| {
            let mut it = line.split_whitespace();
            matches!(
                (it.next(), it.next()),
                (Some("proxyjump"), Some(v)) if v != "none"
            )
        })
    })
    .await
    .unwrap_or(false)
}

type BoxedReader = Box<dyn AsyncRead + Send + Unpin>;
type BoxedWriter = Box<dyn AsyncWrite + Send + Unpin>;

/// `_drain` only on the direct-TCP path: reads sshd's post-rendezvous
/// stdout so local ssh doesn't wedge on a full pipe buffer.
struct Transport {
    child: Child,
    _drain: Option<tokio::task::JoinHandle<()>>,
}

/// CAP_FAST is opt-in via `--fast` because dropping server-side hashing
/// only makes sense when the user has another integrity check.
const CLIENT_CAPS: u8 = CAP_LZ4 | CAP_ZSTD | CAP_DEDUP;

/// Below this size the HaveBlocks/MissingBlocks round trip dominates.
const DEDUP_MIN_FILE_SIZE: u64 = 16 * 1024 * 1024;
const DEDUP_BLOCK_SIZE: usize = 4 * 1024 * 1024;

pub struct ServeClient {
    transport: Transport,
    reader: BoxedReader,
    /// `Option` because pipelined paths move writer + tx into a writer
    /// task and restore them via `reap_writer_task`.
    writer: Option<BoxedWriter>,
    tx: Option<SendHalf>,
    rx: RecvHalf,
    algo: CompressionAlgo,
    dedup_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct FileTransfer {
    pub remote: String,
    pub local: std::path::PathBuf,
    pub size: u64,
}

impl ServeClient {
    pub async fn connect(ssh_target: &str) -> Result<Self, BcmrError> {
        Self::connect_with_caps(ssh_target, CLIENT_CAPS).await
    }

    pub async fn connect_with_caps(ssh_target: &str, caps: u8) -> Result<Self, BcmrError> {
        let spawn = ssh_transport::spawn_remote(ssh_target).await?;
        let mut client = Self::from_ssh_spawn(spawn);
        client.handshake(caps).await?;
        Ok(client)
    }

    #[allow(dead_code)]
    pub async fn connect_local_with_caps(caps: u8) -> Result<Self, BcmrError> {
        let mut client = Self::spawn_local_serve().await?;
        client.handshake(caps).await?;
        Ok(client)
    }

    #[allow(dead_code)]
    pub async fn connect_local() -> Result<Self, BcmrError> {
        let mut client = Self::spawn_local_serve().await?;
        client.handshake(CLIENT_CAPS).await?;
        Ok(client)
    }

    /// `caps` are the data-session caps; CAP_DIRECT_TCP is set internally
    /// on the control handshake regardless.
    #[allow(dead_code)]
    pub async fn connect_direct_with_caps(
        ssh_target: &str,
        caps: u8,
    ) -> Result<Self, BcmrError> {
        let spawn = ssh_transport::spawn_remote(ssh_target).await?;
        Self::promote_to_direct_tcp(spawn, caps, Some(ssh_target)).await
    }

    #[allow(dead_code)]
    pub async fn connect_direct_local_with_caps(caps: u8) -> Result<Self, BcmrError> {
        let bcmr_path = Self::locate_bcmr_binary()?;
        let spawn = ssh_transport::spawn_local(&bcmr_path).await?;
        Self::promote_to_direct_tcp(spawn, caps, None).await
    }

    #[allow(dead_code)]
    pub async fn connect_direct_local() -> Result<Self, BcmrError> {
        Self::connect_direct_local_with_caps(CLIENT_CAPS).await
    }

    #[allow(dead_code)]
    async fn spawn_local_serve() -> Result<Self, BcmrError> {
        let bcmr_path = Self::locate_bcmr_binary()?;
        let spawn = ssh_transport::spawn_local(&bcmr_path).await?;
        Ok(Self::from_ssh_spawn(spawn))
    }

    #[allow(dead_code)]
    fn locate_bcmr_binary() -> Result<std::path::PathBuf, BcmrError> {
        // current_exe() is the test binary itself, so look alongside it.
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
        candidates
            .iter()
            .find(|p| p.exists())
            .cloned()
            .ok_or_else(|| {
                BcmrError::InvalidInput(format!(
                    "bcmr binary not found at {} or {}",
                    candidates[0].display(),
                    candidates[1].display()
                ))
            })
    }

    async fn promote_to_direct_tcp(
        mut spawn: ssh_transport::SshSpawn,
        caps: u8,
        ssh_target: Option<&str>,
    ) -> Result<Self, BcmrError> {
        use tokio::io::{AsyncWriteExt as _, AsyncReadExt as _};

        protocol::write_message(
            &mut spawn.stdin,
            &Message::Hello {
                version: PROTOCOL_VERSION,
                caps: caps | CAP_DIRECT_TCP,
            },
        )
        .await?;
        spawn.stdin.flush().await?;

        let control_caps = match protocol::read_message(&mut spawn.stdout).await? {
            Some(Message::Welcome {
                caps: server_caps, ..
            }) => server_caps,
            Some(Message::Error { message }) => return Err(BcmrError::InvalidInput(message)),
            Some(other) => {
                return Err(BcmrError::InvalidInput(format!(
                    "unexpected handshake response: {other:?}"
                )))
            }
            None => {
                return Err(BcmrError::InvalidInput(
                    "server closed connection during handshake".into(),
                ))
            }
        };
        if (control_caps & CAP_DIRECT_TCP) == 0 {
            return Err(BcmrError::InvalidInput(
                "server did not negotiate CAP_DIRECT_TCP; cannot use direct-TCP transport".into(),
            ));
        }

        protocol::write_message(&mut spawn.stdin, &Message::OpenDirectChannel).await?;
        spawn.stdin.flush().await?;
        // Zeroize the session key at decode — downstream AuthHello MAC +
        // AEAD half construction inherit the scrubbing scope.
        let (addr, session_key) = match protocol::read_message(&mut spawn.stdout).await? {
            Some(Message::DirectChannelReady { addr, session_key }) => {
                (addr, zeroize::Zeroizing::new(session_key))
            }
            Some(Message::Error { message }) => return Err(BcmrError::InvalidInput(message)),
            Some(other) => {
                return Err(BcmrError::InvalidInput(format!(
                    "expected DirectChannelReady, got {other:?}"
                )))
            }
            None => {
                return Err(BcmrError::InvalidInput(
                    "server closed connection before DirectChannelReady".into(),
                ))
            }
        };

        // Dial before releasing SSH stdin so the rendezvous listener
        // has us in its backlog even if the SSH EOF cascade is fast.
        let stream = match tokio::net::TcpStream::connect(&addr).await {
            Ok(s) => s,
            Err(e) => {
                let mut msg = format!(
                    "direct-TCP dial to {addr} failed: {e}. The server bound its \
                     rendezvous listener on the interface where SSH arrived, but \
                     this client can't reach that address."
                );
                if let Some(target) = ssh_target {
                    if ssh_target_uses_proxyjump(target).await {
                        msg.push_str(
                            " This SSH target uses ProxyJump — when the jump host is on \
                             a different subnet from the client, direct-TCP rendezvous is \
                             not reachable. Use --direct=ssh for this target.",
                        );
                    }
                }
                return Err(BcmrError::InvalidInput(msg));
            }
        };
        let (tcp_reader, tcp_writer) = stream.into_split();

        // EOF the control session so the remote falls through to the
        // rendezvous task.
        drop(spawn.stdin);

        // Local ssh would wedge on a full pipe buffer if we stopped
        // reading — drain whatever still arrives.
        let mut stdout = spawn.stdout;
        let stdout_drain = tokio::spawn(async move {
            let mut buf = [0u8; 512];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });

        let (tx, rx) = framing::plain_halves();
        let mut client = Self {
            transport: Transport {
                child: spawn.child,
                _drain: Some(stdout_drain),
            },
            reader: Box::new(tcp_reader),
            writer: Some(Box::new(tcp_writer)),
            tx: Some(tx),
            rx,
            algo: CompressionAlgo::None,
            dedup_enabled: false,
        };

        // Prove knowledge of the SSH-delivered session key, then handshake
        // on the data plane advertising CAP_AEAD.
        let mac = *blake3::keyed_hash(&session_key, AUTH_HELLO_TAG).as_bytes();
        client.send(&Message::AuthHello { mac }).await?;
        client
            .handshake_with_key(caps | CAP_AEAD, Some(&session_key))
            .await?;
        Ok(client)
    }

    fn from_ssh_spawn(spawn: ssh_transport::SshSpawn) -> Self {
        let (tx, rx) = framing::plain_halves();
        Self {
            transport: Transport {
                child: spawn.child,
                _drain: None,
            },
            reader: Box::new(spawn.stdout),
            writer: Some(Box::new(spawn.stdin)),
            tx: Some(tx),
            rx,
            algo: CompressionAlgo::None,
            dedup_enabled: false,
        }
    }

    async fn handshake(&mut self, caps: u8) -> Result<(), BcmrError> {
        self.handshake_with_key(caps, None).await
    }

    /// Flips framing to AEAD immediately after Welcome iff CAP_AEAD is in
    /// the negotiated caps AND a session key was provided. Matches the
    /// server's flip point.
    async fn handshake_with_key(
        &mut self,
        caps: u8,
        session_key: Option<&[u8; 32]>,
    ) -> Result<(), BcmrError> {
        self.send(&Message::Hello {
            version: PROTOCOL_VERSION,
            caps,
        })
        .await?;
        match self.recv().await? {
            Message::Welcome {
                caps: server_caps, ..
            } => {
                let effective = caps & server_caps;
                self.algo = CompressionAlgo::negotiate(caps, server_caps);
                self.dedup_enabled = (effective & CAP_DEDUP) != 0;
                if (effective & CAP_AEAD) != 0 {
                    let key = session_key.ok_or_else(|| {
                        BcmrError::CryptoFailure(
                            "server negotiated CAP_AEAD but client has no session key".into(),
                        )
                    })?;
                    let (tx, rx) = framing::aead_halves(
                        key,
                        Direction::ClientToServer,
                        Direction::ServerToClient,
                    )?;
                    self.tx = Some(tx);
                    self.rx = rx;
                } else if session_key.is_some() {
                    // Downgrade guard: if we asked for direct-TCP but the
                    // caps intersection dropped CAP_AEAD, something stripped
                    // the bit — refuse to run the data plane in cleartext.
                    return Err(BcmrError::CryptoFailure(
                        "direct-TCP negotiated without CAP_AEAD — possible downgrade, refusing"
                            .into(),
                    ));
                }
                Ok(())
            }
            Message::Error { message } => Err(BcmrError::InvalidInput(message)),
            other => Err(BcmrError::InvalidInput(format!(
                "unexpected handshake response: {other:?}"
            ))),
        }
    }

    #[allow(dead_code)]
    pub fn negotiated_algo(&self) -> CompressionAlgo {
        self.algo
    }

    #[allow(dead_code)]
    pub fn is_aead_negotiated(&self) -> bool {
        matches!(
            self.tx.as_ref(),
            Some(framing::SendHalf::Aead { .. })
        )
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

    #[allow(dead_code)]
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

    /// Kept for e2e tests; production path is `pipelined_get_files`.
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

    /// Paired with `handle_put_chunked` on the server.
    #[allow(dead_code)]
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

    /// Paired with `handle_get_chunked` on the server.
    #[allow(dead_code)]
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

        // Pre-open + seek so concurrent chunks from other pool clients
        // don't serialise on `OpenOptions::open`.
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
        // Re-read the file for the transfer rather than buffering — the
        // alternative caps file size at RAM.
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
            // read() may return short; loop to a full block or EOF.
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

    /// Pair: `reap_writer_task` restores both. SendHalf moves with the
    /// writer because AEAD's per-direction counter belongs to one side.
    fn take_writer(&mut self) -> Result<(BoxedWriter, SendHalf), BcmrError> {
        let writer = self.writer.take().ok_or_else(|| {
            BcmrError::InvalidInput("writer already taken (concurrent op?)".into())
        })?;
        let tx = self.tx.take().ok_or_else(|| {
            BcmrError::InvalidInput("send framing already taken (concurrent op?)".into())
        })?;
        Ok((writer, tx))
    }

    async fn reap_writer_task(
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
                    // Reader's error is the proximate cause; the writer
                    // I/O error is the same wire closing under it. Let
                    // the caller surface the reader error.
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

    /// Pipelined batch PUT over one connection. Writer task emits Put +
    /// Data* + Done for every file in send order while this task reads
    /// Ok/Error in matching FIFO order. Backpressure comes for free from
    /// the OS pipe + SSH send window.
    ///
    /// Skips the dedup pre-flight (each HaveBlocks/MissingBlocks round
    /// trip would re-serialize). Callers should keep large files on the
    /// per-file `put()` path where dedup wins.
    ///
    /// Bails on the first error — connection is left indeterminate, so
    /// the caller should drop the client afterwards.
    ///
    /// Callbacks are `Send + Sync + 'static` because `on_chunk` moves
    /// into the writer task.
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

        let (mut wire, mut tx) = self.take_writer()?;
        let algo = self.algo;
        // Snapshot what on_complete needs — the writer task consumes `files`.
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

    /// Pipelined batch GET. Mirror of `pipelined_put_files`: writer task
    /// emits `Get { path }` in send order, this task reads Data*/Ok in
    /// matching FIFO order and writes each file's bytes to its dst.
    ///
    /// Validates `ft.size` against the received stream — protocol-level
    /// catch for server overrun or premature `Ok`.
    ///
    /// Bails on the first error. Files completed beforehand are intact;
    /// the in-progress file is left half-written at its final path (no
    /// `.tmp` + rename, inherited from the pre-pipelining loop). The
    /// TempFileGuard-based SIGINT cleanup doesn't cover this path yet.
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

        let (mut wire, mut tx) = self.take_writer()?;

        let request_paths: Vec<String> = files.iter().map(|f| f.remote.clone()).collect();

        // ~30 B per Get; N=10000 cumulative ~300 KiB fits the OS pipe +
        // SSH socket buffer. If it didn't, write_message() would
        // backpressure on its own.
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
            // NOT tokio::fs — it would spawn_blocking per 4 MiB chunk on
            // NVMe, thread bounces dominating actual I/O. Sync writes
            // in-task are measurably faster.
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
        // Drop stdin → EOF → server exits cleanly. After wait() the Drop
        // impl's start_kill() is a no-op on the exited child.
        self.writer.take();
        self.transport.wait().await;
        Ok(())
    }

    /// Hook for the pool: drop writer + wait through `&mut self` so
    /// the pool can iterate instead of consuming by value.
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
        let tx = self
            .tx
            .as_mut()
            .ok_or_else(|| BcmrError::InvalidInput("send framing taken".into()))?;
        tx.write_message(w, msg).await?;
        w.flush().await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Message, BcmrError> {
        self.rx
            .read_message(&mut self.reader)
            .await?
            .ok_or_else(|| BcmrError::InvalidInput("server closed connection unexpectedly".into()))
    }
}

impl Transport {
    async fn wait(&mut self) {
        let _ = self.child.wait().await;
    }
}

/// N parallel ServeClient connections. Each connection is its own TCP +
/// cipher stream, so N clients lift the single-core AEAD ceiling by N×
/// until NIC/disk/syscall overhead takes over (mscp-style "just open
/// more ssh"). Striped variants partition files round-robin and run
/// concurrently. Completion order is not preserved; PUT hashes are
/// re-scattered to input order from each bucket's saved indices.
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

    /// N parallel direct-TCP + AEAD connections. Each runs its own
    /// rendezvous, so N listeners open briefly on the server during setup.
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

    #[allow(dead_code)]
    pub async fn connect_local(n: usize) -> Result<Self, BcmrError> {
        if n == 0 {
            return Err(BcmrError::InvalidInput("pool size must be >= 1".into()));
        }
        let futures: Vec<_> = (0..n).map(|_| ServeClient::connect_local()).collect();
        let clients = futures::future::try_join_all(futures).await?;
        Ok(Self { clients })
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.clients.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    /// First client, for one-shot ops (stat/list/mkdir/single put) where
    /// striping would just waste handshakes. Always present.
    pub fn first_mut(&mut self) -> &mut ServeClient {
        &mut self.clients[0]
    }

    /// Stays on client[0]: parent→child ordering matters, and an mkdir
    /// is ~2 ms on LAN — not worth fanning out.
    pub async fn mkdir(&mut self, path: &str) -> Result<(), BcmrError> {
        self.clients[0].mkdir(path).await
    }

    /// Returns PUT hashes in input order (re-scattered from bucket indices).
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

        // Each bucket keeps original indices so hashes re-scatter to
        // input order at the end.
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

    /// Stripe one file across N clients; dst is pre-truncated to handle
    /// stale tails and the zero-byte case. Returns the client-side
    /// whole-file BLAKE3 — wire integrity is the AEAD per-frame MAC.
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

    /// Caller supplies `remote_size` (from `client.stat` or an upper bound).
    pub async fn striped_get_file(
        &mut self,
        remote: &str,
        local: &Path,
        remote_size: u64,
    ) -> Result<[u8; 32], BcmrError> {
        if self.clients.is_empty() {
            return Err(BcmrError::InvalidInput("pool is empty".into()));
        }
        // Pre-size so every chunk's seek+write lands at its final
        // offset — otherwise one chunk's growth could race past
        // another's seek target.
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

/// Last range absorbs the remainder so union = [0, total). Callers
/// filter zero-length entries when total == 0.
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

async fn write_file_data_frames<W>(
    writer: &mut W,
    tx: &mut SendHalf,
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
        tx.write_message(writer, &frame).await?;
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
