#![cfg(unix)]

mod common;

use common::{bytes_to_hex, create_file};
use std::fs;
use std::path::Path;
use std::sync::Arc;

use bcmr::core::checksum;
use bcmr::core::serve_client::{FileTransfer, ServeClient, ServeClientPool};

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
        create_file(&p, 1024 + i * 16);
        expected_hashes.push(checksum::calculate_hash(&p).unwrap());
        srcs.push(p);
    }

    let files: Vec<FileTransfer> = srcs
        .iter()
        .enumerate()
        .map(|(i, p)| FileTransfer {
            remote: dst_dir
                .join(format!("f_{i}.bin"))
                .to_string_lossy()
                .to_string(),
            local: p.clone(),
            size: p.metadata().unwrap().len(),
            metadata: None,
        })
        .collect();
    let total_expected: u64 = files.iter().map(|f| f.size).sum();

    let mut client = ServeClient::connect_local().await.unwrap();
    let completed = std::cell::Cell::new(0usize);
    let chunk_bytes = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let chunk_bytes_w = Arc::clone(&chunk_bytes);
    let hashes = client
        .pipelined_put_files(
            files,
            false,
            move |n| {
                chunk_bytes_w.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
            },
            |_idx, _path: &Path, _size| {
                completed.set(completed.get() + 1);
            },
        )
        .await
        .unwrap();

    assert_eq!(hashes.len(), n);
    assert_eq!(completed.get(), n);
    assert_eq!(
        chunk_bytes.load(std::sync::atomic::Ordering::Relaxed),
        total_expected,
        "chunk callback must report every byte the writer sent"
    );
    for (i, h) in hashes.iter().enumerate() {
        assert_eq!(bytes_to_hex(h), expected_hashes[i]);
        let dst_file = dst_dir.join(format!("f_{i}.bin"));
        assert_eq!(
            checksum::calculate_hash(&dst_file).unwrap(),
            expected_hashes[i]
        );
    }

    let probe_path = dst_dir.join("f_0.bin");
    let (probe_size, _, _) = client.stat(probe_path.to_str().unwrap()).await.unwrap();
    assert_eq!(
        probe_size, 1024,
        "first file should be exactly its declared size"
    );
    client.close().await.unwrap();
}

#[tokio::test]
async fn serve_pipelined_put_enforces_the_batch_overwrite_policy() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("source.bin");
    let dst = dir.path().join("destination.bin");
    fs::write(&src, b"replacement").unwrap();
    fs::write(&dst, b"original").unwrap();

    let transfer = || FileTransfer {
        remote: dst.to_string_lossy().into_owned(),
        local: src.clone(),
        size: src.metadata().unwrap().len(),
        metadata: None,
    };

    let mut refusing_client = ServeClient::connect_local().await.unwrap();
    let refused = refusing_client
        .pipelined_put_files(
            vec![transfer()],
            false,
            |_| {},
            |_idx, _path: &Path, _size| {},
        )
        .await;
    assert!(refused.is_err());
    assert_eq!(fs::read(&dst).unwrap(), b"original");
    drop(refusing_client);

    let mut overwrite_client = ServeClient::connect_local().await.unwrap();
    overwrite_client
        .pipelined_put_files(
            vec![transfer()],
            true,
            |_| {},
            |_idx, _path: &Path, _size| {},
        )
        .await
        .unwrap();
    overwrite_client.close().await.unwrap();
    assert_eq!(fs::read(&dst).unwrap(), b"replacement");
}

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

    let files: Vec<FileTransfer> = srcs
        .iter()
        .enumerate()
        .map(|(i, p)| FileTransfer {
            remote: p.to_string_lossy().to_string(),
            local: dst_dir.join(format!("g_{i}.bin")),
            size: p.metadata().unwrap().len(),
            metadata: None,
        })
        .collect();
    let total_expected: u64 = files.iter().map(|f| f.size).sum();

    let mut client = ServeClient::connect_local().await.unwrap();
    let started = std::cell::Cell::new(0usize);
    let received = std::cell::Cell::new(0u64);
    client
        .pipelined_get_files(
            files,
            false,
            true,
            |_idx, _path: &Path, _size| {
                started.set(started.get() + 1);
            },
            |n| {
                received.set(received.get() + n);
            },
        )
        .await
        .unwrap();

    assert_eq!(started.get(), n, "on_file_start must fire once per file");
    assert_eq!(
        received.get(),
        total_expected,
        "on_chunk must report every byte received across the batch"
    );
    for (i, expected) in expected_hashes.iter().enumerate() {
        let dst_file = dst_dir.join(format!("g_{i}.bin"));
        assert_eq!(&checksum::calculate_hash(&dst_file).unwrap(), expected);
    }

    let (probe_size, _, _) = client.stat(srcs[0].to_str().unwrap()).await.unwrap();
    assert_eq!(
        probe_size, 2048,
        "first source should be exactly its declared size"
    );
    client.close().await.unwrap();
}

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

    let files: Vec<FileTransfer> = vec![
        FileTransfer {
            remote: dst_dir.join("g.bin").to_string_lossy().to_string(),
            local: good.clone(),
            size: good.metadata().unwrap().len(),
            metadata: None,
        },
        FileTransfer {
            remote: dst_dir.join("m.bin").to_string_lossy().to_string(),
            local: missing,
            size: 4096,
            metadata: None,
        },
    ];

    let mut client = ServeClient::connect_local().await.unwrap();
    let result = client
        .pipelined_put_files(files, false, |_| {}, |_idx, _path: &Path, _size| {})
        .await;
    assert!(
        result.is_err(),
        "expected pipelined_put_files to fail when a source file is missing"
    );
    drop(client);
}

