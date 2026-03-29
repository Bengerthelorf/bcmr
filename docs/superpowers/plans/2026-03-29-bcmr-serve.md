# `bcmr serve` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace per-file SSH process spawning with a persistent binary protocol over a single SSH connection, eliminating roundtrip overhead and enabling server-side hashing.

**Architecture:** `bcmr serve` is a subcommand that reads binary-framed requests from stdin and writes responses to stdout. The local bcmr client launches it via `ssh host 'bcmr serve'` and communicates through the pipe. The protocol uses length-prefixed frames with a 1-byte message type. Data transfer uses zstd streaming compression at the application layer.

**Tech Stack:** Existing tokio async runtime, `zstd` crate for streaming compression, existing `blake3` for server-side hashing. No new network libraries — the transport is SSH stdin/stdout.

---

## File Structure

| File | Responsibility |
|------|---------------|
| `src/core/protocol.rs` (create) | Binary frame format: encode/decode, message types, constants |
| `src/commands/serve.rs` (create) | `bcmr serve` server loop: read requests, dispatch, write responses |
| `src/core/serve_client.rs` (create) | Client-side: launch SSH, send requests, receive responses |
| `src/cli.rs` (modify) | Add `Serve` and `Deploy` subcommands |
| `src/main.rs` (modify) | Route `Serve` and `Deploy` commands |
| `src/commands/remote_copy.rs` (modify) | Use serve client when remote has bcmr, fall back to legacy SSH |
| `src/commands/mod.rs` (modify) | Register `serve` module |
| `src/core/mod.rs` (modify) | Register `protocol` and `serve_client` modules |
| `tests/serve_protocol_tests.rs` (create) | Unit tests for protocol encode/decode |
| `tests/e2e_serve_tests.rs` (create) | End-to-end tests for serve (local loopback) |

---

### Task 1: Binary Frame Protocol

**Files:**
- Create: `src/core/protocol.rs`
- Modify: `src/core/mod.rs`
- Test: `tests/serve_protocol_tests.rs`

The protocol is the foundation. Every message is:
```
[4 bytes: payload length, little-endian][1 byte: message type][payload]
```

- [ ] **Step 1: Write failing tests for protocol encode/decode**

```rust
// tests/serve_protocol_tests.rs
use bcmr::core::protocol::{Message, encode_message, decode_message};

#[test]
fn test_encode_decode_list() {
    let msg = Message::List { path: "/data".into() };
    let bytes = encode_message(&msg);
    let decoded = decode_message(&bytes).unwrap();
    assert!(matches!(decoded, Message::List { ref path } if path == "/data"));
}

#[test]
fn test_encode_decode_stat_response() {
    let msg = Message::StatResponse {
        size: 1048576,
        mtime: 1700000000,
        is_dir: false,
    };
    let bytes = encode_message(&msg);
    let decoded = decode_message(&bytes).unwrap();
    match decoded {
        Message::StatResponse { size, mtime, is_dir } => {
            assert_eq!(size, 1048576);
            assert_eq!(mtime, 1700000000);
            assert!(!is_dir);
        }
        _ => panic!("wrong message type"),
    }
}

#[test]
fn test_encode_decode_get_with_offset() {
    let msg = Message::Get { path: "/data/big.iso".into(), offset: 4194304 };
    let bytes = encode_message(&msg);
    let decoded = decode_message(&bytes).unwrap();
    match decoded {
        Message::Get { path, offset } => {
            assert_eq!(path, "/data/big.iso");
            assert_eq!(offset, 4194304);
        }
        _ => panic!("wrong type"),
    }
}

#[test]
fn test_encode_decode_data_chunk() {
    let data = vec![0xABu8; 65536];
    let msg = Message::Data { payload: data.clone() };
    let bytes = encode_message(&msg);
    let decoded = decode_message(&bytes).unwrap();
    match decoded {
        Message::Data { payload } => assert_eq!(payload, data),
        _ => panic!("wrong type"),
    }
}

#[test]
fn test_encode_decode_error() {
    let msg = Message::Error { message: "file not found".into() };
    let bytes = encode_message(&msg);
    let decoded = decode_message(&bytes).unwrap();
    match decoded {
        Message::Error { message } => assert_eq!(message, "file not found"),
        _ => panic!("wrong type"),
    }
}

#[test]
fn test_decode_empty_fails() {
    assert!(decode_message(&[]).is_none());
}

#[test]
fn test_decode_truncated_fails() {
    assert!(decode_message(&[0x05, 0x00, 0x00, 0x00]).is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test serve_protocol_tests 2>&1 | tail -5`
Expected: Compilation error — `protocol` module doesn't exist yet.

- [ ] **Step 3: Implement the protocol module**

