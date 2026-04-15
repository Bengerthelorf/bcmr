use crate::core::cas;
use crate::core::compress;
use crate::core::protocol::{
    self, CompressionAlgo, ListEntry, Message, CAP_DEDUP, CAP_FAST, CAP_LZ4, CAP_ZSTD,
    PROTOCOL_VERSION,
};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::{self, AsyncWriteExt};

const CHUNK_SIZE: usize = 4 * 1024 * 1024; // 4 MiB

/// Server advertises every cap it knows; the client picks by intersecting
/// with its own. CAP_FAST is always offered — the actual implementation
/// either skips inline hashing (any platform) or also uses splice(2) on
/// Linux. Either way the client opts in via --fast.
const SERVER_CAPS: u8 = CAP_LZ4 | CAP_ZSTD | CAP_DEDUP | CAP_FAST;

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
    let effective_caps = match protocol::read_message(&mut stdin).await? {
        Some(Message::Hello { version, caps }) => {
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
            SERVER_CAPS & caps
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
    };

    // Server and client both call negotiate(caps, caps) with this shared
    // intersection so they land on the same algorithm without a second
    // round trip.
    let algo = CompressionAlgo::negotiate(effective_caps, effective_caps);
    let fast = (effective_caps & CAP_FAST) != 0;

    protocol::write_message(
        &mut stdout,
        &Message::Welcome {
            version: PROTOCOL_VERSION,
            caps: effective_caps,
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
                            handle_get(p.to_str().unwrap_or(&path), offset, algo, fast, &mut stdout)
                                .await
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
                Ok(p) => {
                    handle_put(p.to_str().unwrap_or(&path), size, &mut stdout, &mut stdin).await
                }
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
async fn handle_get<W>(
    path: &str,
    offset: u64,
    algo: CompressionAlgo,
    fast: bool,
    out: &mut W,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    // The Linux splice fast path skips compression too — by definition
    // we don't have userspace bytes to feed the encoder. fall back to
    // the buffered path whenever compression is active or we're not on
    // Linux. CAP_FAST without splice still wins from skipping the hash.
    #[cfg(target_os = "linux")]
    {
        if fast && algo == CompressionAlgo::None {
            return handle_get_splice_linux(path, offset, out).await;
        }
    }

    use tokio::io::AsyncReadExt;

    let mut file = fs::File::open(path).await?;
    if offset > 0 {
        use tokio::io::AsyncSeekExt;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
    }

    let mut hasher = (!fast).then(blake3::Hasher::new);
    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        if let Some(h) = hasher.as_mut() {
            h.update(&buf[..n]);
        }
        let frame = compress::encode_block(algo, buf[..n].to_vec());
        protocol::write_message(out, &frame).await?;
    }

    let hash = hasher.map(|h| h.finalize().to_hex().to_string());
    protocol::write_message(out, &Message::Ok { hash }).await?;
    Ok(())
}

/// Linux zero-copy GET: file → pipe → stdout via splice(2). Skips the
/// userspace memcpy that the buffered path needs to fill the Data
/// frame's Vec<u8>. Each Data frame's 9-byte header (4B length + 1B
/// type + 4B payload-len) is still written through the normal tokio
/// path; only the payload bytes go through splice.
#[cfg(target_os = "linux")]
async fn handle_get_splice_linux<W>(path: &str, offset: u64, out: &mut W) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
    use tokio::io::AsyncWriteExt;

    let path = path.to_owned();
    let mut std_file = tokio::task::spawn_blocking(move || std::fs::File::open(&path)).await??;

    if offset > 0 {
        use std::io::Seek;
        std_file.seek(std::io::SeekFrom::Start(offset))?;
    }
    let total_size = std_file.metadata()?.len() - offset;

    // Create a pipe and grow it to the chunk size so each splice round
    // moves up to 4 MiB without bouncing through the default 64 KiB
    // pipe buffer. F_SETPIPE_SZ is best-effort; Linux silently caps at
    // /proc/sys/fs/pipe-max-size for non-root.
    let mut fds = [0i32; 2];
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let pipe_r = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let pipe_w = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    unsafe {
        libc::fcntl(
            pipe_w.as_raw_fd(),
            libc::F_SETPIPE_SZ,
            CHUNK_SIZE as libc::c_int,
        );
    }

    let mut remaining = total_size;
    let stdout_fd = libc::STDOUT_FILENO;
    let file_fd = std_file.as_raw_fd();
    let pipe_w_fd = pipe_w.as_raw_fd();
    let pipe_r_fd = pipe_r.as_raw_fd();

    while remaining > 0 {
        let chunk = remaining.min(CHUNK_SIZE as u64) as usize;

        // Frame header: [4B payload_len_total][1B TYPE_DATA][4B payload_len].
        // payload_len_total = 1 + 4 + chunk = 5 + chunk.
        let mut header = Vec::with_capacity(9);
        header.extend_from_slice(&((5 + chunk) as u32).to_le_bytes());
        header.push(0x84); // TYPE_DATA
        header.extend_from_slice(&(chunk as u32).to_le_bytes());
        out.write_all(&header).await?;
        out.flush().await?;

        // Now splice `chunk` bytes through the pipe. Each splice may
        // move fewer bytes than requested, so we loop until the chunk
        // is drained. spawn_blocking keeps the syscall off the tokio
        // reactor threads.
        let to_move = chunk;
        let r = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let mut moved = 0usize;
            while moved < to_move {
                let want = to_move - moved;
                let n = unsafe {
                    libc::splice(
                        file_fd,
                        std::ptr::null_mut(),
                        pipe_w_fd,
                        std::ptr::null_mut(),
                        want,
                        libc::SPLICE_F_MOVE | libc::SPLICE_F_MORE,
                    )
                };
                if n < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if n == 0 {
                    break;
                }
                let mut drained = 0usize;
                while drained < n as usize {
                    let n2 = unsafe {
                        libc::splice(
                            pipe_r_fd,
                            std::ptr::null_mut(),
                            stdout_fd,
                            std::ptr::null_mut(),
                            n as usize - drained,
                            libc::SPLICE_F_MOVE | libc::SPLICE_F_MORE,
                        )
                    };
                    if n2 < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if n2 == 0 {
                        break;
                    }
                    drained += n2 as usize;
                }
                moved += drained;
            }
            Ok(())
        })
        .await?;
        r?;
        remaining -= chunk as u64;
    }

    drop(pipe_r);
    drop(pipe_w);
    drop(std_file);

    protocol::write_message(out, &Message::Ok { hash: None }).await?;
    Ok(())
}

