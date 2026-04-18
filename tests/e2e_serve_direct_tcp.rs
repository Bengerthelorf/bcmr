#![cfg(unix)]

mod common;

use common::{bytes_to_hex, create_file, spawn_serve, spawn_serve_with_env, ServeChild};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bcmr::core::checksum;
use bcmr::core::serve_client::{FileTransfer, ServeClient, ServeClientPool};

#[tokio::test]
async fn serve_open_direct_channel_reply_is_well_formed() {
    use bcmr::core::protocol::{
        read_message, write_message, Message, CAP_DIRECT_TCP, PROTOCOL_VERSION,
    };
    use tokio::net::TcpStream;

    // Tight rendezvous timeout so the per-channel listener doesn't pin the
    // child for the full 30 s default.
    let ServeChild {
        mut child,
        mut stdin,
        mut stdout,
    } = spawn_serve_with_env("/", &[("BCMR_RENDEZVOUS_TIMEOUT_SECS", "2")]);

    write_message(
        &mut stdin,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            caps: CAP_DIRECT_TCP,
        },
    )
    .await
    .unwrap();
    let _welcome = read_message(&mut stdout).await.unwrap();

    write_message(&mut stdin, &Message::OpenDirectChannel)
        .await
        .unwrap();
    let reply1 = read_message(&mut stdout).await.unwrap().unwrap();
    let (addr1, key1) = match reply1 {
        Message::DirectChannelReady { addr, session_key } => (addr, session_key),
        other => panic!("expected DirectChannelReady, got {other:?}"),
    };

    let parsed: std::net::SocketAddr = addr1.parse().expect("reply addr must parse");
    assert_ne!(parsed.port(), 0, "listener must bind to a concrete port");
    assert_ne!(key1, [0u8; 32], "session key must be randomised");

    let sock = TcpStream::connect(parsed).await.expect("dial accept");
    drop(sock);

    write_message(&mut stdin, &Message::OpenDirectChannel)
        .await
        .unwrap();
    let reply2 = read_message(&mut stdout).await.unwrap().unwrap();
    let key2 = match reply2 {
        Message::DirectChannelReady { session_key, .. } => session_key,
        other => panic!("expected DirectChannelReady on 2nd request, got {other:?}"),
    };
    assert_ne!(
        key1, key2,
        "two rendezvous requests must produce different session keys"
    );

    drop(stdin);
    let _ = child.wait().await;
}

#[tokio::test]
async fn serve_open_direct_channel_requires_cap() {
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

    write_message(&mut stdin, &Message::OpenDirectChannel)
        .await
        .unwrap();
    match read_message(&mut stdout).await.unwrap().unwrap() {
        Message::Error { message } => {
            assert!(
                message.contains("CAP_DIRECT_TCP"),
                "error should mention the missing cap, got: {message}"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }

    drop(stdin);
    let _ = child.wait().await;
}

#[tokio::test]
async fn serve_direct_tcp_put_get_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let remote_dst = dir.path().join("remote_dst.bin");
    create_file(&src, 3 * 1024 * 1024);
    let src_hash = checksum::calculate_hash(&src).unwrap();

    let mut client = ServeClient::connect_direct_local().await.unwrap();
    assert!(
        client.is_aead_negotiated(),
        "default direct-TCP connect must flip to AEAD framing",
    );
    let put_hash = client
        .put(remote_dst.to_str().unwrap(), &src)
        .await
        .unwrap();
    assert_eq!(bytes_to_hex(&put_hash), src_hash);

    let received: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let received_clone = Arc::clone(&received);
    client
        .get(remote_dst.to_str().unwrap(), 0, move |chunk| {
            received_clone.lock().unwrap().extend_from_slice(chunk);
        })
        .await
        .unwrap();
    client.close().await.unwrap();

    let got = Arc::try_unwrap(received).unwrap().into_inner().unwrap();
    let got_path = dir.path().join("got.bin");
    fs::write(&got_path, &got).unwrap();
    assert_eq!(checksum::calculate_hash(&got_path).unwrap(), src_hash);
    assert_eq!(checksum::calculate_hash(&remote_dst).unwrap(), src_hash);
}

/// Regression guard against a DoS: a squatter that connects first with a
/// bogus AuthHello must NOT consume the rendezvous listener.
#[tokio::test]
async fn serve_direct_tcp_squatter_does_not_starve_real_client() {
    use bcmr::core::protocol::{
        read_message, write_message, Message, CAP_AEAD, CAP_DIRECT_TCP, PROTOCOL_VERSION,
    };
    use tokio::net::TcpStream;

    let ServeChild {
        mut child,
        stdin: mut ssh_stdin,
        stdout: mut ssh_stdout,
    } = spawn_serve("/");

    write_message(
        &mut ssh_stdin,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            caps: CAP_DIRECT_TCP,
        },
    )
    .await
    .unwrap();
    let _ = read_message(&mut ssh_stdout).await.unwrap();
    write_message(&mut ssh_stdin, &Message::OpenDirectChannel)
        .await
        .unwrap();
    let (addr, key) = match read_message(&mut ssh_stdout).await.unwrap().unwrap() {
        Message::DirectChannelReady { addr, session_key } => (addr, session_key),
        other => panic!("expected DirectChannelReady, got {other:?}"),
    };

    let squatter = TcpStream::connect(&addr).await.unwrap();
    let (_sr, mut sw) = squatter.into_split();
    write_message(&mut sw, &Message::AuthHello { mac: [0xAAu8; 32] })
        .await
        .unwrap();
    drop(sw);

    // Mirror the real bcmr client flow: close SSH stdin once the rendezvous
    // has handed off, then run the TCP handshake.
    let real = TcpStream::connect(&addr).await.unwrap();
    drop(ssh_stdin);
    let (mut rr, mut rw) = real.into_split();
    let good_mac = *blake3::keyed_hash(&key, b"bcmr-direct-v1").as_bytes();
    write_message(&mut rw, &Message::AuthHello { mac: good_mac })
        .await
        .unwrap();
    write_message(
        &mut rw,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            caps: CAP_AEAD,
        },
    )
    .await
    .unwrap();
    match read_message(&mut rr).await.unwrap() {
        Some(Message::Welcome { .. }) => {}
        other => panic!("real client was starved by squatter, got {other:?}"),
    }

    drop(rw);
    drop(rr);
    let _ = child.wait().await;
}