```rust
// src/core/protocol.rs

/// Binary frame protocol for bcmr serve.
///
/// Frame format: [4 bytes LE payload length][1 byte type][payload]
/// All integers are little-endian. Strings are length-prefixed (4 bytes LE + UTF-8).

/// Protocol version. Client and server must match on major version.
pub const PROTOCOL_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    // === Requests (client → server) ===
    /// Handshake: client sends version
    Hello { version: u8 },
    /// List files in directory. Response: ListResponse.
    List { path: String },
    /// Stat a single path. Response: StatResponse.
    Stat { path: String },
    /// Compute BLAKE3 hash. Response: HashResponse.
    Hash { path: String, offset: u64, limit: Option<u64> },
    /// Download file from offset. Response: stream of Data, then Ok.
    Get { path: String, offset: u64 },
    /// Upload file. Followed by stream of Data, then Done. Response: Ok with hash.
    Put { path: String, size: u64 },
    /// Create directory. Response: Ok.
    Mkdir { path: String },
    /// Check resume state. Response: ResumeResponse.
    Resume { path: String },
    /// Signal end of data stream (for Put).
    Done,

    // === Responses (server → client) ===
    /// Handshake accepted.
    Welcome { version: u8 },
    /// Generic success, optional hash.
    Ok { hash: Option<[u8; 32]> },
    /// Error.
    Error { message: String },
    /// Raw data chunk (for Get/Put streaming).
    Data { payload: Vec<u8> },
    /// Stat result.
    StatResponse { size: u64, mtime: u64, is_dir: bool },
    /// Hash result.
    HashResponse { hash: [u8; 32] },
    /// Directory listing.
    ListResponse { entries: Vec<ListEntry> },
    /// Resume state.
    ResumeResponse { size: u64, block_hash: Option<[u8; 32]> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListEntry {
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
}

// Message type bytes
const MSG_HELLO: u8 = 0x01;
const MSG_LIST: u8 = 0x02;
const MSG_STAT: u8 = 0x03;
const MSG_HASH: u8 = 0x04;
const MSG_GET: u8 = 0x05;
const MSG_PUT: u8 = 0x06;
const MSG_MKDIR: u8 = 0x07;
const MSG_RESUME: u8 = 0x08;
const MSG_DONE: u8 = 0x09;

const MSG_WELCOME: u8 = 0x80;
const MSG_OK: u8 = 0x81;
const MSG_ERROR: u8 = 0x82;
const MSG_DATA: u8 = 0x83;
const MSG_STAT_RESP: u8 = 0x84;
const MSG_HASH_RESP: u8 = 0x85;
const MSG_LIST_RESP: u8 = 0x86;
const MSG_RESUME_RESP: u8 = 0x87;

pub fn encode_message(msg: &Message) -> Vec<u8> {
    let mut payload = Vec::new();
    let msg_type = match msg {
        Message::Hello { version } => {
            payload.push(*version);
            MSG_HELLO
        }
        Message::List { path } => {
            encode_string(&mut payload, path);
            MSG_LIST
        }
        Message::Stat { path } => {
            encode_string(&mut payload, path);
            MSG_STAT
        }
        Message::Hash { path, offset, limit } => {
            encode_string(&mut payload, path);
            payload.extend_from_slice(&offset.to_le_bytes());
            match limit {
                Some(l) => {
                    payload.push(1);
                    payload.extend_from_slice(&l.to_le_bytes());
                }
                None => payload.push(0),
            }
            MSG_HASH
        }
        Message::Get { path, offset } => {
            encode_string(&mut payload, path);
            payload.extend_from_slice(&offset.to_le_bytes());
            MSG_GET
        }
        Message::Put { path, size } => {
            encode_string(&mut payload, path);
            payload.extend_from_slice(&size.to_le_bytes());
            MSG_PUT
        }
        Message::Mkdir { path } => {
            encode_string(&mut payload, path);
            MSG_MKDIR
        }
        Message::Resume { path } => {
            encode_string(&mut payload, path);
            MSG_RESUME
        }
        Message::Done => MSG_DONE,
        Message::Welcome { version } => {
            payload.push(*version);
            MSG_WELCOME
        }
        Message::Ok { hash } => {
            match hash {
                Some(h) => {
                    payload.push(1);
                    payload.extend_from_slice(h);
                }
                None => payload.push(0),
            }
            MSG_OK
        }
        Message::Error { message } => {
            encode_string(&mut payload, message);
            MSG_ERROR
        }
        Message::Data { payload: data } => {
            payload.extend_from_slice(data);
            MSG_DATA
        }
        Message::StatResponse { size, mtime, is_dir } => {
            payload.extend_from_slice(&size.to_le_bytes());
            payload.extend_from_slice(&mtime.to_le_bytes());
            payload.push(if *is_dir { 1 } else { 0 });
            MSG_STAT_RESP
        }
        Message::HashResponse { hash } => {
            payload.extend_from_slice(hash);
            MSG_HASH_RESP
        }
        Message::ListResponse { entries } => {
            payload.extend_from_slice(&(entries.len() as u32).to_le_bytes());
            for entry in entries {
                encode_string(&mut payload, &entry.path);
                payload.extend_from_slice(&entry.size.to_le_bytes());
                payload.push(if entry.is_dir { 1 } else { 0 });
            }
            MSG_LIST_RESP
        }
        Message::ResumeResponse { size, block_hash } => {
            payload.extend_from_slice(&size.to_le_bytes());
            match block_hash {
                Some(h) => {
                    payload.push(1);
                    payload.extend_from_slice(h);
                }
                None => payload.push(0),
            }
            MSG_RESUME_RESP
        }
    };

    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.extend_from_slice(&(payload.len() as u32 + 1).to_le_bytes()); // +1 for type byte
    frame.push(msg_type);
    frame.extend_from_slice(&payload);
    frame
}

pub fn decode_message(data: &[u8]) -> Option<Message> {
    if data.len() < 5 {
        return None;
    }
    let payload_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    if data.len() < 4 + payload_len || payload_len == 0 {
        return None;
    }
    let msg_type = data[4];
    let payload = &data[5..4 + payload_len];
    let mut r = Reader::new(payload);

    match msg_type {
        MSG_HELLO => Some(Message::Hello { version: r.read_u8()? }),
        MSG_LIST => Some(Message::List { path: r.read_string()? }),
        MSG_STAT => Some(Message::Stat { path: r.read_string()? }),
        MSG_HASH => {
            let path = r.read_string()?;
            let offset = r.read_u64()?;
            let has_limit = r.read_u8()?;
            let limit = if has_limit == 1 { Some(r.read_u64()?) } else { None };
            Some(Message::Hash { path, offset, limit })
        }
        MSG_GET => {
            let path = r.read_string()?;
            let offset = r.read_u64()?;
            Some(Message::Get { path, offset })
        }
        MSG_PUT => {
            let path = r.read_string()?;
            let size = r.read_u64()?;
            Some(Message::Put { path, size })
        }
        MSG_MKDIR => Some(Message::Mkdir { path: r.read_string()? }),
        MSG_RESUME => Some(Message::Resume { path: r.read_string()? }),
        MSG_DONE => Some(Message::Done),
        MSG_WELCOME => Some(Message::Welcome { version: r.read_u8()? }),
        MSG_OK => {
            let has_hash = r.read_u8()?;
            let hash = if has_hash == 1 { Some(r.read_hash()?) } else { None };
            Some(Message::Ok { hash })
        }
        MSG_ERROR => Some(Message::Error { message: r.read_string()? }),
        MSG_DATA => Some(Message::Data { payload: payload.to_vec() }),
        MSG_STAT_RESP => {
            let size = r.read_u64()?;
            let mtime = r.read_u64()?;
            let is_dir = r.read_u8()? != 0;
            Some(Message::StatResponse { size, mtime, is_dir })
        }
        MSG_HASH_RESP => {
            let hash = r.read_hash()?;
            Some(Message::HashResponse { hash })
        }
        MSG_LIST_RESP => {
            let count = r.read_u32()? as usize;
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                entries.push(ListEntry {
                    path: r.read_string()?,
                    size: r.read_u64()?,
                    is_dir: r.read_u8()? != 0,
                });
            }
            Some(Message::ListResponse { entries })
        }
        MSG_RESUME_RESP => {
            let size = r.read_u64()?;
            let has_hash = r.read_u8()?;
            let block_hash = if has_hash == 1 { Some(r.read_hash()?) } else { None };
            Some(Message::ResumeResponse { size, block_hash })
        }
        _ => None,
    }
}

/// Async helpers for reading/writing framed messages on tokio streams.
pub async fn write_message<W: tokio::io::AsyncWriteExt + Unpin>(
    writer: &mut W,
    msg: &Message,
) -> std::io::Result<()> {
    let frame = encode_message(msg);
    writer.write_all(&frame).await
}

pub async fn read_message<R: tokio::io::AsyncReadExt + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<Message>> {
    let mut header = [0u8; 4];
    match reader.read_exact(&mut header).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let payload_len = u32::from_le_bytes(header) as usize;
    if payload_len == 0 {
        return Ok(None);
    }
    let mut buf = vec![0u8; payload_len];
    reader.read_exact(&mut buf).await?;

    // Reconstruct full frame for decode_message
    let mut frame = Vec::with_capacity(4 + payload_len);
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&buf);

    Ok(decode_message(&frame))
}

// --- Internal helpers ---

fn encode_string(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read_u8(&mut self) -> Option<u8> {
        if self.pos >= self.data.len() { return None; }
        let v = self.data[self.pos];
        self.pos += 1;
        Some(v)
    }

    fn read_u32(&mut self) -> Option<u32> {
        if self.pos + 4 > self.data.len() { return None; }
        let v = u32::from_le_bytes([
            self.data[self.pos], self.data[self.pos+1],
            self.data[self.pos+2], self.data[self.pos+3],
        ]);
        self.pos += 4;
        Some(v)
    }

    fn read_u64(&mut self) -> Option<u64> {
        if self.pos + 8 > self.data.len() { return None; }
        let v = u64::from_le_bytes([
            self.data[self.pos], self.data[self.pos+1], self.data[self.pos+2], self.data[self.pos+3],
            self.data[self.pos+4], self.data[self.pos+5], self.data[self.pos+6], self.data[self.pos+7],
        ]);
        self.pos += 8;
        Some(v)
    }

    fn read_string(&mut self) -> Option<String> {
        let len = self.read_u32()? as usize;
        if self.pos + len > self.data.len() { return None; }
        let s = String::from_utf8_lossy(&self.data[self.pos..self.pos + len]).into_owned();
        self.pos += len;
        Some(s)
    }

    fn read_hash(&mut self) -> Option<[u8; 32]> {
        if self.pos + 32 > self.data.len() { return None; }
        let mut h = [0u8; 32];
        h.copy_from_slice(&self.data[self.pos..self.pos + 32]);
        self.pos += 32;
        Some(h)
    }
}
```

