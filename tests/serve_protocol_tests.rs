use bcmr::core::{
    compress::decode_data_block,
    protocol::{
        checked_transfer_total, decode_message, encode_message, read_message, ListEntry, Message,
        MAX_CONTENT_BLOCK_SIZE, PROTOCOL_VERSION,
    },
};
use tokio::io::AsyncWriteExt;

fn roundtrip(msg: Message) -> Message {
    let encoded = encode_message(&msg);
    decode_message(&encoded).expect("decode must succeed for a valid encoded message")
}

#[test]
fn test_protocol_version_constant() {
    assert_eq!(PROTOCOL_VERSION, 2);
}

#[test]
fn test_hello_welcome_roundtrip() {
    assert_eq!(
        roundtrip(Message::Hello {
            version: 1,
            caps: 0
        }),
        Message::Hello {
            version: 1,
            caps: 0
        }
    );
    assert_eq!(
        roundtrip(Message::Welcome {
            version: 1,
            caps: 0
        }),
        Message::Welcome {
            version: 1,
            caps: 0
        }
    );
    assert_eq!(
        roundtrip(Message::Hello {
            version: 1,
            caps: 3
        }),
        Message::Hello {
            version: 1,
            caps: 3
        }
    );
}

#[test]
fn test_data_compressed_roundtrip() {
    let msg = Message::DataCompressed {
        algo: 1,
        original_size: 4096,
        payload: vec![0xAA; 1024],
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn data_block_rejects_hostile_declared_original_size_before_decompression() {
    let frame = encode_message(&Message::DataCompressed {
        algo: 1,
        original_size: u32::MAX,
        payload: vec![0],
    });
    assert!(frame.len() < 64, "the hostile frame must stay tiny");

    assert!(decode_message(&frame).is_none());
}

#[test]
fn data_block_rejects_raw_payload_larger_than_the_content_block_limit() {
    let message = Message::Data {
        payload: vec![0; MAX_CONTENT_BLOCK_SIZE + 1],
    };

    assert!(decode_data_block(message).is_err());
}

#[test]
fn data_block_rejects_data_compressed_with_algo_none() {
    let message = Message::DataCompressed {
        algo: 0,
        original_size: 1,
        payload: vec![0],
    };

    assert!(decode_data_block(message).is_err());
}

#[test]
fn codec_rejects_trailing_bytes_inside_a_data_frame() {
    let mut frame = Vec::new();
    frame.extend_from_slice(&7u32.to_le_bytes());
    frame.push(0x84);
    frame.extend_from_slice(&1u32.to_le_bytes());
    frame.extend_from_slice(&[0xAA, 0xBB]);

    assert!(decode_message(&frame).is_none());
}

#[test]
fn codec_rejects_bytes_after_the_declared_outer_frame() {
    let mut frame = encode_message(&Message::Data {
        payload: vec![0xAA],
    });
    frame.extend_from_slice(&[0xBB, 0xCC]);

    assert!(decode_message(&frame).is_none());
}

#[tokio::test]
async fn plain_wire_rejects_oversized_raw_data_before_reading_its_payload() {
    let (mut writer, mut reader) = tokio::io::duplex(64);
    let raw_len = (MAX_CONTENT_BLOCK_SIZE + 1) as u32;
    let frame_len = 1 + 4 + raw_len;
    writer.write_all(&frame_len.to_le_bytes()).await.unwrap();
    writer.write_all(&[0x84]).await.unwrap();
    writer.write_all(&raw_len.to_le_bytes()).await.unwrap();
    writer.shutdown().await.unwrap();

    let err = read_message(&mut reader).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn plain_wire_rejects_an_empty_declared_frame_before_reading_a_type_byte() {
    let (mut writer, mut reader) = tokio::io::duplex(64);
    writer.write_all(&0u32.to_le_bytes()).await.unwrap();
    writer.shutdown().await.unwrap();

    let err = read_message(&mut reader).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn transfer_accounting_accepts_the_exact_declared_boundary() {
    assert_eq!(
        checked_transfer_total(u64::MAX - 1, 1, u64::MAX).unwrap(),
        u64::MAX
    );
}

#[test]
fn transfer_accounting_rejects_overflow_without_wrapping() {
    assert!(checked_transfer_total(u64::MAX, 1, u64::MAX).is_err());
}

#[test]
fn test_have_blocks_roundtrip() {
    let msg = Message::HaveBlocks {
        block_size: 4 * 1024 * 1024,
        hashes: vec![[0xab; 32], [0xcd; 32], [0xef; 32]],
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_have_blocks_empty() {
    let msg = Message::HaveBlocks {
        block_size: 4 * 1024 * 1024,
        hashes: vec![],
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_missing_blocks_roundtrip() {
    let msg = Message::MissingBlocks {
        bits: vec![0b0000_0001, 0b1010_0000, 0xff],
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_list_roundtrip() {
    let msg = Message::List {
        path: "/home/user/docs".to_string(),
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_stat_roundtrip() {
    let msg = Message::Stat {
        path: "/tmp/file.txt".to_string(),
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_stat_response_roundtrip() {
    let msg = Message::StatResponse {
        size: 1_048_576,
        mtime: 1_700_000_000,
        is_dir: false,
    };
    assert_eq!(roundtrip(msg.clone()), msg);

    let dir_msg = Message::StatResponse {
        size: 0,
        mtime: -1,
        is_dir: true,
    };
    assert_eq!(roundtrip(dir_msg.clone()), dir_msg);
}

#[test]
fn test_get_with_offset_roundtrip() {
    let msg = Message::Get {
        path: "/data/file.bin".to_string(),
        offset: 65536,
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_data_chunk_65kb_roundtrip() {
    let payload = vec![0xABu8; 65 * 1024];
    let msg = Message::Data { payload };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_error_roundtrip() {
    let msg = Message::Error {
        message: "file not found: /nonexistent".to_string(),
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_hash_with_limit_roundtrip() {
    let msg = Message::Hash {
        path: "/data/large.bin".to_string(),
        offset: 0,
        limit: Some(4_194_304),
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_hash_without_limit_roundtrip() {
    let msg = Message::Hash {
        path: "/data/large.bin".to_string(),
        offset: 1024,
        limit: None,
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_hash_response_roundtrip() {
    let hash = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc5a0b850f37bc5a".to_string();
    let msg = Message::HashResponse { hash };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_list_response_multiple_entries_roundtrip() {
    let msg = Message::ListResponse {
        entries: vec![
            ListEntry {
                path: "/home/user/a.txt".to_string(),
                size: 1024,
                mtime: 1700000000,
                is_dir: false,
            },
            ListEntry {
                path: "/home/user/subdir".to_string(),
                size: 0,
                mtime: 0,
                is_dir: true,
            },
            ListEntry {
                path: "/home/user/b.bin".to_string(),
                size: 999_999,
                mtime: -1,
                is_dir: false,
            },
        ],
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_list_response_empty_entries_roundtrip() {
    let msg = Message::ListResponse { entries: vec![] };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_resume_response_with_hash_roundtrip() {
    let msg = Message::ResumeResponse {
        size: 2_097_152,
        block_hash: Some(
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc5a0b850f37bc5a".to_string(),
        ),
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_resume_response_without_hash_roundtrip() {
    let msg = Message::ResumeResponse {
        size: 0,
        block_hash: None,
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_put_roundtrip() {
    let msg = Message::Put {
        path: "/remote/dest.bin".to_string(),
        size: 4_294_967_295,
        offset: 123_456,
        overwrite: false,
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_mkdir_roundtrip() {
    let msg = Message::Mkdir {
        path: "/remote/new_dir".to_string(),
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_resume_request_roundtrip() {
    let msg = Message::Resume {
        path: "/remote/partial.bin".to_string(),
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_done_roundtrip() {
    assert_eq!(roundtrip(Message::Done), Message::Done);
}

#[test]
fn test_ok_with_hash_roundtrip() {
    let msg = Message::Ok {
        hash: Some("af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc5a0b850f37bc5a".to_string()),
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_ok_without_hash_roundtrip() {
    let msg = Message::Ok { hash: None };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_empty_input_returns_none() {
    assert_eq!(decode_message(&[]), None);
}

#[test]
fn test_truncated_length_returns_none() {
    assert_eq!(decode_message(&[0x01, 0x00, 0x00]), None);
}

#[test]
fn test_truncated_payload_returns_none() {
    let mut frame = vec![10u8, 0, 0, 0];
    frame.extend_from_slice(&[0x02, 0x03, 0x04]);
    assert_eq!(decode_message(&frame), None);
}

#[test]
fn test_unknown_message_type_returns_none() {
    let mut frame = vec![1u8, 0, 0, 0];
    frame.push(0xFF);
    assert_eq!(decode_message(&frame), None);
}

#[tokio::test]
async fn test_async_write_read_roundtrip() {
    use bcmr::core::protocol::{read_message, write_message};
    use tokio::io::duplex;

    let messages = vec![
        Message::Hello {
            version: 1,
            caps: 0,
        },
        Message::List {
            path: "/tmp".to_string(),
        },
        Message::Get {
            path: "/tmp/file".to_string(),
            offset: 0,
        },
        Message::Data {
            payload: vec![1u8, 2, 3, 4, 5],
        },
        Message::Ok { hash: None },
        Message::Done,
    ];

    let (mut client, mut server) = duplex(65536);

    for msg in &messages {
        write_message(&mut client, msg).await.unwrap();
    }
    drop(client);

    for expected in &messages {
        let received = read_message(&mut server).await.unwrap();
        assert_eq!(received.as_ref(), Some(expected));
    }

    let eof = read_message(&mut server).await.unwrap();
    assert_eq!(eof, None);
}

#[test]
fn test_open_direct_channel_roundtrip() {
    assert_eq!(
        roundtrip(Message::OpenDirectChannel),
        Message::OpenDirectChannel
    );
}

#[test]
fn test_auth_hello_roundtrip() {
    let mut mac = [0u8; 32];
    for (i, b) in mac.iter_mut().enumerate() {
        *b = (i * 7 + 3) as u8;
    }
    assert_eq!(
        roundtrip(Message::AuthHello { mac }),
        Message::AuthHello { mac }
    );
}

#[test]
fn test_direct_channel_ready_roundtrip() {
    let mut key = [0u8; 32];
    for (i, b) in key.iter_mut().enumerate() {
        *b = 0xA0u8.wrapping_add(i as u8);
    }
    let msg = Message::DirectChannelReady {
        addr: "127.0.0.1:47281".to_string(),
        session_key: key,
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_direct_ready_addr_empty_string() {
    let msg = Message::DirectChannelReady {
        addr: String::new(),
        session_key: [0x42; 32],
    };
    assert_eq!(roundtrip(msg.clone()), msg);
}

#[test]
fn test_auth_hello_truncated_payload_returns_none() {
    let mut payload = vec![0x0cu8];
    payload.extend_from_slice(&[0u8; 31]);
    let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&payload);
    assert_eq!(decode_message(&frame), None);
}
