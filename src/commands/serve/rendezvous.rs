use crate::core::protocol::{self, Message, AUTH_HELLO_TAG};
use anyhow::Result;
use std::path::{Path, PathBuf};

const RENDEZVOUS_ACCEPT_TIMEOUT_SECS: u64 = 30;

const AUTH_HELLO_READ_TIMEOUT_SECS: u64 = 2;

pub(super) const MAX_RENDEZVOUS_PER_SESSION: usize = 16;

fn rendezvous_accept_timeout() -> std::time::Duration {
    let secs = std::env::var("BCMR_RENDEZVOUS_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(RENDEZVOUS_ACCEPT_TIMEOUT_SECS);
    std::time::Duration::from_secs(secs)
}

pub(super) struct RendezvousTasks {
    handles: Vec<tokio::task::JoinHandle<()>>,
}

impl RendezvousTasks {
    pub(super) fn new() -> Self {
        Self {
            handles: Vec::new(),
        }
    }

    pub(super) fn push(&mut self, h: tokio::task::JoinHandle<()>) {
        self.handles.push(h);
    }

    pub(super) fn len(&self) -> usize {
        self.handles.len()
    }

    pub(super) async fn drain_gracefully(mut self) {
        for h in self.handles.drain(..) {
            let _ = h.await;
        }
    }
}

impl Drop for RendezvousTasks {
    fn drop(&mut self) {
        for h in &self.handles {
            h.abort();
        }
    }
}

fn rendezvous_bind_ip() -> std::net::IpAddr {
    use std::net::{IpAddr, Ipv4Addr};
    let parsed = std::env::var("SSH_CONNECTION").ok().and_then(|v| {
        let parts: Vec<&str> = v.split_whitespace().collect();
        if parts.len() == 4 {
            parts[2].parse::<IpAddr>().ok()
        } else {
            None
        }
    });
    parsed.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

pub(super) fn handle_open_direct_channel(
    root: PathBuf,
) -> Result<(Message, tokio::task::JoinHandle<()>)> {
    use ring::rand::{SecureRandom, SystemRandom};
    use zeroize::Zeroizing;

    let bind_addr = std::net::SocketAddr::new(rendezvous_bind_ip(), 0);
    let std_listener = std::net::TcpListener::bind(bind_addr)?;
    std_listener.set_nonblocking(true)?;
    let addr = std_listener.local_addr()?;
    let listener = tokio::net::TcpListener::from_std(std_listener)?;

    let rng = SystemRandom::new();
    let mut session_key: Zeroizing<[u8; 32]> = Zeroizing::new([0u8; 32]);
    rng.fill(session_key.as_mut())
        .map_err(|_| anyhow::anyhow!("ring::rand failed to produce session key"))?;
    let key_out = *session_key;

    let handle = tokio::spawn(run_rendezvous(listener, session_key, root));

    Ok((
        Message::DirectChannelReady {
            addr: addr.to_string(),
            session_key: key_out,
        },
        handle,
    ))
}

async fn run_rendezvous(
    listener: tokio::net::TcpListener,
    session_key: zeroize::Zeroizing<[u8; 32]>,
    root: PathBuf,
) {
    let deadline = tokio::time::Instant::now() + rendezvous_accept_timeout();
    let authed = loop {
        let remaining = match deadline.checked_duration_since(tokio::time::Instant::now()) {
            Some(d) if !d.is_zero() => d,
            _ => return,
        };
        let mut stream = match tokio::time::timeout(remaining, listener.accept()).await {
            Ok(Ok((stream, _peer))) => stream,
            Ok(Err(e)) => {
                eprintln!("serve: direct-tcp accept failed: {e}");
                return;
            }
            Err(_) => return,
        };
        if verify_auth_hello(&mut stream, &session_key).await {
            break stream;
        }
    };
    drop(listener);

    if let Err(e) = run_direct_session(authed, &session_key, &root).await {
        eprintln!("serve: direct-tcp session error: {e}");
    }
}

async fn verify_auth_hello(stream: &mut tokio::net::TcpStream, session_key: &[u8; 32]) -> bool {
    use ring::rand::{SecureRandom, SystemRandom};
    let read_budget = std::time::Duration::from_secs(AUTH_HELLO_READ_TIMEOUT_SECS);

    let rng = SystemRandom::new();
    let mut nonce = [0u8; 32];
    if rng.fill(&mut nonce).is_err() {
        return false;
    }

    let challenge = protocol::encode_message(&Message::AuthChallenge { nonce });
    if tokio::time::timeout(
        read_budget,
        tokio::io::AsyncWriteExt::write_all(stream, &challenge),
    )
    .await
    .is_err()
    {
        return false;
    }

    let mac = match tokio::time::timeout(read_budget, protocol::read_message(stream)).await {
        Ok(Ok(Some(Message::AuthHello { mac }))) => mac,
        _ => return false,
    };

    let expected = expected_auth_mac(session_key, &nonce);
    // blake3::Hash::PartialEq is constant-time.
    blake3::Hash::from(mac) == expected
}

pub(crate) fn expected_auth_mac(session_key: &[u8; 32], nonce: &[u8; 32]) -> blake3::Hash {
    let mut input = [0u8; AUTH_HELLO_TAG.len() + 32];
    input[..AUTH_HELLO_TAG.len()].copy_from_slice(AUTH_HELLO_TAG);
    input[AUTH_HELLO_TAG.len()..].copy_from_slice(nonce);
    blake3::keyed_hash(session_key, &input)
}

async fn run_direct_session(
    stream: tokio::net::TcpStream,
    session_key: &[u8; 32],
    root: &Path,
) -> Result<()> {
    let (mut reader, mut writer) = stream.into_split();

    Box::pin(super::session::run_session(
        &mut reader,
        &mut writer,
        root,
        false,
        false,
        Some(session_key),
    ))
    .await
}
