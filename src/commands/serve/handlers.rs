use crate::core::cas;
use crate::core::compress;
use crate::core::framing::Framing;
use crate::core::protocol::{CompressionAlgo, ListEntry, Message};
use anyhow::{bail, Result};
use std::path::Path;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use super::filesystem::RootFs;
use super::session::CHUNK_SIZE;

pub(super) async fn handle_stat(filesystem: &RootFs, path: &str) -> Result<Message> {
    let info = filesystem.info(Path::new(path))?;
    Ok(Message::StatResponse {
        size: info.size,
        mtime: info.mtime,
        is_dir: info.is_dir,
    })
}

pub(super) async fn handle_list(filesystem: &RootFs, path: &str) -> Result<Message> {
    let base = Path::new(path).to_path_buf();
    let filesystem = filesystem.clone();
    let entries =
        tokio::task::spawn_blocking(move || collect_list_bound(base, filesystem, |_, _| {}))
            .await??;
    Ok(Message::ListResponse { entries })
}

#[cfg(all(test, unix))]
fn collect_list(
    base: std::path::PathBuf,
    root: std::path::PathBuf,
    before_read: impl FnMut(&Path, &Path),
) -> Result<Vec<ListEntry>> {
    collect_list_bound(base, RootFs::new(&root)?, before_read)
}

fn collect_list_bound(
    base: std::path::PathBuf,
    filesystem: RootFs,
    mut before_read: impl FnMut(&Path, &Path),
) -> Result<Vec<ListEntry>> {
    let root = filesystem.path();
    let binding = super::listing::BoundListing::from_filesystem(filesystem.clone());
    let mut out = Vec::new();
    let mut pending = vec![(base.clone(), Vec::<std::path::PathBuf>::new())];
    while let Some((path, mut ancestors)) = pending.pop() {
        // Validate each followed target before opening or enumerating it.
        // Checking only the initial LIST root permits descendant symlinks
        // to disclose names and metadata outside the server jail.
        let physical = std::fs::canonicalize(&path)?;
        if !physical.starts_with(root) {
            bail!(
                "LIST path '{}' escapes server root '{}'",
                path.display(),
                root.display()
            );
        }
        before_read(&path, &physical);
        let observation = binding.observe(&physical)?;
        if observation.is_dir {
            if ancestors.contains(&physical) {
                bail!("symbolic-link directory cycle at '{}'", path.display());
            }
            ancestors.push(physical);
            let mut children = observation
                .children
                .into_iter()
                .map(|name| path.join(name))
                .collect::<Vec<_>>();
            children.sort();
            for child in children.into_iter().rev() {
                pending.push((child, ancestors.clone()));
            }
        }
        if path == base {
            continue;
        }
        let rel = path.strip_prefix(&base)?.to_string_lossy().into_owned();
        out.push(ListEntry {
            path: rel,
            size: observation.size,
            mtime: observation.mtime,
            is_dir: observation.is_dir,
        });
    }
    Ok(out)
}

