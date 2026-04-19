#![cfg(unix)]

mod common;

use common::{bytes_to_hex, cas_test_lock, create_file, spawn_serve, ServeChild};
use std::fs;
use std::sync::{Arc, Mutex};

use bcmr::core::checksum;
use bcmr::core::serve_client::ServeClient;

#[tokio::test]
async fn serve_handshake() {
    let client = ServeClient::connect_local().await.unwrap();
    client.close().await.unwrap();
}

#[tokio::test]
async fn serve_root_jail_rejects_escape() {
    let jail = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let src = outside.path().join("payload.bin");
    std::fs::write(&src, b"hello").unwrap();

    let ServeChild {
        mut child,
        mut stdin,
        mut stdout,
    } = spawn_serve(jail.path().to_str().unwrap());
    use bcmr::core::protocol::{read_message, write_message, Message, PROTOCOL_VERSION};
    write_message(
        &mut stdin,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            caps: 0,
        },
    )
    .await
    .unwrap();
    let _ = read_message(&mut stdout).await.unwrap();
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

#[tokio::test]
async fn serve_put_size_bound_rejects_oversized() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("capped.bin");

    use bcmr::core::protocol::{read_message, write_message, Message, PROTOCOL_VERSION};
    let ServeChild {
        mut child,
        mut stdin,
        mut stdout,
    } = spawn_serve("/");

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
async fn serve_put_size_bound_rejects_short_write() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("truncated.bin");

    use bcmr::core::protocol::{read_message, write_message, Message, PROTOCOL_VERSION};
    let ServeChild {
        mut child,
        mut stdin,
        mut stdout,
    } = spawn_serve("/");

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
    write_message(
        &mut stdin,
        &Message::Data {
            payload: vec![0xcd; 5],
        },
    )
    .await
    .unwrap();
    write_message(&mut stdin, &Message::Done).await.unwrap();

    let reply = read_message(&mut stdout).await.unwrap().unwrap();
    match reply {
        Message::Error { message } => {
            assert!(
                message.contains("declared 10 bytes, received 5"),
                "expected short-write error, got: {message}"
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

#[tokio::test]
async fn serve_get_compressible_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.txt");
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
    assert!(
        blob_count <= 8,
        "expected ≤8 blobs after eviction, got {}",
        blob_count
    );

    std::env::remove_var("BCMR_CAS_DIR");
    std::env::remove_var("BCMR_CAS_CAP_MB");
}

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

    let mut client = ServeClient::connect_local().await.unwrap();
    let h1 = client.put(dst1.to_str().unwrap(), &src).await.unwrap();
    client.close().await.unwrap();
    assert_eq!(bytes_to_hex(&h1), src_hash);
    assert_eq!(checksum::calculate_hash(&dst1).unwrap(), src_hash);

    let mut client = ServeClient::connect_local().await.unwrap();
    let h2 = client.put(dst2.to_str().unwrap(), &src).await.unwrap();
    client.close().await.unwrap();
    assert_eq!(bytes_to_hex(&h2), src_hash);
    assert_eq!(checksum::calculate_hash(&dst2).unwrap(), src_hash);

    std::env::remove_var("BCMR_CAS_DIR");
}

#[tokio::test]
async fn serve_get_fast_returns_no_hash_but_correct_bytes() {
    use bcmr::core::protocol::CAP_FAST;

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("fast.bin");
    create_file(&src, 8 * 1024 * 1024);
    let src_hash = checksum::calculate_hash(&src).unwrap();

    let received: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let received_clone = Arc::clone(&received);

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
}

#[tokio::test]
async fn serve_get_fast_with_offset_past_eof_returns_empty() {
    use bcmr::core::protocol::CAP_FAST;

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("fast-offset.bin");
    create_file(&src, 1024 * 1024);

    let received: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let received_clone = Arc::clone(&received);

    let mut client = bcmr::core::serve_client::ServeClient::connect_local_with_caps(CAP_FAST)
        .await
        .unwrap();
    let server_hash = tokio::time::timeout(
        tokio::time::Duration::from_secs(5),
        client.get(src.to_str().unwrap(), 2 * 1024 * 1024, move |chunk| {
            received_clone.lock().unwrap().extend_from_slice(chunk);
        }),
    )
    .await
    .expect("fast GET with offset past EOF should terminate")
    .unwrap();
    client.close().await.unwrap();

    assert!(
        server_hash.is_none(),
        "fast mode should still suppress the server hash past EOF"
    );
    let data = Arc::try_unwrap(received).unwrap().into_inner().unwrap();
    assert!(data.is_empty(), "expected no bytes beyond EOF");
}

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
