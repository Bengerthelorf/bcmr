/// End-to-end integration tests for bcmr serve (local loopback).
///
/// Each test spins up a local `bcmr serve` subprocess via `ServeClient::connect_local()`
/// and exercises a single protocol operation against it.
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use bcmr::core::checksum;
use bcmr::core::serve_client::ServeClient;

/// Write deterministic pseudo-random bytes to `path`.
/// Uses a simple LCG so the data is never all-zeros, making hash collisions detectable.
fn create_file(path: &Path, size: usize) {
    let mut file = fs::File::create(path).unwrap();
    let mut state: u64 = 0xdeadbeef_cafebabe;
    let mut buf = Vec::with_capacity(size.min(65536));
    let mut remaining = size;
    while remaining > 0 {
        let chunk = remaining.min(65536);
        buf.clear();
        for _ in 0..chunk {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            buf.push((state >> 33) as u8);
        }
        file.write_all(&buf).unwrap();
        remaining -= chunk;
    }
    file.flush().unwrap();
}

fn bytes_to_hex(hash: &[u8; 32]) -> String {
    hash.iter().map(|b| format!("{:02x}", b)).collect()
}

#[tokio::test]
async fn serve_handshake() {
    let client = ServeClient::connect_local().await.unwrap();
    client.close().await.unwrap();
}

#[tokio::test]
async fn serve_stat_file() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("test.bin");
    create_file(&file_path, 1234);

    let mut client = ServeClient::connect_local().await.unwrap();
    let (size, _mtime, is_dir) = client.stat(file_path.to_str().unwrap()).await.unwrap();
    client.close().await.unwrap();

    assert_eq!(size, 1234);
    assert!(!is_dir);
}

#[tokio::test]
async fn serve_stat_nonexistent() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does_not_exist.bin");

    let mut client = ServeClient::connect_local().await.unwrap();
    let result = client.stat(missing.to_str().unwrap()).await;
    client.close().await.unwrap();

    assert!(result.is_err(), "expected error for nonexistent path");
}

#[tokio::test]
async fn serve_list_directory() {
    let dir = tempfile::tempdir().unwrap();
    create_file(&dir.path().join("alpha.bin"), 100);
    create_file(&dir.path().join("beta.bin"), 200);
    fs::create_dir(dir.path().join("subdir")).unwrap();
    create_file(&dir.path().join("subdir").join("gamma.bin"), 50);

    let mut client = ServeClient::connect_local().await.unwrap();
    let entries = client.list(dir.path().to_str().unwrap()).await.unwrap();
    client.close().await.unwrap();

    let names: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    assert!(
        names.iter().any(|n| *n == "alpha.bin"),
        "missing alpha.bin in {names:?}"
    );
    assert!(
        names.iter().any(|n| *n == "beta.bin"),
        "missing beta.bin in {names:?}"
    );
    assert!(
        names.iter().any(|n| n.contains("gamma.bin")),
        "missing gamma.bin in {names:?}"
    );
}

#[tokio::test]
async fn serve_hash_file() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("data.bin");
    create_file(&file_path, 4 * 1024 * 1024);

    let local_hex = checksum::calculate_hash(&file_path).unwrap();

    let mut client = ServeClient::connect_local().await.unwrap();
    let server_hash = client
        .hash(file_path.to_str().unwrap(), 0, None)
        .await
        .unwrap();
    client.close().await.unwrap();

    assert_eq!(bytes_to_hex(&server_hash), local_hex);
}

#[tokio::test]
async fn serve_get_download() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_file(&src, 2 * 1024 * 1024);

    let received: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let received_clone = Arc::clone(&received);

    let mut client = ServeClient::connect_local().await.unwrap();
    let server_hash = client
        .get(src.to_str().unwrap(), 0, move |chunk| {
            received_clone.lock().unwrap().extend_from_slice(chunk);
        })
        .await
        .unwrap();
    client.close().await.unwrap();

    let data = Arc::try_unwrap(received).unwrap().into_inner().unwrap();
    fs::write(&dst, &data).unwrap();

    let src_hash = checksum::calculate_hash(&src).unwrap();
    let dst_hash = checksum::calculate_hash(&dst).unwrap();
    assert_eq!(src_hash, dst_hash);

    if let Some(hash) = server_hash {
        assert_eq!(bytes_to_hex(&hash), src_hash);
    }
}

/// Compressible content: the server's GET path must produce DataCompressed
/// frames that the client decompresses back to the exact source bytes.
#[tokio::test]
async fn serve_get_compressible_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.txt");
    // Highly compressible repeated pattern.
    let text = "the quick brown fox jumps over the lazy dog\n".repeat(100_000);
    fs::write(&src, text.as_bytes()).unwrap();

    let received: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let received_clone = Arc::clone(&received);

    let mut client = ServeClient::connect_local().await.unwrap();
    let algo = client.negotiated_algo();
    assert_ne!(
        algo,
        bcmr::core::protocol::CompressionAlgo::None,
        "client and server should negotiate a compression algorithm"
    );
    client
        .get(src.to_str().unwrap(), 0, move |chunk| {
            received_clone.lock().unwrap().extend_from_slice(chunk);
        })
        .await
        .unwrap();
    client.close().await.unwrap();

    let data = Arc::try_unwrap(received).unwrap().into_inner().unwrap();
    assert_eq!(data, text.as_bytes());
}