#[tokio::test]
async fn serve_pipelined_get_server_error_propagates() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    let dst_dir = dir.path().join("dst");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&dst_dir).unwrap();

    let good = src_dir.join("good.bin");
    create_file(&good, 4096);

    let bogus_remote = src_dir.join("does_not_exist.bin");
    let untouched_destination = dst_dir.join("b.bin");
    let original_destination = b"pre-existing destination must survive";
    fs::write(&untouched_destination, original_destination).unwrap();

    let files: Vec<FileTransfer> = vec![
        FileTransfer {
            remote: good.to_string_lossy().to_string(),
            local: dst_dir.join("g.bin"),
            size: good.metadata().unwrap().len(),
            metadata: None,
        },
        FileTransfer {
            remote: bogus_remote.to_string_lossy().to_string(),
            local: untouched_destination.clone(),
            size: 4096,
            metadata: None,
        },
    ];

    let mut client = ServeClient::connect_local().await.unwrap();
    let result = client
        .pipelined_get_files(files, false, false, |_idx, _path: &Path, _size| {}, |_n| {})
        .await;
    assert!(
        result.is_err(),
        "expected pipelined_get_files to fail when a remote source is missing"
    );
    assert_eq!(
        fs::read(&untouched_destination).unwrap(),
        original_destination,
        "a failed GET must not truncate or delete the destination that existed before transfer"
    );
    assert!(
        fs::read_dir(&dst_dir).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".bcmr.receive.")),
        "a failed GET must clean its sibling staging file"
    );
    drop(client);
}

#[tokio::test]
async fn serve_pipelined_get_refuses_a_destination_replaced_after_staging() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("remote.bin");
    let dst = dir.path().join("destination.bin");
    let replacement = dir.path().join("replacement.bin");
    create_file(&src, 4096);
    fs::write(&dst, b"original destination").unwrap();
    fs::write(&replacement, b"concurrent replacement").unwrap();

    let files = vec![FileTransfer {
        remote: src.to_string_lossy().to_string(),
        local: dst.clone(),
        size: src.metadata().unwrap().len(),
        metadata: None,
    }];

    let mut client = ServeClient::connect_local().await.unwrap();
    let dst_for_callback = dst.clone();
    let replacement_for_callback = replacement.clone();
    let result = client
        .pipelined_get_files(
            files,
            false,
            false,
            move |_idx, _path: &Path, _size| {
                fs::rename(&replacement_for_callback, &dst_for_callback).unwrap();
            },
            |_n| {},
        )
        .await;

    assert!(
        result.is_err(),
        "commit must fail closed when another writer replaces the destination"
    );
    assert_eq!(
        fs::read(&dst).unwrap(),
        b"concurrent replacement",
        "the transfer must not overwrite the competing destination entry"
    );
    drop(client);
}