/// Downgrade-attack guard: a rendezvous-authenticated peer that does NOT
/// advertise CAP_AEAD must be rejected on the direct-TCP channel.
#[tokio::test]
async fn serve_direct_tcp_refuses_session_without_aead() {
    use bcmr::core::protocol::{
        read_message, write_message, Message, CAP_DIRECT_TCP, PROTOCOL_VERSION,
    };
    use tokio::net::TcpStream;

    let ServeChild {
        mut child,
        stdin: mut ssh_stdin,
        stdout: mut ssh_stdout,
    } = spawn_serve("/");

    write_message(
        &mut ssh_stdin,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            caps: CAP_DIRECT_TCP,
        },
    )
    .await
    .unwrap();
    let _welcome = read_message(&mut ssh_stdout).await.unwrap();
    write_message(&mut ssh_stdin, &Message::OpenDirectChannel)
        .await
        .unwrap();
    let (addr, session_key) = match read_message(&mut ssh_stdout).await.unwrap().unwrap() {
        Message::DirectChannelReady { addr, session_key } => (addr, session_key),
        other => panic!("expected DirectChannelReady, got {other:?}"),
    };

    let stream = TcpStream::connect(&addr).await.unwrap();
    let (mut rdr, mut wtr) = stream.into_split();
    let mac = *blake3::keyed_hash(&session_key, b"bcmr-direct-v1").as_bytes();
    write_message(&mut wtr, &Message::AuthHello { mac })
        .await
        .unwrap();
    write_message(
        &mut wtr,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            caps: 0,
        },
    )
    .await
    .unwrap();

    match read_message(&mut rdr).await.unwrap() {
        Some(Message::Error { message }) => {
            assert!(
                message.contains("CAP_AEAD"),
                "expected AEAD-required error, got: {message}"
            );
        }
        other => panic!("direct-TCP session accepted without CAP_AEAD, got {other:?}"),
    }

    drop(ssh_stdin);
    let _ = child.wait().await;
}