/// Content-addressed dedup: upload the same 32 MiB file twice. The
/// second run should populate every block from the local CAS and the
/// resulting file must still be byte-identical to the source.
#[tokio::test]
async fn serve_dedup_repeats_use_cas() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("repeat.bin");
    let dst1 = dir.path().join("dst1.bin");
    let dst2 = dir.path().join("dst2.bin");
    create_file(&src, 32 * 1024 * 1024);
    let src_hash = checksum::calculate_hash(&src).unwrap();

    // First upload: populates CAS, returns hash.
    let mut client = ServeClient::connect_local().await.unwrap();
    let h1 = client.put(dst1.to_str().unwrap(), &src).await.unwrap();
    client.close().await.unwrap();
    assert_eq!(bytes_to_hex(&h1), src_hash);
    assert_eq!(checksum::calculate_hash(&dst1).unwrap(), src_hash);

    // Second upload to a fresh dst: every block should now be a CAS hit.
    let mut client = ServeClient::connect_local().await.unwrap();
    let h2 = client.put(dst2.to_str().unwrap(), &src).await.unwrap();
    client.close().await.unwrap();
    assert_eq!(bytes_to_hex(&h2), src_hash);
    assert_eq!(checksum::calculate_hash(&dst2).unwrap(), src_hash);
}

/// Compressible content via PUT: client compresses, server decompresses,
/// stored file matches the source byte-for-byte.
#[tokio::test]
async fn serve_put_compressible_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("upload.txt");
    let dst = dir.path().join("dst.txt");
    let text = "function foo() { return 42; }\n".repeat(50_000);
    fs::write(&src, text.as_bytes()).unwrap();

    let local_hash = checksum::calculate_hash(&src).unwrap();

    let mut client = ServeClient::connect_local().await.unwrap();
    let server_hash = client.put(dst.to_str().unwrap(), &src).await.unwrap();
    client.close().await.unwrap();

    assert_eq!(bytes_to_hex(&server_hash), local_hash);
    let dst_hash = checksum::calculate_hash(&dst).unwrap();
    assert_eq!(dst_hash, local_hash);
}

#[tokio::test]
async fn serve_put_upload() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("upload_src.bin");
    let dst_path = dir.path().join("upload_dst.bin");
    create_file(&src, 3 * 1024 * 1024);

    let local_hash = checksum::calculate_hash(&src).unwrap();

    let mut client = ServeClient::connect_local().await.unwrap();
    let server_hash = client.put(dst_path.to_str().unwrap(), &src).await.unwrap();
    client.close().await.unwrap();

    assert_eq!(bytes_to_hex(&server_hash), local_hash);
    assert!(dst_path.exists(), "uploaded file should exist at dst");

    let dst_hash = checksum::calculate_hash(&dst_path).unwrap();
    assert_eq!(dst_hash, local_hash);
}

#[tokio::test]
async fn serve_get_with_offset() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("large.bin");
    let total = 8 * 1024 * 1024usize;
    let half = total / 2;
    create_file(&file_path, total);

    let file_bytes = fs::read(&file_path).unwrap();
    let expected_second_half = file_bytes[half..].to_vec();

    let received: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let received_clone = Arc::clone(&received);

    let mut client = ServeClient::connect_local().await.unwrap();
    client
        .get(file_path.to_str().unwrap(), half as u64, move |chunk| {
            received_clone.lock().unwrap().extend_from_slice(chunk);
        })
        .await
        .unwrap();
    client.close().await.unwrap();

    let data = Arc::try_unwrap(received).unwrap().into_inner().unwrap();
    assert_eq!(data.len(), half, "expected {} bytes from offset", half);
    assert_eq!(data, expected_second_half);
}

#[tokio::test]
async fn serve_mkdir() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("a").join("b").join("c");

    let mut client = ServeClient::connect_local().await.unwrap();
    client.mkdir(nested.to_str().unwrap()).await.unwrap();
    client.close().await.unwrap();

    assert!(nested.is_dir(), "nested directory a/b/c should exist");
}

#[tokio::test]
async fn serve_resume_check() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("resume.bin");
    create_file(&file_path, 20 * 1024 * 1024);

    let mut client = ServeClient::connect_local().await.unwrap();
    let (size, block_hash) = client
        .resume_check(file_path.to_str().unwrap())
        .await
        .unwrap();
    client.close().await.unwrap();

    assert_eq!(size, 20 * 1024 * 1024);
    assert!(
        block_hash.is_some(),
        "20MB file should have a block_hash for resume"
    );
}

#[tokio::test]
async fn serve_resume_nonexistent() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no_such_file.bin");

    let mut client = ServeClient::connect_local().await.unwrap();
    let (size, block_hash) = client
        .resume_check(missing.to_str().unwrap())
        .await
        .unwrap();
    client.close().await.unwrap();

    assert_eq!(size, 0);
    assert!(block_hash.is_none());
}
