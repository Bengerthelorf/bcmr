use crate::core::cas;
use crate::core::compress;
use crate::core::framing::Framing;
use crate::core::protocol::{CompressionAlgo, ListEntry, Message};
use anyhow::{bail, Result};
use std::path::Path;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use super::session::CHUNK_SIZE;

pub(super) async fn handle_stat(path: &str) -> Result<Message> {
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

pub(super) async fn handle_list(path: &str) -> Result<Message> {
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
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            out.push(ListEntry {
                path: rel,
                size: meta.len(),
                mtime,
                is_dir: meta.is_dir(),
            });
        }
        Ok(out)
    })
    .await??;

    Ok(Message::ListResponse { entries })
}

pub(super) async fn handle_hash(path: &str, offset: u64, limit: Option<u64>) -> Result<Message> {
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

pub(super) async fn handle_get<W>(
    path: &str,
    offset: u64,
    algo: CompressionAlgo,
    fast: bool,
    allow_splice: bool,
    out: &mut W,
    framing: &mut Framing,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    // splice(2) hardcodes STDOUT_FILENO and bypasses userspace, so it
    // can't run with compression or AEAD.
    #[cfg(target_os = "linux")]
    {
        if fast && algo == CompressionAlgo::None && allow_splice && !framing.is_aead() {
            return handle_get_splice_linux(path, offset, out).await;
        }
    }
    let _ = allow_splice;

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
        framing.write_message(out, &frame).await?;
    }

    let hash = hasher.map(|h| h.finalize().to_hex().to_string());
    framing.write_message(out, &Message::Ok { hash }).await?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) async fn handle_get_splice_linux<W>(path: &str, offset: u64, out: &mut W) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use std::io::Seek;
    use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
    use tokio::io::AsyncWriteExt;

    out.flush().await?;

    let path = path.to_owned();
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut std_file = std::fs::File::open(&path)?;
        if offset > 0 {
            std_file.seek(std::io::SeekFrom::Start(offset))?;
        }
        let total_size = std_file.metadata()?.len().saturating_sub(offset);

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

        let file_fd = std_file.as_raw_fd();
        let pipe_w_fd = pipe_w.as_raw_fd();
        let pipe_r_fd = pipe_r.as_raw_fd();
        let stdout_fd = libc::STDOUT_FILENO;

        let mut remaining = total_size;
        while remaining > 0 {
            let chunk = remaining.min(CHUNK_SIZE as u64) as usize;

            // Wire frame: [u32 payload_len=5+chunk][u8 TYPE_DATA=0x84][u32 chunk_len].
            let mut header = [0u8; 9];
            header[0..4].copy_from_slice(&((5 + chunk) as u32).to_le_bytes());
            header[4] = 0x84;
            header[5..9].copy_from_slice(&(chunk as u32).to_le_bytes());
            write_all_fd(stdout_fd, &header)?;

            splice_n(file_fd, pipe_w_fd, pipe_r_fd, stdout_fd, chunk)?;
            remaining -= chunk as u64;
        }

        // Ok { hash: None } = [u32 payload_len=2][u8 TYPE_OK=0x82][u8 present=0].
        let mut ok_frame = [0u8; 6];
        ok_frame[0..4].copy_from_slice(&2u32.to_le_bytes());
        ok_frame[4] = 0x82;
        ok_frame[5] = 0;
        write_all_fd(stdout_fd, &ok_frame)?;

        drop(pipe_r);
        drop(pipe_w);
        drop(std_file);
        Ok(())
    })
    .await?;
    result.map_err(Into::into)
}

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
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "splice hit EOF before the advertised frame payload was produced",
            ));
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
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "splice could not drain the pipe into stdout",
                ));
            }
            drained += n2 as usize;
        }
        moved += drained;
    }
    Ok(())
}

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

