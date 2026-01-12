use blake3::Hasher;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

/// BLAKE3 hash
pub fn calculate_hash(path: &Path) -> io::Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Hasher::new();
    let mut buffer = [0; 8192];

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    Ok(hasher.finalize().to_hex().to_string())
}

/// Partial BLAKE3 hash (limit)
pub fn calculate_partial_hash(path: &Path, limit: u64) -> io::Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file).take(limit);
    let mut hasher = Hasher::new();
    let mut buffer = [0; 8192];

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    Ok(hasher.finalize().to_hex().to_string())
}