- [ ] **Step 4: Register protocol module**

```rust
// src/core/mod.rs — add line:
pub mod protocol;
```

- [ ] **Step 5: Add lib.rs export**

```rust
// src/lib.rs — update to:
pub mod core;
```
(Already done — just confirm `protocol` is accessible via `bcmr::core::protocol`.)

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --test serve_protocol_tests 2>&1 | tail -5`
Expected: 7 tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/core/protocol.rs src/core/mod.rs tests/serve_protocol_tests.rs
git commit -m "feat(serve): add binary frame protocol for bcmr serve"
```

---

### Task 2: Serve Subcommand (Server Side)

**Files:**
- Create: `src/commands/serve.rs`
- Modify: `src/commands/mod.rs`
- Modify: `src/cli.rs`
- Modify: `src/main.rs`

The server reads requests from stdin, dispatches to filesystem operations, writes responses to stdout. All logging goes to stderr (stdout is the data channel).

- [ ] **Step 1: Add `Serve` to CLI**

In `src/cli.rs`, add to the `Commands` enum:

```rust
    /// Run as a remote helper (called via SSH, not directly by users)
    #[command(hide = true)]
    Serve,
```

- [ ] **Step 2: Create serve.rs with the main loop**

```rust
// src/commands/serve.rs
use crate::core::protocol::{self, Message, ListEntry};
use std::path::Path;
use tokio::io::{self, AsyncReadExt, AsyncWriteExt};

pub async fn run() -> anyhow::Result<()> {
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();

    // Handshake
    let hello = protocol::read_message(&mut stdin).await?;
    match hello {
        Some(Message::Hello { version }) if version == protocol::PROTOCOL_VERSION => {
            protocol::write_message(
                &mut stdout,
                &Message::Welcome { version: protocol::PROTOCOL_VERSION },
            ).await?;
            stdout.flush().await?;
        }
        _ => {
            protocol::write_message(
                &mut stdout,
                &Message::Error { message: "protocol version mismatch".into() },
            ).await?;
            stdout.flush().await?;
            return Ok(());
        }
    }

    // Main request loop
    loop {
        let msg = match protocol::read_message(&mut stdin).await? {
            Some(m) => m,
            None => break, // EOF — client disconnected
        };

        match msg {
            Message::Stat { path } => handle_stat(&mut stdout, &path).await?,
            Message::List { path } => handle_list(&mut stdout, &path).await?,
            Message::Hash { path, offset, limit } => {
                handle_hash(&mut stdout, &path, offset, limit).await?
            }
            Message::Get { path, offset } => handle_get(&mut stdout, &path, offset).await?,
            Message::Put { path, size } => {
                handle_put(&mut stdin, &mut stdout, &path, size).await?
            }
            Message::Mkdir { path } => handle_mkdir(&mut stdout, &path).await?,
            Message::Resume { path } => handle_resume(&mut stdout, &path).await?,
            _ => {
                protocol::write_message(
                    &mut stdout,
                    &Message::Error { message: format!("unexpected message: {:?}", msg) },
                ).await?;
                stdout.flush().await?;
            }
        }
    }

    Ok(())
}

async fn handle_stat<W: AsyncWriteExt + Unpin>(out: &mut W, path: &str) -> io::Result<()> {
    match tokio::fs::metadata(path).await {
        Ok(meta) => {
            let mtime = meta.modified()
                .unwrap_or(std::time::UNIX_EPOCH)
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            protocol::write_message(out, &Message::StatResponse {
                size: meta.len(),
                mtime,
                is_dir: meta.is_dir(),
            }).await?;
        }
        Err(e) => {
            protocol::write_message(out, &Message::Error {
                message: format!("stat {}: {}", path, e),
            }).await?;
        }
    }
    out.flush().await
}

async fn handle_list<W: AsyncWriteExt + Unpin>(out: &mut W, path: &str) -> io::Result<()> {
    let mut entries = Vec::new();
    let base = Path::new(path);

    match collect_entries(base, base, &mut entries).await {
        Ok(()) => {
            protocol::write_message(out, &Message::ListResponse { entries }).await?;
        }
        Err(e) => {
            protocol::write_message(out, &Message::Error {
                message: format!("list {}: {}", path, e),
            }).await?;
        }
    }
    out.flush().await
}

async fn collect_entries(
    base: &Path,
    dir: &Path,
    entries: &mut Vec<ListEntry>,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut read_dir = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = read_dir.next_entry().await? {
        let path = entry.path();
        let meta = entry.metadata().await?;
        let rel = path.strip_prefix(base).unwrap_or(&path);

        entries.push(ListEntry {
            path: rel.to_string_lossy().into_owned(),
            size: meta.len(),
            is_dir: meta.is_dir(),
        });

        if meta.is_dir() {
            Box::pin(collect_entries(base, &path, entries)).await?;
        }
    }
    Ok(())
}

async fn handle_hash<W: AsyncWriteExt + Unpin>(
    out: &mut W,
    path: &str,
    offset: u64,
    limit: Option<u64>,
) -> io::Result<()> {
    let path = path.to_string();
    let result = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut file = std::fs::File::open(&path)?;
        if offset > 0 {
            use std::io::Seek;
            file.seek(std::io::SeekFrom::Start(offset))?;
        }
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; 4 * 1024 * 1024];
        let mut total = 0u64;
        loop {
            let to_read = match limit {
                Some(l) => buf.len().min((l - total) as usize),
                None => buf.len(),
            };
            if to_read == 0 { break; }
            let n = file.read(&mut buf[..to_read])?;
            if n == 0 { break; }
            hasher.update(&buf[..n]);
            total += n as u64;
        }
        Ok::<[u8; 32], std::io::Error>(*hasher.finalize().as_bytes())
    }).await.unwrap();

    match result {
        Ok(hash) => {
            protocol::write_message(out, &Message::HashResponse { hash }).await?;
        }
        Err(e) => {
            protocol::write_message(out, &Message::Error {
                message: format!("hash: {}", e),
            }).await?;
        }
    }
    out.flush().await
}

async fn handle_get<W: AsyncWriteExt + Unpin>(
    out: &mut W,
    path: &str,
    offset: u64,
) -> io::Result<()> {
    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => {
            protocol::write_message(out, &Message::Error {
                message: format!("open {}: {}", path, e),
            }).await?;
            return out.flush().await;
        }
    };

    if offset > 0 {
        use tokio::io::AsyncSeekExt;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
    }

    // Stream data in 4MB chunks
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    let mut hasher = blake3::Hasher::new();
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
        protocol::write_message(out, &Message::Data { payload: buf[..n].to_vec() }).await?;
    }
    // Send Ok with the hash of streamed data
    let hash = *hasher.finalize().as_bytes();
    protocol::write_message(out, &Message::Ok { hash: Some(hash) }).await?;
    out.flush().await
}

async fn handle_put<R, W>(
    inp: &mut R,
    out: &mut W,
    path: &str,
    _size: u64,
) -> io::Result<()>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    // Ensure parent directory exists
    if let Some(parent) = Path::new(path).parent() {
        if !parent.exists() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
    }

    let mut file = tokio::fs::File::create(path).await?;
    let mut hasher = blake3::Hasher::new();

    // Receive data chunks until Done
    loop {
        let msg = match protocol::read_message(inp).await? {
            Some(m) => m,
            None => break,
        };
        match msg {
            Message::Data { payload } => {
                hasher.update(&payload);
                file.write_all(&payload).await?;
            }
            Message::Done => break,
            _ => {
                protocol::write_message(out, &Message::Error {
                    message: "expected Data or Done during Put".into(),
                }).await?;
                out.flush().await?;
                return Ok(());
            }
        }
    }

    file.sync_all().await?;
    let hash = *hasher.finalize().as_bytes();
    protocol::write_message(out, &Message::Ok { hash: Some(hash) }).await?;
    out.flush().await
}

async fn handle_mkdir<W: AsyncWriteExt + Unpin>(out: &mut W, path: &str) -> io::Result<()> {
    match tokio::fs::create_dir_all(path).await {
        Ok(()) => protocol::write_message(out, &Message::Ok { hash: None }).await?,
        Err(e) => {
            protocol::write_message(out, &Message::Error {
                message: format!("mkdir {}: {}", path, e),
            }).await?
        }
    }
    out.flush().await
}

async fn handle_resume<W: AsyncWriteExt + Unpin>(out: &mut W, path: &str) -> io::Result<()> {
    match tokio::fs::metadata(path).await {
        Ok(meta) => {
            let size = meta.len();
            // Hash the last 4MB block for tail-block verification
            let block_hash = if size >= 4 * 1024 * 1024 {
                let p = path.to_string();
                tokio::task::spawn_blocking(move || {
                    use std::io::{Read, Seek};
                    let mut f = std::fs::File::open(&p).ok()?;
                    let block_start = (size / (4 * 1024 * 1024) - 1) * 4 * 1024 * 1024;
                    f.seek(std::io::SeekFrom::Start(block_start)).ok()?;
                    let mut buf = vec![0u8; 4 * 1024 * 1024];
                    let n = f.read(&mut buf).ok()?;
                    if n == 4 * 1024 * 1024 {
                        Some(*blake3::hash(&buf[..n]).as_bytes())
                    } else {
                        None
                    }
                }).await.unwrap()
            } else {
                None
            };
            protocol::write_message(out, &Message::ResumeResponse { size, block_hash }).await?;
        }
        Err(_) => {
            protocol::write_message(out, &Message::ResumeResponse {
                size: 0,
                block_hash: None,
            }).await?;
        }
    }
    out.flush().await
}
```

