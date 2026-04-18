//! Framing wrapper over `protocol::{read,write}_message` with an optional
//! AEAD envelope. Created Plain for the cleartext Hello/Welcome; flipped
//! to Aead after both peers agree on CAP_AEAD.

use std::sync::Arc;

use ring::aead::LessSafeKey;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::core::error::BcmrError;
use crate::core::protocol::{self, Message};
use crate::core::protocol_aead::{self, Direction};

pub struct AeadState {
    key: LessSafeKey,
    send_dir: Direction,
    recv_dir: Direction,
    send_counter: u64,
    recv_counter: u64,
}

// Aead variant holds ~576 B key schedule; one Framing per session so the
// variant-size asymmetry is not a hot-path cost.
#[allow(clippy::large_enum_variant)]
pub enum Framing {
    Plain,
    Aead(AeadState),
}

impl Framing {
    pub fn plain() -> Self {
        Framing::Plain
    }

    /// `send_dir` is this peer's transmit direction; `recv_dir` the opposite.
    pub fn aead_from_key(
        key_bytes: &[u8; 32],
        send_dir: Direction,
        recv_dir: Direction,
    ) -> Result<Self, BcmrError> {
        let key = protocol_aead::key_from_bytes(key_bytes)?;
        Ok(Framing::Aead(AeadState {
            key,
            send_dir,
            recv_dir,
            send_counter: 0,
            recv_counter: 0,
        }))
    }

    #[allow(dead_code)] // splice-gate on Linux only
    pub fn is_aead(&self) -> bool {
        matches!(self, Framing::Aead(_))
    }

    pub async fn read_message<R>(&mut self, reader: &mut R) -> Result<Option<Message>, BcmrError>
    where
        R: AsyncRead + Unpin,
    {
        match self {
            Framing::Plain => Ok(protocol::read_message(reader).await?),
            Framing::Aead(state) => {
                protocol_aead::read_encrypted_message(
                    reader,
                    &state.key,
                    state.recv_dir,
                    &mut state.recv_counter,
                )
                .await
            }
        }
    }

    pub async fn write_message<W>(
        &mut self,
        writer: &mut W,
        msg: &Message,
    ) -> Result<(), BcmrError>
    where
        W: AsyncWrite + Unpin,
    {
        match self {
            Framing::Plain => Ok(protocol::write_message(writer, msg).await?),
            Framing::Aead(state) => {
                protocol_aead::write_encrypted_message(
                    writer,
                    msg,
                    &state.key,
                    state.send_dir,
                    &mut state.send_counter,
                )
                .await
            }
        }
    }
}

/// Split-half of a Framing so the pipelined writer task and the reader task
/// can each hold their own direction's counter without sharing a `&mut`.
pub enum SendHalf {
    Plain,
    Aead {
        key: Arc<LessSafeKey>,
        dir: Direction,
        counter: u64,
    },
}

pub enum RecvHalf {
    Plain,
    Aead {
        key: Arc<LessSafeKey>,
        dir: Direction,
        counter: u64,
    },
}

pub fn plain_halves() -> (SendHalf, RecvHalf) {
    (SendHalf::Plain, RecvHalf::Plain)
}

/// Key lives behind Arc so each half can move into its own task.
pub fn aead_halves(
    key_bytes: &[u8; 32],
    send_dir: Direction,
    recv_dir: Direction,
) -> Result<(SendHalf, RecvHalf), BcmrError> {
    let key = Arc::new(protocol_aead::key_from_bytes(key_bytes)?);
    Ok((
        SendHalf::Aead {
            key: Arc::clone(&key),
            dir: send_dir,
            counter: 0,
        },
        RecvHalf::Aead {
            key,
            dir: recv_dir,
            counter: 0,
        },
    ))
}

impl SendHalf {
    pub async fn write_message<W>(
        &mut self,
        writer: &mut W,
        msg: &Message,
    ) -> Result<(), BcmrError>
    where
        W: AsyncWrite + Unpin,
    {
        match self {
            SendHalf::Plain => Ok(protocol::write_message(writer, msg).await?),
            SendHalf::Aead { key, dir, counter } => {
                protocol_aead::write_encrypted_message(writer, msg, key, *dir, counter).await
            }
        }
    }
}

impl RecvHalf {
    pub async fn read_message<R>(&mut self, reader: &mut R) -> Result<Option<Message>, BcmrError>
    where
        R: AsyncRead + Unpin,
    {
        match self {
            RecvHalf::Plain => Ok(protocol::read_message(reader).await?),
            RecvHalf::Aead { key, dir, counter } => {
                protocol_aead::read_encrypted_message(reader, key, *dir, counter).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn plain_round_trip_is_unchanged() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let mut tx = Framing::plain();
        let mut rx = Framing::plain();
        tx.write_message(&mut a, &Message::OpenDirectChannel)
            .await
            .unwrap();
        let got = rx.read_message(&mut b).await.unwrap().unwrap();
        assert_eq!(got, Message::OpenDirectChannel);
    }

    #[tokio::test]
    async fn aead_round_trip_server_to_client_and_back() {
        let key = [0x77u8; 32];
        let mut server = Framing::aead_from_key(
            &key,
            Direction::ServerToClient,
            Direction::ClientToServer,
        )
        .unwrap();
        let mut client = Framing::aead_from_key(
            &key,
            Direction::ClientToServer,
            Direction::ServerToClient,
        )
        .unwrap();

        let (mut s_to_c_a, mut s_to_c_b) = tokio::io::duplex(64 * 1024);
        let (mut c_to_s_a, mut c_to_s_b) = tokio::io::duplex(64 * 1024);

        server
            .write_message(
                &mut s_to_c_a,
                &Message::Welcome {
                    version: 1,
                    caps: 0xff,
                },
            )
            .await
            .unwrap();
        let got = client.read_message(&mut s_to_c_b).await.unwrap().unwrap();
        assert_eq!(
            got,
            Message::Welcome {
                version: 1,
                caps: 0xff
            }
        );

        client
            .write_message(&mut c_to_s_a, &Message::Stat { path: "/x".into() })
            .await
            .unwrap();
        let got = server.read_message(&mut c_to_s_b).await.unwrap().unwrap();
        assert_eq!(got, Message::Stat { path: "/x".into() });
    }

    #[tokio::test]
    async fn aead_wrong_key_fails_decrypt() {
        let mut tx = Framing::aead_from_key(
            &[0x11u8; 32],
            Direction::ClientToServer,
            Direction::ServerToClient,
        )
        .unwrap();
        let mut rx = Framing::aead_from_key(
            &[0x22u8; 32],
            Direction::ServerToClient,
            Direction::ClientToServer,
        )
        .unwrap();
        let (mut a, mut b) = tokio::io::duplex(4096);
        tx.write_message(&mut a, &Message::Done).await.unwrap();
        rx.read_message(&mut b).await.unwrap_err();
    }
}
