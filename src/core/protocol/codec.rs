use super::{
    compressed_block_size, validate_content_block_size, ListEntry, Message, TYPE_AUTH_CHALLENGE,
    TYPE_AUTH_HELLO, TYPE_DATA, TYPE_DATA_COMPRESSED, TYPE_DIRECT_READY, TYPE_DONE, TYPE_ERROR,
    TYPE_GET, TYPE_GET_CHUNKED, TYPE_HASH, TYPE_HASH_RESPONSE, TYPE_HAVE_BLOCKS, TYPE_HELLO,
    TYPE_LIST, TYPE_LIST_RESPONSE, TYPE_MISSING_BLOCKS, TYPE_MKDIR, TYPE_OK, TYPE_OPEN_DIRECT,
    TYPE_PUT, TYPE_PUT_CHUNKED, TYPE_RESUME, TYPE_RESUME_RESPONSE, TYPE_STAT, TYPE_STAT_RESPONSE,
    TYPE_TRUNCATE, TYPE_WELCOME,
};

// Caps Vec preallocation on peer-supplied u32 counts; prevents OOM DoS.
const MAX_DECODE_COUNT: usize = 1_048_576;

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
    write_i64_le(buf, entry.mtime);
    write_u8(buf, entry.is_dir as u8);
}