- [ ] **Step 3: Register serve module and route command**

In `src/commands/mod.rs`:
```rust
pub mod serve;
```

In `src/main.rs`, add to the command match:
```rust
Commands::Serve => {
    commands::serve::run().await?;
}
```

- [ ] **Step 4: Build and verify compilation**

Run: `cargo build 2>&1 | grep "^error" | head -5`
Expected: No errors.

- [ ] **Step 5: Commit**

```bash
git add src/commands/serve.rs src/commands/mod.rs src/cli.rs src/main.rs
git commit -m "feat(serve): add bcmr serve server-side command"
```

---

### Task 3: Serve Client (Client Side)

**Files:**
- Create: `src/core/serve_client.rs`
- Modify: `src/core/mod.rs`

The client wraps an SSH subprocess running `bcmr serve`, providing typed request/response methods.

- [ ] **Step 1: Implement serve_client.rs**

```rust
// src/core/serve_client.rs
use crate::core::protocol::{self, Message, ListEntry, PROTOCOL_VERSION};
use crate::core::error::BcmrError;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use std::path::Path;

pub struct ServeClient {
    child: Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
}

impl ServeClient {
    /// Connect to a remote host by launching `bcmr serve` over SSH.
    pub async fn connect(ssh_target: &str) -> Result<Self, BcmrError> {
        let mut child = Command::new("ssh")
            .arg("-o").arg("BatchMode=yes")
            .arg("-o").arg("ConnectTimeout=10")
            .arg(ssh_target)
            .arg("bcmr")
            .arg("serve")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| BcmrError::InvalidInput(format!("ssh spawn failed: {}", e)))?;

        let stdin = child.stdin.take().ok_or_else(|| {
            BcmrError::InvalidInput("failed to capture ssh stdin".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            BcmrError::InvalidInput("failed to capture ssh stdout".into())
        })?;

        let mut client = Self { child, stdin, stdout };

        // Handshake
        protocol::write_message(&mut client.stdin, &Message::Hello {
            version: PROTOCOL_VERSION,
        }).await.map_err(BcmrError::Io)?;
        client.stdin.flush().await.map_err(BcmrError::Io)?;

        match protocol::read_message(&mut client.stdout).await.map_err(BcmrError::Io)? {
            Some(Message::Welcome { .. }) => Ok(client),
            Some(Message::Error { message }) => {
                Err(BcmrError::InvalidInput(format!("serve handshake failed: {}", message)))
            }
            other => {
                Err(BcmrError::InvalidInput(format!("unexpected handshake response: {:?}", other)))
            }
        }
    }

    /// Connect via local loopback (for testing).
    pub async fn connect_local() -> Result<Self, BcmrError> {
        let exe = std::env::current_exe()
            .map_err(|e| BcmrError::InvalidInput(e.to_string()))?;

        let mut child = Command::new(&exe)
            .arg("serve")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| BcmrError::InvalidInput(format!("spawn failed: {}", e)))?;

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut client = Self { child, stdin, stdout };

        protocol::write_message(&mut client.stdin, &Message::Hello {
            version: PROTOCOL_VERSION,
        }).await.map_err(BcmrError::Io)?;
        client.stdin.flush().await.map_err(BcmrError::Io)?;

        match protocol::read_message(&mut client.stdout).await.map_err(BcmrError::Io)? {
            Some(Message::Welcome { .. }) => Ok(client),
            other => Err(BcmrError::InvalidInput(format!("bad handshake: {:?}", other))),
        }
    }

    async fn send(&mut self, msg: &Message) -> Result<(), BcmrError> {
        protocol::write_message(&mut self.stdin, msg).await.map_err(BcmrError::Io)?;
        self.stdin.flush().await.map_err(BcmrError::Io)
    }

    async fn recv(&mut self) -> Result<Message, BcmrError> {
        protocol::read_message(&mut self.stdout)
            .await
            .map_err(BcmrError::Io)?
            .ok_or_else(|| BcmrError::InvalidInput("connection closed".into()))
    }

    pub async fn stat(&mut self, path: &str) -> Result<(u64, u64, bool), BcmrError> {
        self.send(&Message::Stat { path: path.into() }).await?;
        match self.recv().await? {
            Message::StatResponse { size, mtime, is_dir } => Ok((size, mtime, is_dir)),
            Message::Error { message } => Err(BcmrError::InvalidInput(message)),
            _ => Err(BcmrError::InvalidInput("unexpected response".into())),
        }
    }

    pub async fn list(&mut self, path: &str) -> Result<Vec<ListEntry>, BcmrError> {
        self.send(&Message::List { path: path.into() }).await?;
        match self.recv().await? {
            Message::ListResponse { entries } => Ok(entries),
            Message::Error { message } => Err(BcmrError::InvalidInput(message)),
            _ => Err(BcmrError::InvalidInput("unexpected response".into())),
        }
    }

    pub async fn hash(
        &mut self,
        path: &str,
        offset: u64,
        limit: Option<u64>,
    ) -> Result<[u8; 32], BcmrError> {
        self.send(&Message::Hash { path: path.into(), offset, limit }).await?;
        match self.recv().await? {
            Message::HashResponse { hash } => Ok(hash),
            Message::Error { message } => Err(BcmrError::InvalidInput(message)),
            _ => Err(BcmrError::InvalidInput("unexpected response".into())),
        }
    }

    /// Download a file. Calls `on_data` for each chunk received.
    pub async fn get(
        &mut self,
        path: &str,
        offset: u64,
        on_data: impl Fn(&[u8]),
    ) -> Result<Option<[u8; 32]>, BcmrError> {
        self.send(&Message::Get { path: path.into(), offset }).await?;
        loop {
            match self.recv().await? {
                Message::Data { payload } => on_data(&payload),
                Message::Ok { hash } => return Ok(hash),
                Message::Error { message } => return Err(BcmrError::InvalidInput(message)),
                _ => return Err(BcmrError::InvalidInput("unexpected response".into())),
            }
        }
    }

    /// Upload a file. Returns the server-computed hash.
    pub async fn put(
        &mut self,
        path: &str,
        data: &Path,
    ) -> Result<[u8; 32], BcmrError> {
        let meta = tokio::fs::metadata(data).await.map_err(BcmrError::Io)?;
        self.send(&Message::Put { path: path.into(), size: meta.len() }).await?;

        // Stream file data
        let mut file = tokio::fs::File::open(data).await.map_err(BcmrError::Io)?;
        let mut buf = vec![0u8; 4 * 1024 * 1024];
        loop {
            let n = file.read(&mut buf).await.map_err(BcmrError::Io)?;
            if n == 0 { break; }
            self.send(&Message::Data { payload: buf[..n].to_vec() }).await?;
        }
        self.send(&Message::Done).await?;

        match self.recv().await? {
            Message::Ok { hash: Some(h) } => Ok(h),
            Message::Ok { hash: None } => Err(BcmrError::InvalidInput("no hash in put response".into())),
            Message::Error { message } => Err(BcmrError::InvalidInput(message)),
            _ => Err(BcmrError::InvalidInput("unexpected response".into())),
        }
    }

    pub async fn mkdir(&mut self, path: &str) -> Result<(), BcmrError> {
        self.send(&Message::Mkdir { path: path.into() }).await?;
        match self.recv().await? {
            Message::Ok { .. } => Ok(()),
            Message::Error { message } => Err(BcmrError::InvalidInput(message)),
            _ => Err(BcmrError::InvalidInput("unexpected response".into())),
        }
    }

    pub async fn resume_check(&mut self, path: &str) -> Result<(u64, Option<[u8; 32]>), BcmrError> {
        self.send(&Message::Resume { path: path.into() }).await?;
        match self.recv().await? {
            Message::ResumeResponse { size, block_hash } => Ok((size, block_hash)),
            Message::Error { message } => Err(BcmrError::InvalidInput(message)),
            _ => Err(BcmrError::InvalidInput("unexpected response".into())),
        }
    }

    pub async fn close(mut self) -> Result<(), BcmrError> {
        drop(self.stdin); // close stdin → server sees EOF → exits
        let _ = self.child.wait().await;
        Ok(())
    }
}
```

