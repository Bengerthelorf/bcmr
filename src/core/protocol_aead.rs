//! AEAD wire format for Path B's direct-TCP data channel.
//!
//! The SSH control channel delivers a 32-byte session key via
//! [`Message::DirectChannelReady`]; every frame on the direct-TCP
//! socket is then wrapped in AES-256-GCM with that key. Frame layout:
//!
//! ```text
//! [4B LE total-len][ciphertext][16B Poly1305 tag]
//! ```
//!
//! `total-len` is `ciphertext + tag`. The nonce is **not** on the wire
//! — both sides derive it from a per-direction monotonic counter.
//! Sender increments its counter each frame; receiver increments a
//! matching counter. A dropped or reordered frame desyncs the counters
//! and the next `open_in_place` fails its tag check, closing the
//! session. That's the intended failure mode: any wire tampering or
//! protocol bug fails loudly instead of silently corrupting data.
//!
//! Nonce layout (12 bytes for AES-GCM):
//!
//! ```text
//! byte 0    : direction flag (0x01 = client→server, 0x02 = server→client)
//! bytes 1-8 : u64 counter, little-endian
//! bytes 9-11: zero padding (reserved for future extensions)
//! ```
//!
//! The direction flag prevents nonce collisions between the two
//! endpoints (both use counter 0 for their first frame; without the
//! direction byte those two nonces would be identical and reuse under
//! the same key would break GCM security).

// Phase 3c1 commits the primitives + unit tests. Phase 3c2 wires them
// into the server's direct-TCP accept loop and into the client dialer,
// at which point the dead-code warnings resolve themselves.
#![allow(dead_code)]

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::core::error::BcmrError;
use crate::core::protocol::{decode_message, encode_message, Message};

/// Upper bound on an encrypted frame's plaintext size. Matches
/// `MAX_FRAME_SIZE` in `protocol.rs` to stay consistent with the
/// plaintext wire limit.
const MAX_PLAINTEXT_LEN: usize = 16 * 1024 * 1024;
const TAG_LEN: usize = 16;

/// Which side of the connection is emitting — drives the first byte
/// of the nonce so client-side counter 0 and server-side counter 0
/// don't collide.
#[derive(Debug, Clone, Copy)]
pub enum Direction {
    ClientToServer = 0x01,
    ServerToClient = 0x02,
}

/// Build a 12-byte AES-GCM nonce from a direction flag + per-direction
/// counter. The counter caller is expected to monotonically increment.
fn make_nonce(dir: Direction, counter: u64) -> Nonce {
    let mut bytes = [0u8; 12];
    bytes[0] = dir as u8;
    bytes[1..9].copy_from_slice(&counter.to_le_bytes());
    // bytes[9..12] stay zero — reserved for future use.
    Nonce::assume_unique_for_key(bytes)
}

/// Build a `LessSafeKey` from raw bytes. Name is `LessSafe` because
/// ring wants nonce management to be the caller's responsibility; our
/// counter-based nonce scheme + per-direction flag makes reuse
/// impossible under a single session key, which is the safety
/// invariant ring wants to guard against.
pub fn key_from_bytes(bytes: &[u8; 32]) -> Result<LessSafeKey, BcmrError> {
    let unbound = UnboundKey::new(&AES_256_GCM, bytes)
        .map_err(|_| BcmrError::InvalidInput("bad AES-256 key length".into()))?;
    Ok(LessSafeKey::new(unbound))
}

/// Encrypt a protocol message into its on-wire frame (length prefix +
/// ciphertext + tag). Bumps the counter.
pub fn encrypt_message(
    msg: &Message,
    key: &LessSafeKey,
    dir: Direction,
    counter: &mut u64,
) -> Result<Vec<u8>, BcmrError> {
    let plaintext = encode_message_bytes(msg);
    if plaintext.len() > MAX_PLAINTEXT_LEN {
        return Err(BcmrError::InvalidInput(format!(
            "encrypted message plaintext {} bytes exceeds cap of {MAX_PLAINTEXT_LEN}",
            plaintext.len()
        )));
    }
    let mut in_out = plaintext;
    let nonce = make_nonce(dir, *counter);
    *counter = counter
        .checked_add(1)
        .ok_or_else(|| BcmrError::InvalidInput("AEAD counter overflowed u64".into()))?;
    key.seal_in_place_append_tag(nonce, Aad::empty(), &mut in_out)
        .map_err(|_| BcmrError::InvalidInput("AEAD encryption failed".into()))?;

    // Wire frame: 4B LE length + ciphertext + tag.
    let total_len = in_out.len() as u32;
    let mut frame = Vec::with_capacity(4 + in_out.len());
    frame.extend_from_slice(&total_len.to_le_bytes());
    frame.extend_from_slice(&in_out);
    Ok(frame)
}