pub fn encode_message(msg: &Message) -> Vec<u8> {
    let mut payload = Vec::new();

    match msg {
        Message::Hello { version, caps } => {
            write_u8(&mut payload, TYPE_HELLO);
            write_u8(&mut payload, *version);
            write_u8(&mut payload, *caps);
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
        Message::Put { path, size, offset } => {
            write_u8(&mut payload, TYPE_PUT);
            write_string(&mut payload, path);
            write_u64_le(&mut payload, *size);
            write_u64_le(&mut payload, *offset);
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
        Message::Welcome { version, caps } => {
            write_u8(&mut payload, TYPE_WELCOME);
            write_u8(&mut payload, *version);
            write_u8(&mut payload, *caps);
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
        Message::DataCompressed {
            algo,
            original_size,
            payload: data,
        } => {
            write_u8(&mut payload, TYPE_DATA_COMPRESSED);
            write_u8(&mut payload, *algo);
            write_u32_le(&mut payload, *original_size);
            write_bytes(&mut payload, data);
        }
        Message::HaveBlocks { block_size, hashes } => {
            write_u8(&mut payload, TYPE_HAVE_BLOCKS);
            write_u32_le(&mut payload, *block_size);
            write_u32_le(&mut payload, hashes.len() as u32);
            for h in hashes {
                payload.extend_from_slice(h);
            }
        }
        Message::MissingBlocks { bits } => {
            write_u8(&mut payload, TYPE_MISSING_BLOCKS);
            write_bytes(&mut payload, bits);
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
        Message::OpenDirectChannel => {
            write_u8(&mut payload, TYPE_OPEN_DIRECT);
        }
        Message::AuthHello { mac } => {
            write_u8(&mut payload, TYPE_AUTH_HELLO);
            payload.extend_from_slice(mac);
        }
        Message::DirectChannelReady { addr, session_key } => {
            write_u8(&mut payload, TYPE_DIRECT_READY);
            write_string(&mut payload, addr);
            payload.extend_from_slice(session_key);
        }
        Message::AuthChallenge { nonce } => {
            write_u8(&mut payload, TYPE_AUTH_CHALLENGE);
            payload.extend_from_slice(nonce);
        }
        Message::PutChunked {
            path,
            offset,
            length,
        } => {
            write_u8(&mut payload, TYPE_PUT_CHUNKED);
            write_string(&mut payload, path);
            write_u64_le(&mut payload, *offset);
            write_u64_le(&mut payload, *length);
        }
        Message::GetChunked {
            path,
            offset,
            length,
        } => {
            write_u8(&mut payload, TYPE_GET_CHUNKED);
            write_string(&mut payload, path);
            write_u64_le(&mut payload, *offset);
            write_u64_le(&mut payload, *length);
        }
        Message::Truncate { path, size } => {
            write_u8(&mut payload, TYPE_TRUNCATE);
            write_string(&mut payload, path);
            write_u64_le(&mut payload, *size);
        }
    }

    let mut frame = Vec::with_capacity(4 + payload.len());
    write_u32_le(&mut frame, payload.len() as u32);
    frame.extend_from_slice(&payload);
    frame
}

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

    fn read_content_block(&mut self) -> Option<Vec<u8>> {
        let len = self.read_u32_le()? as usize;
        validate_content_block_size(len).ok()?;
        let bytes = self.data.get(self.pos..self.pos + len)?;
        self.pos += len;
        Some(bytes.to_vec())
    }

    fn read_fixed<const N: usize>(&mut self) -> Option<[u8; N]> {
        let slice = self.data.get(self.pos..self.pos + N)?;
        self.pos += N;
        let mut out = [0u8; N];
        out.copy_from_slice(slice);
        Some(out)
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
        let mtime = self.read_i64_le()?;
        let is_dir = self.read_u8()? != 0;
        Some(ListEntry {
            path,
            size,
            mtime,
            is_dir,
        })
    }
}

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
            caps: p.read_u8().unwrap_or(0),
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
            offset: p.read_u64_le()?,
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
            caps: p.read_u8().unwrap_or(0),
        },
        TYPE_OK => Message::Ok {
            hash: p.read_opt_string()?,
        },
        TYPE_ERROR => Message::Error {
            message: p.read_string()?,
        },
        TYPE_DATA => Message::Data {
            payload: p.read_content_block()?,
        },
        TYPE_DATA_COMPRESSED => {
            let algo = p.read_u8()?;
            let original_size = p.read_u32_le()?;
            compressed_block_size(algo, original_size).ok()?;
            Message::DataCompressed {
                algo,
                original_size,
                payload: p.read_bytes()?,
            }
        }
        TYPE_HAVE_BLOCKS => {
            let block_size = p.read_u32_le()?;
            let count = p.read_u32_le()? as usize;
            if count > MAX_DECODE_COUNT {
                return None;
            }
            let mut hashes = Vec::with_capacity(count);
            for _ in 0..count {
                let mut h = [0u8; 32];
                for byte in &mut h {
                    *byte = p.read_u8()?;
                }
                hashes.push(h);
            }
            Message::HaveBlocks { block_size, hashes }
        }
        TYPE_MISSING_BLOCKS => Message::MissingBlocks {
            bits: p.read_bytes()?,
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
            if count > MAX_DECODE_COUNT {
                return None;
            }
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
        TYPE_OPEN_DIRECT => Message::OpenDirectChannel,
        TYPE_AUTH_HELLO => Message::AuthHello {
            mac: p.read_fixed::<32>()?,
        },
        TYPE_DIRECT_READY => Message::DirectChannelReady {
            addr: p.read_string()?,
            session_key: p.read_fixed::<32>()?,
        },
        TYPE_AUTH_CHALLENGE => Message::AuthChallenge {
            nonce: p.read_fixed::<32>()?,
        },
        TYPE_PUT_CHUNKED => Message::PutChunked {
            path: p.read_string()?,
            offset: p.read_u64_le()?,
            length: p.read_u64_le()?,
        },
        TYPE_GET_CHUNKED => Message::GetChunked {
            path: p.read_string()?,
            offset: p.read_u64_le()?,
            length: p.read_u64_le()?,
        },
        TYPE_TRUNCATE => Message::Truncate {
            path: p.read_string()?,
            size: p.read_u64_le()?,
        },
        _ => return None,
    };

    Some(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_with_payload(payload: Vec<u8>) -> Vec<u8> {
        let mut frame = Vec::with_capacity(4 + payload.len());
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&payload);
        frame
    }

    #[test]
    fn decode_rejects_have_blocks_count_above_cap() {
        let mut payload = vec![TYPE_HAVE_BLOCKS];
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&u32::MAX.to_le_bytes());
        let frame = frame_with_payload(payload);
        assert!(decode_message(&frame).is_none());
    }

    #[test]
    fn decode_rejects_list_response_count_above_cap() {
        let mut payload = vec![TYPE_LIST_RESPONSE];
        payload.extend_from_slice(&u32::MAX.to_le_bytes());
        let frame = frame_with_payload(payload);
        assert!(decode_message(&frame).is_none());
    }
}