/// The SSH transport has no session key to derive an AEAD key from, so it
/// must mask CAP_AEAD off; otherwise clients would attempt AEAD framing that
/// the data plane can't honor.
#[tokio::test]
async fn serve_ssh_transport_does_not_offer_cap_aead() {
    use bcmr::core::protocol::{read_message, write_message, CAP_AEAD, PROTOCOL_VERSION};

    let ServeChild {
        mut child,
        mut stdin,
        mut stdout,
    } = spawn_serve("/");

    write_message(
        &mut stdin,
        &bcmr::core::protocol::Message::Hello {
            version: PROTOCOL_VERSION,
            caps: CAP_AEAD,
        },
    )
    .await
    .unwrap();
    match read_message(&mut stdout).await.unwrap().unwrap() {
        bcmr::core::protocol::Message::Welcome { caps, .. } => {
            assert_eq!(
                caps & CAP_AEAD,
                0,
                "SSH transport must not negotiate CAP_AEAD, got caps={caps:#x}",
            );
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
    drop(stdin);
    let _ = child.wait().await;
}

/// Exercises the split SendHalf / RecvHalf framing over direct-TCP: the
/// writer task owns the send counter + TCP write half, the reader task owns
/// the recv counter + TCP read half.
#[tokio::test]
async fn serve_direct_tcp_pipelined_put_many_files_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    let dst_dir = dir.path().join("dst");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&dst_dir).unwrap();

    let n = 20usize;
    let mut srcs: Vec<PathBuf> = Vec::with_capacity(n);
    let mut expected: Vec<String> = Vec::with_capacity(n);
    for i in 0..n {
        let p = src_dir.join(format!("p_{i}.bin"));
        create_file(&p, 4096 + i * 64);
        expected.push(checksum::calculate_hash(&p).unwrap());
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

    let mut client = ServeClient::connect_direct_local().await.unwrap();
    assert!(
        client.is_aead_negotiated(),
        "direct-TCP must flip to AEAD; pipelining must carry the split send/recv halves through it",
    );
    let hashes = client
        .pipelined_put_files(files, |_n| {}, |_idx, _path: &Path, _size| {})
        .await
        .unwrap();

    assert_eq!(hashes.len(), n);
    for (i, h) in hashes.iter().enumerate() {
        assert_eq!(bytes_to_hex(h), expected[i]);
        let dst = dst_dir.join(format!("p_{i}.bin"));
        assert_eq!(checksum::calculate_hash(&dst).unwrap(), expected[i]);
    }
    client.close().await.unwrap();
}

/// Regression guard: the AEAD recv counter must advance correctly across the
/// demultiplexed Data*/Ok stream during pipelined GET.
#[tokio::test]
async fn serve_direct_tcp_pipelined_get_many_files_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    let dst_dir = dir.path().join("dst");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&dst_dir).unwrap();

    let n = 20usize;
    let mut srcs: Vec<PathBuf> = Vec::with_capacity(n);
    let mut expected: Vec<String> = Vec::with_capacity(n);
    for i in 0..n {
        let p = src_dir.join(format!("g_{i}.bin"));
        create_file(&p, 4096 + i * 64);
        expected.push(checksum::calculate_hash(&p).unwrap());
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

    let mut client = ServeClient::connect_direct_local().await.unwrap();
    assert!(client.is_aead_negotiated());
    client
        .pipelined_get_files(files, false, |_idx, _path: &Path, _size| {}, |_n| {})
        .await
        .unwrap();
    client.close().await.unwrap();

    for (i, expected_hash) in expected.iter().enumerate() {
        let dst = dst_dir.join(format!("g_{i}.bin"));
        assert_eq!(&checksum::calculate_hash(&dst).unwrap(), expected_hash);
    }
}

/// Regression guard: the pool layer must be transport-agnostic — every
/// bucket flips to its own AEAD state and striped scatter/gather still works
/// across four concurrent rendezvous sessions.
#[tokio::test]
async fn serve_direct_tcp_pool_striped_put_n4_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    let dst_dir = dir.path().join("dst");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&dst_dir).unwrap();

    let n = 40usize;
    let mut srcs: Vec<PathBuf> = Vec::with_capacity(n);
    let mut expected: Vec<String> = Vec::with_capacity(n);
    for i in 0..n {
        let p = src_dir.join(format!("s_{i}.bin"));
        create_file(&p, 512 + i * 16);
        expected.push(checksum::calculate_hash(&p).unwrap());
        srcs.push(p);
    }

    let files: Vec<FileTransfer> = srcs
        .iter()
        .enumerate()
        .map(|(i, p)| FileTransfer {
            remote: dst_dir
                .join(format!("s_{i}.bin"))
                .to_string_lossy()
                .to_string(),
            local: p.clone(),
            size: p.metadata().unwrap().len(),
        })
        .collect();

    let mut pool = ServeClientPool::connect_direct_local(4).await.unwrap();
    assert_eq!(pool.len(), 4);

    let received = Arc::new(Mutex::new(0u64));
    let recv_c = Arc::clone(&received);
    let hashes = pool
        .pipelined_put_files_striped(
            files,
            move |n| {
                *recv_c.lock().unwrap() += n;
            },
            |_idx, _path: &Path, _size| {},
        )
        .await
        .unwrap();
    pool.close().await.unwrap();

    assert_eq!(hashes.len(), n);
    for (i, h) in hashes.iter().enumerate() {
        assert_eq!(bytes_to_hex(h), expected[i]);
        let dst = dst_dir.join(format!("s_{i}.bin"));
        assert_eq!(checksum::calculate_hash(&dst).unwrap(), expected[i]);
    }
    let total_expected: u64 = (0..n).map(|i| (512 + i * 16) as u64).sum();
    assert_eq!(*received.lock().unwrap(), total_expected);
}

