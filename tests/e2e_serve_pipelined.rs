#![cfg(unix)]
//! End-to-end tests for the pipelined transfer paths: single-client
//! `pipelined_put_files` / `pipelined_get_files` and the N-way
//! `ServeClientPool::*_striped` variants.

mod common;

use common::{bytes_to_hex, create_file};
use std::fs;
use std::path::Path;
use std::sync::Arc;

use bcmr::core::checksum;
use bcmr::core::serve_client::{FileTransfer, ServeClient, ServeClientPool};

/// Pipelined PUT of many small files: send Put/Data/Done streams for all
/// files back-to-back via the writer task while the reader collects
/// FIFO-ordered Ok hashes. Verifies every dst file lands with the
/// correct contents and the connection is reusable afterwards.
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

    let files: Vec<FileTransfer> = srcs
        .iter()
        .enumerate()
        .map(|(i, p)| FileTransfer {
            remote: p.to_string_lossy().to_string(),
            local: dst_dir.join(format!("g_{i}.bin")),
            size: p.metadata().unwrap().len(),
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
    assert!(probe_size > 0);
    client.close().await.unwrap();
}

/// Pipelined PUT where one local source file doesn't exist: writer task
/// hits open() error mid-batch, returns Err. Reader sees the connection
/// die and propagates the error.
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
        },
        FileTransfer {
            remote: dst_dir.join("m.bin").to_string_lossy().to_string(),
            local: missing,
            size: 4096,
        },
    ];

    let mut client = ServeClient::connect_local().await.unwrap();
    let result = client
        .pipelined_put_files(files, |_| {}, |_idx, _path: &Path, _size| {})
        .await;
    assert!(
        result.is_err(),
        "expected pipelined_put_files to fail when a source file is missing"
    );
    drop(client);
}

/// Pipelined GET where one server-side path doesn't exist: server emits
/// Error mid-stream. Reader catches it and propagates.
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

    let files: Vec<FileTransfer> = vec![
        FileTransfer {
            remote: good.to_string_lossy().to_string(),
            local: dst_dir.join("g.bin"),
            size: good.metadata().unwrap().len(),
        },
        FileTransfer {
            remote: bogus_remote.to_string_lossy().to_string(),
            local: dst_dir.join("b.bin"),
            size: 4096,
        },
    ];

    let mut client = ServeClient::connect_local().await.unwrap();
    let result = client
        .pipelined_get_files(files, false, |_idx, _path: &Path, _size| {}, |_n| {})
        .await;
    assert!(
        result.is_err(),
        "expected pipelined_get_files to fail when a remote source is missing"
    );
    drop(client);
}

/// ServeClientPool with N=4: round-robin striping works end-to-end.
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

/// ServeClientPool GET with N=4: mirror of the PUT test.
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

/// Pool with N=1 must behave identically to a single ServeClient.
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
    }];

    let mut pool = ServeClientPool::connect_local(1).await.unwrap();
    assert_eq!(pool.len(), 1);
    let hashes = pool
        .pipelined_put_files_striped(files, |_| {}, |_, _: &Path, _| {})
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

/// ServeClientPool error propagation: any bucket's writer task
/// erroring mid-batch must cancel the siblings and surface Err.
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
        });
    }

    let mut pool = ServeClientPool::connect_local(4).await.unwrap();
    let result = pool
        .pipelined_put_files_striped(files, |_| {}, |_, _: &Path, _| {})
        .await;
    assert!(
        result.is_err(),
        "one bucket failing must propagate as Err from the pool, got {result:?}"
    );
    drop(pool);
}
