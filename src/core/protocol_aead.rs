// Wire format:  [4B LE (ct + tag)][ciphertext][16B Poly1305 tag]
//
// Nonce is not on the wire. Both sides derive it from a per-direction
// u64 counter; the direction byte prevents nonce collision when both
// endpoints' counters start at 0 under the same session key. A
// dropped / reordered / tampered frame desyncs the counters and the
// next tag check fails, killing the session — loud failure by design.

#![allow(dead_code)]

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::core::error::BcmrError;
use crate::core::protocol::{decode_message, encode_message, Message};

const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;
const TAG_LEN: usize = 16;

#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum Direction {
    ClientToServer = 0x01,
    ServerToClient = 0x02,
}

fn make_nonce(dir: Direction, counter: u64) -> Nonce {
    let mut bytes = [0u8; 12];
    bytes[0] = dir as u8;
    bytes[1..9].copy_from_slice(&counter.to_le_bytes());
    Nonce::assume_unique_for_key(bytes)
}

pub fn key_from_bytes(bytes: &[u8; 32]) -> Result<LessSafeKey, BcmrError> {
    let unbound = UnboundKey::new(&AES_256_GCM, bytes)
        .map_err(|_| BcmrError::InvalidInput("bad AES-256 key length".into()))?;
    Ok(LessSafeKey::new(unbound))
}

pub fn encrypt_message(
    msg: &Message,
    key: &LessSafeKey,
    dir: Direction,
    counter: &mut u64,
) -> Result<Vec<u8>, BcmrError> {
    let plaintext = encode_message(msg);
    if plaintext.len() > MAX_FRAME_LEN {
        return Err(BcmrError::InvalidInput(format!(
            "encoded message {} bytes exceeds {MAX_FRAME_LEN}",
            plaintext.len()
        )));
    }
    let nonce = make_nonce(dir, *counter);
    let mut in_out = plaintext;
    key.seal_in_place_append_tag(nonce, Aad::empty(), &mut in_out)
        .map_err(|_| BcmrError::CryptoFailure("AEAD seal failed".into()))?;
    *counter = counter
        .checked_add(1)
        .ok_or_else(|| BcmrError::CryptoFailure("AEAD counter overflow".into()))?;

    let total_len = in_out.len() as u32;
    let mut frame = Vec::with_capacity(4 + in_out.len());
    frame.extend_from_slice(&total_len.to_le_bytes());
    frame.extend_from_slice(&in_out);
    Ok(frame)
}

