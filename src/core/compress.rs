use crate::core::protocol::{CompressionAlgo, Message};

const AUTO_SKIP_RATIO: f64 = 0.95;

const ZSTD_LEVEL: i32 = 3;

/// Maximum uncompressed content block accepted by the transfer protocol.
///
/// Encoded frames remain independently limited to 16 MiB; all current content
/// block producers use 4 MiB chunks.
pub const MAX_CONTENT_BLOCK_SIZE: usize = 4 * 1024 * 1024;

pub fn encode_block(algo: CompressionAlgo, raw: Vec<u8>) -> Message {
    if algo == CompressionAlgo::None || raw.is_empty() {
        return Message::Data { payload: raw };
    }

    let original_size = raw.len();
    let encoded = match algo {
        CompressionAlgo::Lz4 => lz4_flex::compress(&raw),
        CompressionAlgo::Zstd => match zstd::bulk::compress(&raw, ZSTD_LEVEL) {
            Ok(v) => v,
            Err(_) => return Message::Data { payload: raw },
        },
        CompressionAlgo::None => unreachable!(),
    };

    if (encoded.len() as f64) > AUTO_SKIP_RATIO * original_size as f64 {
        return Message::Data { payload: raw };
    }

    Message::DataCompressed {
        algo: algo.to_byte(),
        original_size: original_size as u32,
        payload: encoded,
    }
}

pub fn decode_block(
    algo_byte: u8,
    original_size: u32,
    compressed: &[u8],
) -> std::io::Result<Vec<u8>> {
    let original_size = usize::try_from(original_size)
        .map_err(|_| invalid_data("declared decompressed size does not fit usize"))?;
    validate_content_block_size(original_size)?;
    if original_size == 0 {
        return Err(invalid_data("DataCompressed frame declares an empty block"));
    }

    let decoded = match CompressionAlgo::from_byte(algo_byte) {
        CompressionAlgo::Lz4 => lz4_flex::decompress(compressed, original_size)
            .map_err(|e| invalid_data(e.to_string()))?,
        CompressionAlgo::Zstd => zstd::bulk::decompress(compressed, original_size)
            .map_err(|e| invalid_data(e.to_string()))?,
        CompressionAlgo::None => return Err(invalid_data("DataCompressed frame with algo=None")),
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

fn validate_content_block_size(size: usize) -> std::io::Result<()> {
    if size > MAX_CONTENT_BLOCK_SIZE {
        return Err(invalid_data(format!(
            "content block size {size} exceeds protocol maximum {MAX_CONTENT_BLOCK_SIZE}"
        )));
    }
    Ok(())
}

fn invalid_data(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let msg = encode_block(CompressionAlgo::Lz4, data.clone());
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
        let msg = encode_block(CompressionAlgo::Zstd, data.clone());
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
        let msg = encode_block(CompressionAlgo::Lz4, data.clone());
        match msg {
            Message::Data { payload } => assert_eq!(payload, data),
            Message::DataCompressed { .. } => panic!("expected raw Data for incompressible input"),
            _ => panic!("unexpected message type"),
        }
    }

    #[test]
    fn none_always_raw() {
        let data = b"aaaaaaaaaaaaaaaaaaaaaa".to_vec();
        let msg = encode_block(CompressionAlgo::None, data.clone());
        if let Message::Data { payload } = msg {
            assert_eq!(payload, data);
        } else {
            panic!("CompressionAlgo::None must always produce Data");
        }
    }

    #[test]
    fn empty_block_is_raw() {
        let msg = encode_block(CompressionAlgo::Zstd, Vec::new());
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
}