#[tokio::test]
async fn serve_pipelined_verify_rejects_tampered_staging_before_publish() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("remote.bin");
    let dst_dir = dir.path().join("dst");
    let dst = dst_dir.join("destination.bin");
    fs::create_dir_all(&dst_dir).unwrap();
    fs::write(&src, b"authentic remote payload").unwrap();
    fs::write(&dst, b"original destination").unwrap();

    let files = vec![FileTransfer {
        remote: src.to_string_lossy().to_string(),
        local: dst.clone(),
        size: src.metadata().unwrap().len(),
        metadata: None,
    }];

    let mut client = ServeClient::connect_local().await.unwrap();
    let tamper_parent = dst_dir.clone();
    let tamper_len = src.metadata().unwrap().len() as usize;
    let result = client
        .pipelined_get_files(
            files,
            false,
            true,
            |_idx, _path: &Path, _size| {},
            move |_n| {
                let transaction = fs::read_dir(&tamper_parent)
                    .unwrap()
                    .map(|entry| entry.unwrap())
                    .find(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".bcmr.receive.")
                    })
                    .expect("the private receive transaction must exist during transfer");
                fs::write(transaction.path().join("payload"), vec![b'X'; tamper_len]).unwrap();
            },
        )
        .await;

    assert!(
        matches!(
            result,
            Err(bcmr::core::error::BcmrError::VerificationError(ref path)) if path == &dst
        ),
        "the streamed hash mismatch must be reported before publication: {result:?}"
    );
    assert_eq!(
        fs::read(&dst).unwrap(),
        b"original destination",
        "verification failure must preserve the previously visible destination"
    );
    assert!(
        fs::read_dir(&dst_dir).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".bcmr.receive.")),
        "verification failure must clean the private transaction"
    );
    drop(client);
}

#[tokio::test]
async fn serve_pool_pipelined_put_n4_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    let dst_dir = dir.path().join("dst");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&dst_dir).unwrap();

    let n = 100usize;
    let mut srcs: Vec<std::path::PathBuf> = Vec::with_capacity(n);
    let mut expected_hashes: Vec<String> = Vec::with_capacity(n);
    for i in 0..n {
        let p = src_dir.join(format!("p_{i}.bin"));
        create_file(&p, 512 + i * 8);
        expected_hashes.push(checksum::calculate_hash(&p).unwrap());
        srcs.push(p);
    }

    let files: Vec<FileTransfer> = srcs
        .iter()
        .enumerate()
        .map(|(i, p)| FileTransfer {
            remote: dst_dir
                .join(format!("p_{i}.bin"))
                .to_string_lossy()
                .to_string(),
            local: p.clone(),
            size: p.metadata().unwrap().len(),
            metadata: None,
        })
        .collect();

    let mut pool = ServeClientPool::connect_local(4).await.unwrap();
    assert_eq!(pool.len(), 4);

    let bytes_via_chunks = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let completions = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let chunks = Arc::clone(&bytes_via_chunks);
    let completes = Arc::clone(&completions);
    let hashes = pool
        .pipelined_put_files_striped(
            files,
            false,
            move |n| {
                chunks.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
            },
            move |_idx, _path: &Path, _size| {
                completes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            },
        )
        .await
        .unwrap();

    assert_eq!(hashes.len(), n);
    for (i, h) in hashes.iter().enumerate() {
        assert_eq!(
            bytes_to_hex(h),
            expected_hashes[i],
            "hash at index {i} must match input-order source file"
        );
        let dst = dst_dir.join(format!("p_{i}.bin"));
        assert_eq!(checksum::calculate_hash(&dst).unwrap(), expected_hashes[i]);
    }
    assert_eq!(completions.load(std::sync::atomic::Ordering::Relaxed), n);
    let total_size: u64 = (0..n).map(|i| (512 + i * 8) as u64).sum();
    assert_eq!(
        bytes_via_chunks.load(std::sync::atomic::Ordering::Relaxed),
        total_size,
        "chunk callback must fire for every byte across the 4 writer tasks"
    );

    pool.close().await.unwrap();
}