- [ ] **Step 2: Register module**

In `src/core/mod.rs`:
```rust
pub mod serve_client;
```

- [ ] **Step 3: Build**

Run: `cargo build 2>&1 | grep "^error" | head -5`
Expected: No errors.

- [ ] **Step 4: Commit**

```bash
git add src/core/serve_client.rs src/core/mod.rs
git commit -m "feat(serve): add serve client with typed request/response API"
```

---

### Task 4: End-to-End Serve Tests (Local Loopback)

**Files:**
- Create: `tests/e2e_serve_tests.rs`

These tests use `ServeClient::connect_local()` to test the full protocol without SSH.

- [ ] **Step 1: Write e2e tests**

```rust
// tests/e2e_serve_tests.rs
use bcmr::core::serve_client::ServeClient;
use std::fs;
use std::io::Write;

fn create_file(path: &std::path::Path, size: usize) {
    let mut f = fs::File::create(path).unwrap();
    let mut remaining = size;
    let mut seed: u64 = 0xDEADBEEF;
    let mut buf = vec![0u8; 4096];
    while remaining > 0 {
        let n = remaining.min(buf.len());
        for b in buf[..n].iter_mut() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            *b = (seed >> 33) as u8;
        }
        f.write_all(&buf[..n]).unwrap();
        remaining -= n;
    }
    f.sync_all().unwrap();
}

#[tokio::test]
async fn serve_handshake() {
    let client = ServeClient::connect_local().await.unwrap();
    client.close().await.unwrap();
}

#[tokio::test]
async fn serve_stat_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.bin");
    create_file(&path, 1024);

    let mut client = ServeClient::connect_local().await.unwrap();
    let (size, _mtime, is_dir) = client.stat(path.to_str().unwrap()).await.unwrap();
    assert_eq!(size, 1024);
    assert!(!is_dir);
    client.close().await.unwrap();
}

#[tokio::test]
async fn serve_stat_nonexistent() {
    let mut client = ServeClient::connect_local().await.unwrap();
    let result = client.stat("/nonexistent/path/xyz").await;
    assert!(result.is_err());
    client.close().await.unwrap();
}

#[tokio::test]
async fn serve_list_directory() {
    let dir = tempfile::tempdir().unwrap();
    create_file(&dir.path().join("a.txt"), 100);
    create_file(&dir.path().join("b.txt"), 200);
    fs::create_dir(dir.path().join("subdir")).unwrap();
    create_file(&dir.path().join("subdir").join("c.txt"), 300);

    let mut client = ServeClient::connect_local().await.unwrap();
    let entries = client.list(dir.path().to_str().unwrap()).await.unwrap();
    assert!(entries.len() >= 3); // a.txt, b.txt, subdir, subdir/c.txt
    client.close().await.unwrap();
}

#[tokio::test]
async fn serve_hash_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hash_test.bin");
    create_file(&path, 8 * 1024 * 1024);

    let expected = bcmr::core::checksum::calculate_hash(&path).unwrap();

    let mut client = ServeClient::connect_local().await.unwrap();
    let hash = client.hash(path.to_str().unwrap(), 0, None).await.unwrap();
    let hash_hex = hash.iter().map(|b| format!("{:02x}", b)).collect::<String>();
    assert_eq!(hash_hex, expected);
    client.close().await.unwrap();
}

#[tokio::test]
async fn serve_get_download() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    create_file(&src, 10 * 1024 * 1024);

    let mut client = ServeClient::connect_local().await.unwrap();
    let dst = dir.path().join("dst.bin");
    let mut dst_file = fs::File::create(&dst).unwrap();

    let hash = client.get(src.to_str().unwrap(), 0, |data| {
        dst_file.write_all(data).unwrap();
    }).await.unwrap();
    drop(dst_file);

    // Verify hash matches
    let expected = bcmr::core::checksum::calculate_hash(&src).unwrap();
    let hash_hex = hash.unwrap().iter().map(|b| format!("{:02x}", b)).collect::<String>();
    assert_eq!(hash_hex, expected);

    // Verify file content matches
    let src_hash = bcmr::core::checksum::calculate_hash(&src).unwrap();
    let dst_hash = bcmr::core::checksum::calculate_hash(&dst).unwrap();
    assert_eq!(src_hash, dst_hash);

    client.close().await.unwrap();
}

#[tokio::test]
async fn serve_put_upload() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("upload_src.bin");
    create_file(&src, 10 * 1024 * 1024);

    let mut client = ServeClient::connect_local().await.unwrap();
    let dst_path = dir.path().join("upload_dst.bin");

    let server_hash = client.put(dst_path.to_str().unwrap(), &src).await.unwrap();

    // Verify server-computed hash matches local hash
    let local_hash = bcmr::core::checksum::calculate_hash(&src).unwrap();
    let server_hex = server_hash.iter().map(|b| format!("{:02x}", b)).collect::<String>();
    assert_eq!(server_hex, local_hash);

    // Verify content
    let dst_hash = bcmr::core::checksum::calculate_hash(&dst_path).unwrap();
    assert_eq!(dst_hash, local_hash);

    client.close().await.unwrap();
}

#[tokio::test]
async fn serve_get_with_offset() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("offset.bin");
    create_file(&src, 8 * 1024 * 1024);

    let mut client = ServeClient::connect_local().await.unwrap();
    let mut received = Vec::new();

    // Download from offset 4MB
    client.get(src.to_str().unwrap(), 4 * 1024 * 1024, |data| {
        received.extend_from_slice(data);
    }).await.unwrap();

    assert_eq!(received.len(), 4 * 1024 * 1024); // should get the second half

    // Verify content matches the second half of source
    let full = fs::read(&src).unwrap();
    assert_eq!(received, full[4 * 1024 * 1024..]);

    client.close().await.unwrap();
}

#[tokio::test]
async fn serve_mkdir() {
    let dir = tempfile::tempdir().unwrap();
    let new_dir = dir.path().join("a/b/c");

    let mut client = ServeClient::connect_local().await.unwrap();
    client.mkdir(new_dir.to_str().unwrap()).await.unwrap();
    assert!(new_dir.exists());
    client.close().await.unwrap();
}

#[tokio::test]
async fn serve_resume_check() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("partial.bin");
    create_file(&path, 20 * 1024 * 1024);

    let mut client = ServeClient::connect_local().await.unwrap();
    let (size, block_hash) = client.resume_check(path.to_str().unwrap()).await.unwrap();
    assert_eq!(size, 20 * 1024 * 1024);
    assert!(block_hash.is_some()); // file is >= 4MB, should have tail hash

    client.close().await.unwrap();
}

#[tokio::test]
async fn serve_resume_nonexistent() {
    let mut client = ServeClient::connect_local().await.unwrap();
    let (size, hash) = client.resume_check("/nonexistent/file").await.unwrap();
    assert_eq!(size, 0);
    assert!(hash.is_none());
    client.close().await.unwrap();
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --test e2e_serve_tests 2>&1 | tail -15`
Expected: 10 tests pass.

