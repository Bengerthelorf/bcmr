//! Wire compression for the serve protocol.
//!
//! Data frames may be sent as either `Data` (raw) or `DataCompressed`
//! (LZ4/Zstd). The sender picks per-block based on the negotiated algorithm
//! and an auto-skip heuristic — if the compressed size is within 5% of
//! the original, the block is sent raw since the receiver would pay a
//! decompression pass for negligible bandwidth savings. This makes the
//! protocol robust to mixed workloads where some blocks compress well
//! and others (already-compressed files, random data) don't.

use crate::core::protocol::{CompressionAlgo, Message};

/// Threshold below which compressed output is still worth sending. 0.95
/// means we accept compression if the output is at most 95% of input.
const AUTO_SKIP_RATIO: f64 = 0.95;

/// Zstd compression level. 3 is Zstd's own default — decent ratio, fast
/// enough that on a 1 Gbps link encode throughput is still multiples of
/// the wire. Higher levels buy diminishing returns on file content.
const ZSTD_LEVEL: i32 = 3;

/// Encode a 4MB-ish block for transport. Returns either `Data` (raw) or
/// `DataCompressed` depending on the negotiated algorithm and the
/// per-block compressibility. Callers pass `algo = None` when no
/// compression was negotiated.
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
        // Compression didn't help — drop the encoded buffer and send raw.
        return Message::Data { payload: raw };
    }

    Message::DataCompressed {
        algo: algo.to_byte(),
        original_size: original_size as u32,
        payload: encoded,
    }
}

/// Decode a DataCompressed message back into raw bytes.
pub fn decode_block(
    algo_byte: u8,
    original_size: u32,
    compressed: &[u8],
) -> std::io::Result<Vec<u8>> {
    match CompressionAlgo::from_byte(algo_byte) {
        CompressionAlgo::Lz4 => lz4_flex::decompress(compressed, original_size as usize)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
        CompressionAlgo::Zstd => zstd::bulk::decompress(compressed, original_size as usize)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
        CompressionAlgo::None => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "DataCompressed frame with algo=None",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // High-entropy bytes from a strong LCG. LZ4 on a 4MB block of this
        // comes back essentially at 1.00 ratio and should auto-skip.
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
}
