use crate::core::protocol::{self, ListEntry, Message, PROTOCOL_VERSION};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::{self, AsyncWriteExt};

const CHUNK_SIZE: usize = 4 * 1024 * 1024; // 4 MiB

/// Validate and canonicalize a path from the client.
/// Prevents directory traversal attacks (e.g. "../../../etc/shadow").
/// All paths must be absolute after canonicalization.
fn validate_path(raw: &str) -> Result<PathBuf> {
    let path = Path::new(raw);

    // Reject obviously malicious patterns before touching the filesystem
    if raw.contains('\0') {
        bail!("path contains null byte");
    }

    // For existing paths, canonicalize resolves symlinks and ..
    if path.exists() {
        return Ok(std::fs::canonicalize(path)?);
    }

    // Reject any path containing ".." components — even if parent doesn't exist yet
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            bail!("path contains '..'");
        }
    }

    // For new files (put/mkdir), canonicalize the parent if it exists
    if let Some(parent) = path.parent() {
        if parent.exists() {
            let canonical_parent = std::fs::canonicalize(parent)?;
            if let Some(name) = path.file_name() {
                return Ok(canonical_parent.join(name));
            }
        }
    }

    Ok(path.to_path_buf())
}

pub async fn run() -> Result<()> {
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();

    // --- Handshake ---
    match protocol::read_message(&mut stdin).await? {
        Some(Message::Hello { version }) => {
            if version != PROTOCOL_VERSION {
                protocol::write_message(
                    &mut stdout,
                    &Message::Error {
                        message: format!(
                            "protocol version mismatch: client={version} server={PROTOCOL_VERSION}"
                        ),
                    },
                )
                .await?;
                stdout.flush().await?;
                return Ok(());
            }
        }
        Some(other) => {
            protocol::write_message(
                &mut stdout,
                &Message::Error {
                    message: format!("expected Hello, got {other:?}"),
                },
            )
            .await?;
            stdout.flush().await?;
            return Ok(());
        }
        None => return Ok(()), // clean EOF before handshake
    }

    protocol::write_message(
        &mut stdout,
        &Message::Welcome {
            version: PROTOCOL_VERSION,
        },
    )
    .await?;
    stdout.flush().await?;

    // --- Dispatch loop ---
    loop {
        let msg = match protocol::read_message(&mut stdin).await? {
            Some(m) => m,
            None => break, // clean EOF
        };

        // Get writes Data+Ok directly to stdout (streaming), so it bypasses
        // the normal dispatch-loop write. All other handlers return a message
        // for the dispatch loop to write.
        let response = match msg {
            Message::Get { path, offset } => {
                match validate_path(&path) {
                    Ok(p) => {
                        if let Err(e) =
                            handle_get(p.to_str().unwrap_or(&path), offset, &mut stdout).await
                        {
                            eprintln!("serve: handler error: {e}");
                            protocol::write_message(
                                &mut stdout,
                                &Message::Error {
                                    message: e.to_string(),
                                },
                            )
                            .await?;
                        }
                    }
                    Err(e) => {
                        protocol::write_message(
                            &mut stdout,
                            &Message::Error {
                                message: e.to_string(),
                            },
                        )
                        .await?;
                    }
                }
                stdout.flush().await?;
                continue;
            }
            Message::Stat { path } => match validate_path(&path) {
                Ok(p) => handle_stat(p.to_str().unwrap_or(&path)).await,
                Err(e) => Err(e),
            },
            Message::List { path } => match validate_path(&path) {
                Ok(p) => handle_list(p.to_str().unwrap_or(&path)).await,
                Err(e) => Err(e),
            },
            Message::Hash {
                path,
                offset,
                limit,
            } => match validate_path(&path) {
                Ok(p) => handle_hash(p.to_str().unwrap_or(&path), offset, limit).await,
                Err(e) => Err(e),
            },
            Message::Put { path, size } => match validate_path(&path) {
                Ok(p) => handle_put(p.to_str().unwrap_or(&path), size, &mut stdin).await,
                Err(e) => Err(e),
            },
            Message::Mkdir { path } => match validate_path(&path) {
                Ok(p) => handle_mkdir(p.to_str().unwrap_or(&path)).await,
                Err(e) => Err(e),
            },
            Message::Resume { path } => match validate_path(&path) {
                Ok(p) => handle_resume(p.to_str().unwrap_or(&path)).await,
                Err(e) => Err(e),
            },
            other => Err(anyhow::anyhow!("unexpected message: {other:?}")),
        };

        let reply = match response {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("serve: handler error: {e}");
                Message::Error {
                    message: e.to_string(),
                }
            }
        };

        protocol::write_message(&mut stdout, &reply).await?;
        stdout.flush().await?;
    }

    Ok(())
}

async fn handle_stat(path: &str) -> Result<Message> {
    let meta = fs::metadata(path).await?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs() as i64)
        })
        .unwrap_or(0);
    Ok(Message::StatResponse {
        size: meta.len(),
        mtime,
        is_dir: meta.is_dir(),
    })
}

