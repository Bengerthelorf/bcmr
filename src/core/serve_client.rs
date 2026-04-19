use std::path::Path;

use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Child;

use crate::core::compress;
use crate::core::error::BcmrError;
use crate::core::framing::{self, RecvHalf, SendHalf};
use crate::core::protocol::{
    CompressionAlgo, Message, CAP_AEAD, CAP_DEDUP, CAP_LZ4, CAP_ZSTD, PROTOCOL_VERSION,
};
use crate::core::protocol_aead::Direction;
use crate::core::transport::ssh as ssh_transport;

mod ops;
mod pipelined;
mod pool;
mod promote;

#[cfg(any(test, feature = "test-support"))]
mod test_support;

pub use pool::ServeClientPool;

type BoxedReader = Box<dyn AsyncRead + Send + Unpin>;
type BoxedWriter = Box<dyn AsyncWrite + Send + Unpin>;

struct Transport {
    child: Child,
    _drain: Option<tokio::task::JoinHandle<()>>,
}

const CLIENT_CAPS: u8 = CAP_LZ4 | CAP_ZSTD | CAP_DEDUP;

const DEDUP_MIN_FILE_SIZE: u64 = 16 * 1024 * 1024;
const DEDUP_BLOCK_SIZE: usize = 4 * 1024 * 1024;

pub struct ServeClient {
    transport: Transport,
    reader: BoxedReader,
    writer: Option<BoxedWriter>,
    tx: Option<SendHalf>,
    rx: RecvHalf,
    algo: CompressionAlgo,
    dedup_enabled: bool,
    poisoned: bool,
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
            poisoned: false,
        }
    }

    async fn handshake(&mut self, caps: u8) -> Result<(), BcmrError> {
        self.handshake_with_key(caps, None).await
    }

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

    async fn send(&mut self, msg: &Message) -> Result<(), BcmrError> {
        if self.poisoned {
            return Err(BcmrError::InvalidInput(
                "ServeClient poisoned after a prior pipelined error; drop and reconnect".into(),
            ));
        }
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
        if self.poisoned {
            return Err(BcmrError::InvalidInput(
                "ServeClient poisoned after a prior pipelined error; drop and reconnect".into(),
            ));
        }
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
    write_file_data_frames_from(writer, tx, data, 0, algo, on_chunk).await
}

async fn write_file_data_frames_from<W>(
    writer: &mut W,
    tx: &mut SendHalf,
    data: &Path,
    offset: u64,
    algo: CompressionAlgo,
    on_chunk: &(impl Fn(u64) + ?Sized),
) -> Result<(), BcmrError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncSeekExt;
    let mut file = File::open(data).await?;
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset)).await?;
    }
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