pub(super) async fn handle_put<W, R>(
    path: &str,
    declared_size: u64,
    offset: u64,
    sync: bool,
    out: &mut W,
    reader: &mut R,
    framing: &mut Framing,
) -> Result<Message>
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncSeekExt;

    if offset > declared_size {
        bail!(
            "put: offset {} past declared size {}",
            offset,
            declared_size
        );
    }
    ensure_parent_dir(path).await?;

    let mut file = if offset == 0 {
        fs::File::create(path).await?
    } else {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .await?;
        f.seek(std::io::SeekFrom::Start(offset)).await?;
        f
    };
    let mut hasher = if offset == 0 {
        Some(blake3::Hasher::new())
    } else {
        None
    };
    let mut written: u64 = offset;

    let first = framing.read_message(reader).await?;
    let mut dedup_state: Option<DedupState> = None;
    let mut next: Option<Message> = first;

    if offset == 0 {
        if let Some(Message::HaveBlocks { hashes, .. }) = next {
            if let Some(cap) = cas::cap_bytes() {
                let _ = tokio::task::spawn_blocking(move || cas::evict_to_cap(cap)).await;
            }

            let mut bits = vec![0u8; hashes.len().div_ceil(8)];
            for (i, h) in hashes.iter().enumerate() {
                if !cas::has(h) {
                    bits[i / 8] |= 1 << (i % 8);
                }
            }
            framing
                .write_message(out, &Message::MissingBlocks { bits: bits.clone() })
                .await?;
            out.flush().await?;
            dedup_state = Some(DedupState {
                hashes,
                bits,
                cursor: 0,
            });
            next = None;
        }
    }

    let mut msg = next;
    loop {
        let m = match msg.take() {
            Some(m) => m,
            None => match framing.read_message(reader).await? {
                Some(m) => m,
                None => break,
            },
        };
        match m {
            Message::Data { payload } => {
                consume_block(
                    &payload,
                    &mut file,
                    hasher.as_mut(),
                    dedup_state.as_mut(),
                    &mut written,
                    declared_size,
                )
                .await?;
            }
            Message::DataCompressed {
                algo,
                original_size,
                payload,
            } => {
                let decoded = compress::decode_block(algo, original_size, &payload)?;
                consume_block(
                    &decoded,
                    &mut file,
                    hasher.as_mut(),
                    dedup_state.as_mut(),
                    &mut written,
                    declared_size,
                )
                .await?;
            }
            Message::Done => break,
            other => return Err(anyhow::anyhow!("put: unexpected message {other:?}")),
        }
    }

    if let Some(state) = dedup_state.as_mut() {
        flush_remaining_cas_blocks(
            state,
            &mut file,
            hasher.as_mut(),
            &mut written,
            declared_size,
        )
        .await?;
    }
    if written != declared_size {
        bail!(
            "put: declared {} bytes, received {}",
            declared_size,
            written
        );
    }

    file.flush().await?;
    if sync {
        file.sync_all().await?;
    }
    drop(file);

    let hash = hasher.map(|h| h.finalize().to_hex().to_string());
    Ok(Message::Ok { hash })
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
    mut hasher: Option<&mut blake3::Hasher>,
    dedup: Option<&mut DedupState>,
    written: &mut u64,
    declared_size: u64,
) -> Result<()> {
    if let Some(state) = dedup {
        while state.cursor < state.hashes.len() && !state.is_missing(state.cursor) {
            let cached = cas::read(&state.hashes[state.cursor])?;
            enforce_write_bound(*written, cached.len(), declared_size)?;
            if let Some(h) = hasher.as_mut() {
                h.update(&cached);
            }
            file.write_all(&cached).await?;
            *written += cached.len() as u64;
            state.cursor += 1;
        }
        if state.cursor < state.hashes.len() && state.is_missing(state.cursor) {
            let mut h = [0u8; 32];
            h.copy_from_slice(blake3::hash(block).as_bytes());
            let _ = cas::write(&h, block);
            state.cursor += 1;
        }
    }
    enforce_write_bound(*written, block.len(), declared_size)?;
    if let Some(h) = hasher.as_mut() {
        h.update(block);
    }
    file.write_all(block).await?;
    *written += block.len() as u64;
    Ok(())
}

