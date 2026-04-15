use bcmr::core::protocol::{decode_message, encode_message, ListEntry, Message, PROTOCOL_VERSION};

fn roundtrip(msg: Message) -> Message {
    let encoded = encode_message(&msg);
    decode_message(&encoded).expect("decode must succeed for a valid encoded message")
}

#[test]
fn test_protocol_version_constant() {
    assert_eq!(PROTOCOL_VERSION, 1);
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
                is_dir: false,
            },
            ListEntry {
                path: "/home/user/subdir".to_string(),
                size: 0,
                is_dir: true,
            },
            ListEntry {
                path: "/home/user/b.bin".to_string(),
                size: 999_999,
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
    // Only 3 bytes — can't read 4-byte length prefix
    assert_eq!(decode_message(&[0x01, 0x00, 0x00]), None);
}

#[test]
fn test_truncated_payload_returns_none() {
    // Length says 10 bytes, only 3 bytes of payload follow
    let mut frame = vec![10u8, 0, 0, 0];
    frame.extend_from_slice(&[0x02, 0x03, 0x04]);
    assert_eq!(decode_message(&frame), None);
}

#[test]
fn test_unknown_message_type_returns_none() {
    // Payload of 1 byte with an unrecognised type
    let mut frame = vec![1u8, 0, 0, 0];
    frame.push(0xFF); // unknown type
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

    // After all messages, EOF should yield None
    let eof = read_message(&mut server).await.unwrap();
    assert_eq!(eof, None);
}
