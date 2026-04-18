#![cfg(unix)]
//! End-to-end tests for Path B: SSH rendezvous → direct-TCP data
//! plane with AES-256-GCM framing. Covers the reply well-formedness,
//! cap gating, the full PUT+GET round-trip, the squatter
//! DoS-resistance of the accept loop, and both sides' downgrade
//! guards around CAP_AEAD.

mod common;

use common::{bytes_to_hex, create_file};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bcmr::core::checksum;
use bcmr::core::serve_client::{FileTransfer, ServeClient, ServeClientPool};

/// OpenDirectChannel → DirectChannelReady: reply addr is reachable, key
/// is random, two requests get two different keys.
#[tokio::test]
async fn serve_open_direct_channel_reply_is_well_formed() {
    use bcmr::core::protocol::{
        read_message, write_message, Message, CAP_DIRECT_TCP, PROTOCOL_VERSION,
    };
    use std::process::Stdio;
    use tokio::net::TcpStream;

    let exe = std::env::current_exe().unwrap();
    let bin_name = if cfg!(windows) { "bcmr.exe" } else { "bcmr" };
    let bin = exe.parent().unwrap().parent().unwrap().join(bin_name);
    // Shorten the rendezvous accept timeout for tests — the child
    // serve process would otherwise hold its random listener ports
    // for the full 30 s default after this test's explicit teardown.
    let mut child = tokio::process::Command::new(&bin)
        .args(["serve", "--root", "/"])
        .env("BCMR_RENDEZVOUS_TIMEOUT_SECS", "2")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();

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

    // Fresh rendezvous → fresh key.
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

/// Without CAP_DIRECT_TCP in the negotiated caps, the server must
/// refuse OpenDirectChannel with an Error. Forward compat for old
/// clients that don't know about Path B.
#[tokio::test]
async fn serve_open_direct_channel_requires_cap() {
    use bcmr::core::protocol::{read_message, write_message, Message, PROTOCOL_VERSION};
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

/// Full SSH → OpenDirectChannel → TCP rendezvous → AuthHello → PUT →
/// GET round-trip. Proves the direct-TCP transport carries a complete
/// data session end-to-end with AEAD framing active.
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

/// DoS-resistance: a squatter that connects first with a bogus
/// AuthHello must NOT consume the rendezvous listener. The real
/// client, dialing in second with the right MAC, still reaches an
/// open listener and completes the handshake.
#[tokio::test]
async fn serve_direct_tcp_squatter_does_not_starve_real_client() {
    use bcmr::core::protocol::{
        read_message, write_message, Message, CAP_AEAD, CAP_DIRECT_TCP, PROTOCOL_VERSION,
    };
    use std::process::Stdio;
    use tokio::net::TcpStream;

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
    let mut ssh_stdin = child.stdin.take().unwrap();
    let mut ssh_stdout = child.stdout.take().unwrap();

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

    // Squatter: connects first, sends an AuthHello with a bogus MAC.
    let squatter = TcpStream::connect(&addr).await.unwrap();
    let (_sr, mut sw) = squatter.into_split();
    write_message(
        &mut sw,
        &Message::AuthHello {
            mac: [0xAAu8; 32],
        },
    )
    .await
    .unwrap();
    drop(sw);

    // Real client: mirror the real bcmr client flow — close SSH stdin
    // once the rendezvous has handed off, then run the TCP handshake.
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

/// Downgrade-attack guard: a rendezvous-authenticated peer that does
/// NOT advertise CAP_AEAD must be rejected on the direct-TCP channel.
#[tokio::test]
async fn serve_direct_tcp_refuses_session_without_aead() {
    use bcmr::core::protocol::{
        read_message, write_message, Message, CAP_DIRECT_TCP, PROTOCOL_VERSION,
    };
    use std::process::Stdio;
    use tokio::net::TcpStream;

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
    let mut ssh_stdin = child.stdin.take().unwrap();
    let mut ssh_stdout = child.stdout.take().unwrap();

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
            caps: 0, // CAP_AEAD deliberately missing
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

/// CAP_AEAD must be masked off on the SSH transport: the server has no
/// session key to derive the AEAD key from, so advertising the cap
/// would be a lie that the data plane can't honor.
#[tokio::test]
async fn serve_ssh_transport_does_not_offer_cap_aead() {
    use bcmr::core::protocol::{read_message, write_message, CAP_AEAD, PROTOCOL_VERSION};
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

/// Pipelined PUT of many small files over direct-TCP. Exercises the
/// split SendHalf / RecvHalf framing: the writer task owns the send
/// counter + TCP write half, the reader task owns the recv counter +
/// TCP read half. Must produce byte-identical dst files with correct
/// hashes returned in input order.
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

/// Pipelined GET of many small files over direct-TCP. Mirror of the
/// PUT test above. Proves the AEAD recv counter advances correctly
/// across the demultiplexed Data*/Ok stream.
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
        .pipelined_get_files(
            files,
            false,
            |_idx, _path: &Path, _size| {},
            |_n| {},
        )
        .await
        .unwrap();
    client.close().await.unwrap();

    for (i, expected_hash) in expected.iter().enumerate() {
        let dst = dst_dir.join(format!("g_{i}.bin"));
        assert_eq!(&checksum::calculate_hash(&dst).unwrap(), expected_hash);
    }
}

/// ServeClientPool with N=4 direct-TCP connections: four independent
/// rendezvous + TCP + AEAD sessions running concurrently, files
/// round-robin'd across them. Proves the pool layer is transport-
/// agnostic — every bucket flips to its own AEAD state and the
/// striped scatter/gather still works.
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

/// Striping: pool with N=4 direct-TCP connections splits a single
/// 16 MiB file into 4 non-overlapping ranges, each connection pwrites
/// its range to the shared dst path. Proves the per-client pwrite
/// concurrency actually composes: total file must be byte-identical
/// to the source and correctly sized.
#[tokio::test]
async fn serve_direct_tcp_striped_put_single_large_file() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("big.bin");
    let dst = dir.path().join("big_dst.bin");
    create_file(&src, 16 * 1024 * 1024 + 731); // non-aligned for range-split edge case
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

/// Mirror: striped GET over direct-TCP for a single large file. Pool
/// with N=4, source file on the server side, each client pulls its
/// range and writes into the shared local dst. Must produce a
/// byte-identical copy.
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

/// I1 regression guard: a striped PUT to a dst that is already
/// larger than the new src must leave the dst at exactly the new
/// src's size — not silently keep stale tail bytes past the new
/// end. The Truncate preamble before the chunk fanout is what
/// makes this correct; without it the smaller new file's chunks
/// would only overwrite the prefix and the tail would survive.
#[tokio::test]
async fn serve_direct_tcp_striped_put_truncates_existing_dst() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("small.bin");
    let dst = dir.path().join("dst.bin");
    // Prior run: a larger dst with recognisable sentinel bytes.
    std::fs::write(&dst, vec![0xEEu8; 20 * 1024 * 1024]).unwrap();
    // New run: overwrite with a smaller src.
    create_file(&src, 2 * 1024 * 1024 + 17); // above dedup threshold? no — we go through striped anyway
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

/// M4 regression guard: striping a zero-byte file must still
/// produce an empty file on the server. The fanout skips empty
/// ranges, so without the Truncate preamble the dst would simply
/// not exist after the call.
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

    assert!(dst.exists(), "striped PUT of an empty file must still create the dst");
    assert_eq!(std::fs::metadata(&dst).unwrap().len(), 0);
    // BLAKE3 of empty input is a known constant.
    assert_eq!(
        bytes_to_hex(&returned_hash),
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    );
}

/// Edge case: striping a file smaller than pool-size * 1 byte. Some
/// buckets end up with a zero-byte share. Those buckets must be
/// skipped rather than sending a PutChunked with length=0 (which
/// would create a degenerate no-op frame).
#[tokio::test]
async fn serve_direct_tcp_striped_put_tiny_file_skips_empty_buckets() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("tiny.bin");
    let dst = dir.path().join("tiny_dst.bin");
    create_file(&src, 7); // 7 bytes, pool N=4 → chunk sizes 2, 2, 2, 1
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