- [ ] **Step 3: Commit**

```bash
git add tests/e2e_serve_tests.rs
git commit -m "test(serve): add end-to-end serve protocol tests via local loopback"
```

---

### Task 5: Wire Serve Client into Remote Copy

**Files:**
- Modify: `src/commands/remote_copy.rs`
- Modify: `src/core/remote.rs`

Add a detection step: before using legacy SSH commands, try to connect via `bcmr serve`. If it works, use the serve protocol. If not (remote doesn't have bcmr), fall back to legacy.

- [ ] **Step 1: Add serve detection to remote_copy.rs**

At the top of `handle_remote_copy`, after SSH validation, try to establish a serve connection:

```rust
// In handle_remote_copy, after validate_ssh_connection:
let serve_available = {
    match ServeClient::connect(&check_target.ssh_target()).await {
        Ok(client) => {
            let _ = client.close().await;
            true
        }
        Err(_) => false,
    }
};

if serve_available {
    eprintln!("(using bcmr serve protocol)");
    // Use serve-based transfer
} else {
    // Legacy SSH path (existing code)
}
```

This is a **detection-only** step. The actual serve-based transfer functions are wired in Task 6.

- [ ] **Step 2: Build and verify**

Run: `cargo build 2>&1 | grep "^error" | head -5`
Expected: No errors.

- [ ] **Step 3: Commit**

```bash
git add src/commands/remote_copy.rs
git commit -m "feat(serve): add serve protocol detection in remote copy path"
```

---

### Task 6: Serve-Based Upload/Download

**Files:**
- Modify: `src/commands/remote_copy.rs`

Implement `handle_serve_upload` and `handle_serve_download` that use the serve client instead of spawning SSH processes.

- [ ] **Step 1: Implement serve upload**

```rust
async fn handle_serve_upload(
    ssh_target: &str,
    sources: &[std::path::PathBuf],
    rdest: &RemotePath,
    progress: &ProgressRunner,
    recursive: bool,
) -> Result<()> {
    let mut client = ServeClient::connect(ssh_target).await?;

    for src in sources {
        if src.is_file() {
            let remote_path = format!("{}/{}", rdest.path,
                src.file_name().unwrap_or_default().to_string_lossy());
            let size = src.metadata()?.len();
            (progress.file_callback())(
                &src.file_name().unwrap_or_default().to_string_lossy(), size);

            let server_hash = client.put(&remote_path, src).await?;

            // Verify against local hash
            let local_hash = {
                let p = src.clone();
                tokio::task::spawn_blocking(move || {
                    crate::core::checksum::calculate_hash(&p)
                }).await??
            };
            let server_hex: String = server_hash.iter()
                .map(|b| format!("{:02x}", b)).collect();
            if server_hex != local_hash {
                bail!("hash mismatch for {}", src.display());
            }
            (progress.inc_callback())(size);
        } else if src.is_dir() && recursive {
            // Recursive: list local files, mkdir remote dirs, put files
            upload_dir_via_serve(&mut client, src, rdest, progress).await?;
        }
    }

    client.close().await?;
    Ok(())
}
```

- [ ] **Step 2: Implement serve download**

Similar pattern: `client.stat()` → `client.get()` with offset support for resume.

- [ ] **Step 3: Wire into handle_remote_copy**

Replace the `if serve_available` block with actual calls.

- [ ] **Step 4: Test manually via SSH to a remote with bcmr installed**

Run: `bcmr copy local_file user@remote:/tmp/test`
Expected: Output shows "(using bcmr serve protocol)", file transfers correctly.

- [ ] **Step 5: Commit**

```bash
git add src/commands/remote_copy.rs
git commit -m "feat(serve): implement serve-based upload and download"
```

---

### Task 7: Add Deploy Subcommand

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/main.rs`
- Create: `src/commands/deploy.rs`
- Modify: `src/commands/mod.rs`

- [ ] **Step 1: Add Deploy to CLI**

```rust
// In Commands enum:
    /// Deploy bcmr to a remote host
    Deploy {
        /// Remote target (user@host)
        target: String,

        /// Installation path on remote
        #[arg(long, default_value = "~/.local/bin/bcmr")]
        path: Option<String>,
    },
```

- [ ] **Step 2: Implement deploy.rs**

Logic:
1. `ssh target 'uname -sm'` → detect OS + arch
2. Compare with local OS + arch
3. If same → scp local binary to target
4. If different → download from GitHub Release, then scp
5. `ssh target 'chmod +x path && path --version'` → verify

- [ ] **Step 3: Test**

Run: `bcmr deploy user@host`
Expected: Binary deployed, version printed.

- [ ] **Step 4: Commit**

```bash
git add src/commands/deploy.rs src/commands/mod.rs src/cli.rs src/main.rs
git commit -m "feat: add bcmr deploy command for remote installation"
```

---

### Task 8: Final Integration + CI

- [ ] **Step 1: Run full test suite**

Run: `cargo test 2>&1 | grep "test result"`
Expected: All suites pass.

- [ ] **Step 2: Run CI checks**

Run: `cargo fmt && cargo clippy -- -D warnings && cargo fmt --check`
Expected: Clean.

- [ ] **Step 3: Push and verify CI**

```bash
git push
gh run list --limit 1
```
Expected: CI green.
