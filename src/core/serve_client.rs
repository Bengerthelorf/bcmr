use std::path::Path;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::core::compress;
use crate::core::error::BcmrError;
use crate::core::protocol::{
    self, CompressionAlgo, ListEntry, Message, CAP_DEDUP, CAP_LZ4, CAP_ZSTD, PROTOCOL_VERSION,
};

/// What the client is willing to speak. The user's --compress flag
/// intersects this before the Hello is sent so users can force "raw" for
/// debugging or to disable compression on trusted LANs where the CPU
/// cost outweighs the bandwidth savings.
const CLIENT_CAPS: u8 = CAP_LZ4 | CAP_ZSTD | CAP_DEDUP;

/// Files smaller than this skip the dedup pre-flight: the round-trip
/// cost of HaveBlocks/MissingBlocks dominates for tiny payloads.
const DEDUP_MIN_FILE_SIZE: u64 = 16 * 1024 * 1024;
const DEDUP_BLOCK_SIZE: usize = 4 * 1024 * 1024;

pub struct ServeClient {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    algo: CompressionAlgo,
    dedup_enabled: bool,
}

impl ServeClient {
    /// Connect with default capabilities (advertise all compression the
    /// client supports). Prefer `connect_with_caps` when the caller has
    /// a user-specified `--compress` value to honour.
    pub async fn connect(ssh_target: &str) -> Result<Self, BcmrError> {
        Self::connect_with_caps(ssh_target, CLIENT_CAPS).await
    }

    pub async fn connect_with_caps(ssh_target: &str, caps: u8) -> Result<Self, BcmrError> {
        let mut child = Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                ssh_target,
                "bcmr",
                "serve",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| BcmrError::InvalidInput("failed to open child stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| BcmrError::InvalidInput("failed to open child stdout".into()))?;

        let mut client = Self::from_child(child, stdin, stdout);
        client.handshake(caps).await?;
        Ok(client)
    }

    #[allow(dead_code)] // used by integration tests
    pub async fn connect_local() -> Result<Self, BcmrError> {
        // Find the bcmr binary in the same directory as the test binary.
        // current_exe() returns the test binary itself, not bcmr.
        let exe = std::env::current_exe()?;
        let bin_dir = exe
            .parent()
            .ok_or_else(|| BcmrError::InvalidInput("cannot find binary directory".into()))?;

        let bin_name = if cfg!(windows) { "bcmr.exe" } else { "bcmr" };

        // Try: same directory (release builds), then parent (cargo test puts
        // test binaries in deps/ while the main binary is one level up).
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

        let mut child = Command::new(&bcmr_path)
            .arg("serve")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| BcmrError::InvalidInput("failed to open child stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| BcmrError::InvalidInput("failed to open child stdout".into()))?;

        let mut client = Self::from_child(child, stdin, stdout);
        client.handshake(CLIENT_CAPS).await?;
        Ok(client)
    }

    fn from_child(child: Child, stdin: ChildStdin, stdout: ChildStdout) -> Self {
        Self {
            child,
            stdin: Some(stdin),
            stdout,
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
        let mut file = File::open(data).await?;
        let mut buf = vec![0u8; DEDUP_BLOCK_SIZE];
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            let frame = compress::encode_block(self.algo, buf[..n].to_vec());
            self.send(&frame).await?;
        }
        Ok(())
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
        self.stdin.take();
        let _ = self.child.wait().await;
        Ok(())
    }

    async fn send(&mut self, msg: &Message) -> Result<(), BcmrError> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| BcmrError::InvalidInput("connection closed".into()))?;
        protocol::write_message(stdin, msg).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Message, BcmrError> {
        protocol::read_message(&mut self.stdout)
            .await?
            .ok_or_else(|| BcmrError::InvalidInput("server closed connection unexpectedly".into()))
    }
}

/// Kill child process on drop to prevent orphaned bcmr serve processes.
impl Drop for ServeClient {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
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
