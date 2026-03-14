use blake3::Hasher;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

const BUFFER_SIZE: usize = 4 * 1024 * 1024; // 4MB — matches copy buffer, better for BLAKE3 SIMD

pub fn calculate_hash(path: &Path) -> io::Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(BUFFER_SIZE, file);
    let mut hasher = Hasher::new();
    let mut buffer = vec![0; BUFFER_SIZE];

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    Ok(hasher.finalize().to_hex().to_string())
}

pub fn calculate_partial_hash(path: &Path, limit: u64) -> io::Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(BUFFER_SIZE, file).take(limit);
    let mut hasher = Hasher::new();
    let mut buffer = vec![0; BUFFER_SIZE];

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    Ok(hasher.finalize().to_hex().to_string())
}
