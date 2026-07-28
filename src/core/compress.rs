use crate::core::protocol::{
    compressed_block_size, validate_content_block_size, CompressionAlgo, Message,
};

const AUTO_SKIP_RATIO: f64 = 0.95;

const ZSTD_LEVEL: i32 = 3;

pub fn encode_block(algo: CompressionAlgo, raw: Vec<u8>) -> std::io::Result<Message> {
    validate_content_block_size(raw.len())?;
    if algo == CompressionAlgo::None || raw.is_empty() {
        return Ok(Message::Data { payload: raw });
    }

    let original_size = u32::try_from(raw.len())
        .map_err(|_| invalid_data("outbound content block size does not fit protocol u32"))?;
    let encoded = match algo {
        CompressionAlgo::Lz4 => lz4_flex::compress(&raw),
        CompressionAlgo::Zstd => match zstd::bulk::compress(&raw, ZSTD_LEVEL) {
            Ok(v) => v,
            Err(_) => return Ok(Message::Data { payload: raw }),
        },
        CompressionAlgo::None => unreachable!(),
    };

    if (encoded.len() as f64) > AUTO_SKIP_RATIO * original_size as f64 {
        return Ok(Message::Data { payload: raw });
    }

    Ok(Message::DataCompressed {
        algo: algo.to_byte(),
        original_size,
        payload: encoded,
    })
}

pub fn decode_block(
    algo_byte: u8,
    original_size: u32,
    compressed: &[u8],
) -> std::io::Result<Vec<u8>> {
    let original_size = compressed_block_size(algo_byte, original_size)?;

    let decoded = match CompressionAlgo::from_byte(algo_byte) {
        CompressionAlgo::Lz4 => lz4_flex::decompress(compressed, original_size)
            .map_err(|e| invalid_data(e.to_string()))?,
        CompressionAlgo::Zstd => zstd::bulk::decompress(compressed, original_size)
            .map_err(|e| invalid_data(e.to_string()))?,
        CompressionAlgo::None => unreachable!("compressed_block_size validates algorithm"),
    };

    if decoded.len() != original_size {
        return Err(invalid_data(format!(
            "decoded block size {} does not match declared {original_size}",
            decoded.len()
        )));
    }
    Ok(decoded)
}

/// Converts a protocol data frame to bytes after applying the content-block limit.
pub fn decode_data_block(message: Message) -> std::io::Result<Vec<u8>> {
    match message {
        Message::Data { payload } => {
            validate_content_block_size(payload.len())?;
            Ok(payload)
        }
        Message::DataCompressed {
            algo,
            original_size,
            payload,
        } => decode_block(algo, original_size, &payload),
        other => Err(invalid_data(format!("expected data block, got {other:?}"))),
    }
}

fn invalid_data(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::protocol::MAX_CONTENT_BLOCK_SIZE;

    fn compressed_with(algo: CompressionAlgo, data: &[u8]) -> Vec<u8> {
        match algo {
            CompressionAlgo::Lz4 => lz4_flex::compress(data),
            CompressionAlgo::Zstd => zstd::bulk::compress(data, ZSTD_LEVEL).unwrap(),
            CompressionAlgo::None => unreachable!(),
        }
    }

    #[test]
    fn roundtrip_lz4_compressible() {
        let data = b"hello world ".repeat(1000);
        let msg = encode_block(CompressionAlgo::Lz4, data.clone()).unwrap();
        if let Message::DataCompressed {
            algo,
            original_size,
            payload,
        } = msg
        {
            let out = decode_block(algo, original_size, &payload).unwrap();
            assert_eq!(out, data);
        } else {
            panic!("expected DataCompressed for compressible input");
        }
    }

    #[test]
    fn roundtrip_zstd_compressible() {
        let data = b"the quick brown fox jumps over the lazy dog. ".repeat(500);
        let msg = encode_block(CompressionAlgo::Zstd, data.clone()).unwrap();
        if let Message::DataCompressed {
            algo,
            original_size,
            payload,
        } = msg
        {
            let out = decode_block(algo, original_size, &payload).unwrap();
            assert_eq!(out, data);
        } else {
            panic!("expected DataCompressed for compressible input");
        }
    }

    #[test]
    fn auto_skip_incompressible() {
        let mut data = vec![0u8; 4 * 1024 * 1024];
        let mut x: u64 = 0xdeadbeefcafebabe;
        for b in data.iter_mut() {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *b = (x >> 33) as u8;
        }
        let msg = encode_block(CompressionAlgo::Lz4, data.clone()).unwrap();
        match msg {
            Message::Data { payload } => assert_eq!(payload, data),
            Message::DataCompressed { .. } => panic!("expected raw Data for incompressible input"),
            _ => panic!("unexpected message type"),
        }
    }

    #[test]
    fn none_always_raw() {
        let data = b"aaaaaaaaaaaaaaaaaaaaaa".to_vec();
        let msg = encode_block(CompressionAlgo::None, data.clone()).unwrap();
        if let Message::Data { payload } = msg {
            assert_eq!(payload, data);
        } else {
            panic!("CompressionAlgo::None must always produce Data");
        }
    }

    #[test]
    fn empty_block_is_raw() {
        let msg = encode_block(CompressionAlgo::Zstd, Vec::new()).unwrap();
        assert!(matches!(msg, Message::Data { payload } if payload.is_empty()));
    }

    #[test]
    fn lz4_rejects_a_declared_size_larger_than_the_decoded_payload() {
        let data = b"small lz4 payload";
        let compressed = compressed_with(CompressionAlgo::Lz4, data);

        assert!(decode_block(
            CompressionAlgo::Lz4.to_byte(),
            data.len() as u32 + 1,
            &compressed
        )
        .is_err());
    }

    #[test]
    fn zstd_rejects_a_declared_size_larger_than_the_decoded_payload() {
        let data = b"small zstd payload";
        let compressed = compressed_with(CompressionAlgo::Zstd, data);

        assert!(decode_block(
            CompressionAlgo::Zstd.to_byte(),
            data.len() as u32 + 1,
            &compressed
        )
        .is_err());
    }

    #[test]
    fn encoder_accepts_a_block_at_the_protocol_limit() {
        let msg = encode_block(CompressionAlgo::None, vec![0; MAX_CONTENT_BLOCK_SIZE]).unwrap();
        assert!(
            matches!(msg, Message::Data { payload } if payload.len() == MAX_CONTENT_BLOCK_SIZE)
        );
    }

    #[test]
    fn encoder_rejects_a_block_above_the_protocol_limit() {
        assert!(encode_block(CompressionAlgo::None, vec![0; MAX_CONTENT_BLOCK_SIZE + 1]).is_err());
    }

    #[test]
    fn compressed_block_rejects_zero_and_unknown_algorithms() {
        assert!(decode_block(0, 1, &[0]).is_err());
        assert!(decode_block(99, 1, &[0]).is_err());
    }
}
