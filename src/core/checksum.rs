use blake3::Hasher;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

const BUFFER_SIZE: usize = 4 * 1024 * 1024; // 4MB — matches copy buffer, better for BLAKE3 SIMD

pub fn calculate_hash(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Hasher::new();
    let mut buffer = vec![0; BUFFER_SIZE];

    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    Ok(hasher.finalize().to_hex().to_string())
}

pub fn calculate_partial_hash(path: &Path, limit: u64) -> io::Result<String> {
    let file = File::open(path)?;
    let mut reader = file.take(limit);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_calculate_hash_known_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"hello world").unwrap();

        let hash = calculate_hash(&path).unwrap();
        let expected = blake3::hash(b"hello world").to_hex().to_string();
        assert_eq!(hash, expected);
    }

    #[test]
    fn test_calculate_hash_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, b"").unwrap();

        let hash = calculate_hash(&path).unwrap();
        let expected = blake3::hash(b"").to_hex().to_string();
        assert_eq!(hash, expected);
    }

    #[test]
    fn test_calculate_partial_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partial.txt");
        std::fs::write(&path, b"hello world").unwrap();

        let partial = calculate_partial_hash(&path, 5).unwrap();
        let expected = blake3::hash(b"hello").to_hex().to_string();
        assert_eq!(partial, expected);
    }

    #[test]
    fn test_partial_hash_beyond_file_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("short.txt");
        std::fs::write(&path, b"abc").unwrap();

        let partial = calculate_partial_hash(&path, 100).unwrap();
        let full = calculate_hash(&path).unwrap();
        assert_eq!(partial, full);
    }

    #[test]
    fn test_calculate_hash_large_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.bin");
        let mut f = std::fs::File::create(&path).unwrap();
        let chunk = vec![0xABu8; 1024 * 1024];
        for _ in 0..5 {
            f.write_all(&chunk).unwrap();
        }
        drop(f);

        let hash = calculate_hash(&path).unwrap();
        let mut hasher = blake3::Hasher::new();
        for _ in 0..5 {
            hasher.update(&chunk);
        }
        let expected = hasher.finalize().to_hex().to_string();
        assert_eq!(hash, expected);
    }
}