async fn flush_remaining_cas_blocks(
    state: &mut DedupState,
    file: &mut tokio::fs::File,
    mut hasher: Option<&mut blake3::Hasher>,
    written: &mut u64,
    declared_size: u64,
) -> Result<()> {
    while state.cursor < state.hashes.len() {
        if state.is_missing(state.cursor) {
            return Err(anyhow::anyhow!(
                "client said block {} was missing but never sent it",
                state.cursor
            ));
        }
        let cached = cas::read(&state.hashes[state.cursor])?;
        enforce_write_bound(*written, cached.len(), declared_size)?;
        if let Some(h) = hasher.as_mut() {
            h.update(&cached);
        }
        file.write_all(&cached).await?;
        *written += cached.len() as u64;
        state.cursor += 1;
    }
    Ok(())
}

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

pub(super) async fn handle_put_chunked<R>(
    path: &str,
    offset: u64,
    length: u64,
    sync: bool,
    reader: &mut R,
    framing: &mut Framing,
) -> Result<Message>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::{AsyncSeekExt, AsyncWriteExt as _};

    ensure_parent_dir(path).await?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;

    let mut written: u64 = 0;
    loop {
        if written == length {
            break;
        }
        match framing.read_message(reader).await? {
            Some(Message::Data { payload }) => {
                if written + payload.len() as u64 > length {
                    bail!(
                        "put_chunked: client sent {} bytes past the declared {}",
                        written + payload.len() as u64 - length,
                        length
                    );
                }
                file.write_all(&payload).await?;
                written += payload.len() as u64;
            }
            Some(Message::DataCompressed {
                algo,
                original_size,
                payload,
            }) => {
                let decoded = compress::decode_block(algo, original_size, &payload)?;
                if written + decoded.len() as u64 > length {
                    bail!(
                        "put_chunked: client sent {} bytes past the declared {}",
                        written + decoded.len() as u64 - length,
                        length
                    );
                }
                file.write_all(&decoded).await?;
                written += decoded.len() as u64;
            }
            Some(Message::Done) => break,
            Some(other) => bail!("put_chunked: unexpected message {other:?}"),
            None => bail!("put_chunked: client closed connection before Done"),
        }
    }
    if written != length {
        bail!(
            "put_chunked: declared {} bytes, received {}",
            length,
            written
        );
    }
    if sync {
        file.sync_all().await?;
    }
    Ok(Message::Ok { hash: None })
}

pub(super) async fn handle_get_chunked<W>(
    path: &str,
    offset: u64,
    length: u64,
    algo: CompressionAlgo,
    out: &mut W,
    framing: &mut Framing,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut file = fs::File::open(path).await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;

    let mut remaining = length;
    let mut buf = vec![0u8; CHUNK_SIZE];
    while remaining > 0 {
        let want = remaining.min(CHUNK_SIZE as u64) as usize;
        let n = file.read(&mut buf[..want]).await?;
        if n == 0 {
            bail!(
                "get_chunked: unexpected EOF at offset {} (still needed {} bytes)",
                offset + length - remaining,
                remaining
            );
        }
        let frame = compress::encode_block(algo, buf[..n].to_vec());
        framing.write_message(out, &frame).await?;
        remaining -= n as u64;
    }
    framing
        .write_message(out, &Message::Ok { hash: None })
        .await?;
    Ok(())
}

pub(super) async fn handle_mkdir(path: &str) -> Result<Message> {
    fs::create_dir_all(path).await?;
    Ok(Message::Ok { hash: None })
}

async fn ensure_parent_dir(path: &str) -> Result<()> {
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).await?;
        }
    }
    Ok(())
}

pub(super) async fn handle_truncate(path: &str, size: u64) -> Result<Message> {
    ensure_parent_dir(path).await?;
    let f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .await?;
    f.set_len(size).await?;
    Ok(Message::Ok { hash: None })
}

pub(super) async fn handle_resume(path: &str) -> Result<Message> {
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