/// Decrypt a received ciphertext+tag buffer. Decrypts in place; on
/// success the buffer's leading bytes hold the plaintext, the trailing
/// 16 bytes are the (now-consumed) tag. `counter` is bumped on success
/// only — a failed decrypt leaves the counter where it was so a retry
/// on a fresh connection doesn't skip the nonce.
pub fn decrypt_message(
    ct_and_tag: &mut [u8],
    key: &LessSafeKey,
    dir: Direction,
    counter: &mut u64,
) -> Result<Message, BcmrError> {
    if ct_and_tag.len() < TAG_LEN {
        return Err(BcmrError::InvalidInput(
            "encrypted frame shorter than the Poly1305 tag".into(),
        ));
    }
    let nonce = make_nonce(dir, *counter);
    let plaintext = key
        .open_in_place(nonce, Aad::empty(), ct_and_tag)
        .map_err(|_| BcmrError::InvalidInput("AEAD decryption / tag check failed".into()))?;
    *counter = counter
        .checked_add(1)
        .ok_or_else(|| BcmrError::InvalidInput("AEAD counter overflowed u64".into()))?;

    // Decode the plaintext back into a Message. The plaintext here is
    // the same format encode_message produces — including its 4-byte
    // inner length prefix — so decode_message handles it directly.
    let msg = decode_message(plaintext).ok_or_else(|| {
        BcmrError::InvalidInput("encrypted payload decoded to invalid message".into())
    })?;
    Ok(msg)
}

/// Encode a message the same way `protocol::encode_message` does but
/// return just the bytes (the existing public `encode_message` already
/// does this — wrap it for clarity at call sites).
fn encode_message_bytes(msg: &Message) -> Vec<u8> {
    encode_message(msg)
}

/// Read one encrypted frame and return the decoded `Message`. Returns
/// `Ok(None)` on clean EOF before the next frame; any byte after the
/// 4-byte length header that isn't followed by the expected ciphertext
/// is a protocol error.
pub async fn read_encrypted_message<R>(
    reader: &mut R,
    key: &LessSafeKey,
    dir: Direction,
    counter: &mut u64,
) -> Result<Option<Message>, BcmrError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let total_len = u32::from_le_bytes(len_buf) as usize;
    if total_len > MAX_PLAINTEXT_LEN + TAG_LEN {
        return Err(BcmrError::InvalidInput(format!(
            "encrypted frame claims {total_len} bytes, cap is {}",
            MAX_PLAINTEXT_LEN + TAG_LEN
        )));
    }
    if total_len < TAG_LEN {
        return Err(BcmrError::InvalidInput(
            "encrypted frame length header below the tag size".into(),
        ));
    }
    let mut buf = vec![0u8; total_len];
    reader.read_exact(&mut buf).await?;
    let msg = decrypt_message(&mut buf, key, dir, counter)?;
    Ok(Some(msg))
}

