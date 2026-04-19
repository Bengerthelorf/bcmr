use crate::core::framing::Framing;
use crate::core::protocol::{
    CompressionAlgo, Message, CAP_AEAD, CAP_DIRECT_TCP, CAP_FAST, CAP_SYNC, PROTOCOL_VERSION,
};
use crate::core::protocol_aead::Direction;
use anyhow::Result;
use std::path::Path;
use tokio::io::AsyncWriteExt;

use super::handlers::{
    handle_get, handle_get_chunked, handle_hash, handle_list, handle_mkdir, handle_put,
    handle_put_chunked, handle_resume, handle_stat, handle_truncate,
};
use super::rendezvous::{handle_open_direct_channel, RendezvousTasks, MAX_RENDEZVOUS_PER_SESSION};
use super::{validate_path, SERVER_CAPS};

pub(super) const CHUNK_SIZE: usize = 4 * 1024 * 1024;

pub(super) async fn run_session<R, W>(
    reader: &mut R,
    writer: &mut W,
    root: &Path,
    allow_splice: bool,
    allow_direct_tcp: bool,
    direct_tcp_key: Option<&[u8; 32]>,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    // Non-TTY transports must not offer CAP_DIRECT_TCP (recursive
    // rendezvous amplification) or CAP_AEAD (needs a session key).
    let mut offered_caps = SERVER_CAPS;
    if !allow_direct_tcp {
        offered_caps &= !CAP_DIRECT_TCP;
    }
    if direct_tcp_key.is_none() {
        offered_caps &= !CAP_AEAD;
    }
    let mut rendezvous_tasks = RendezvousTasks::new();

    let mut framing = Framing::plain();

    let effective_caps = match framing.read_message(reader).await? {
        Some(Message::Hello { version, caps }) => {
            if version != PROTOCOL_VERSION {
                framing
                    .write_message(
                        writer,
                        &Message::Error {
                            message: format!(
                                "protocol version mismatch: client={version} server={PROTOCOL_VERSION}"
                            ),
                        },
                    )
                    .await?;
                writer.flush().await?;
                return Ok(());
            }
            offered_caps & caps
        }
        Some(other) => {
            framing
                .write_message(
                    writer,
                    &Message::Error {
                        message: format!("expected Hello, got {other:?}"),
                    },
                )
                .await?;
            writer.flush().await?;
            return Ok(());
        }
        None => return Ok(()),
    };

    let algo = CompressionAlgo::negotiate(effective_caps, effective_caps);
    let fast = (effective_caps & CAP_FAST) != 0;
    let sync = (effective_caps & CAP_SYNC) != 0;
    let direct_tcp = (effective_caps & CAP_DIRECT_TCP) != 0;
    let aead = (effective_caps & CAP_AEAD) != 0;

    // Downgrade guard: MITM stripping CAP_AEAD would run this session plain.
    if direct_tcp_key.is_some() && !aead {
        framing
            .write_message(
                writer,
                &Message::Error {
                    message: "direct-TCP transport requires CAP_AEAD; refusing plain session"
                        .to_string(),
                },
            )
            .await?;
        writer.flush().await?;
        return Ok(());
    }

    framing
        .write_message(
            writer,
            &Message::Welcome {
                version: PROTOCOL_VERSION,
                caps: effective_caps,
            },
        )
        .await?;
    writer.flush().await?;

    // INVARIANT: Welcome is the plain→AEAD switchover; both sides flip here.
    if aead {
        let key = direct_tcp_key
            .expect("cap mask guarantees direct_tcp_key is Some when CAP_AEAD negotiated");
        framing =
            Framing::aead_from_key(key, Direction::ServerToClient, Direction::ClientToServer)?;
    }

    loop {
        let msg = match framing.read_message(reader).await? {
            Some(m) => m,
            None => break,
        };

        // Get/GetChunked arms drive `writer` directly; dispatch must not reply.
        let response = match msg {
            Message::Get { path, offset } => {
                match validate_path(&path, root) {
                    Ok(p) => {
                        if let Err(e) = handle_get(
                            p.to_str().unwrap_or(&path),
                            offset,
                            algo,
                            fast,
                            allow_splice,
                            writer,
                            &mut framing,
                        )
                        .await
                        {
                            eprintln!("serve: handler error: {e}");
                            framing
                                .write_message(
                                    writer,
                                    &Message::Error {
                                        message: e.to_string(),
                                    },
                                )
                                .await?;
                        }
                    }
                    Err(e) => {
                        framing
                            .write_message(
                                writer,
                                &Message::Error {
                                    message: e.to_string(),
                                },
                            )
                            .await?;
                    }
                }
                writer.flush().await?;
                continue;
            }
            Message::Stat { path } => match validate_path(&path, root) {
                Ok(p) => handle_stat(p.to_str().unwrap_or(&path)).await,
                Err(e) => Err(e),
            },
            Message::List { path } => match validate_path(&path, root) {
                Ok(p) => handle_list(p.to_str().unwrap_or(&path)).await,
                Err(e) => Err(e),
            },
            Message::Hash {
                path,
                offset,
                limit,
            } => match validate_path(&path, root) {
                Ok(p) => handle_hash(p.to_str().unwrap_or(&path), offset, limit).await,
                Err(e) => Err(e),
            },
            Message::Put { path, size } => match validate_path(&path, root) {
                Ok(p) => {
                    handle_put(
                        p.to_str().unwrap_or(&path),
                        size,
                        sync,
                        writer,
                        reader,
                        &mut framing,
                    )
                    .await
                }
                Err(e) => Err(e),
            },
            Message::PutChunked {
                path,
                offset,
                length,
            } => match validate_path(&path, root) {
                Ok(p) => {
                    handle_put_chunked(
                        p.to_str().unwrap_or(&path),
                        offset,
                        length,
                        sync,
                        reader,
                        &mut framing,
                    )
                    .await
                }
                Err(e) => Err(e),
            },
            Message::GetChunked {
                path,
                offset,
                length,
            } => {
                match validate_path(&path, root) {
                    Ok(p) => {
                        if let Err(e) = handle_get_chunked(
                            p.to_str().unwrap_or(&path),
                            offset,
                            length,
                            algo,
                            writer,
                            &mut framing,
                        )
                        .await
                        {
                            eprintln!("serve: handler error: {e}");
                            framing
                                .write_message(
                                    writer,
                                    &Message::Error {
                                        message: e.to_string(),
                                    },
                                )
                                .await?;
                        }
                    }
                    Err(e) => {
                        framing
                            .write_message(
                                writer,
                                &Message::Error {
                                    message: e.to_string(),
                                },
                            )
                            .await?;
                    }
                }
                writer.flush().await?;
                continue;
            }
            Message::Mkdir { path } => match validate_path(&path, root) {
                Ok(p) => handle_mkdir(p.to_str().unwrap_or(&path)).await,
                Err(e) => Err(e),
            },
            Message::Truncate { path, size } => match validate_path(&path, root) {
                Ok(p) => handle_truncate(p.to_str().unwrap_or(&path), size).await,
                Err(e) => Err(e),
            },
            Message::Resume { path } => match validate_path(&path, root) {
                Ok(p) => handle_resume(p.to_str().unwrap_or(&path)).await,
                Err(e) => Err(e),
            },
            Message::OpenDirectChannel => {
                if !direct_tcp {
                    Err(anyhow::anyhow!(
                        "CAP_DIRECT_TCP not negotiated on this session"
                    ))
                } else if rendezvous_tasks.len() >= MAX_RENDEZVOUS_PER_SESSION {
                    Err(anyhow::anyhow!(
                        "too many concurrent direct-TCP rendezvous requests \
                         on this session (limit {MAX_RENDEZVOUS_PER_SESSION})"
                    ))
                } else {
                    match handle_open_direct_channel(root.to_path_buf()) {
                        Ok((msg, handle)) => {
                            rendezvous_tasks.push(handle);
                            Ok(msg)
                        }
                        Err(e) => Err(e),
                    }
                }
            }
            other => Err(anyhow::anyhow!("unexpected message: {other:?}")),
        };

        let reply = match response {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("serve: handler error: {e}");
                Message::Error {
                    message: e.to_string(),
                }
            }
        };

        framing.write_message(writer, &reply).await?;
        writer.flush().await?;
    }

    rendezvous_tasks.drain_gracefully().await;
    Ok(())
}