/// Receive Data messages until Done, write to file, fsync, compute hash.
///
/// When the client opens a put with HaveBlocks (CAP_DEDUP negotiated), we
/// short-circuit by serving any blocks already in the local CAS without
/// the wire transfer. New blocks are written to the file *and* deposited
/// in the CAS for future requests. The composite hash returned in Ok
/// still covers the entire file regardless of whether each byte arrived
/// over the wire or came out of the cache.
async fn handle_put<W, R>(path: &str, _size: u64, out: &mut W, reader: &mut R) -> Result<Message>
where
    W: tokio::io::AsyncWrite + Unpin,
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

    // Peek at the first message: the client might open with HaveBlocks
    // (dedup mode) or jump straight to Data (legacy / dedup-disabled).
    let first = protocol::read_message(reader).await?;
    let mut dedup_state: Option<DedupState> = None;
    let mut next: Option<Message> = first;

    if let Some(Message::HaveBlocks { hashes, .. }) = next {
        // Trim CAS before processing so this PUT doesn't push the
        // store further past the cap. Failure here is non-fatal —
        // worst case the disk fills up later.
        if let Some(cap) = cas::cap_bytes() {
            let _ = tokio::task::spawn_blocking(move || cas::evict_to_cap(cap)).await;
        }

        let mut bits = vec![0u8; hashes.len().div_ceil(8)];
        for (i, h) in hashes.iter().enumerate() {
            if !cas::has(h) {
                bits[i / 8] |= 1 << (i % 8);
            }
        }
        protocol::write_message(out, &Message::MissingBlocks { bits: bits.clone() }).await?;
        out.flush().await?;
        dedup_state = Some(DedupState {
            hashes,
            bits,
            cursor: 0,
        });
        next = None;
    }

    let mut msg = next;
    loop {
        let m = match msg.take() {
            Some(m) => m,
            None => match protocol::read_message(reader).await? {
                Some(m) => m,
                None => break,
            },
        };
        match m {
            Message::Data { payload } => {
                consume_block(&payload, &mut file, &mut hasher, dedup_state.as_mut()).await?;
            }
            Message::DataCompressed {
                algo,
                original_size,
                payload,
            } => {
                let decoded = compress::decode_block(algo, original_size, &payload)?;
                consume_block(&decoded, &mut file, &mut hasher, dedup_state.as_mut()).await?;
            }
            Message::Done => break,
            other => return Err(anyhow::anyhow!("put: unexpected message {other:?}")),
        }
    }

    // Serve any trailing blocks that weren't sent because the CAS already
    // had them. We do this *after* draining the wire so the server's
    // stream of writes follows source order.
    if let Some(state) = dedup_state.as_mut() {
        flush_remaining_cas_blocks(state, &mut file, &mut hasher).await?;
    }

    file.flush().await?;
    file.sync_all().await?;
    drop(file);

    let hash = hasher.finalize().to_hex().to_string();
    Ok(Message::Ok { hash: Some(hash) })
}

struct DedupState {
    hashes: Vec<[u8; 32]>,
    bits: Vec<u8>,
    cursor: usize,
}

impl DedupState {
    fn is_missing(&self, idx: usize) -> bool {
        (self.bits.get(idx / 8).copied().unwrap_or(0) >> (idx % 8)) & 1 == 1
    }
}

async fn consume_block(
    block: &[u8],
    file: &mut tokio::fs::File,
    hasher: &mut blake3::Hasher,
    dedup: Option<&mut DedupState>,
) -> Result<()> {
    if let Some(state) = dedup {
        // Walk the cursor over any cached blocks ahead of the wire bytes.
        while state.cursor < state.hashes.len() && !state.is_missing(state.cursor) {
            let cached = cas::read(&state.hashes[state.cursor])?;
            hasher.update(&cached);
            file.write_all(&cached).await?;
            state.cursor += 1;
        }
        // The just-arrived bytes correspond to the next missing index.
        if state.cursor < state.hashes.len() && state.is_missing(state.cursor) {
            // Deposit into CAS for future runs.
            let mut h = [0u8; 32];
            h.copy_from_slice(blake3::hash(block).as_bytes());
            // Best-effort write; serving the file matters more than caching.
            let _ = cas::write(&h, block);
            state.cursor += 1;
        }
    }
    hasher.update(block);
    file.write_all(block).await?;
    Ok(())
}

async fn flush_remaining_cas_blocks(
    state: &mut DedupState,
    file: &mut tokio::fs::File,
    hasher: &mut blake3::Hasher,
) -> Result<()> {
    while state.cursor < state.hashes.len() {
        if state.is_missing(state.cursor) {
            // Should have been delivered over the wire already.
            return Err(anyhow::anyhow!(
                "client said block {} was missing but never sent it",
                state.cursor
            ));
        }
        let cached = cas::read(&state.hashes[state.cursor])?;
        hasher.update(&cached);
        file.write_all(&cached).await?;
        state.cursor += 1;
    }
    Ok(())
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
