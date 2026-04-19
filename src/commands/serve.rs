use crate::core::protocol::{
    CAP_AEAD, CAP_DEDUP, CAP_DIRECT_TCP, CAP_FAST, CAP_LZ4, CAP_SYNC, CAP_ZSTD,
};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use tokio::io;

mod handlers;
mod rendezvous;
mod session;

pub(super) const SERVER_CAPS: u8 =
    CAP_LZ4 | CAP_ZSTD | CAP_DEDUP | CAP_FAST | CAP_SYNC | CAP_DIRECT_TCP | CAP_AEAD;

fn resolve_root(arg: Option<PathBuf>) -> Result<PathBuf> {
    let raw = match arg {
        Some(p) => p,
        None => directories::UserDirs::new()
            .map(|u| u.home_dir().to_path_buf())
            .ok_or_else(|| anyhow::anyhow!("no $HOME to use as default --root"))?,
    };
    std::fs::create_dir_all(&raw)?;
    Ok(std::fs::canonicalize(&raw)?)
}

pub(super) fn validate_path(raw: &str, root: &Path) -> Result<PathBuf> {
    if raw.contains('\0') {
        bail!("path contains null byte");
    }
    let path = Path::new(raw);

    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            bail!("path contains '..'");
        }
    }

    let canonical = canonicalize_with_ancestor(path)?;
    if !canonical.starts_with(root) {
        bail!(
            "path {} escapes server root {}",
            canonical.display(),
            root.display()
        );
    }
    Ok(canonical)
}

fn canonicalize_with_ancestor(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(std::fs::canonicalize(path)?);
    }
    let mut ancestor = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !ancestor.exists() {
        match ancestor.file_name() {
            Some(n) => tail.push(n.to_os_string()),
            None => break,
        }
        if !ancestor.pop() {
            break;
        }
    }
    let mut out = if ancestor.as_os_str().is_empty() {
        std::env::current_dir()?
    } else if ancestor.exists() {
        std::fs::canonicalize(&ancestor)?
    } else {
        return Err(anyhow::anyhow!(
            "no existing ancestor for {}",
            path.display()
        ));
    };
    for seg in tail.iter().rev() {
        out.push(seg);
    }
    Ok(out)
}

pub async fn run(root: Option<PathBuf>) -> Result<()> {
    let root = resolve_root(root)?;
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();
    session::run_session(&mut stdin, &mut stdout, &root, true, true, None).await
}

pub async fn run_listen(root: Option<PathBuf>, addr: std::net::SocketAddr) -> Result<()> {
    if !addr.ip().is_loopback() {
        if std::env::var("BCMR_UNSAFE_LAN_LISTEN").is_ok_and(|v| v == "1") {
            eprintln!(
                "bcmr serve: WARNING — binding {addr} on a non-loopback address with no peer \
                 authentication. Anyone who can reach this port can read and write files under \
                 the --root jail."
            );
        } else {
            bail!(
                "bcmr serve --listen refuses non-loopback address {addr} without \
                 BCMR_UNSAFE_LAN_LISTEN=1 (no peer auth on this transport yet)"
            );
        }
    }
    let root = resolve_root(root)?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    use tokio::io::AsyncWriteExt as _;
    let mut stdout = io::stdout();
    stdout
        .write_all(format!("LISTENING {bound}\n").as_bytes())
        .await?;
    stdout.flush().await?;

    loop {
        let (stream, _peer) = listener.accept().await?;
        let root = root.clone();
        tokio::spawn(async move {
            let (mut reader, mut writer) = stream.into_split();
            let _ = session::run_session(&mut reader, &mut writer, &root, false, false, None).await;
        });
    }
}
