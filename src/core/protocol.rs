use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const PROTOCOL_VERSION: u8 = 1;

// Message type discriminants
const TYPE_HELLO: u8 = 0x01;
const TYPE_LIST: u8 = 0x02;
const TYPE_STAT: u8 = 0x03;
const TYPE_HASH: u8 = 0x04;
const TYPE_GET: u8 = 0x05;
const TYPE_PUT: u8 = 0x06;
const TYPE_MKDIR: u8 = 0x07;
const TYPE_RESUME: u8 = 0x08;
const TYPE_DONE: u8 = 0x09;

const TYPE_WELCOME: u8 = 0x81;
const TYPE_OK: u8 = 0x82;
const TYPE_ERROR: u8 = 0x83;
const TYPE_DATA: u8 = 0x84;
const TYPE_STAT_RESPONSE: u8 = 0x85;
const TYPE_HASH_RESPONSE: u8 = 0x86;
const TYPE_LIST_RESPONSE: u8 = 0x87;
const TYPE_RESUME_RESPONSE: u8 = 0x88;

#[derive(Debug, PartialEq, Clone)]
pub struct ListEntry {
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Message {
    // Requests (client → server)
    Hello {
        version: u8,
    },
    List {
        path: String,
    },
    Stat {
        path: String,
    },
    Hash {
        path: String,
        offset: u64,
        limit: Option<u64>,
    },
    Get {
        path: String,
        offset: u64,
    },
    Put {
        path: String,
        size: u64,
    },
    Mkdir {
        path: String,
    },
    Resume {
        path: String,
    },
    Done,

    // Responses (server → client)
    Welcome {
        version: u8,
    },
    Ok {
        hash: Option<String>,
    },
    Error {
        message: String,
    },
    Data {
        payload: Vec<u8>,
    },
    StatResponse {
        size: u64,
        mtime: i64,
        is_dir: bool,
    },
    HashResponse {
        hash: String,
    },
    ListResponse {
        entries: Vec<ListEntry>,
    },
    ResumeResponse {
        size: u64,
        block_hash: Option<String>,
    },
}

// --- Encoding helpers ---

fn write_u8(buf: &mut Vec<u8>, v: u8) {
    buf.push(v);
}

fn write_u32_le(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_u64_le(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_i64_le(buf: &mut Vec<u8>, v: i64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_string(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    write_u32_le(buf, bytes.len() as u32);
    buf.extend_from_slice(bytes);
}

fn write_bytes(buf: &mut Vec<u8>, data: &[u8]) {
    write_u32_le(buf, data.len() as u32);
    buf.extend_from_slice(data);
}

fn write_opt_string(buf: &mut Vec<u8>, opt: &Option<String>) {
    match opt {
        Some(s) => {
            write_u8(buf, 1);
            write_string(buf, s);
        }
        None => write_u8(buf, 0),
    }
}

fn write_opt_u64(buf: &mut Vec<u8>, opt: &Option<u64>) {
    match opt {
        Some(v) => {
            write_u8(buf, 1);
            write_u64_le(buf, *v);
        }
        None => write_u8(buf, 0),
    }
}

fn write_list_entry(buf: &mut Vec<u8>, entry: &ListEntry) {
    write_string(buf, &entry.path);
    write_u64_le(buf, entry.size);
    write_u8(buf, entry.is_dir as u8);
}

/// Encode `msg` into a framed byte vector: `[4 LE payload_len][1 type][payload...]`.
pub fn encode_message(msg: &Message) -> Vec<u8> {
    let mut payload = Vec::new();

    match msg {
        Message::Hello { version } => {
            write_u8(&mut payload, TYPE_HELLO);
            write_u8(&mut payload, *version);
        }
        Message::List { path } => {
            write_u8(&mut payload, TYPE_LIST);
            write_string(&mut payload, path);
        }
        Message::Stat { path } => {
            write_u8(&mut payload, TYPE_STAT);
            write_string(&mut payload, path);
        }
        Message::Hash {
            path,
            offset,
            limit,
        } => {
            write_u8(&mut payload, TYPE_HASH);
            write_string(&mut payload, path);
            write_u64_le(&mut payload, *offset);
            write_opt_u64(&mut payload, limit);
        }
        Message::Get { path, offset } => {
            write_u8(&mut payload, TYPE_GET);
            write_string(&mut payload, path);
            write_u64_le(&mut payload, *offset);
        }
        Message::Put { path, size } => {
            write_u8(&mut payload, TYPE_PUT);
            write_string(&mut payload, path);
            write_u64_le(&mut payload, *size);
        }
        Message::Mkdir { path } => {
            write_u8(&mut payload, TYPE_MKDIR);
            write_string(&mut payload, path);
        }
        Message::Resume { path } => {
            write_u8(&mut payload, TYPE_RESUME);
            write_string(&mut payload, path);
        }
        Message::Done => {
            write_u8(&mut payload, TYPE_DONE);
        }
        Message::Welcome { version } => {
            write_u8(&mut payload, TYPE_WELCOME);
            write_u8(&mut payload, *version);
        }
        Message::Ok { hash } => {
            write_u8(&mut payload, TYPE_OK);
            write_opt_string(&mut payload, hash);
        }
        Message::Error { message } => {
            write_u8(&mut payload, TYPE_ERROR);
            write_string(&mut payload, message);
        }
        Message::Data { payload: data } => {
            write_u8(&mut payload, TYPE_DATA);
            write_bytes(&mut payload, data);
        }
        Message::StatResponse {
            size,
            mtime,
            is_dir,
        } => {
            write_u8(&mut payload, TYPE_STAT_RESPONSE);
            write_u64_le(&mut payload, *size);
            write_i64_le(&mut payload, *mtime);
            write_u8(&mut payload, *is_dir as u8);
        }
        Message::HashResponse { hash } => {
            write_u8(&mut payload, TYPE_HASH_RESPONSE);
            write_string(&mut payload, hash);
        }
        Message::ListResponse { entries } => {
            write_u8(&mut payload, TYPE_LIST_RESPONSE);
            write_u32_le(&mut payload, entries.len() as u32);
            for entry in entries {
                write_list_entry(&mut payload, entry);
            }
        }
        Message::ResumeResponse { size, block_hash } => {
            write_u8(&mut payload, TYPE_RESUME_RESPONSE);
            write_u64_le(&mut payload, *size);
            write_opt_string(&mut payload, block_hash);
        }
    }

    let mut frame = Vec::with_capacity(4 + payload.len());
    write_u32_le(&mut frame, payload.len() as u32);
    frame.extend_from_slice(&payload);
    frame
}

// --- Decoding helpers ---

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read_u8(&mut self) -> Option<u8> {
        if self.pos >= self.data.len() {
            return None;
        }
        let v = self.data[self.pos];
        self.pos += 1;
        Some(v)
    }

    fn read_u32_le(&mut self) -> Option<u32> {
        let bytes = self.data.get(self.pos..self.pos + 4)?;
        self.pos += 4;
        Some(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn read_u64_le(&mut self) -> Option<u64> {
        let bytes = self.data.get(self.pos..self.pos + 8)?;
        self.pos += 8;
        Some(u64::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn read_i64_le(&mut self) -> Option<i64> {
        let bytes = self.data.get(self.pos..self.pos + 8)?;
        self.pos += 8;
        Some(i64::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn read_string(&mut self) -> Option<String> {
        let len = self.read_u32_le()? as usize;
        let bytes = self.data.get(self.pos..self.pos + len)?;
        self.pos += len;
        String::from_utf8(bytes.to_vec()).ok()
    }

    fn read_bytes(&mut self) -> Option<Vec<u8>> {
        let len = self.read_u32_le()? as usize;
        let bytes = self.data.get(self.pos..self.pos + len)?;
        self.pos += len;
        Some(bytes.to_vec())
    }

    fn read_opt_string(&mut self) -> Option<Option<String>> {
        let present = self.read_u8()?;
        if present == 1 {
            Some(Some(self.read_string()?))
        } else {
            Some(None)
        }
    }

    fn read_opt_u64(&mut self) -> Option<Option<u64>> {
        let present = self.read_u8()?;
        if present == 1 {
            Some(Some(self.read_u64_le()?))
        } else {
            Some(None)
        }
    }

    fn read_list_entry(&mut self) -> Option<ListEntry> {
        let path = self.read_string()?;
        let size = self.read_u64_le()?;
        let is_dir = self.read_u8()? != 0;
        Some(ListEntry { path, size, is_dir })
    }
}

/// Decode a complete framed message from `data`.
///
/// `data` must begin with the 4-byte LE payload length followed by the payload.
/// Returns `None` for empty input, truncated frames, or unknown/malformed messages.
pub fn decode_message(data: &[u8]) -> Option<Message> {
    if data.is_empty() {
        return None;
    }

    let mut c = Cursor::new(data);
    let payload_len = c.read_u32_le()? as usize;
    let payload = data.get(c.pos..c.pos + payload_len)?;

    let mut p = Cursor::new(payload);
    let msg_type = p.read_u8()?;

    let msg = match msg_type {
        TYPE_HELLO => Message::Hello {
            version: p.read_u8()?,
        },
        TYPE_LIST => Message::List {
            path: p.read_string()?,
        },
        TYPE_STAT => Message::Stat {
            path: p.read_string()?,
        },
        TYPE_HASH => Message::Hash {
            path: p.read_string()?,
            offset: p.read_u64_le()?,
            limit: p.read_opt_u64()?,
        },
        TYPE_GET => Message::Get {
            path: p.read_string()?,
            offset: p.read_u64_le()?,
        },
        TYPE_PUT => Message::Put {
            path: p.read_string()?,
            size: p.read_u64_le()?,
        },
        TYPE_MKDIR => Message::Mkdir {
            path: p.read_string()?,
        },
        TYPE_RESUME => Message::Resume {
            path: p.read_string()?,
        },
        TYPE_DONE => Message::Done,
        TYPE_WELCOME => Message::Welcome {
            version: p.read_u8()?,
        },
        TYPE_OK => Message::Ok {
            hash: p.read_opt_string()?,
        },
        TYPE_ERROR => Message::Error {
            message: p.read_string()?,
        },
        TYPE_DATA => Message::Data {
            payload: p.read_bytes()?,
        },
        TYPE_STAT_RESPONSE => Message::StatResponse {
            size: p.read_u64_le()?,
            mtime: p.read_i64_le()?,
            is_dir: p.read_u8()? != 0,
        },
        TYPE_HASH_RESPONSE => Message::HashResponse {
            hash: p.read_string()?,
        },
        TYPE_LIST_RESPONSE => {
            let count = p.read_u32_le()? as usize;
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                entries.push(p.read_list_entry()?);
            }
            Message::ListResponse { entries }
        }
        TYPE_RESUME_RESPONSE => Message::ResumeResponse {
            size: p.read_u64_le()?,
            block_hash: p.read_opt_string()?,
        },
        _ => return None,
    };

    Some(msg)
}

/// Write a framed message to an async writer.
pub async fn write_message<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    msg: &Message,
) -> io::Result<()> {
    let frame = encode_message(msg);
    writer.write_all(&frame).await
}

/// Read a framed message from an async reader.
///
/// Returns `Ok(None)` on clean EOF (zero bytes read for the length prefix).
pub async fn read_message<R: AsyncReadExt + Unpin>(reader: &mut R) -> io::Result<Option<Message>> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }

    let payload_len = u32::from_le_bytes(len_buf) as usize;

    // Guard against malicious/corrupt peers sending huge frame sizes.
    const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024; // 16 MiB
    if payload_len > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "frame too large: {} bytes (max {})",
                payload_len, MAX_FRAME_SIZE
            ),
        ));
    }

    let mut payload = vec![0u8; payload_len];
    reader.read_exact(&mut payload).await?;

    // Reconstruct the framed buffer so decode_message can parse it uniformly.
    let mut frame = Vec::with_capacity(4 + payload_len);
    frame.extend_from_slice(&len_buf);
    frame.extend_from_slice(&payload);

    decode_message(&frame)
        .map(Some)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed protocol message"))
}
