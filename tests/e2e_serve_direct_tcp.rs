#![cfg(unix)]
//! End-to-end tests for Path B: SSH rendezvous → direct-TCP data
//! plane with AES-256-GCM framing. Covers the reply well-formedness,
//! cap gating, the full PUT+GET round-trip, the squatter
//! DoS-resistance of the accept loop, and both sides' downgrade
//! guards around CAP_AEAD.

mod common;

use common::{bytes_to_hex, create_file};
use std::fs;
use std::sync::{Arc, Mutex};

use bcmr::core::checksum;
use bcmr::core::serve_client::ServeClient;

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
