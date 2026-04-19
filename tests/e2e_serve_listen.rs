#![cfg(unix)]

#[tokio::test]
async fn serve_listen_tcp_handshake_and_put() {
    use bcmr::core::protocol::{read_message, write_message, Message, PROTOCOL_VERSION};
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::TcpStream;
    use tokio::process::Command;

    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("tcp_roundtrip.bin");

    let exe = std::env::current_exe().unwrap();
    let bin_name = if cfg!(windows) { "bcmr.exe" } else { "bcmr" };
    let bin = exe.parent().unwrap().parent().unwrap().join(bin_name);
    let mut child = Command::new(&bin)
        .args(["serve", "--listen", "127.0.0.1:0", "--root", "/"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let mut stdout = BufReader::new(stdout);
    let mut line = String::new();
    stdout.read_line(&mut line).await.unwrap();
    assert!(
        line.starts_with("LISTENING "),
        "expected 'LISTENING <addr>' announcement, got {line:?}"
    );
    let addr: std::net::SocketAddr = line
        .trim_start_matches("LISTENING ")
        .trim()
        .parse()
        .expect("parseable SocketAddr in announce line");

    let sock = TcpStream::connect(addr).await.unwrap();
    let (mut reader, mut writer) = sock.into_split();

    write_message(
        &mut writer,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            caps: 0,
        },
    )
    .await
    .unwrap();
    match read_message(&mut reader).await.unwrap().unwrap() {
        Message::Welcome { version, .. } => {
            assert_eq!(version, PROTOCOL_VERSION);
        }
        other => panic!("expected Welcome, got {other:?}"),
    }

    let payload = b"hello";
    write_message(
        &mut writer,
        &Message::Put {
            path: dst.to_string_lossy().into_owned(),
            size: payload.len() as u64,
        },
    )
    .await
    .unwrap();
    write_message(
        &mut writer,
        &Message::Data {
            payload: payload.to_vec(),
        },
    )
    .await
    .unwrap();
    write_message(&mut writer, &Message::Done).await.unwrap();

    match read_message(&mut reader).await.unwrap().unwrap() {
        Message::Ok { hash } => {
            assert!(hash.is_some(), "PUT reply should carry a hash");
        }
        other => panic!("expected Ok, got {other:?}"),
    }

    let got = std::fs::read(&dst).unwrap();
    assert_eq!(got, payload);

    drop(writer);
    drop(reader);
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
async fn serve_listen_does_not_offer_direct_tcp_cap() {
    use bcmr::core::protocol::{
        read_message, write_message, Message, CAP_DIRECT_TCP, PROTOCOL_VERSION,
    };
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::TcpStream;

    let exe = std::env::current_exe().unwrap();
    let bin_name = if cfg!(windows) { "bcmr.exe" } else { "bcmr" };
    let bin = exe.parent().unwrap().parent().unwrap().join(bin_name);
    let mut child = tokio::process::Command::new(&bin)
        .args(["serve", "--listen", "127.0.0.1:0", "--root", "/"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut stdout = BufReader::new(stdout);
    let mut line = String::new();
    stdout.read_line(&mut line).await.unwrap();
    let addr: std::net::SocketAddr = line
        .trim_start_matches("LISTENING ")
        .trim()
        .parse()
        .unwrap();

    let sock = TcpStream::connect(addr).await.unwrap();
    let (mut reader, mut writer) = sock.into_split();
    write_message(
        &mut writer,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            caps: CAP_DIRECT_TCP,
        },
    )
    .await
    .unwrap();
    match read_message(&mut reader).await.unwrap().unwrap() {
        Message::Welcome { caps, .. } => {
            assert_eq!(
                caps & CAP_DIRECT_TCP,
                0,
                "TCP-transport server must not offer CAP_DIRECT_TCP, got caps={caps:#x}"
            );
        }
        other => panic!("expected Welcome, got {other:?}"),
    }

    drop(writer);
    drop(reader);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