pub fn decrypt_message(
    ct_and_tag: &mut [u8],
    key: &LessSafeKey,
    dir: Direction,
    counter: &mut u64,
) -> Result<Message, BcmrError> {
    if ct_and_tag.len() < TAG_LEN {
        return Err(BcmrError::CryptoFailure(
            "frame shorter than Poly1305 tag".into(),
        ));
    }
    let nonce = make_nonce(dir, *counter);
    let plaintext = key
        .open_in_place(nonce, Aad::empty(), ct_and_tag)
        .map_err(|_| {
            // counter stays put so a retry on a fresh session starts clean
            BcmrError::CryptoFailure(format!(
                "AEAD open failed (dir {:?} counter {})",
                dir, *counter
            ))
        })?;
    *counter = counter
        .checked_add(1)
        .ok_or_else(|| BcmrError::CryptoFailure("AEAD counter overflow".into()))?;

    decode_message(plaintext).ok_or_else(|| {
        BcmrError::InvalidInput("AEAD-decrypted payload is not a valid Message".into())
    })
}

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
    // Distinguish clean EOF (read 0 bytes) from a half-read length
    // prefix (1-3 bytes then socket close) — the latter is a protocol
    // error, not a graceful shutdown.
    let first = reader.read(&mut len_buf[..1]).await?;
    if first == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut len_buf[1..]).await?;
    let total_len = u32::from_le_bytes(len_buf) as usize;
    if !(TAG_LEN..=MAX_FRAME_LEN + TAG_LEN).contains(&total_len) {
        return Err(BcmrError::CryptoFailure(format!(
            "frame length {total_len} out of bounds"
        )));
    }
    let mut buf = vec![0u8; total_len];
    reader.read_exact(&mut buf).await?;
    Ok(Some(decrypt_message(&mut buf, key, dir, counter)?))
}

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
        key_from_bytes(&[0x42; 32]).unwrap()
    }

    #[test]
    fn roundtrip_counter_advances_on_both_sides() {
        let key = test_key();
        let msg = Message::Hello {
            version: 1,
            caps: 0x0f,
        };
        let mut send = 0u64;
        let frame = encrypt_message(&msg, &key, Direction::ClientToServer, &mut send).unwrap();
        assert_eq!(send, 1);
        let mut body = frame[4..].to_vec();
        let mut recv = 0u64;
        assert_eq!(
            decrypt_message(&mut body, &key, Direction::ClientToServer, &mut recv).unwrap(),
            msg
        );
        assert_eq!(recv, 1);
    }

    #[test]
    fn wrong_key_fails_and_leaves_counter_untouched() {
        let good = key_from_bytes(&[0x11; 32]).unwrap();
        let bad = key_from_bytes(&[0x22; 32]).unwrap();
        let mut send = 0u64;
        let frame = encrypt_message(
            &Message::OpenDirectChannel,
            &good,
            Direction::ClientToServer,
            &mut send,
        )
        .unwrap();
        let mut body = frame[4..].to_vec();
        let mut recv = 0u64;
        decrypt_message(&mut body, &bad, Direction::ClientToServer, &mut recv).unwrap_err();
        assert_eq!(recv, 0);
    }

    #[test]
    fn direction_mismatch_fails_tag_check() {
        let key = test_key();
        let mut send = 0u64;
        let frame = encrypt_message(
            &Message::OpenDirectChannel,
            &key,
            Direction::ClientToServer,
            &mut send,
        )
        .unwrap();
        let mut body = frame[4..].to_vec();
        let mut recv = 0u64;
        // Receiver misconfigured — reading a C→S frame as if it were S→C
        // gives a different nonce, so the tag fails. Not an attacker
        // capability (direction byte isn't on the wire) but this is the
        // only path the mismatch could reach in production.
        decrypt_message(&mut body, &key, Direction::ServerToClient, &mut recv).unwrap_err();
    }

    #[test]
    fn single_bit_tamper_fails() {
        let key = test_key();
        let mut send = 0u64;
        let mut frame = encrypt_message(
            &Message::Data {
                payload: vec![0xAA; 100],
            },
            &key,
            Direction::ClientToServer,
            &mut send,
        )
        .unwrap();
        frame[14] ^= 0x01;
        let mut body = frame[4..].to_vec();
        let mut recv = 0u64;
        decrypt_message(&mut body, &key, Direction::ClientToServer, &mut recv).unwrap_err();
    }

    #[test]
    fn counter_desync_fails() {
        let key = test_key();
        let mut send = 0u64;
        let frame = encrypt_message(
            &Message::Hello {
                version: 1,
                caps: 0,
            },
            &key,
            Direction::ClientToServer,
            &mut send,
        )
        .unwrap();
        let mut body = frame[4..].to_vec();
        let mut recv = 5u64;
        decrypt_message(&mut body, &key, Direction::ClientToServer, &mut recv).unwrap_err();
    }

    #[test]
    fn multi_message_ordered_decode() {
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
        let mut send = 0u64;
        let frames: Vec<_> = msgs
            .iter()
            .map(|m| encrypt_message(m, &key, Direction::ServerToClient, &mut send).unwrap())
            .collect();
        let mut recv = 0u64;
        for (frame, expected) in frames.iter().zip(msgs.iter()) {
            let mut body = frame[4..].to_vec();
            assert_eq!(
                decrypt_message(&mut body, &key, Direction::ServerToClient, &mut recv).unwrap(),
                *expected
            );
        }
        assert_eq!(send, 3);
        assert_eq!(recv, 3);
    }

    #[tokio::test]
    async fn stream_duplex_read_write_integration() {
        let key = test_key();
        let (mut a, mut b) = tokio::io::duplex(64 * 1024);
        let key_c = test_key();
        let msgs = vec![
            Message::Hello {
                version: 1,
                caps: 0,
            },
            Message::Stat { path: "x".into() },
            Message::Data {
                payload: vec![0u8; 4096],
            },
        ];
        let msgs_clone = msgs.clone();
        let writer = tokio::spawn(async move {
            let mut c = 0u64;
            for m in msgs_clone {
                write_encrypted_message(&mut a, &m, &key_c, Direction::ClientToServer, &mut c)
                    .await
                    .unwrap();
            }
        });
        let mut c = 0u64;
        for expected in &msgs {
            let got = read_encrypted_message(&mut b, &key, Direction::ClientToServer, &mut c)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(got, *expected);
        }
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn stream_tampered_mid_byte_fails_next_decode() {
        // Real TCP tamper scenario: N frames arrive, byte flipped in frame
        // 2, frame 1 decodes fine and frame 2 fails. Counters never
        // recover so frame 3 also fails if we tried to read past.
        let key = test_key();
        let mut send_c = 0u64;
        let f1 = encrypt_message(
            &Message::Hello {
                version: 1,
                caps: 0,
            },
            &key,
            Direction::ClientToServer,
            &mut send_c,
        )
        .unwrap();
        let mut f2 = encrypt_message(
            &Message::Stat { path: "x".into() },
            &key,
            Direction::ClientToServer,
            &mut send_c,
        )
        .unwrap();
        f2[10] ^= 0x80;

        let (mut a, mut b) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            a.write_all(&f1).await.unwrap();
            a.write_all(&f2).await.unwrap();
        });

        let mut recv = 0u64;
        read_encrypted_message(&mut b, &key, Direction::ClientToServer, &mut recv)
            .await
            .unwrap()
            .unwrap();
        read_encrypted_message(&mut b, &key, Direction::ClientToServer, &mut recv)
            .await
            .unwrap_err();
    }
}
