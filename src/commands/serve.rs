use crate::core::cas;
use crate::core::compress;
use crate::core::protocol::{
    self, CompressionAlgo, ListEntry, Message, CAP_DEDUP, CAP_FAST, CAP_LZ4, CAP_SYNC, CAP_ZSTD,
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
const SERVER_CAPS: u8 = CAP_LZ4 | CAP_ZSTD | CAP_DEDUP | CAP_FAST | CAP_SYNC;

/// Resolve the configured root jail. Explicit `--root <path>` wins;
/// otherwise the invoking user's `$HOME` is used. A user that really
/// wants pre-v0.5.10 behaviour (no sandbox) can pass `--root /`.
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

/// Validate a client path against the root jail.
///
/// Rules:
/// - null byte → reject
/// - reject any path component literally `..` (belt-and-suspenders; the
///   canonicalize below would catch symlink tricks too)
/// - canonicalize (for existing paths) or canonicalize the deepest
///   existing ancestor and splice the remainder
/// - require the result to be under `root` (lexical prefix after canon.)
///
/// With `root = /` the prefix check passes everything — that's the
/// explicit opt-out. Default is $HOME so arbitrary `/etc/evil` is
/// rejected even on a root-invoked serve.
fn validate_path(raw: &str, root: &Path) -> Result<PathBuf> {
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

/// Canonicalize `path` if it exists; otherwise canonicalize the closest
/// existing ancestor and re-append the remainder. Only follows symlinks
/// on the existing prefix, so a symlink pointing outside `root` is
/// caught by the caller's `starts_with` check.
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
    let sync = (effective_caps & CAP_SYNC) != 0;

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
                match validate_path(&path, &root) {
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
            Message::Stat { path } => match validate_path(&path, &root) {
                Ok(p) => handle_stat(p.to_str().unwrap_or(&path)).await,
                Err(e) => Err(e),
            },
            Message::List { path } => match validate_path(&path, &root) {
                Ok(p) => handle_list(p.to_str().unwrap_or(&path)).await,
                Err(e) => Err(e),
            },
            Message::Hash {
                path,
                offset,
                limit,
            } => match validate_path(&path, &root) {
                Ok(p) => handle_hash(p.to_str().unwrap_or(&path), offset, limit).await,
                Err(e) => Err(e),
            },
            Message::Put { path, size } => match validate_path(&path, &root) {
                Ok(p) => {
                    handle_put(
                        p.to_str().unwrap_or(&path),
                        size,
                        sync,
                        &mut stdout,
                        &mut stdin,
                    )
                    .await
                }
                Err(e) => Err(e),
            },
            Message::Mkdir { path } => match validate_path(&path, &root) {
                Ok(p) => handle_mkdir(p.to_str().unwrap_or(&path)).await,
                Err(e) => Err(e),
            },
            Message::Resume { path } => match validate_path(&path, &root) {
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

/// Linux zero-copy GET: file → pipe → stdout via splice(2).
///
/// v0.5.10 reshaped this path after Experiment 14 caught the original
/// implementation being *slower* than the buffered path. Two fixes:
///
/// 1. **One spawn_blocking for the whole file**. The previous version
///    dispatched a fresh blocking task per 4 MiB chunk — 256 thread
///    bounces per GiB, the same anti-pattern Exp 13 caught in the
///    local copy path. Now the loop owns the file fd, both pipe fds,
///    and the stdout fd, and never crosses back into the async reactor
///    until the Ok frame.
/// 2. **Frame headers via raw write(2)**. The headers used to go
///    through the tokio async stdout; that forced the splice loop to
///    re-acquire async context per chunk. Writing the 9 bytes of
///    frame header directly to the stdout fd inside the blocking
///    closure keeps everything on one thread.
///
/// `F_SETPIPE_SZ(CHUNK_SIZE)` is still attempted and still
/// best-effort: on Ubuntu default `/proc/sys/fs/pipe-max-size` is
/// 1 MiB, so the pipe stays at 1 MiB and each 4 MiB chunk takes 4
/// splice rounds instead of 1. That's fine — the syscall cost is
/// negligible; the regression was the thread bounces, not the round
/// count.
#[cfg(target_os = "linux")]
async fn handle_get_splice_linux<W>(path: &str, offset: u64, out: &mut W) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use std::io::Seek;
    use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
    use tokio::io::AsyncWriteExt;

    // Flush anything the async stdout has buffered so the raw write(2)
    // calls inside the blocking loop don't race with it.
    out.flush().await?;

    let path = path.to_owned();
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut std_file = std::fs::File::open(&path)?;
        if offset > 0 {
            std_file.seek(std::io::SeekFrom::Start(offset))?;
        }
        let total_size = std_file.metadata()?.len() - offset;

        // Create a pipe. F_SETPIPE_SZ is advisory; the actual size the
        // kernel picks is whatever fcntl returns (negative = err).
        let mut fds = [0i32; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let pipe_r = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let pipe_w = unsafe { OwnedFd::from_raw_fd(fds[1]) };

        let _requested_sz = unsafe {
            libc::fcntl(
                pipe_w.as_raw_fd(),
                libc::F_SETPIPE_SZ,
                CHUNK_SIZE as libc::c_int,
            )
        };
        // Record the actual pipe size — at least for diagnostic logs.
        // We don't need to read it back; the splice loop handles
        // whatever number of rounds the kernel ends up needing.

        let file_fd = std_file.as_raw_fd();
        let pipe_w_fd = pipe_w.as_raw_fd();
        let pipe_r_fd = pipe_r.as_raw_fd();
        let stdout_fd = libc::STDOUT_FILENO;

        let mut remaining = total_size;
        while remaining > 0 {
            let chunk = remaining.min(CHUNK_SIZE as u64) as usize;

            // Frame header: [4B payload_len_total][1B TYPE_DATA][4B payload_len].
            // payload_len_total = 1 + 4 + chunk = 5 + chunk.
            let mut header = [0u8; 9];
            header[0..4].copy_from_slice(&((5 + chunk) as u32).to_le_bytes());
            header[4] = 0x84; // TYPE_DATA
            header[5..9].copy_from_slice(&(chunk as u32).to_le_bytes());
            write_all_fd(stdout_fd, &header)?;

            splice_n(file_fd, pipe_w_fd, pipe_r_fd, stdout_fd, chunk)?;
            remaining -= chunk as u64;
        }

        // Ok { hash: None } frame = [4B payload_len=2][1B TYPE_OK=0x82][1B present=0].
        let mut ok_frame = [0u8; 6];
        ok_frame[0..4].copy_from_slice(&2u32.to_le_bytes());
        ok_frame[4] = 0x82; // TYPE_OK
        ok_frame[5] = 0; // hash option: absent
        write_all_fd(stdout_fd, &ok_frame)?;

        drop(pipe_r);
        drop(pipe_w);
        drop(std_file);
        Ok(())
    })
    .await?;
    result.map_err(Into::into)
}

/// Move exactly `n` bytes through the pipe from `file_fd` to `stdout_fd`.
/// Loops because splice may move fewer bytes than requested (pipe size
/// smaller than `n`, or PIPE_BUF semantics). Pairs the read-side and
/// write-side splices one after the other — standard file→pipe→sock
/// pattern.
#[cfg(target_os = "linux")]
fn splice_n(
    file_fd: i32,
    pipe_w_fd: i32,
    pipe_r_fd: i32,
    stdout_fd: i32,
    n: usize,
) -> std::io::Result<()> {
    let mut moved = 0usize;
    while moved < n {
        let want = n - moved;
        let got = unsafe {
            libc::splice(
                file_fd,
                std::ptr::null_mut(),
                pipe_w_fd,
                std::ptr::null_mut(),
                want,
                libc::SPLICE_F_MOVE | libc::SPLICE_F_MORE,
            )
        };
        if got < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if got == 0 {
            break;
        }
        let mut drained = 0usize;
        while drained < got as usize {
            let n2 = unsafe {
                libc::splice(
                    pipe_r_fd,
                    std::ptr::null_mut(),
                    stdout_fd,
                    std::ptr::null_mut(),
                    got as usize - drained,
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
}

/// Equivalent of Write::write_all on a raw fd — loops on short writes
/// / EINTR. Used inside the splice-path spawn_blocking so headers and
/// the Ok frame share the same thread as the splice loop.
#[cfg(target_os = "linux")]
fn write_all_fd(fd: i32, mut buf: &[u8]) -> std::io::Result<()> {
    while !buf.is_empty() {
        let n = unsafe { libc::write(fd, buf.as_ptr() as *const _, buf.len()) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "write returned 0",
            ));
        }
        buf = &buf[n as usize..];
    }
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
async fn handle_put<W, R>(
    path: &str,
    declared_size: u64,
    sync: bool,
    out: &mut W,
    reader: &mut R,
) -> Result<Message>
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
    let mut written: u64 = 0;

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
                enforce_write_bound(written, payload.len(), declared_size)?;
                consume_block(&payload, &mut file, &mut hasher, dedup_state.as_mut()).await?;
                written += payload.len() as u64;
            }
            Message::DataCompressed {
                algo,
                original_size,
                payload,
            } => {
                let decoded = compress::decode_block(algo, original_size, &payload)?;
                enforce_write_bound(written, decoded.len(), declared_size)?;
                consume_block(&decoded, &mut file, &mut hasher, dedup_state.as_mut()).await?;
                written += decoded.len() as u64;
            }
            Message::Done => break,
            other => return Err(anyhow::anyhow!("put: unexpected message {other:?}")),
        }
    }

    // Serve any trailing blocks that weren't sent because the CAS already
    // had them. We do this *after* draining the wire so the server's
    // stream of writes follows source order.
    if let Some(state) = dedup_state.as_mut() {
        flush_remaining_cas_blocks(state, &mut file, &mut hasher, &mut written, declared_size)
            .await?;
    }

    file.flush().await?;
    if sync {
        file.sync_all().await?;
    }
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
    written: &mut u64,
    declared_size: u64,
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
        enforce_write_bound(*written, cached.len(), declared_size)?;
        hasher.update(&cached);
        file.write_all(&cached).await?;
        *written += cached.len() as u64;
        state.cursor += 1;
    }
    Ok(())
}

/// Refuse to grow the destination past the size the client declared
/// on PUT. Protects the server from a malicious or buggy client that
/// sends unbounded Data frames — without this, `size: 100` could
/// followed by TB of Data and the server would dutifully write it all.
/// A small per-block tolerance isn't meaningful; the declared size
/// should equal the final dst size exactly.
fn enforce_write_bound(written: u64, incoming: usize, declared: u64) -> Result<()> {
    if written + incoming as u64 > declared {
        bail!(
            "put: client would write {} bytes past the declared size of {} \
             (already wrote {})",
            written + incoming as u64 - declared,
            declared,
            written
        );
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