#[tokio::test]
async fn serve_direct_tcp_striped_put_single_large_file() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("big.bin");
    let dst = dir.path().join("big_dst.bin");
    create_file(&src, 16 * 1024 * 1024 + 731);
    let src_hash_hex = checksum::calculate_hash(&src).unwrap();

    let mut pool = ServeClientPool::connect_direct_local(4).await.unwrap();
    let returned_hash = pool
        .striped_put_file(&src, dst.to_str().unwrap())
        .await
        .unwrap();
    pool.close().await.unwrap();

    assert_eq!(
        bytes_to_hex(&returned_hash),
        src_hash_hex,
        "pool.striped_put_file must return the whole-file BLAKE3 computed client-side",
    );
    assert_eq!(
        checksum::calculate_hash(&dst).unwrap(),
        src_hash_hex,
        "dst on the server side must be byte-identical to src after striped PUT",
    );
    assert_eq!(
        std::fs::metadata(&dst).unwrap().len(),
        std::fs::metadata(&src).unwrap().len(),
        "dst size must match src size exactly — no holes, no overrun",
    );
}

#[tokio::test]
async fn serve_direct_tcp_striped_get_single_large_file() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("big_src.bin");
    let dst = dir.path().join("big_got.bin");
    create_file(&src, 16 * 1024 * 1024 + 501);
    let src_size = std::fs::metadata(&src).unwrap().len();
    let src_hash_hex = checksum::calculate_hash(&src).unwrap();

    let mut pool = ServeClientPool::connect_direct_local(4).await.unwrap();
    let got_hash = pool
        .striped_get_file(src.to_str().unwrap(), &dst, src_size)
        .await
        .unwrap();
    pool.close().await.unwrap();

    assert_eq!(bytes_to_hex(&got_hash), src_hash_hex);
    assert_eq!(checksum::calculate_hash(&dst).unwrap(), src_hash_hex);
    assert_eq!(std::fs::metadata(&dst).unwrap().len(), src_size);
}

/// I1 regression guard: without the Truncate preamble before the chunk
/// fanout, a smaller new src would only overwrite the prefix and the old
/// dst's tail bytes would survive past the new end.
#[tokio::test]
async fn serve_direct_tcp_striped_put_truncates_existing_dst() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("small.bin");
    let dst = dir.path().join("dst.bin");
    std::fs::write(&dst, vec![0xEEu8; 20 * 1024 * 1024]).unwrap();
    create_file(&src, 2 * 1024 * 1024 + 17);
    let src_hash_hex = checksum::calculate_hash(&src).unwrap();

    let mut pool = ServeClientPool::connect_direct_local(4).await.unwrap();
    let _ = pool
        .striped_put_file(&src, dst.to_str().unwrap())
        .await
        .unwrap();
    pool.close().await.unwrap();

    assert_eq!(
        std::fs::metadata(&dst).unwrap().len(),
        std::fs::metadata(&src).unwrap().len(),
        "dst size must match new src exactly — no stale tail bytes",
    );
    assert_eq!(
        checksum::calculate_hash(&dst).unwrap(),
        src_hash_hex,
        "dst content must be byte-identical to new src (no residue of old 0xEE payload)",
    );
}

/// M4 regression guard: the fanout skips empty ranges, so without the
/// Truncate preamble a zero-byte src would leave the dst non-existent.
#[tokio::test]
async fn serve_direct_tcp_striped_put_zero_byte_file() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("empty.bin");
    let dst = dir.path().join("empty_dst.bin");
    std::fs::write(&src, b"").unwrap();

    let mut pool = ServeClientPool::connect_direct_local(4).await.unwrap();
    let returned_hash = pool
        .striped_put_file(&src, dst.to_str().unwrap())
        .await
        .unwrap();
    pool.close().await.unwrap();

    assert!(
        dst.exists(),
        "striped PUT of an empty file must still create the dst"
    );
    assert_eq!(std::fs::metadata(&dst).unwrap().len(), 0);
    assert_eq!(
        bytes_to_hex(&returned_hash),
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    );
}

/// Regression guard: buckets that end up with a zero-byte share on a
/// tiny-file stripe must be skipped, not sent as a degenerate
/// PutChunked{length=0} frame.
#[tokio::test]
async fn serve_direct_tcp_striped_put_tiny_file_skips_empty_buckets() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("tiny.bin");
    let dst = dir.path().join("tiny_dst.bin");
    create_file(&src, 7);
    let src_hash_hex = checksum::calculate_hash(&src).unwrap();

    let mut pool = ServeClientPool::connect_direct_local(4).await.unwrap();
    let returned_hash = pool
        .striped_put_file(&src, dst.to_str().unwrap())
        .await
        .unwrap();
    pool.close().await.unwrap();

    assert_eq!(bytes_to_hex(&returned_hash), src_hash_hex);
    assert_eq!(checksum::calculate_hash(&dst).unwrap(), src_hash_hex);
}