async fn handle_list(path: &str) -> Result<Message> {
    let base = Path::new(path).to_path_buf();
    let entries = tokio::task::spawn_blocking(move || -> Result<Vec<ListEntry>> {
        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(&base).min_depth(1) {
            let entry = entry?;
            let rel = entry
                .path()
                .strip_prefix(&base)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .into_owned();
            let meta = entry.metadata()?;
            out.push(ListEntry {
                path: rel,
                size: meta.len(),
                is_dir: meta.is_dir(),
            });
        }
        Ok(out)
    })
    .await??;

    Ok(Message::ListResponse { entries })
}

async fn handle_hash(path: &str, offset: u64, limit: Option<u64>) -> Result<Message> {
    let path = path.to_owned();
    let hash = tokio::task::spawn_blocking(move || -> Result<String> {
        use std::io::Read;
        let mut file = std::fs::File::open(&path)?;
        if offset > 0 {
            use std::io::Seek;
            file.seek(std::io::SeekFrom::Start(offset))?;
        }
        let mut hasher = blake3::Hasher::new();
        let mut remaining = limit;
        let mut buf = vec![0u8; CHUNK_SIZE];
        loop {
            let to_read = match remaining {
                Some(0) => break,
                Some(r) => r.min(buf.len() as u64) as usize,
                None => buf.len(),
            };
            let n = file.read(&mut buf[..to_read])?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            if let Some(r) = remaining.as_mut() {
                *r -= n as u64;
            }
        }
        Ok(hasher.finalize().to_hex().to_string())
    })
    .await??;

    Ok(Message::HashResponse { hash })
}

/// Stream file data in 4 MiB chunks. Sends Data messages then Ok { hash }.
/// Returns Ok(Message::Ok{..}) but writes Data messages directly to `out`.
/// Streams file data as Data messages, then writes Ok with hash directly.
/// Returns Result<()> because it writes responses directly to the output —
/// the dispatch loop must NOT write another response for Get commands.
async fn handle_get<W>(path: &str, offset: u64, out: &mut W) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut file = fs::File::open(path).await?;
    if offset > 0 {
        use tokio::io::AsyncSeekExt;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
    }

    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        protocol::write_message(
            out,
            &Message::Data {
                payload: buf[..n].to_vec(),
            },
        )
        .await?;
    }

    let hash = hasher.finalize().to_hex().to_string();
    protocol::write_message(out, &Message::Ok { hash: Some(hash) }).await?;
    Ok(())
}

/// Receive Data messages until Done, write to file, fsync, compute hash.
async fn handle_put<R>(path: &str, _size: u64, reader: &mut R) -> Result<Message>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let parent = Path::new(path).parent();
    if let Some(p) = parent {
        if !p.as_os_str().is_empty() {
            fs::create_dir_all(p).await?;
        }
    }

    let mut file = fs::File::create(path).await?;
    let mut hasher = blake3::Hasher::new();

    loop {
        match protocol::read_message(reader).await? {
            Some(Message::Data { payload }) => {
                hasher.update(&payload);
                file.write_all(&payload).await?;
            }
            Some(Message::Done) => break,
            Some(other) => {
                return Err(anyhow::anyhow!("put: unexpected message {other:?}"));
            }
            None => break, // EOF treated as implicit Done
        }
    }

    file.flush().await?;
    file.sync_all().await?;
    drop(file);

    let hash = hasher.finalize().to_hex().to_string();
    Ok(Message::Ok { hash: Some(hash) })
}

async fn handle_mkdir(path: &str) -> Result<Message> {
    fs::create_dir_all(path).await?;
    Ok(Message::Ok { hash: None })
}

async fn handle_resume(path: &str) -> Result<Message> {
    let meta = match fs::metadata(path).await {
        Ok(m) => m,
        Err(_) => {
            return Ok(Message::ResumeResponse {
                size: 0,
                block_hash: None,
            });
        }
    };

    let size = meta.len();
    if size < CHUNK_SIZE as u64 {
        return Ok(Message::ResumeResponse {
            size,
            block_hash: None,
        });
    }

    // Hash the last complete 4 MiB block.
    let block_start = (size / CHUNK_SIZE as u64 - 1) * CHUNK_SIZE as u64;
    let path = path.to_owned();
    let block_hash = tokio::task::spawn_blocking(move || -> Result<String> {
        use std::io::{Read, Seek};
        let mut file = std::fs::File::open(&path)?;
        file.seek(std::io::SeekFrom::Start(block_start))?;
        let mut buf = vec![0u8; CHUNK_SIZE];
        let mut hasher = blake3::Hasher::new();
        let n = file.read(&mut buf)?;
        hasher.update(&buf[..n]);
        Ok(hasher.finalize().to_hex().to_string())
    })
    .await??;

    Ok(Message::ResumeResponse {
        size,
        block_hash: Some(block_hash),
    })
}