pub(super) async fn handle_hash(
    filesystem: &RootFs,
    path: &str,
    offset: u64,
    limit: Option<u64>,
) -> Result<Message> {
    let mut file = filesystem.read(Path::new(path))?;
    let hash = tokio::task::spawn_blocking(move || -> Result<String> {
        use std::io::Read;
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

#[allow(clippy::too_many_arguments)] // Carry the filesystem capability alongside existing wire options.
pub(super) async fn handle_get<W>(
    filesystem: &RootFs,
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
    let file = filesystem.read(Path::new(path))?;
    // splice(2) hardcodes STDOUT_FILENO and bypasses userspace, so it
    // can't run with compression or AEAD.
    #[cfg(target_os = "linux")]
    {
        if fast && algo == CompressionAlgo::None && allow_splice && !framing.is_aead() {
            return handle_get_splice_linux(file, offset, out).await;
        }
    }
    let _ = allow_splice;

    use tokio::io::AsyncReadExt;

    let mut file = fs::File::from_std(file);
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
        let frame = compress::encode_block(algo, buf[..n].to_vec())?;
        framing.write_message(out, &frame).await?;
    }

    let hash = hasher.map(|h| h.finalize().to_hex().to_string());
    framing.write_message(out, &Message::Ok { hash }).await?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) async fn handle_get_splice_linux<W>(
    mut std_file: std::fs::File,
    offset: u64,
    out: &mut W,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use std::io::Seek;
    use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
    use tokio::io::AsyncWriteExt;

    out.flush().await?;

    let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
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

pub(super) struct PutOptions {
    pub declared_size: u64,
    pub offset: u64,
    pub overwrite: bool,
    pub sync: bool,
}

pub(super) async fn handle_put<W, R>(
    filesystem: &RootFs,
    store: &cas::Store,
    path: &str,
    options: PutOptions,
    out: &mut W,
    reader: &mut R,
    framing: &mut Framing,
) -> Result<Message>
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncSeekExt;

    let PutOptions {
        declared_size,
        offset,
        overwrite,
        sync,
    } = options;
    if offset > declared_size {
        bail!(
            "put: offset {} past declared size {}",
            offset,
            declared_size
        );
    }
    // A fresh PUT is written through a handle-bound transaction. The
    // overwrite policy is enforced both at staging time and again by the
    // atomic commit, so a concurrent creator cannot slip between a preflight
    // check and publication. offset>0 is a content-proved resume.
    let (mut file, transaction) = if offset == 0 {
        let transaction = filesystem.atomic_file(Path::new(path), overwrite, sync)?;
        let file = fs::File::from_std(transaction.try_clone_file()?);
        (file, Some(transaction))
    } else {
        let mut f = fs::File::from_std(filesystem.write(Path::new(path), false, sync)?);
        f.seek(std::io::SeekFrom::Start(offset)).await?;
        (f, None)
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
        if let Some(Message::HaveBlocks { block_size, hashes }) = next {
            if block_size as usize != CHUNK_SIZE {
                bail!(
                    "put: client block_size {} does not match server CHUNK_SIZE {} — \
                     dedup would misalign CAS boundaries",
                    block_size,
                    CHUNK_SIZE
                );
            }
            if let Some(cap) = cas::cap_bytes() {
                let store = store.clone();
                let _ = tokio::task::spawn_blocking(move || store.evict_to_cap(cap)).await;
            }

            let mut bits = vec![0u8; hashes.len().div_ceil(8)];
            for (i, h) in hashes.iter().enumerate() {
                if !store.has(h) {
                    bits[i / 8] |= 1 << (i % 8);
                }
            }
            framing
                .write_message(out, &Message::MissingBlocks { bits: bits.clone() })
                .await?;
            out.flush().await?;
            dedup_state = Some(DedupState {
                store: store.clone(),
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
                None => bail!("put: client closed connection before Done"),
            },
        };
        match m {
            message @ (Message::Data { .. } | Message::DataCompressed { .. }) => {
                let decoded = compress::decode_data_block(message)?;
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
    if sync && transaction.is_none() {
        file.sync_all().await?;
    }
    drop(file);

    if let Some(transaction) = transaction {
        transaction.commit(sync)?;
    }

    let hash = hasher.map(|h| h.finalize().to_hex().to_string());
    Ok(Message::Ok { hash })
}

struct DedupState {
    store: cas::Store,
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
            let cached = state.store.read(&state.hashes[state.cursor])?;
            enforce_write_bound(*written, cached.len(), declared_size)?;
            if let Some(h) = hasher.as_mut() {
                h.update(&cached);
            }
            file.write_all(&cached).await?;
            *written = crate::core::protocol::checked_transfer_total(
                *written,
                cached.len(),
                declared_size,
            )?;
            state.cursor += 1;
        }
        if state.cursor < state.hashes.len() && state.is_missing(state.cursor) {
            let mut h = [0u8; 32];
            h.copy_from_slice(blake3::hash(block).as_bytes());
            let _ = state.store.write(&h, block);
            state.cursor += 1;
        }
    }
    enforce_write_bound(*written, block.len(), declared_size)?;
    if let Some(h) = hasher.as_mut() {
        h.update(block);
    }
    file.write_all(block).await?;
    *written = crate::core::protocol::checked_transfer_total(*written, block.len(), declared_size)?;
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
        let cached = state.store.read(&state.hashes[state.cursor])?;
        enforce_write_bound(*written, cached.len(), declared_size)?;
        if let Some(h) = hasher.as_mut() {
            h.update(&cached);
        }
        file.write_all(&cached).await?;
        *written =
            crate::core::protocol::checked_transfer_total(*written, cached.len(), declared_size)?;
        state.cursor += 1;
    }
    Ok(())
}

fn enforce_write_bound(written: u64, incoming: usize, declared: u64) -> Result<()> {
    crate::core::protocol::checked_transfer_total(written, incoming, declared).map(|_| ())?;
    Ok(())
}

pub(super) async fn handle_put_chunked<R>(
    filesystem: &RootFs,
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

    let mut file = fs::File::from_std(filesystem.write(Path::new(path), false, sync)?);
    file.seek(std::io::SeekFrom::Start(offset)).await?;

    let mut written: u64 = 0;
    loop {
        // The byte count does not terminate a framed PUT. Consume Done even
        // for empty ranges so it cannot become the next session command.
        match framing.read_message(reader).await? {
            Some(message @ (Message::Data { .. } | Message::DataCompressed { .. })) => {
                let decoded = compress::decode_data_block(message)?;
                let next_written =
                    crate::core::protocol::checked_transfer_total(written, decoded.len(), length)?;
                file.write_all(&decoded).await?;
                written = next_written;
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
    // Acknowledgement also orders a following GET on this connection after
    // Tokio's pending file write, including the final short payload block.
    file.flush().await?;
    if sync {
        file.sync_all().await?;
    }
    Ok(Message::Ok { hash: None })
}

pub(super) async fn handle_get_chunked<W>(
    filesystem: &RootFs,
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

    let mut file = fs::File::from_std(filesystem.read(Path::new(path))?);
    let metadata = file.metadata().await?;
    if !metadata.is_file() {
        bail!("get_chunked: source is not a regular file");
    }
    let requested_end = offset
        .checked_add(length)
        .ok_or_else(|| anyhow::anyhow!("get_chunked: requested range overflows u64"))?;
    if requested_end > metadata.len() || (length == 0 && metadata.len() != 0) {
        bail!(
            "get_chunked: requested range {offset}..{requested_end} does not match source size {}",
            metadata.len()
        );
    }
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
        let frame = compress::encode_block(algo, buf[..n].to_vec())?;
        framing.write_message(out, &frame).await?;
        remaining -= n as u64;
    }
    framing
        .write_message(out, &Message::Ok { hash: None })
        .await?;
    Ok(())
}

pub(super) async fn handle_mkdir(filesystem: &RootFs, path: &str, sync: bool) -> Result<Message> {
    filesystem.create_directories(Path::new(path), sync)?;
    Ok(Message::Ok { hash: None })
}

pub(super) async fn handle_truncate(
    filesystem: &RootFs,
    path: &str,
    size: u64,
    sync: bool,
) -> Result<Message> {
    let file = filesystem.write(Path::new(path), true, sync)?;
    file.set_len(size)?;
    if sync {
        file.sync_all()?;
    }
    Ok(Message::Ok { hash: None })
}

pub(super) async fn handle_resume(filesystem: &RootFs, path: &str) -> Result<Message> {
    let mut file = match filesystem.read(Path::new(path)) {
        Ok(file) => file,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(Message::ResumeResponse {
                size: 0,
                block_hash: None,
            });
        }
        Err(error) => return Err(error),
    };

    let size = file.metadata()?.len();
    if size < CHUNK_SIZE as u64 {
        return Ok(Message::ResumeResponse {
            size,
            block_hash: None,
        });
    }

    let block_start = (size / CHUNK_SIZE as u64 - 1) * CHUNK_SIZE as u64;
    let block_hash = tokio::task::spawn_blocking(move || -> Result<String> {
        use std::io::{Read, Seek};
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

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::collect_list;
    use super::handle_mkdir;

    #[cfg(unix)]
    struct ReplacedAncestor {
        _temporary: tempfile::TempDir,
        root: std::path::PathBuf,
        checked: std::path::PathBuf,
        outside: std::path::PathBuf,
    }

    #[cfg(unix)]
    impl ReplacedAncestor {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().join("root");
            std::fs::create_dir_all(root.join("ancestor/inner")).unwrap();
            let root = root.canonicalize().unwrap();
            let outside = temporary.path().join("outside");
            std::fs::create_dir_all(outside.join("inner")).unwrap();
            let checked = root.join("ancestor/inner/payload");
            std::fs::write(&checked, b"inside").unwrap();
            std::fs::write(outside.join("inner/payload"), b"outside").unwrap();
            Self {
                _temporary: temporary,
                root,
                checked,
                outside,
            }
        }

        fn replace(&self) {
            std::fs::rename(self.root.join("ancestor"), self.root.join("retained")).unwrap();
            std::os::unix::fs::symlink(&self.outside, self.root.join("ancestor")).unwrap();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn root_boundary_atomic_put_cannot_target_root_or_leave_staging() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let filesystem = super::RootFs::new(&root).unwrap();
        let wire = crate::core::protocol::encode_message(&crate::core::protocol::Message::Done);
        let result = super::handle_put(
            &filesystem,
            &crate::core::cas::Store::disabled(),
            root.to_str().unwrap(),
            super::PutOptions {
                declared_size: 0,
                offset: 0,
                overwrite: false,
                sync: false,
            },
            &mut Vec::new(),
            &mut wire.as_slice(),
            &mut crate::core::framing::Framing::plain(),
        )
        .await;
        assert!(
            result.is_err(),
            "the server root is a directory, not a file destination"
        );
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            0,
            "a refused root PUT must not leave a transaction directory"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn root_boundary_get_rejects_a_replaced_ancestor() {
        let fixture = ReplacedAncestor::new();
        let checked =
            crate::commands::serve::validate_path(fixture.checked.to_str().unwrap(), &fixture.root)
                .unwrap();
        let filesystem = super::RootFs::new(&fixture.root).unwrap();
        fixture.replace();
        let mut output = Vec::new();
        let result = super::handle_get(
            &filesystem,
            checked.to_str().unwrap(),
            0,
            crate::core::protocol::CompressionAlgo::None,
            false,
            false,
            &mut output,
            &mut crate::core::framing::Framing::plain(),
        )
        .await;
        assert!(
            result.is_err(),
            "GET must not reopen a changed ancestor outside the captured root"
        );
        assert!(
            output.is_empty(),
            "a refused GET must not emit outside content"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn root_boundary_truncate_rejects_a_replaced_ancestor() {
        let fixture = ReplacedAncestor::new();
        let checked =
            crate::commands::serve::validate_path(fixture.checked.to_str().unwrap(), &fixture.root)
                .unwrap();
        let filesystem = super::RootFs::new(&fixture.root).unwrap();
        fixture.replace();
        let result = super::handle_truncate(&filesystem, checked.to_str().unwrap(), 1, false).await;
        assert!(
            result.is_err(),
            "TRUNCATE must not reopen a changed ancestor outside the captured root"
        );
        assert_eq!(
            std::fs::read(fixture.outside.join("inner/payload")).unwrap(),
            b"outside"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn root_boundary_mkdir_rejects_a_replaced_ancestor() {
        let fixture = ReplacedAncestor::new();
        let target = fixture.checked.with_file_name("new/empty");
        let checked =
            crate::commands::serve::validate_path(target.to_str().unwrap(), &fixture.root).unwrap();
        let filesystem = super::RootFs::new(&fixture.root).unwrap();
        fixture.replace();
        let result = super::handle_mkdir(&filesystem, checked.to_str().unwrap(), false).await;
        assert!(result.is_err(), "MKDIR must stay below the captured root");
        assert!(!fixture.outside.join("inner/new").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn root_boundary_atomic_put_rejects_a_replaced_ancestor() {
        let fixture = ReplacedAncestor::new();
        let target = fixture.checked.with_file_name("new");
        let checked =
            crate::commands::serve::validate_path(target.to_str().unwrap(), &fixture.root).unwrap();
        let filesystem = super::RootFs::new(&fixture.root).unwrap();
        fixture.replace();
        let mut output = Vec::new();
        let wire = [
            crate::core::protocol::encode_message(&crate::core::protocol::Message::Data {
                payload: b"new".to_vec(),
            }),
            crate::core::protocol::encode_message(&crate::core::protocol::Message::Done),
        ]
        .concat();
        let result = super::handle_put(
            &filesystem,
            &crate::core::cas::Store::disabled(),
            checked.to_str().unwrap(),
            super::PutOptions {
                declared_size: 3,
                offset: 0,
                overwrite: false,
                sync: false,
            },
            &mut output,
            &mut wire.as_slice(),
            &mut crate::core::framing::Framing::plain(),
        )
        .await;
        assert!(
            result.is_err(),
            "atomic PUT must capture its parent relative to the root"
        );
        assert!(!fixture.outside.join("inner/new").exists());
    }

    #[cfg(unix)]
    #[test]
    fn list_metadata_stays_on_checked_target_when_alias_is_retargeted() {
        let directory = tempfile::tempdir().unwrap();
        let jail = directory.path().join("jail");
        std::fs::create_dir(&jail).unwrap();
        let jail = std::fs::canonicalize(jail).unwrap();
        let safe = jail.join("safe");
        std::fs::write(&safe, b"abc").unwrap();
        let outside = directory.path().join("outside");
        std::fs::write(&outside, [b'x'; 101]).unwrap();
        let alias = jail.join("alias");
        std::os::unix::fs::symlink(&safe, &alias).unwrap();
        let mut changed = false;
        let entries = collect_list(jail.clone(), jail, |logical, _| {
            if logical == alias {
                std::fs::remove_file(&alias).unwrap();
                std::os::unix::fs::symlink(&outside, &alias).unwrap();
                changed = true;
            }
        })
        .unwrap();
        assert!(
            changed,
            "test did not reach the canonicalization/open boundary"
        );
        let entry = entries.iter().find(|entry| entry.path == "alias").unwrap();
        assert_eq!(
            entry.size, 3,
            "LIST disclosed metadata of an outside replacement"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn synced_mkdir_creates_nested_empty_directories_through_existing_link_prefix() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let filesystem = super::RootFs::new(&root).unwrap();
        let real = directory.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = directory.path().join("linked-prefix");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let target = link.join("parent/empty");
        handle_mkdir(
            &filesystem,
            crate::commands::serve::validate_path(target.to_str().unwrap(), &root)
                .unwrap()
                .to_str()
                .unwrap(),
            true,
        )
        .await
        .unwrap();
        assert!(real.join("parent/empty").is_dir());
        assert_eq!(
            std::fs::read_dir(real.join("parent/empty"))
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn synced_mkdir_reports_creation_failure_without_replacing_blocker() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let filesystem = super::RootFs::new(&root).unwrap();
        let blocker = directory.path().join("blocker");
        std::fs::write(&blocker, b"existing bytes").unwrap();
        let error = handle_mkdir(
            &filesystem,
            crate::commands::serve::validate_path(blocker.join("child").to_str().unwrap(), &root)
                .unwrap()
                .to_str()
                .unwrap(),
            true,
        )
        .await
        .expect_err("durability/creation failures must not become successful MKDIR replies");
        let io_error = error.downcast_ref::<std::io::Error>().unwrap();
        #[cfg(unix)]
        assert_eq!(io_error.kind(), std::io::ErrorKind::NotADirectory);
        #[cfg(not(unix))]
        assert_eq!(io_error.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(std::fs::read(blocker).unwrap(), b"existing bytes");
    }

    #[cfg(not(unix))]
    #[tokio::test]
    async fn unsupported_synced_mkdir_refuses_before_creating_any_directory() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let filesystem = super::RootFs::new(&root).unwrap();
        let target = directory.path().join("missing/empty");
        handle_mkdir(
            &filesystem,
            crate::commands::serve::validate_path(target.to_str().unwrap(), &root)
                .unwrap()
                .to_str()
                .unwrap(),
            true,
        )
        .await
        .expect_err("unsupported durable namespace creation must fail closed");
        assert!(!directory.path().join("missing").exists());
    }
}
