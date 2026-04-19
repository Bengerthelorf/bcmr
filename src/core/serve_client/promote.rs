use crate::core::error::BcmrError;
use crate::core::framing;
use crate::core::protocol::{
    self, CompressionAlgo, Message, AUTH_HELLO_TAG, CAP_AEAD, CAP_DIRECT_TCP, PROTOCOL_VERSION,
};
use crate::core::transport::ssh as ssh_transport;

use super::{ServeClient, Transport};

fn auth_hello_mac(session_key: &[u8; 32], nonce: &[u8; 32]) -> blake3::Hash {
    let mut input = [0u8; AUTH_HELLO_TAG.len() + 32];
    input[..AUTH_HELLO_TAG.len()].copy_from_slice(AUTH_HELLO_TAG);
    input[AUTH_HELLO_TAG.len()..].copy_from_slice(nonce);
    blake3::keyed_hash(session_key, &input)
}

async fn ssh_target_uses_proxyjump(target: &str) -> bool {
    let target = target.to_owned();
    tokio::task::spawn_blocking(move || {
        let Ok(out) = std::process::Command::new("ssh")
            .args(["-G", &target])
            .output()
        else {
            return false;
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        stdout.lines().any(|line| {
            let mut it = line.split_whitespace();
            matches!(
                (it.next(), it.next()),
                (Some("proxyjump"), Some(v)) if v != "none"
            )
        })
    })
    .await
    .unwrap_or(false)
}

impl ServeClient {
    pub async fn connect_direct_with_caps(ssh_target: &str, caps: u8) -> Result<Self, BcmrError> {
        let spawn = ssh_transport::spawn_remote(ssh_target).await?;
        Self::promote_to_direct_tcp(spawn, caps, Some(ssh_target)).await
    }

    pub(super) async fn promote_to_direct_tcp(
        mut spawn: ssh_transport::SshSpawn,
        caps: u8,
        ssh_target: Option<&str>,
    ) -> Result<Self, BcmrError> {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        protocol::write_message(
            &mut spawn.stdin,
            &Message::Hello {
                version: PROTOCOL_VERSION,
                caps: caps | CAP_DIRECT_TCP,
            },
        )
        .await?;
        spawn.stdin.flush().await?;

        let control_caps = match protocol::read_message(&mut spawn.stdout).await? {
            Some(Message::Welcome {
                caps: server_caps, ..
            }) => server_caps,
            Some(Message::Error { message }) => return Err(BcmrError::InvalidInput(message)),
            Some(other) => {
                return Err(BcmrError::InvalidInput(format!(
                    "unexpected handshake response: {other:?}"
                )))
            }
            None => {
                return Err(BcmrError::InvalidInput(
                    "server closed connection during handshake".into(),
                ))
            }
        };
        if (control_caps & CAP_DIRECT_TCP) == 0 {
            return Err(BcmrError::InvalidInput(
                "server did not negotiate CAP_DIRECT_TCP; cannot use direct-TCP transport".into(),
            ));
        }

        protocol::write_message(&mut spawn.stdin, &Message::OpenDirectChannel).await?;
        spawn.stdin.flush().await?;
        let (addr, session_key) = match protocol::read_message(&mut spawn.stdout).await? {
            Some(Message::DirectChannelReady { addr, session_key }) => {
                (addr, zeroize::Zeroizing::new(session_key))
            }
            Some(Message::Error { message }) => return Err(BcmrError::InvalidInput(message)),
            Some(other) => {
                return Err(BcmrError::InvalidInput(format!(
                    "expected DirectChannelReady, got {other:?}"
                )))
            }
            None => {
                return Err(BcmrError::InvalidInput(
                    "server closed connection before DirectChannelReady".into(),
                ))
            }
        };

        let stream = match tokio::net::TcpStream::connect(&addr).await {
            Ok(s) => s,
            Err(e) => {
                let mut msg = format!(
                    "direct-TCP dial to {addr} failed: {e}. The server bound its \
                     rendezvous listener on the interface where SSH arrived, but \
                     this client can't reach that address."
                );
                if let Some(target) = ssh_target {
                    if ssh_target_uses_proxyjump(target).await {
                        msg.push_str(
                            " This SSH target uses ProxyJump — when the jump host is on \
                             a different subnet from the client, direct-TCP rendezvous is \
                             not reachable. Use --direct=ssh for this target.",
                        );
                    }
                }
                return Err(BcmrError::InvalidInput(msg));
            }
        };
        let (tcp_reader, tcp_writer) = stream.into_split();

        drop(spawn.stdin);

        let mut stdout = spawn.stdout;
        let stdout_drain = tokio::spawn(async move {
            let mut buf = [0u8; 512];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });

        let (tx, rx) = framing::plain_halves();
        let mut client = Self {
            transport: Transport {
                child: spawn.child,
                _drain: Some(stdout_drain),
            },
            reader: Box::new(tcp_reader),
            writer: Some(Box::new(tcp_writer)),
            tx: Some(tx),
            rx,
            algo: CompressionAlgo::None,
            dedup_enabled: false,
            poisoned: false,
        };

        let nonce = match client.recv().await? {
            Message::AuthChallenge { nonce } => nonce,
            other => {
                return Err(BcmrError::InvalidInput(format!(
                    "expected AuthChallenge, got {:?}",
                    other
                )));
            }
        };
        let mac = *auth_hello_mac(&session_key, &nonce).as_bytes();
        client.send(&Message::AuthHello { mac }).await?;
        client
            .handshake_with_key(caps | CAP_AEAD, Some(&session_key))
            .await?;
        Ok(client)
    }
}
