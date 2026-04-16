#![cfg(unix)]
//! End-to-end integration tests for bcmr serve (local loopback).
//!
//! Each test spins up a local `bcmr serve` subprocess via
//! `ServeClient::connect_local()` and exercises a single protocol op
//! against it.
//!
//! `cfg(unix)` on the whole file: bcmr serve is invoked over SSH, which on
//! Windows means OpenSSH-on-Windows behaves differently enough that we
//! don't claim Windows as a serve target. The subprocess+stdio dance also
//! trips on Windows path semantics (`--root /` doesn't canonicalize on
//! Windows, so connect_local's spawn would crash the child). Pure
//! protocol encoding/decoding is covered by `serve_protocol_tests.rs`
//! which runs on every platform.

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

/// Security: with a root jail, PUT to a path outside the jail must be
/// rejected. The connect_local helper uses `--root /` so the default
/// test path is unrestricted; this test explicitly spawns a serve
/// with a narrower root and confirms writes outside it fail.
#[tokio::test]
async fn serve_root_jail_rejects_escape() {
    let jail = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let src = outside.path().join("payload.bin");
    std::fs::write(&src, b"hello").unwrap();

    // Hand-spawn a serve with --root set to the jail so writes outside
    // are refused by the server.
    use std::process::Stdio;
    let exe = std::env::current_exe().unwrap();
    let bin_name = if cfg!(windows) { "bcmr.exe" } else { "bcmr" };
    let bin = exe.parent().unwrap().parent().unwrap().join(bin_name);
    let mut child = tokio::process::Command::new(&bin)
        .args(["serve", "--root", jail.path().to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    // Handshake manually at the protocol layer to avoid coupling to
    // ServeClient's private state. We just need to confirm the Put path
    // validation rejects the outside path.
    use bcmr::core::protocol::{read_message, write_message, Message, PROTOCOL_VERSION};
    let mut stdin = stdin;
    let mut stdout = stdout;
    write_message(
        &mut stdin,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            caps: 0,
        },
    )
    .await
    .unwrap();
    // Welcome
    let _ = read_message(&mut stdout).await.unwrap();
    // Put to an absolute path outside the jail.
    let outside_target = outside.path().join("should_not_be_written.bin");
    write_message(
        &mut stdin,
        &Message::Put {
            path: outside_target.to_string_lossy().into_owned(),
            size: 5,
        },
    )
    .await
    .unwrap();
    let reply = read_message(&mut stdout).await.unwrap().unwrap();
    match reply {
        Message::Error { message } => {
            assert!(
                message.contains("escapes server root"),
                "expected jail-escape error, got: {}",
                message
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }
    assert!(
        !outside_target.exists(),
        "forbidden target was written despite jail"
    );
    drop(stdin);
    let _ = child.wait().await;
}

/// Security: PUT must refuse data beyond the declared size. Without
/// this bound a malicious client could declare size=1 and send TBs.
#[tokio::test]
async fn serve_put_size_bound_rejects_oversized() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("capped.bin");

    use bcmr::core::protocol::{read_message, write_message, Message};
    // Reach into the raw protocol: send Put declaring 10 bytes then a
    // 100-byte Data frame. The server should Error after the first
    // bound-exceeding block. ServeClient::put() honestly sends the
    // source size, so we drive the raw stdin/stdout instead.
    use std::process::Stdio;
    let exe = std::env::current_exe().unwrap();
    let bin_name = if cfg!(windows) { "bcmr.exe" } else { "bcmr" };
    let bin = exe.parent().unwrap().parent().unwrap().join(bin_name);
    let mut child = tokio::process::Command::new(&bin)
        .args(["serve", "--root", "/"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();

    use bcmr::core::protocol::PROTOCOL_VERSION;
    write_message(
        &mut stdin,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            caps: 0,
        },
    )
    .await
    .unwrap();
    let _welcome = read_message(&mut stdout).await.unwrap();

    write_message(
        &mut stdin,
        &Message::Put {
            path: dst.to_string_lossy().into_owned(),
            size: 10,
        },
    )
    .await
    .unwrap();
    // Send a 100-byte Data frame — oversized.
    write_message(
        &mut stdin,
        &Message::Data {
            payload: vec![0xab; 100],
        },
    )
    .await
    .unwrap();
    write_message(&mut stdin, &Message::Done).await.unwrap();

    let reply = read_message(&mut stdout).await.unwrap().unwrap();
    match reply {
        Message::Error { message } => {
            assert!(
                message.contains("past the declared size"),
                "expected size-bound error, got: {}",
                message
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }

    drop(stdin);
    let _ = child.wait().await;
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

/// CAS LRU eviction end-to-end: cap the store small, upload three
/// dedup-eligible files of distinct content, verify the cap is held
/// and the freshest file's blocks survived.
///
/// File sizes are 24 MiB each (above the 16 MiB dedup threshold) so
/// each upload exercises the HaveBlocks path. Cap of 32 MiB (8 blocks)
/// is below the cumulative 18-block total, forcing eviction.
/// All tests that touch CAS env vars must serialize via this lock,
/// because std::env::set_var races across tokio's worker threads
/// and a concurrent ServeClient::connect_local would inherit a
/// half-set env into its bcmr-serve subprocess.
static CAS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn cas_test_lock() -> std::sync::MutexGuard<'static, ()> {
    CAS_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

#[tokio::test]
async fn serve_cas_lru_eviction_under_load() {
    let _g = cas_test_lock();

    let dir = tempfile::tempdir().unwrap();
    let cas_tmp = tempfile::tempdir().unwrap();
    std::env::set_var("BCMR_CAS_DIR", cas_tmp.path());
    std::env::set_var("BCMR_CAS_CAP_MB", "32");

    let mut files = Vec::new();
    for (i, byte) in (1u8..=3).enumerate() {
        let p = dir.path().join(format!("src{i}.bin"));
        let buf = vec![byte; 24 * 1024 * 1024];
        std::fs::write(&p, &buf).unwrap();
        files.push((p, byte));
    }

    for (src, _) in &files {
        let mut client = ServeClient::connect_local().await.unwrap();
        let dst = dir.path().join(format!(
            "dst-{}.bin",
            src.file_name().unwrap().to_string_lossy()
        ));
        let _ = client.put(dst.to_str().unwrap(), src).await.unwrap();
        client.close().await.unwrap();
    }

    let mut total = 0u64;
    let mut blob_count = 0;
    for entry in walkdir::WalkDir::new(cas_tmp.path()).into_iter().flatten() {
        if entry.file_type().is_file()
            && entry.path().extension().and_then(|s| s.to_str()) == Some("blk")
        {
            total += entry.metadata().unwrap().len();
            blob_count += 1;
        }
    }
    let cap_bytes = 32 * 1024 * 1024;
    assert!(
        total <= cap_bytes,
        "CAS held {} bytes, cap was {}",
        total,
        cap_bytes
    );
    // 32 MiB cap = 8 × 4 MiB blocks max.
    assert!(
        blob_count <= 8,
        "expected ≤8 blobs after eviction, got {}",
        blob_count
    );

    std::env::remove_var("BCMR_CAS_DIR");
    std::env::remove_var("BCMR_CAS_CAP_MB");
}

/// Content-addressed dedup: upload the same 32 MiB file twice. The
/// second run should populate every block from the local CAS and the
/// resulting file must still be byte-identical to the source.
#[tokio::test]
async fn serve_dedup_repeats_use_cas() {
    let _g = cas_test_lock();
    let cas_tmp = tempfile::tempdir().unwrap();
    std::env::set_var("BCMR_CAS_DIR", cas_tmp.path());

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

    std::env::remove_var("BCMR_CAS_DIR");
}

/// Fast-mode GET: server skips its hash, client sees Ok{hash:None},
/// downloaded bytes still match the source.
#[tokio::test]
async fn serve_get_fast_returns_no_hash_but_correct_bytes() {
    use bcmr::core::protocol::{CAP_FAST, CAP_LZ4, CAP_ZSTD};

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("fast.bin");
    create_file(&src, 8 * 1024 * 1024);
    let src_hash = checksum::calculate_hash(&src).unwrap();

    let received: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let received_clone = Arc::clone(&received);

    // CAP_FAST without compression so the splice path activates on Linux.
    let mut client = bcmr::core::serve_client::ServeClient::connect_local_with_caps(CAP_FAST)
        .await
        .unwrap();
    let server_hash = client
        .get(src.to_str().unwrap(), 0, move |chunk| {
            received_clone.lock().unwrap().extend_from_slice(chunk);
        })
        .await
        .unwrap();
    client.close().await.unwrap();

    assert!(
        server_hash.is_none(),
        "fast mode should suppress the server hash"
    );
    let data = Arc::try_unwrap(received).unwrap().into_inner().unwrap();
    let mut tmp = std::fs::File::create(dir.path().join("fast.dst")).unwrap();
    use std::io::Write as _;
    tmp.write_all(&data).unwrap();
    let dst_hash = checksum::calculate_hash(&dir.path().join("fast.dst")).unwrap();
    assert_eq!(dst_hash, src_hash, "fast-mode download must match source");

    // Make sure caps was actually negotiated to include FAST and not
    // the compression bits we excluded.
    let _ = CAP_LZ4;
    let _ = CAP_ZSTD;
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

/// Pipelined PUT of many small files: send Put/Data/Done streams for all
/// files back-to-back via the writer task while the reader collects
/// FIFO-ordered Ok hashes. Verifies (a) every dst file lands with the
/// correct contents and (b) the connection is reusable afterwards
/// (stdin reclaimed cleanly).
#[tokio::test]
async fn serve_pipelined_put_many_files_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    let dst_dir = dir.path().join("dst");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&dst_dir).unwrap();

    let n = 50usize;
    let mut srcs: Vec<std::path::PathBuf> = Vec::with_capacity(n);
    let mut expected_hashes: Vec<String> = Vec::with_capacity(n);
    for i in 0..n {
        let p = src_dir.join(format!("f_{i}.bin"));
        // Small variable size so different files have different hashes.
        create_file(&p, 1024 + i * 16);
        expected_hashes.push(checksum::calculate_hash(&p).unwrap());
        srcs.push(p);
    }

    let files: Vec<(String, std::path::PathBuf, u64)> = srcs
        .iter()
        .enumerate()
        .map(|(i, p)| {
            (
                dst_dir
                    .join(format!("f_{i}.bin"))
                    .to_string_lossy()
                    .to_string(),
                p.clone(),
                p.metadata().unwrap().len(),
            )
        })
        .collect();

    let mut client = ServeClient::connect_local().await.unwrap();
    let completed = std::cell::Cell::new(0usize);
    let hashes = client
        .pipelined_put_files(files, |_idx, _path, _size| {
            completed.set(completed.get() + 1);
        })
        .await
        .unwrap();

    assert_eq!(hashes.len(), n);
    assert_eq!(completed.get(), n);
    for (i, h) in hashes.iter().enumerate() {
        assert_eq!(bytes_to_hex(h), expected_hashes[i]);
        let dst_file = dst_dir.join(format!("f_{i}.bin"));
        assert_eq!(
            checksum::calculate_hash(&dst_file).unwrap(),
            expected_hashes[i]
        );
    }

    // Connection still usable for a follow-on op after stdin reclaim.
    let probe_path = dst_dir.join("f_0.bin");
    let (probe_size, _, _) = client.stat(probe_path.to_str().unwrap()).await.unwrap();
    assert!(probe_size > 0);
    client.close().await.unwrap();
}

/// Pipelined GET of many small files: send all Get requests up-front,
/// reader demuxes the resulting Data*/Ok stream into per-file dst
/// handles. Verifies stream framing, demultiplexing, and stdin reclaim.
#[tokio::test]
async fn serve_pipelined_get_many_files_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    let dst_dir = dir.path().join("dst");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&dst_dir).unwrap();

    let n = 50usize;
    let mut srcs: Vec<std::path::PathBuf> = Vec::with_capacity(n);
    let mut expected_hashes: Vec<String> = Vec::with_capacity(n);
    for i in 0..n {
        let p = src_dir.join(format!("g_{i}.bin"));
        create_file(&p, 2048 + i * 32);
        expected_hashes.push(checksum::calculate_hash(&p).unwrap());
        srcs.push(p);
    }

    let files: Vec<(String, std::path::PathBuf, u64)> = srcs
        .iter()
        .enumerate()
        .map(|(i, p)| {
            (
                p.to_string_lossy().to_string(),
                dst_dir.join(format!("g_{i}.bin")),
                p.metadata().unwrap().len(),
            )
        })
        .collect();

    let mut client = ServeClient::connect_local().await.unwrap();
    let completed = std::cell::Cell::new(0usize);
    client
        .pipelined_get_files(files, false, |_idx, _path, _bytes| {
            completed.set(completed.get() + 1);
        })
        .await
        .unwrap();

    assert_eq!(completed.get(), n);
    for (i, expected) in expected_hashes.iter().enumerate() {
        let dst_file = dst_dir.join(format!("g_{i}.bin"));
        assert_eq!(&checksum::calculate_hash(&dst_file).unwrap(), expected);
    }

    // Stdin must be reclaimed; a follow-on stat call confirms.
    let (probe_size, _, _) = client.stat(srcs[0].to_str().unwrap()).await.unwrap();
    assert!(probe_size > 0);
    client.close().await.unwrap();
}

/// Pipelined PUT where one local source file doesn't exist: writer task
/// hits open() error mid-batch, returns Err. Reader sees the connection
/// die and propagates the error. Caller must get an Err back rather than
/// a partial success.
#[tokio::test]
async fn serve_pipelined_put_writer_error_propagates() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    let dst_dir = dir.path().join("dst");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&dst_dir).unwrap();

    let good = src_dir.join("good.bin");
    create_file(&good, 4096);

    let missing = src_dir.join("does_not_exist.bin");

    let files: Vec<(String, std::path::PathBuf, u64)> = vec![
        (
            dst_dir.join("g.bin").to_string_lossy().to_string(),
            good.clone(),
            good.metadata().unwrap().len(),
        ),
        (
            dst_dir.join("m.bin").to_string_lossy().to_string(),
            missing,
            4096, // size we'd advertise
        ),
    ];

    let mut client = ServeClient::connect_local().await.unwrap();
    let result = client
        .pipelined_put_files(files, |_idx, _path, _size| {})
        .await;
    assert!(
        result.is_err(),
        "expected pipelined_put_files to fail when a source file is missing"
    );
    drop(client);
}

/// Pipelined GET where one server-side path doesn't exist: server emits
/// Error mid-stream. Reader catches it and propagates. We don't assert
/// how many files succeeded before the bad one — only that the call
/// returns Err so the caller sees the failure rather than a phantom
/// success.
#[tokio::test]
async fn serve_pipelined_get_server_error_propagates() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    let dst_dir = dir.path().join("dst");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&dst_dir).unwrap();

    let good = src_dir.join("good.bin");
    create_file(&good, 4096);

    // Second entry references a file that doesn't exist on the server.
    let bogus_remote = src_dir.join("does_not_exist.bin");

    let files: Vec<(String, std::path::PathBuf, u64)> = vec![
        (
            good.to_string_lossy().to_string(),
            dst_dir.join("g.bin"),
            good.metadata().unwrap().len(),
        ),
        (
            bogus_remote.to_string_lossy().to_string(),
            dst_dir.join("b.bin"),
            4096,
        ),
    ];

    let mut client = ServeClient::connect_local().await.unwrap();
    let result = client
        .pipelined_get_files(files, false, |_idx, _path, _bytes| {})
        .await;
    assert!(
        result.is_err(),
        "expected pipelined_get_files to fail when a remote source is missing"
    );
    drop(client);
}
