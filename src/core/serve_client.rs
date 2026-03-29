#![allow(dead_code)]

use std::path::Path;

use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::core::error::BcmrError;
use crate::core::protocol::{self, ListEntry, Message, PROTOCOL_VERSION};

pub struct ServeClient {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl ServeClient {
    pub async fn connect(ssh_target: &str) -> Result<Self, BcmrError> {
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

        let mut client = Self {
            child,
            stdin,
            stdout,
        };
        client.handshake().await?;
        Ok(client)
    }

    pub async fn connect_local() -> Result<Self, BcmrError> {
        let exe = std::env::current_exe()?;
        let mut child = Command::new(exe)
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

        let mut client = Self {
            child,
            stdin,
            stdout,
        };
        client.handshake().await?;
        Ok(client)
    }

    async fn handshake(&mut self) -> Result<(), BcmrError> {
        self.send(&Message::Hello {
            version: PROTOCOL_VERSION,
        })
        .await?;
        match self.recv().await? {
            Message::Welcome { .. } => Ok(()),
            Message::Error { message } => Err(BcmrError::InvalidInput(message)),
            other => Err(BcmrError::InvalidInput(format!(
                "unexpected handshake response: {other:?}"
            ))),
        }
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

        let mut file = File::open(data).await?;
        let mut buf = vec![0u8; 4 * 1024 * 1024];
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            self.send(&Message::Data {
                payload: buf[..n].to_vec(),
            })
            .await?;
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
        drop(self.stdin);
        self.child.wait().await?;
        Ok(())
    }

    async fn send(&mut self, msg: &Message) -> Result<(), BcmrError> {
        protocol::write_message(&mut self.stdin, msg).await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Message, BcmrError> {
        protocol::read_message(&mut self.stdout)
            .await?
            .ok_or_else(|| BcmrError::InvalidInput("server closed connection unexpectedly".into()))
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