#[tokio::test]
async fn serve_pool_pipelined_get_n4_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    let dst_dir = dir.path().join("dst");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&dst_dir).unwrap();

    let n = 100usize;
    let mut srcs: Vec<std::path::PathBuf> = Vec::with_capacity(n);
    let mut expected_hashes: Vec<String> = Vec::with_capacity(n);
    for i in 0..n {
        let p = src_dir.join(format!("g_{i}.bin"));
        create_file(&p, 1024 + i * 16);
        expected_hashes.push(checksum::calculate_hash(&p).unwrap());
        srcs.push(p);
    }

    let files: Vec<FileTransfer> = srcs
        .iter()
        .enumerate()
        .map(|(i, p)| FileTransfer {
            remote: p.to_string_lossy().to_string(),
            local: dst_dir.join(format!("g_{i}.bin")),
            size: p.metadata().unwrap().len(),
            metadata: None,
        })
        .collect();

    let mut pool = ServeClientPool::connect_local(4).await.unwrap();

    let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let chunks = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let starts_c = Arc::clone(&starts);
    let chunks_c = Arc::clone(&chunks);

    pool.pipelined_get_files_striped(
        files,
        false,
        false,
        move |_idx, _path: &Path, _size| {
            starts_c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        },
        move |n| {
            chunks_c.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
        },
    )
    .await
    .unwrap();

    assert_eq!(starts.load(std::sync::atomic::Ordering::Relaxed), n);
    let total_expected: u64 = (0..n).map(|i| (1024 + i * 16) as u64).sum();
    assert_eq!(
        chunks.load(std::sync::atomic::Ordering::Relaxed),
        total_expected
    );
    for (i, expected) in expected_hashes.iter().enumerate() {
        let dst = dst_dir.join(format!("g_{i}.bin"));
        assert_eq!(&checksum::calculate_hash(&dst).unwrap(), expected);
    }

    pool.close().await.unwrap();
}

#[tokio::test]
async fn serve_pool_n1_degenerate_behaves_like_single_client() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    let dst_dir = dir.path().join("dst");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&dst_dir).unwrap();

    let src = src_dir.join("one.bin");
    create_file(&src, 8192);
    let expected = checksum::calculate_hash(&src).unwrap();

    let files = vec![FileTransfer {
        remote: dst_dir.join("one.bin").to_string_lossy().to_string(),
        local: src.clone(),
        size: src.metadata().unwrap().len(),
        metadata: None,
    }];

    let mut pool = ServeClientPool::connect_local(1).await.unwrap();
    assert_eq!(pool.len(), 1);
    let hashes = pool
        .pipelined_put_files_striped(files, false, |_| {}, |_, _: &Path, _| {})
        .await
        .unwrap();
    assert_eq!(hashes.len(), 1);
    assert_eq!(bytes_to_hex(&hashes[0]), expected);
    assert_eq!(
        checksum::calculate_hash(&dst_dir.join("one.bin")).unwrap(),
        expected
    );
    pool.close().await.unwrap();
}

#[tokio::test]
async fn serve_pool_one_bucket_error_cancels_siblings() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    let dst_dir = dir.path().join("dst");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&dst_dir).unwrap();

    let n = 12usize;
    let bad_idx = 7usize;
    let mut files: Vec<FileTransfer> = Vec::with_capacity(n);
    for i in 0..n {
        let p = if i == bad_idx {
            src_dir.join("does_not_exist.bin")
        } else {
            let p = src_dir.join(format!("ok_{i}.bin"));
            create_file(&p, 2048 + i * 16);
            p
        };
        files.push(FileTransfer {
            remote: dst_dir
                .join(format!("x_{i}.bin"))
                .to_string_lossy()
                .to_string(),
            local: p,
            size: if i == bad_idx { 4096 } else { 2048 + i * 16 } as u64,
            metadata: None,
        });
    }

    let mut pool = ServeClientPool::connect_local(4).await.unwrap();
    let result = pool
        .pipelined_put_files_striped(files, false, |_| {}, |_, _: &Path, _| {})
        .await;
    assert!(
        result.is_err(),
        "one bucket failing must propagate as Err from the pool, got {result:?}"
    );
    drop(pool);
}