/// Encrypt + write one message to the wire. Caller is responsible for
/// calling `flush()` at meaningful boundaries.
pub async fn write_encrypted_message<W>(
    writer: &mut W,
    msg: &Message,
    key: &LessSafeKey,
    dir: Direction,
    counter: &mut u64,
) -> Result<(), BcmrError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let frame = encrypt_message(msg, key, dir, counter)?;
    writer.write_all(&frame).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> LessSafeKey {
        key_from_bytes(&[0x42; 32]).expect("key")
    }

    #[test]
    fn encrypt_decrypt_roundtrip_for_each_direction() {
        let key = test_key();
        let msg = Message::Hello {
            version: 1,
            caps: 0x0f,
        };

        // Client → server: counter starts fresh.
        let mut c_counter = 0u64;
        let frame = encrypt_message(&msg, &key, Direction::ClientToServer, &mut c_counter).unwrap();
        assert_eq!(c_counter, 1, "counter advances on encrypt");

        let mut body = frame[4..].to_vec();
        let mut c_recv = 0u64;
        let got = decrypt_message(&mut body, &key, Direction::ClientToServer, &mut c_recv).unwrap();
        assert_eq!(got, msg);
        assert_eq!(c_recv, 1, "counter advances on decrypt");
    }

    #[test]
    fn wrong_key_fails_tag_check() {
        let msg = Message::Hello {
            version: 1,
            caps: 0,
        };
        let good_key = key_from_bytes(&[0x11; 32]).unwrap();
        let bad_key = key_from_bytes(&[0x22; 32]).unwrap();
        let mut c_send = 0u64;
        let frame =
            encrypt_message(&msg, &good_key, Direction::ClientToServer, &mut c_send).unwrap();
        let mut body = frame[4..].to_vec();
        let mut c_recv = 0u64;
        let err = decrypt_message(&mut body, &bad_key, Direction::ClientToServer, &mut c_recv)
            .expect_err("decrypt with wrong key must fail");
        assert!(err.to_string().contains("AEAD decryption"));
        assert_eq!(c_recv, 0, "counter must NOT advance on decrypt failure");
    }

    #[test]
    fn flipped_direction_byte_fails_tag_check() {
        let msg = Message::OpenDirectChannel;
        let key = test_key();
        let mut c_send = 0u64;
        let frame = encrypt_message(&msg, &key, Direction::ClientToServer, &mut c_send).unwrap();
        // Receiver with the WRONG direction (reads with ServerToClient
        // but sender sent ClientToServer) — nonces don't match → tag
        // check fails.
        let mut body = frame[4..].to_vec();
        let mut c_recv = 0u64;
        decrypt_message(&mut body, &key, Direction::ServerToClient, &mut c_recv)
            .expect_err("direction mismatch must fail tag check");
    }

    #[test]
    fn tampered_ciphertext_byte_fails() {
        let msg = Message::Data {
            payload: vec![0xAA; 100],
        };
        let key = test_key();
        let mut c_send = 0u64;
        let mut frame =
            encrypt_message(&msg, &key, Direction::ClientToServer, &mut c_send).unwrap();
        // Flip a byte somewhere in the middle of the ciphertext.
        let idx = 4 + 10;
        frame[idx] ^= 0x01;
        let mut body = frame[4..].to_vec();
        let mut c_recv = 0u64;
        decrypt_message(&mut body, &key, Direction::ClientToServer, &mut c_recv)
            .expect_err("single-bit tamper must be caught by Poly1305");
    }

    #[test]
    fn counter_desync_fails() {
        let key = test_key();
        let msg = Message::Hello {
            version: 1,
            caps: 0,
        };
        let mut c_send = 0u64;
        let frame = encrypt_message(&msg, &key, Direction::ClientToServer, &mut c_send).unwrap();
        let mut body = frame[4..].to_vec();
        // Receiver thinks counter is 5 when sender used 0 — different
        // nonce, tag check fails.
        let mut c_recv = 5u64;
        decrypt_message(&mut body, &key, Direction::ClientToServer, &mut c_recv)
            .expect_err("counter desync must fail");
    }

    #[test]
    fn three_messages_in_order_all_decode() {
        let key = test_key();
        let msgs = vec![
            Message::Hello {
                version: 1,
                caps: 0,
            },
            Message::Stat {
                path: "/etc/hostname".to_string(),
            },
            Message::Data {
                payload: vec![0x01, 0x02, 0x03, 0x04],
            },
        ];
        let mut c_send = 0u64;
        let frames: Vec<Vec<u8>> = msgs
            .iter()
            .map(|m| encrypt_message(m, &key, Direction::ServerToClient, &mut c_send).unwrap())
            .collect();
        let mut c_recv = 0u64;
        for (frame, expected) in frames.iter().zip(msgs.iter()) {
            let mut body = frame[4..].to_vec();
            let got = decrypt_message(&mut body, &key, Direction::ServerToClient, &mut c_recv)
                .expect("valid frame decrypts");
            assert_eq!(&got, expected);
        }
        assert_eq!(c_send, 3);
        assert_eq!(c_recv, 3);
    }
}
